// Main-thread cost of running Tor in the page: long tasks (>50 ms) during
// bootstrap and a 1 MiB download. Also checks keep-alive RTT. Chromium only
// (longtask timing is Chromium-only).
import { chromium } from 'playwright';
const transport = process.argv[2] || 'websocket';
const kill = setTimeout(() => { console.log('TIMEOUT'); process.exit(2); }, 300000);
const b = await chromium.launch(process.env.CHANNEL ? { channel: process.env.CHANNEL } : {});
const p = await b.newPage();
await p.goto(`http://localhost:${process.env.PORT || 8766}/demo/poc.html`);
await p.evaluate(() => {
  window.longtasks = [];
  new PerformanceObserver((l) => { for (const e of l.getEntries()) window.longtasks.push({ at: Math.round(e.startTime), ms: Math.round(e.duration) }); }).observe({ entryTypes: ['longtask'] });
});
const r = await p.evaluate(async (tr) => {
  const out = {};
  const mark = () => window.longtasks.length;
  let m = mark();
  out.boot = (await window.boot({ transport: tr, storage: 'memory', logLevel: 'debug' })).bootstrapMs;
  const lt = (from) => { const s = window.longtasks.slice(from); return { count: s.length, totalMs: s.reduce((a, b) => a + b.ms, 0), maxMs: Math.max(0, ...s.map((x) => x.ms)) }; };
  out.bootLongtasks = lt(m);
  out.rtt = [];
  for (let i = 0; i < 6; i++) { const t = performance.now(); const res = await window.engine.fetch('https://check.torproject.org/api/ip'); await res.text(); out.rtt.push(Math.round(performance.now() - t)); }
  m = mark();
  const t0 = performance.now();
  const res = await window.engine.fetch('https://proof.ovh.net/files/1Mb.dat');
  const buf = await res.arrayBuffer();
  out.download = { bytes: buf.byteLength, ms: Math.round(performance.now() - t0) };
  out.downloadLongtasks = lt(m);
  out.retries = window.logs.filter((l) => /retrying on a new circuit|Reusing pooled|stale/.test(l)).slice(0, 10);
  return out;
}, transport);
console.log(JSON.stringify(r, null, 1));
clearTimeout(kill);
await b.close();
