// Phase 2: reliability over a long session (default 30 min).
// One small request every 20 s (check.torproject.org/api/ip, ~36 bytes) plus
// one raw TCP echo every 60 s. Records failures, latency, the exit IP (to see
// circuit changes), and Snowflake carrier churn (opened / lost / redial).
// Usage: node bench/soak.mjs <chromium> <webrtc|websocket> [minutes] > bench/results/soak-<t>.json
import { chromium, firefox, webkit } from 'playwright';
const [engineName = 'chromium', transport = 'webrtc', minutesArg = '30'] = process.argv.slice(2);
const bt = { chromium, firefox, webkit }[engineName];
const minutes = Number(minutesArg);
const kill = setTimeout(() => { console.error('TIMEOUT'); process.exit(2); }, (minutes + 10) * 60 * 1000);
const b = await bt.launch();
const p = await b.newPage();
await p.goto(`http://localhost:${process.env.PORT || 8766}/demo/poc.html`);
const result = await p.evaluate(async ({ transport, minutes }) => {
  const { createClient } = await import('../js/src/index.js');
  const events = [];
  const log = (level, target, msg) => {
    if (/carrier|guard|circuit|retrying|redial|snowflake|Could not|failed/i.test(msg) && level !== 'debug') {
      events.push({ t: Math.round(performance.now() / 1000), level, msg: msg.slice(0, 200) });
    }
  };
  const c = createClient({ transport, worker: true, storage: 'memory', logLevel: 'info', log });
  const t0 = performance.now();
  const boot = await c.ready();
  const samples = [];
  const end = t0 + minutes * 60000;
  let i = 0;
  while (performance.now() < end) {
    const s = { t: Math.round((performance.now() - t0) / 1000), kind: i % 3 === 2 ? 'tcp' : 'http' };
    const ts = performance.now();
    try {
      if (s.kind === 'http') {
        const res = await c.fetch('https://check.torproject.org/api/ip', { signal: AbortSignal.timeout(45000) });
        const j = await res.json();
        s.ok = j.IsTor === true;
        s.ip = j.IP;
      } else {
        const sock = await c.connect('tcpbin.com', 4242);
        const w = sock.writable.getWriter(); const r = sock.readable.getReader();
        await w.write(new TextEncoder().encode(`soak ${i}\n`));
        const { value } = await Promise.race([r.read(), new Promise((_, rej) => setTimeout(() => rej(new Error('echo timeout')), 30000))]);
        s.ok = new TextDecoder().decode(value).startsWith(`soak ${i}`);
        await w.close().catch(() => {});
      }
    } catch (e) {
      s.ok = false;
      s.error = `${e.name}: ${e.message}`.slice(0, 160);
      s.torKind = e.torKind;
    }
    s.ms = Math.round(performance.now() - ts);
    samples.push(s);
    i++;
    const wait = 20000 - (performance.now() - ts);
    if (wait > 0) await new Promise((r) => setTimeout(r, wait));
  }
  const stats = await c.stats();
  await c.close();
  return { boot, stats, samples, events };
}, { transport, minutes });
const ok = result.samples.filter((s) => s.ok).length;
const ips = [...new Set(result.samples.map((s) => s.ip).filter(Boolean))];
console.log(JSON.stringify({
  at: new Date().toISOString(), browser: `${engineName} ${b.version()}`, transport, minutes,
  summary: { samples: result.samples.length, ok, failed: result.samples.length - ok, distinctExitIps: ips.length, ...result.stats },
  ...result,
}, null, 1));
clearTimeout(kill);
await b.close();
