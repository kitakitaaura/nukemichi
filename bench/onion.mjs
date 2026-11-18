// Fetch .onion services through Nukemichi, next to the clearnet site for
// comparison. An onion connection ends inside Tor, so no exit relay is used.
// Usage: node bench/onion.mjs [chromium|firefox|webkit] [webrtc|websocket]
// Needs the repo served at http://localhost:${PORT:-8766}/.
import { chromium, firefox, webkit } from 'playwright';
const [engineName = 'chromium', transport = 'websocket'] = process.argv.slice(2);
const bt = { chromium, firefox, webkit }[engineName];

const EXTRA = process.env.URLS ? process.env.URLS.split(' ').map((u) => [u, u]) : null;
const TARGETS = EXTRA || [
  ['Reddit (clearnet)', 'https://www.reddit.com/r/tor/.json?limit=1'],
  ['Reddit (onion)', 'https://www.reddittorjg6rue252oqsxryoxengawnmo46qy4kyii5wtqnwfj4ooad.onion/r/tor/.json?limit=1'],
  ['DuckDuckGo (onion)', 'https://duckduckgogg42xjoc72x3sjasowoarfbgcmvfimaftt6twagswzczad.onion/'],
  ['BBC (onion)', 'https://www.bbcnewsd73hkzno2ini43t4gblxvycyac5aw4gnv7t2rccijh7745uqd.onion/news'],
  ['Proton (onion)', 'https://protonmailrmez3lotccipshtkleegetolb73fuirgj7r4o4vfu7ozyd.onion/'],
  // Tor Project's onion serves plain HTTP only; https:// gets no stream.
  ['Tor Project (onion)', 'http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion/'],
  // Server rejects our TLS ClientHello (HandshakeFailure); cause unknown.
  ['Facebook (onion)', 'https://www.facebookwkhpilnemxj7asaniu7vnjjbiltxjqhye3mhbshg7kx5tfyd.onion/'],
];

const kill = setTimeout(() => { console.error('TIMEOUT'); process.exit(2); }, 15 * 60 * 1000);
const b = await bt.launch(process.env.CHANNEL ? { channel: process.env.CHANNEL } : {});
const p = await b.newPage();
await p.goto(`http://localhost:${process.env.PORT || 8766}/demo/poc.html`);
const result = await p.evaluate(async ({ TARGETS, transport, SHOW_HEADERS }) => {
  const { createClient } = await import('../js/src/index.js');
  const c = createClient({ transport, worker: true, storage: 'indexeddb', logLevel: 'warn' });
  await c.ready();
  const out = [];
  for (const [name, url] of TARGETS) {
    const t0 = performance.now();
    const rec = { name, url };
    try {
      const res = await c.fetch(url, {
        headers: { 'accept': 'text/html,application/json;q=0.9,*/*;q=0.8' },
        signal: AbortSignal.timeout(120000),
        anonOptions: { isolationKey: `onion-${name}` },
      });
      const text = await res.text();
      rec.status = res.status;
      rec.bytes = text.length;
      rec.snippet = text.slice(0, 120).replace(/\s+/g, ' ');
      const msg = text.match(/blocked[^<]{0,200}|<title>[^<]*<\/title>/i);
      if (msg) rec.message = msg[0];
      if (SHOW_HEADERS) rec.headers = Object.fromEntries(res.headers);
    } catch (e) {
      rec.error = String(e && e.message || e);
    }
    rec.ms = Math.round(performance.now() - t0);
    out.push(rec);
  }
  return out;
}, { TARGETS, transport, SHOW_HEADERS: !!process.env.HEADERS });
console.log(JSON.stringify(result, null, 2));
clearTimeout(kill);
await b.close();
