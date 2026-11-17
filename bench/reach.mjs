// Phase 2: HTTPS reach through Tor to popular sites and APIs.
// One GET per target (plus redirects), no logins, small pages only.
// Classifies each as ok / challenge (CAPTCHA or block page) / http-error / fail.
// Usage: node bench/reach.mjs <chromium> <webrtc|websocket> [rounds] > bench/results/reach-<n>.json
import { chromium, firefox, webkit } from 'playwright';
const [engineName = 'chromium', transport = 'websocket', roundsArg = '1'] = process.argv.slice(2);
const bt = { chromium, firefox, webkit }[engineName];

const TARGETS = [
  ['Google', 'https://www.google.com/'],
  ['Wikipedia', 'https://en.wikipedia.org/wiki/Special:Random'],
  ['Wikipedia API', 'https://en.wikipedia.org/w/api.php?action=query&meta=siteinfo&format=json'],
  ['GitHub', 'https://github.com/'],
  ['GitHub API', 'https://api.github.com/repos/torproject/tor'],
  ['Reddit', 'https://www.reddit.com/r/tor/.json?limit=1'],
  ['Amazon', 'https://www.amazon.com/'],
  ['Cloudflare site (cloudflare.com)', 'https://www.cloudflare.com/'],
  ['Stack Overflow', 'https://stackoverflow.com/questions?pagesize=1'],
  ['YouTube', 'https://www.youtube.com/'],
  ['X / Twitter', 'https://x.com/'],
  ['Facebook', 'https://www.facebook.com/'],
  ['Instagram', 'https://www.instagram.com/'],
  ['LinkedIn', 'https://www.linkedin.com/'],
  ['Netflix', 'https://www.netflix.com/'],
  ['BBC', 'https://www.bbc.com/news'],
  ['New York Times', 'https://www.nytimes.com/'],
  ['Hacker News API', 'https://hacker-news.firebaseio.com/v0/maxitem.json'],
  ['npm registry', 'https://registry.npmjs.org/left-pad'],
  ['PyPI JSON', 'https://pypi.org/pypi/requests/json'],
  ['crates.io API', 'https://crates.io/api/v1/crates/serde'],
  ['OpenStreetMap Nominatim', 'https://nominatim.openstreetmap.org/search?q=Tokyo&format=json&limit=1'],
  ['Open-Meteo API', 'https://api.open-meteo.com/v1/forecast?latitude=35.68&longitude=139.69&current=temperature_2m'],
  ['CoinGecko API', 'https://api.coingecko.com/api/v3/ping'],
  ['Coinbase API', 'https://api.coinbase.com/v2/prices/BTC-USD/spot'],
  ['DuckDuckGo', 'https://html.duckduckgo.com/html/?q=tor'],
  ['Archive.org', 'https://archive.org/wayback/available?url=example.com'],
];

// Strict signals only: a page that merely loads reCAPTCHA is not a challenge.
const CHALLENGE = [
  /<title>Just a moment\.\.\.<\/title>|cf-chl-|challenge-platform|<title>Attention Required! \| Cloudflare/i,
  /\/sorry\/index|unusual traffic from your computer network/i,
  /px-captcha|captcha-delivery\.com|datadome|Pardon Our Interruption/i,
  /You have been blocked|blocked by network security|Request blocked\.|<title>Access Denied<\/title>/i,
];

const rounds = Number(roundsArg);
const kill = setTimeout(() => { console.error('TIMEOUT'); process.exit(2); }, 60 * 60 * 1000);
const b = await bt.launch(process.env.CHANNEL ? { channel: process.env.CHANNEL } : {});
const p = await b.newPage();
await p.goto(`http://localhost:${process.env.PORT || 8766}/demo/poc.html`);
const result = await p.evaluate(async ({ TARGETS, CHALLENGE_SRC, transport, rounds }) => {
  const CHALLENGE = CHALLENGE_SRC.map(([s, f]) => new RegExp(s, f));
  const { createClient } = await import('../js/src/index.js');
  const c = createClient({ transport, worker: true, storage: 'indexeddb', logLevel: 'warn' });
  const boot = await c.ready();
  const out = [];
  for (let round = 0; round < rounds; round++) {
    for (const [name, url] of TARGETS) {
      const t0 = performance.now();
      const rec = { round, name, url };
      try {
        // A fresh isolation key per target: a fresh circuit, so one bad exit
        // does not decide every result, and sites are not linked.
        const res = await c.fetch(url, {
          headers: { 'accept': 'text/html,application/json;q=0.9,*/*;q=0.8', 'accept-language': 'en-US,en;q=0.9' },
          signal: AbortSignal.timeout(60000),
          anonOptions: { isolationKey: `reach-${round}-${name}` },
        });
        const text = (await res.text()).slice(0, 200000);
        rec.status = res.status;
        rec.finalUrl = res.url;
        rec.bytes = text.length;
        rec.server = res.headers.get('server');
        const hit = CHALLENGE.find((re) => re.test(text) || re.test(res.url));
        if (hit) {
          rec.outcome = 'challenge';
          rec.matched = String(hit);
        } else if (res.status >= 200 && res.status < 400) {
          rec.outcome = 'ok';
        } else {
          rec.outcome = 'http-error';
        }
        rec.title = text.match(/<title[^>]*>([^<]{0,80})/i)?.[1]?.trim();
      } catch (e) {
        rec.outcome = 'fail';
        rec.error = `${e.name}: ${e.message}`.slice(0, 200);
        rec.torKind = e.torKind;
      }
      rec.ms = Math.round(performance.now() - t0);
      out.push(rec);
    }
  }
  const stats = await c.stats();
  await c.close();
  return { boot, stats, results: out };
}, { TARGETS, CHALLENGE_SRC: CHALLENGE.map((r) => [r.source, r.flags]), transport, rounds });
console.log(JSON.stringify({ at: new Date().toISOString(), browser: `${engineName} ${b.version()}`, transport, ...result }, null, 1));
clearTimeout(kill);
await b.close();
