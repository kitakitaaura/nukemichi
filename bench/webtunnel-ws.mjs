// Can a browser WebSocket talk to a WebTunnel server? Runs WebTunnel's own
// httpupgrade.Server (local echo, scratch/wtecho) and tries from 3 engines.
import { chromium, firefox, webkit } from 'playwright';
for (const [name, bt] of Object.entries({ chromium, firefox, webkit })) {
  const b = await bt.launch();
  const p = await b.newPage();
  await p.goto('http://localhost:8766/demo/poc.html');
  const r = await p.evaluate(() => new Promise((resolve) => {
    const ws = new WebSocket('ws://127.0.0.1:8790/secret');
    ws.binaryType = 'arraybuffer';
    const ev = [];
    ws.onopen = () => { ev.push('open'); ws.send(new Uint8Array([1, 2, 3])); };
    ws.onmessage = (e) => ev.push('message ' + new Uint8Array(e.data).join(','));
    ws.onerror = () => ev.push('error');
    ws.onclose = (e) => { ev.push(`close code=${e.code}`); resolve(ev); };
    setTimeout(() => resolve(ev.concat('timeout')), 5000);
  }));
  console.log(name, b.version(), JSON.stringify(r));
  await b.close();
}
