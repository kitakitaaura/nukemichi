// Phase 1 API checks through the public module (js/src/index.js).
// Usage: node bench/api.mjs <chromium|firefox|webkit> <webrtc|websocket> <worker|inline>
import { chromium, firefox, webkit } from 'playwright';
const [engineName = 'chromium', transport = 'websocket', mode = 'worker'] = process.argv.slice(2);
const bt = { chromium, firefox, webkit }[engineName];
const kill = setTimeout(() => { console.log('TIMEOUT'); process.exit(2); }, 420000);
const b = await bt.launch(process.env.CHANNEL ? { channel: process.env.CHANNEL } : {});
const p = await b.newPage();
await p.goto(`http://localhost:${process.env.PORT || 8766}/demo/poc.html`);
const out = await p.evaluate(async ({ transport, mode }) => {
  const { createClient } = await import('../js/src/index.js');
  const logs = [];
  const r = { };
  const c = createClient({ transport, worker: mode === 'worker', storage: 'memory', logLevel: 'info', log: (l, t, m) => logs.push(`${l} ${t}: ${m}`) });
  const step = async (name, fn) => {
    const t0 = performance.now();
    try { r[name] = { ...(await fn()), ms: Math.round(performance.now() - t0) }; }
    catch (e) { r[name] = { error: `${e.name}: ${e.message}`.slice(0, 300), ms: Math.round(performance.now() - t0) }; }
  };
  await step('ready', async () => ({ timings: await c.ready() }));
  await step('isTor', async () => ({ body: await (await c.fetch('https://check.torproject.org/api/ip')).json() }));
  await step('gzip', async () => { const res = await c.fetch('https://httpbingo.org/gzip'); const j = await res.json(); return { status: res.status, gzipped: j.gzipped, ce: res.headers.get('content-encoding') }; });
  await step('brotli', async () => { const res = await c.fetch('https://httpbin.org/brotli', { headers: { 'accept-encoding': 'br' } }); const j = await res.json(); return { status: res.status, brotli: j.brotli, ce: res.headers.get('content-encoding') }; });
  await step('redirect', async () => { const res = await c.fetch('https://httpbingo.org/redirect/3'); return { status: res.status, url: res.url, redirected: res.redirected }; });
  await step('manualRedirect', async () => { const res = await c.fetch('https://httpbingo.org/redirect/1', { redirect: 'manual' }); return { status: res.status, location: res.headers.get('location') }; });
  await step('post', async () => { const res = await c.fetch('https://httpbingo.org/post', { method: 'POST', headers: { 'content-type': 'application/json', origin: 'https://anything.example' }, body: JSON.stringify({ a: 1 }) }); const j = await res.json(); return { status: res.status, json: j.json, origin: j.headers?.Origin }; });
  await step('streamUpload', async () => {
    const enc = new TextEncoder();
    const body = new ReadableStream({ start(ctl) { ctl.enqueue(enc.encode('part1-')); ctl.enqueue(enc.encode('part2')); ctl.close(); } });
    const res = await c.fetch('https://httpbingo.org/post', { method: 'POST', body, duplex: 'half' });
    const j = await res.json(); return { status: res.status, data: j.data };
  });
  await step('streamDownload', async () => {
    const res = await c.fetch('https://httpbingo.org/stream-bytes/200000?chunk_size=4096');
    const rd = res.body.getReader(); let n = 0, chunks = 0;
    for (;;) { const { value, done } = await rd.read(); if (done) break; n += value.length; chunks++; }
    return { status: res.status, bytes: n, chunks };
  });
  await step('abort', async () => {
    const ac = new AbortController();
    const pr = c.fetch('https://httpbingo.org/delay/8', { signal: ac.signal });
    setTimeout(() => ac.abort(), 1500);
    try { await pr; return { aborted: false }; } catch (e) { return { aborted: e.name === 'AbortError' || /abort/i.test(e.message), err: e.name }; }
  });
  // Certificate checks. badssl.com often times out at exits, so also use a
  // name the server's certificate does not cover: GitHub by IP address.
  await step('badCertIpNoSan', async () => { try { await c.fetch('https://140.82.113.3/'); return { rejected: false }; } catch (e) { return { rejected: /certificate|NotValidForName|invalid peer/i.test(e.message), err: e.message.slice(0, 160) }; } });
  await step('badCertExpired', async () => { try { await c.fetch('https://expired.badssl.com/', { anonOptions: { isolationKey: 'badssl-1' } }); return { rejected: false }; } catch (e) { return { rejected: /certificate|expired/i.test(e.message), err: e.message.slice(0, 160) }; } });
  await step('badCertSelfSigned', async () => { try { await c.fetch('https://self-signed.badssl.com/', { anonOptions: { isolationKey: 'badssl-2' } }); return { rejected: false }; } catch (e) { return { rejected: /certificate|UnknownIssuer/i.test(e.message), err: e.message.slice(0, 160) }; } });

  await step('tcpEcho', async () => {
    const s = await c.connect('tcpbin.com', 4242);
    const w = s.writable.getWriter(); const rd = s.readable.getReader();
    await w.write(new TextEncoder().encode('plain-echo\n'));
    const { value } = await rd.read(); return { got: new TextDecoder().decode(value).trim() };
  });
  await step('tlsEcho', async () => {
    const s = await c.connect('tcpbin.com', 4243, { tls: true });
    const w = s.writable.getWriter(); const rd = s.readable.getReader();
    await w.write(new TextEncoder().encode('tls-echo\n'));
    const { value } = await rd.read(); return { got: new TextDecoder().decode(value).trim() };
  });
  await step('tlsBadCert', async () => { try { await c.connect('140.82.113.3', 443, { tls: true }); return { rejected: false }; } catch (e) { return { rejected: /certificate|NotValidForName|invalid peer/i.test(e.message), err: e.message.slice(0, 160) }; } });
  await step('stats', async () => ({ stats: await c.stats() }));
  r.warnings = logs.filter((l) => /^(warn|error)/.test(l)).slice(0, 12);
  await c.close();
  return r;
}, { transport, mode });
console.log(JSON.stringify({ browser: `${engineName} ${b.version()}`, transport, mode, ...out }, null, 1));
clearTimeout(kill);
await b.close();
