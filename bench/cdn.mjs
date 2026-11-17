// Load Nukemichi the way a CDN user would: the page on one origin, the library
// on another that sends Access-Control-Allow-Origin: * (as jsDelivr does).
// Checks the <script> global (full and bare package URL) and the ES module, one request each.
// Usage: node bench/cdn.mjs <chromium|firefox|webkit> [webrtc|websocket]
import http from 'node:http';
import { readFile } from 'node:fs/promises';
import { extname, join, normalize } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium, firefox, webkit } from 'playwright';

const [engineName = 'chromium', transport = 'websocket'] = process.argv.slice(2);
const ROOT = fileURLToPath(new URL('..', import.meta.url));
const TYPES = { '.js': 'text/javascript', '.wasm': 'application/wasm', '.html': 'text/html' };
// CDN=https://cdn.jsdelivr.net/npm/nukemichi@0.1.0 tests the published package instead.
const CDN = process.env.CDN ?? 'http://localhost:8767';

const cdn = process.env.CDN ? null : http.createServer(async (req, res) => {
  let path = normalize(decodeURIComponent(new URL(req.url, CDN).pathname));
  if (path === '/') path = '/js/src/global.js'; // the CDN's default file for a bare package URL
  try {
    const body = await readFile(join(ROOT, path));
    res.writeHead(200, { 'content-type': TYPES[extname(path)] ?? 'application/octet-stream', 'access-control-allow-origin': '*' });
    res.end(body);
  } catch {
    res.writeHead(404).end();
  }
}).listen(8767);

const RUN = `async function run(n) {
  n.configure({ transport: ${JSON.stringify(transport)}, storage: 'memory' });
  const t0 = performance.now();
  const res = await n.fetch('https://check.torproject.org/api/ip');
  return { ms: Math.round(performance.now() - t0), body: await res.json() };
}`;
const PAGES = {
  '/global.html': `<!doctype html><script src="${CDN}/js/src/global.js"></script>
<script>${RUN}; window.result = run(window.nukemichi);</script>`,
  '/bare.html': `<!doctype html><script src="${process.env.CDN ? CDN : `${CDN}/`}"></script>
<script>${RUN}; window.result = run(window.nukemichi);</script>`,
  '/esm.html': `<!doctype html><script type="module">
import nukemichi from '${CDN}/js/src/index.js';
${RUN}; window.result = run(nukemichi);</script>`,
};
const page = http.createServer((req, res) => {
  const html = PAGES[req.url];
  res.writeHead(html ? 200 : 404, { 'content-type': 'text/html' }).end(html);
}).listen(8768);

const b = await { chromium, firefox, webkit }[engineName].launch();
const out = { browser: `${engineName} ${b.version()}`, transport };
for (const path of Object.keys(PAGES)) {
  const p = await b.newPage();
  const errors = [];
  p.on('pageerror', (e) => errors.push(e.message));
  await p.goto(`http://localhost:8768${path}`);
  try {
    await p.waitForFunction(() => !!window.result, null, { timeout: 10000 });
    out[path] = await p.evaluate(() => Promise.race([
      window.result,
      new Promise((_, rej) => setTimeout(() => rej(new Error('timeout 120s')), 120000)),
    ]));
  } catch (e) {
    out[path] = { error: e.message.split('\n')[0] };
  }
  if (errors.length) out[path].pageErrors = errors;
  await p.close();
}
console.log(JSON.stringify(out, null, 1));
await b.close();
cdn?.close();
page.close();
