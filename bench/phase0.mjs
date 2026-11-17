// Phase 0 proofs: IsTor, no-CORS read, raw TCP echo.
// Usage: node bench/phase0.mjs <chromium|firefox|webkit> <webrtc|websocket>
import { chromium, firefox, webkit } from 'playwright';
const [engineName = 'chromium', transport = 'websocket'] = process.argv.slice(2);
const bt = { chromium, firefox, webkit }[engineName];
const kill = setTimeout(() => { console.log('TIMEOUT'); process.exit(2); }, 400000);
const b = await bt.launch(process.env.CHANNEL ? { channel: process.env.CHANNEL } : {});
const p = await b.newPage();
await p.goto('http://localhost:8766/demo/poc.html');
const out = { browser: `${engineName} ${b.version()}`, transport };
try {
  out.boot = await p.evaluate((tr) => window.boot({ transport: tr, storage: 'memory', logLevel: 'info' }), transport);
  out.results = await p.evaluate(async () => {
    const r = {};
    const e = window.engine;
    // 1. IsTor
    let res = await e.fetch('https://check.torproject.org/api/ip');
    r.checkTorproject = await res.json();
    // 2. No-CORS endpoint: plain fetch must fail, Tor path must succeed.
    const url = 'https://example.com/';
    try { await fetch(url); r.plainFetch = 'unexpectedly succeeded'; }
    catch (err) { r.plainFetch = `${err.name}: ${err.message}`; }
    res = await e.fetch(url);
    const body = await res.text();
    r.torFetch = { status: res.status, acao: res.headers.get('access-control-allow-origin'), bytes: body.length, title: body.match(/<title>(.*?)<\/title>/)?.[1] };
    // 3. Raw TCP echo (tcpbin.com:4242).
    const t0 = performance.now();
    const s = await e.connect('tcpbin.com', 4242);
    const connectMs = performance.now() - t0;
    const w = s.writable.getWriter();
    const rd = s.readable.getReader();
    const msg = `nukemichi raw tcp ${Date.now()}\n`;
    const t1 = performance.now();
    await w.write(new TextEncoder().encode(msg));
    let got = '';
    while (!got.includes('\n')) {
      const { value, done } = await rd.read();
      if (done) break;
      got += new TextDecoder().decode(value);
    }
    r.tcpEcho = { sent: msg.trim(), got: got.trim(), equal: got === msg, connectMs: Math.round(connectMs), echoRttMs: Math.round(performance.now() - t1) };
    await w.close().catch(() => {});
    return r;
  });
} catch (err) {
  out.error = err.message.slice(0, 600);
  out.tail = await p.evaluate(() => window.logs.slice(-25).join('\n')).catch(() => '');
}
console.log(JSON.stringify(out, null, 1));
clearTimeout(kill);
await b.close();
