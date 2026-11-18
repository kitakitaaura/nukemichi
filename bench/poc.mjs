// usage: node bench/poc.mjs <chromium|firefox|webkit> <webrtc|websocket>
import { chromium, firefox, webkit } from 'playwright';
const [engineName = 'chromium', transport = 'websocket'] = process.argv.slice(2);
const bt = { chromium, firefox, webkit }[engineName];
const kill = setTimeout(() => { console.log('TIMEOUT'); process.exit(2); }, 300000);
const b = await bt.launch();
const p = await b.newPage();
p.on('console', (m) => console.log('[console]', m.text().slice(0, 300)));
p.on('pageerror', (e) => console.log('[pageerror]', e.message.slice(0, 300)));
await p.goto('http://localhost:8766/demo/poc.html');
try {
  const t = await p.evaluate((tr) => window.boot({ transport: tr, storage: 'memory', logLevel: 'info' }), transport);
  console.log('BOOT', JSON.stringify(t));
  const r = await p.evaluate(async () => {
    const t0 = performance.now();
    const res = await window.engine.fetch('https://check.torproject.org/api/ip');
    return { ms: performance.now() - t0, status: res.status, body: await res.text() };
  });
  console.log('CHECK', JSON.stringify(r));
} catch (e) {
  console.log('ERR', e.message.slice(0, 800));
}
console.log((await p.evaluate(() => window.logs.filter(l => !/\[(debug|trace)\]/.test(l)).slice(-40).join('\n'))));
clearTimeout(kill);
await b.close();
