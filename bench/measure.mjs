// Phase 0 measurements. One "run" = cold boot (IndexedDB cleared) + RTTs +
// 1 MiB download, then a warm boot in a new page sharing the IndexedDB.
// Usage: node bench/measure.mjs <browser> <transport> <runs> [outfile]
import { chromium, firefox, webkit } from 'playwright';
import { appendFileSync } from 'node:fs';
const [engineName = 'chromium', transport = 'websocket', runsArg = '1', outFile] = process.argv.slice(2);
const bt = { chromium, firefox, webkit }[engineName];
const URL_RTT = 'https://check.torproject.org/api/ip';
const URL_1M = 'https://proof.ovh.net/files/1Mb.dat';

async function bootPage(ctx, opts) {
  const p = await ctx.newPage();
  await p.goto(`http://localhost:${process.env.PORT || 8766}/demo/poc.html`);
  const boot = await p.evaluate((o) => window.boot(o), { transport, storage: 'indexeddb', logLevel: 'info', ...opts });
  return { p, boot };
}

const measureTraffic = (p) => p.evaluate(async ([URL_RTT, URL_1M]) => {
  const e = window.engine;
  const r = { rtt: [] };
  for (let i = 0; i < 6; i++) {
    const t0 = performance.now();
    const res = await e.fetch(URL_RTT);
    await res.text();
    r.rtt.push(Math.round(performance.now() - t0));
  }
  const t0 = performance.now();
  const res = await e.fetch(URL_1M);
  const rd = res.body.getReader();
  let n = 0, tFirst = null;
  for (;;) { const { value, done } = await rd.read(); if (done) break; if (tFirst === null) tFirst = performance.now(); n += value.length; }
  const ms = performance.now() - t0;
  r.download = { bytes: n, ms: Math.round(ms), ttfbMs: Math.round(tFirst - t0), kBps: Math.round(n / 1024 / (ms / 1000)) };
  r.stats = e.stats();
  return r;
}, [URL_RTT, URL_1M]);

const runs = Number(runsArg);
const b = await bt.launch();
for (let i = 0; i < runs; i++) {
  const kill = setTimeout(() => { console.log('TIMEOUT'); process.exit(2); }, 600000);
  const rec = { at: new Date().toISOString(), browser: `${engineName} ${b.version()}`, transport, run: i };
  const ctx = await b.newContext();
  try {
    const cold = await bootPage(ctx, { clearStorage: true });
    rec.cold = { bootMs: Math.round(cold.boot.bootstrapMs), carriers: cold.boot.carriers.length, ...(await measureTraffic(cold.p)) };
    await cold.p.evaluate(() => window.engine.close());
    await cold.p.close();
    await new Promise((r) => setTimeout(r, 1500)); // let the storage lock go
    const warm = await bootPage(ctx, {});
    const t0 = Date.now();
    const first = await warm.p.evaluate(async (u) => { const t = performance.now(); const r = await window.engine.fetch(u); await r.text(); return Math.round(performance.now() - t); }, URL_RTT);
    rec.warm = { bootMs: Math.round(warm.boot.bootstrapMs), firstRequestMs: first };
    rec.warm.logTail = await warm.p.evaluate(() => window.logs.filter((l) => /dirmgr|Bootstrap complete/.test(l)).slice(0, 8));
    await warm.p.close();
  } catch (e) {
    rec.error = e.message.slice(0, 400);
  }
  await ctx.close();
  clearTimeout(kill);
  const line = JSON.stringify(rec);
  console.log(line);
  if (outFile) appendFileSync(outFile, line + '\n');
}
await b.close();
