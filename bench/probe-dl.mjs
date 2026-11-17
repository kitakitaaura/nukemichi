// Which ~1 MiB test files are reachable through Tor exits? 3 circuits each.
import { chromium } from 'playwright';
const kill = setTimeout(() => { console.log('TIMEOUT'); process.exit(2); }, 900000);
const b = await chromium.launch(); const p = await b.newPage();
await p.goto('http://localhost:8766/demo/poc.html');
const r = await p.evaluate(async () => {
  const { createClient } = await import('../js/src/index.js');
  const c = createClient({ transport: 'websocket', worker: true, storage: 'memory' });
  await c.ready();
  const urls = ['https://proof.ovh.net/files/1Mb.dat', 'https://speed.cloudflare.com/__down?bytes=1048576', 'https://www.kernel.org/pub/linux/kernel/v1.0/linux-1.0.tar.gz', 'https://check.torproject.org/api/ip'];
  const out = [];
  for (const u of urls) for (let i = 0; i < 3; i++) {
    const t0 = performance.now();
    try {
      const res = await c.fetch(u, { anonOptions: { isolationKey: `p-${u}-${i}` }, signal: AbortSignal.timeout(90000) });
      const n = (await res.arrayBuffer()).byteLength;
      out.push([u, i, res.status, n, Math.round(performance.now() - t0)]);
    } catch (e) { out.push([u, i, 'ERR', e.message.slice(0, 80), Math.round(performance.now() - t0)]); }
  }
  return out;
});
for (const row of r) console.log(row.join(' | '));
clearTimeout(kill); await b.close();
