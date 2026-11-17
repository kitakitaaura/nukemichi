// A page opened straight from disk (file://) that loads Nukemichi from a CDN
// with default options. Workers cannot start there, so this checks the
// fallback to the main thread. Pass CDN=<url> to test a published copy.
// Usage: node bench/filepage.mjs <chromium|firefox|webkit> <scratch dir>
import http from 'node:http';
import { readFile, writeFile } from 'node:fs/promises';
import { extname, join, normalize } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium, firefox, webkit } from 'playwright';

const [engineName = 'chromium', dir] = process.argv.slice(2);
const ROOT = fileURLToPath(new URL('..', import.meta.url));
const TYPES = { '.js': 'text/javascript', '.wasm': 'application/wasm' };
const CDN = process.env.CDN ?? 'http://localhost:8767';
const server = process.env.CDN ? null : http.createServer(async (req, res) => {
  const path = normalize(decodeURIComponent(new URL(req.url, CDN).pathname));
  try {
    const body = await readFile(join(ROOT, path));
    res.writeHead(200, { 'content-type': TYPES[extname(path)] ?? 'application/octet-stream', 'access-control-allow-origin': '*' }).end(body);
  } catch {
    res.writeHead(404).end();
  }
}).listen(8767);

const page = join(dir, 'filepage.html');
await writeFile(page, `<!doctype html><script src="${CDN}/js/src/global.js"></script>
<script>
window.logs = [];
nukemichi.configure({ logLevel: 'warn', log: (l, t, m) => logs.push(l + ' ' + m) });
window.result = nukemichi.fetch('https://check.torproject.org/api/ip').then((r) => r.json());
</script>`);

const b = await { chromium, firefox, webkit }[engineName].launch();
const p = await b.newPage();
await p.goto(`file://${page}`);
const t0 = Date.now();
const out = await p.evaluate(() => Promise.race([
  window.result.then((j) => ({ isTor: j.IsTor }), (e) => ({ error: e.message })),
  new Promise((ok) => setTimeout(() => ok({ error: 'timeout 150s' }), 150000)),
]));
out.ms = Date.now() - t0;
out.logs = (await p.evaluate(() => window.logs)).filter((l) => /nukemichi|worker|main thread/.test(l)).slice(0, 3);
console.log(`${engineName} ${b.version()}`, JSON.stringify(out));
await b.close();
server?.close();
