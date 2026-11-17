# Nukemichi (抜け道)

Bypass CORS fully client side, with no backend.

Nukemichi gives a web page HTTP and raw TCP through Tor. A normal page can't
read responses from servers that send no `Access-Control-Allow-Origin`, and
can't open TCP connections at all.

Nukemichi runs a Tor client (Arti, the Tor Project's Rust implementation) in
WebAssembly and reaches the Tor network over Snowflake, which a browser can
speak: WebRTC to a volunteer proxy, or a WebSocket to the bridge. A Tor exit
relay opens the real TCP connection, and TLS and HTTP run inside the page. The
browser only sees WebRTC or WebSocket traffic to Snowflake, so CORS never
applies. The target server sees a connection from a Tor exit, so some sites
block it or answer with a CAPTCHA.

## Use

From a CDN, with no build step. One script tag defines `window.nukemichi`:

```html
<script src="https://cdn.jsdelivr.net/npm/nukemichi@0.1/js/src/global.js"></script>
<script>
  nukemichi.fetch('https://example.com/')
    .then((res) => res.text())
    .then(console.log);
</script>
```

Every method on the global returns a Promise, including `createClient`. Or
import the ES module:

```html
<script type="module">
  import nukemichi from 'https://cdn.jsdelivr.net/npm/nukemichi@0.1/js/src/index.js';
  const res = await nukemichi.fetch('https://example.com/');
</script>
```

Load the files as they are, not through jsDelivr's `/+esm` bundling, which
breaks the relative URLs of the worker and the wasm. The worker starts from a
`blob:` URL when the library comes from another origin, so a page with a
Content Security Policy needs `worker-src blob:`, or `worker: false`.

With a bundler (`npm install nukemichi`):

```js
import nukemichi from 'nukemichi';

// fetch() semantics: streaming bodies, redirects, gzip/deflate/br decoding,
// keep-alive connection reuse. Any header may be set (Origin, Cookie, ...).
const res = await nukemichi.fetch('https://example.com/');
console.log(await res.text());

// Raw TCP, optionally TLS (certificate checks are always on).
const sock = await nukemichi.connect('tcpbin.com', 4242);
const w = sock.writable.getWriter();
const r = sock.readable.getReader();
await w.write(new TextEncoder().encode('hello\n'));
console.log(new TextDecoder().decode((await r.read()).value));
```

Options (`nukemichi.configure(opts)` before first use, or `createClient(opts)`):

| option | default | meaning |
|---|---|---|
| `transport` | `'webrtc'` | `'webrtc'`: Snowflake through a volunteer proxy (what Tor Browser does). `'websocket'`: straight to the Snowflake bridge's WebSocket endpoint; no volunteer proxy, but not an officially supported client path. |
| `worker` | `true` | Run Tor in a dedicated worker. The page keeps only WebRTC signaling. |
| `storage` | `'indexeddb'` | Arti guard state and directory cache. Makes warm starts fast. |
| `logLevel`, `log` | `'warn'` | Arti/Nukemichi log level and sink. |

`fetch` and `connect` accept `anonOptions: { isolationKey }`: requests with
different keys never share a circuit.

## Build

Needs Rust (stable, `wasm32-unknown-unknown` target), `wasm-pack`, `wasm-opt`
(binaryen) and a clang with the wasm32 target for `ring` (Apple's clang has
none; on macOS use Homebrew LLVM, which `.cargo/config.toml` points at).

```bash
brew install wasm-pack binaryen llvm
```

```bash
rustup target add wasm32-unknown-unknown
```

```bash
npm run build
```

This runs `wasm-pack` and writes `js/pkg/`, which the npm package ships.

## Demo

Serve the repository root with any static server:

```bash
python3 -m http.server 8766
```

Then open http://localhost:8766/demo/index.html.

## Tests and measurements

Browser automation uses Playwright (Chromium, Firefox, WebKit):

```bash
cd bench && npm install && npx playwright install chromium firefox webkit
```

| script | what |
|---|---|
| `bench/phase0.mjs <browser> <transport>` | IsTor, no-CORS read vs plain `fetch()`, raw TCP echo |
| `bench/measure.mjs <browser> <transport> <runs> [out]` | cold/warm bootstrap, RTT, 1 MiB download |
| `bench/api.mjs <browser> <transport> <worker\|inline>` | fetch/connect semantics, TLS rejection |
| `bench/reach.mjs <browser> <transport>` | HTTPS reach to ~27 popular sites/APIs |
| `bench/exit-ports.mjs` | share of exits allowing each port (Onionoo) |
| `bench/soak.mjs <browser> <transport> [minutes]` | 30-minute reliability run |
| `bench/cdn.mjs <browser> [transport]` | load from another origin, as from a CDN |

Keep test traffic small: these are volunteer-run networks.

The measurements and open risks (exit blocking, CAPTCHAs, Snowflake capacity,
browser support) are in `docs/REPORT.md`.

## Layout

- `crates/nukemichi-wasm/`: the engine. Arti client, Snowflake client
  (`snowflake.rs`: Turbo Tunnel + KCP + smux), HTTP/1.1 (`fetch.rs`),
  connection pool and retry (`conn.rs`), raw TCP (`tcp.rs`).
- `js/src/`: the npm package. `index.js` (API), `worker.js`, `engine.js`,
  `snowflake.js` (broker rendezvous, carriers), `ports.js`, `storage.js`.
- `demo/`, `bench/`, `docs/`.

## Licence

MIT OR Apache-2.0. Derived from tor-js; see `NOTICE`.
