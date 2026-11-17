# Nukemichi (抜け道)

CORS-free HTTP and raw TCP from a web page, through Tor.

A normal page cannot read responses from servers that send no
`Access-Control-Allow-Origin`, and cannot open TCP connections. Nukemichi runs
a Tor client (Arti, the Tor Project's Rust implementation) in WebAssembly. It
reaches the Tor network over Snowflake, which a browser can speak (WebRTC to a
volunteer proxy, or a WebSocket to the bridge). A Tor exit relay opens the real
TCP connection; TLS and HTTP run inside the page. The browser only sees
WebRTC/WebSocket traffic to Snowflake, so CORS never applies, and the target
server sees an ordinary client.

Status: experimental research prototype. See `docs/REPORT.md` for the
measurements and the open risks before using this for anything real.

## Use

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
cd crates/nukemichi-wasm && wasm-pack build --target web --release --out-dir ../../js/pkg --out-name nukemichi
```

## Demo

Serve the repository root over HTTP (any static server) and open
`/demo/index.html`:

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

Keep test traffic small: these are volunteer-run networks.

## Layout

- `crates/nukemichi-wasm/`: the engine. Arti client, Snowflake client
  (`snowflake.rs`: Turbo Tunnel + KCP + smux), HTTP/1.1 (`fetch.rs`),
  connection pool and retry (`conn.rs`), raw TCP (`tcp.rs`).
- `js/src/`: the npm package. `index.js` (API), `worker.js`, `engine.js`,
  `snowflake.js` (broker rendezvous, carriers), `ports.js`, `storage.js`.
- `demo/`, `bench/`, `docs/`.

## Licence

MIT OR Apache-2.0. Derived from tor-js; see `NOTICE`.
