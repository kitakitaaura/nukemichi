# Nukemichi feasibility report

Dates: 2026-11-16 to 2026-11-17. Repo: this directory. Nothing is pushed.

## 1. Verdicts

| Phase | Verdict | One-line evidence |
|---|---|---|
| 0: proof of concept | **Works** with Snowflake (WebRTC proxy or direct WebSocket to the bridge). **WebTunnel does not work** from a page. | IsTor, no-CORS read and raw TCP echo pass in Chromium 141, installed Chrome 157, Firefox 142, WebKit 26 and real Safari 18.6. |
| 1: library | **Works** | `nukemichi.fetch` / `nukemichi.connect` pass the API suite (13 checks) in 3 engines × 2 carriers × worker/inline. 14/14 measurement runs complete once exit retry is in place. |
| 2: real-world reach | **Partially works** | 61% of requests to 27 popular sites/APIs succeed. Google, LinkedIn and Stack Overflow challenge; X, Facebook and Reddit block. Non-web ports reach only 15–58% of exit weight. Two 30-minute sessions (WebRTC and WebSocket): 180/180 requests ok, 0 proxy carriers lost. |

The core idea holds. A page can read any HTTPS response and open raw TCP with
no proxy server of its own, running real Arti with full consensus validation
and TLS checks. The limits come from Tor itself: exit blocking, CAPTCHA
challenges, exit-policy coverage for non-web ports, and second-scale latency.
They do not come from the browser or the wasm build.

## 2. Prior art (checked 2026-11-16)

| Project | Licence | Maintenance | What it is | Verdict |
|---|---|---|---|---|
| **webtor-rs** (privacy-ethereum) v0.5.7 | MIT | Last human commit 2026-01-30. Since then only a daily bot commit (cached consensus). | Vendors a patched Arti 1.8 but uses only `tor-proto`, with its own path selection. Its own TLS stack (`subtle-tls`, SubtleCrypto). Snowflake (WebRTC/WebSocket) in wasm. **WebTunnel is native-only** (`"WebTunnel is not supported in WASM"`, client.rs), although the README says "Native + WASM". | **Rejected.** It loads its consensus from `privacy-ethereum.github.io/webtor-rs/consensus.txt.br`, which returns 404. It never checks consensus signatures (`dangerously_assume_timely()`, then reads `.consensus`). So whoever serves that file picks your relays. At HEAD, with the consensus served locally, it never opened a channel (`Channel not established`). |
| **tor-js** ("TorJS", ethereum/tor-js) v0.4.4 | MIT OR Apache-2.0 | Active (release 2026-10-29) | Full `arti-client` from the voltrevo/arti fork; IndexedDB storage; streaming fetch; isolation keys. In browsers it **needs a gateway server** (KPS over WebRTC, then HTTP CONNECT to relays). No Snowflake/WebTunnel. TLS via `rustls-rustcrypto 0.0.2-alpha`. | **Used as the base.** Its runtime, storage and HTTP code are good. The gateway breaks the "no proxy server" goal, so Nukemichi replaces it with Snowflake. |
| **Arti** upstream 2.7.0 (crates 0.48, 2026-11-04) | MIT OR Apache-2.0 | Active | `arti-client --no-default-features` compiles for wasm32 since spring 2026 (MRs 3816, 3819, 3820). There is **no upstream wasm runtime or storage**: MRs 4160–4166 (in-memory state, BlobStore seam, wasm CI) were closed unmerged 2026-06-28. Issues #2445 and #2446 ("No WASM in CI") are open. #2798 (guardmgr panic on wasm) was fixed 2026-11-12. | Not usable alone. The voltrevo/arti fork is Arti 2.7.0 plus about 2,000 lines: JS-backed dir/state storage, wasm time, zstd-wasm and the guardmgr fix. |

Nukemichi does not fork or vendor Arti. It pins the voltrevo/arti fork that
tor-js uses (rev `2527494`) as a git dependency, unmodified.

## 3. Architecture and crate choices

```
page ── nukemichi.fetch / connect (js/src/index.js: redirects, Request normalization)
  │  MessagePorts (requests, response streams, TCP streams, WebRTC bytes)
  ▼
dedicated worker (js/src/worker.js → engine.js)
  wasm: arti-client (voltrevo/arti 2.7.0) ── WasmRuntime.connect(bridge addr)
          │                                        │
          │ HTTP/1.1 + keep-alive pool (fetch.rs, conn.rs)
          │ rustls + ring + webpki-roots (TLS to target, checks always on)
          ▼                                        ▼
        Tor circuit (bridge → middle → exit)     snowflake.rs: Turbo Tunnel + KCP + smux v2
                                                   │ carrier (byte pipe)
                     ┌─────────────────────────────┴───────────────────┐
             'websocket': WebSocket to                  'webrtc': RTCDataChannel to a volunteer
             wss://snowflake.torproject.net/            proxy (broker rendezvous on the main
             (opened inside the worker)                 thread, bytes relayed over a MessagePort)
```

- **Transport.** Arti is configured with the built-in Snowflake bridge lines
  as plain bridges (`192.0.2.3:80 2B28…`, `192.0.2.4:80 8838…`). The runtime
  routes those placeholder addresses to the Snowflake layer. Arti never dials
  a relay directly, and the connect function refuses anything else. The
  Snowflake client was written from the protocol in the Go client: 8-byte
  Turbo token + 8-byte ClientID, length-prefixed packets, KCP (conv 0, stream
  mode, window 65535, nodelay 0/0/0/nc=1), smux v2 (UPD flow control,
  keepalive). When a proxy dies, it redials with the same ClientID, and the
  bridge re-attaches the KCP session, so the Tor channel survives proxy churn.
- **Threads: Tor runs in a dedicated worker; the main thread does only WebRTC signaling.** Evidence (Playwright, plus Chrome 157):

  | | Chromium 141 / Chrome 157 | Firefox 142 | WebKit 26 |
  |---|---|---|---|
  | `RTCPeerConnection` in a worker | no | no | no |
  | `RTCDataChannel` in a worker | yes | no | yes |
  | Transfer `RTCDataChannel` to a worker | only in the task that creates it | no (`DataCloneError`) | no |
  | Transfer `ReadableStream`/`WritableStream` | yes | yes | **no** ("can not be cloned") |
  | `WebSocket`, IndexedDB, OPFS, `navigator.locks` in a worker | yes | yes | yes |

  You suspected `RTCPeerConnection` is missing from workers. It is, in all
  three engines. Transfer support is uneven, so every byte crossing the worker
  boundary goes over a MessagePort with credit-based flow control
  (`js/src/ports.js`), which works everywhere. The WebSocket carrier runs
  entirely inside the worker. Running Tor inline costs little: 2 long tasks
  at bootstrap (max 148 ms) and none during a 1 MiB download. So `worker:
  false` stays as an option, but the worker is the default, because it keeps
  consensus parsing, crypto and a 1 GiB memory quota off the UI thread.
- **TLS.** rustls 0.23 + **ring** + webpki-roots (Mozilla roots) for target
  servers. The Tor link uses rustls + ring with Tor's own CERTS-cell checks.
  There is no option that disables certificate checks. Verified through Tor:
  - a name the certificate doesn't cover (GitHub by IP) is rejected for both `fetch` and `connect({tls})`: `certificate not valid for name "140.82.113.3"`;
  - `self-signed.badssl.com` is rejected: `invalid peer certificate: UnknownIssuer`;
  - `expired.badssl.com` could not be reached through any exit we tried.
- **Build (crypto for `wasm32-unknown-unknown`):**

  | Backend | Builds? | Toolchain | Size (wasm-opt -Oz, rustls client only) | gzip |
  |---|---|---|---|---|
  | ring 0.17 | yes, with feature `wasm32_unknown_unknown_js` | clang with the wasm32 target (Homebrew LLVM). Apple clang fails: `No available targets are compatible with triple "wasm32-unknown-unknown"` | 687 KB | 210 KB |
  | aws-lc-rs | **no** | `aws-lc-sys` needs libc headers (`'stdlib.h' file not found`) with either clang | – | – |
  | rustls-rustcrypto 0.0.2-alpha | yes, needs `rustls-pki-types/web` | pure Rust | 665 KB | 182 KB |

  ring was chosen over an alpha-version crypto provider. The cost is one build
  requirement: LLVM clang, pinned in `.cargo/config.toml`. Arti's own crypto
  (dalek, RustCrypto) is pure Rust.
- **fetch semantics.**
  - Request and response streaming: the request body goes out as a
    ReadableStream with chunked encoding; the response body is a
    ReadableStream with backpressure.
  - Redirects follow / manual / error, with up to 20 hops, method rewriting
    for 303/301/302, and Authorization dropped on cross-origin hops.
  - Content decoding: gzip/deflate via `DecompressionStream`. br uses native
    `DecompressionStream('brotli')` where it exists (WebKit 26 only). Elsewhere
    it uses a wasm decoder (`brotli-decompressor`), because Chromium 141,
    Chrome 157 and Firefox 142 have no brotli DecompressionStream.
  - Keep-alive connection reuse, keyed by origin and circuit.
  - AbortSignal, and isolation keys (`anonOptions.isolationKey`).
  - Any header may be set (Origin, Cookie, …).
  - Not supported: HTTP/2, cookies jar, caching, `credentials`/`mode`
    (meaningless here), `Response.url`/`redirected` are set as own properties.
- **Reliability fix that mattered most (`conn.rs`).** Arti keeps using a
  circuit after its exit fails, so one bad exit fails every later request.
  Without a fix, only 3 of 14 measurement runs completed; every failure was
  exit-side. Nukemichi now rotates the isolation token on exit-side errors
  (and on a TLS handshake that dies) and retries, up to 3 attempts. After
  this change (and moving the 1 MiB test off proof.ovh.net, which failed
  through 2 of 3 circuits), **14 of 14** runs completed with 0 step errors.
- **Startup.** Arti's guard state and directory cache persist in IndexedDB
  (tor-js's storage seam). A warm start is ready in ~0.4 s (Firefox ~3.7 s).
  Building the first circuit after that adds 2.6–9 s.

## 4. Measurements

All runs: macOS 15.6.1 (arm64), Playwright 1.56.1 browsers unless noted.
Cold = IndexedDB cleared. Warm = new page with the same IndexedDB. RTT =
`GET https://check.torproject.org/api/ip`: the 1st request includes building
a circuit; later requests reuse a keep-alive connection. Download =
`speed.cloudflare.com/__down?bytes=1048576`. Raw data:
`bench/results/phase0.jsonl`.

### Final build (retry + keep-alive), 14 runs

| Browser | Carrier | Runs ok | Cold boot ms, median (range) | Warm boot ms | First request after warm boot ms | RTT ms, 1st / later (median) | 1 MiB download s | KB/s |
|---|---|---|---|---|---|---|---|---|
| Chromium 141 | WebSocket | 3/3 | 4123 (3991–4850) | 426 | 2652 | 1999 / 264 | 2.99 | 342 |
| Chromium 141 | WebRTC | 3/3 | 15392 (9911–15755) | 424 | 9061 | 1360 / 301 | 5.48 | 187 |
| Firefox 142 | WebSocket | 2/2 | 7994 (7237–8751) | 3671 | 3192 | 7297 / 309 | 3.96 | 273 |
| Firefox 142 | WebRTC | 2/2 | 10631 (9324–11938) | 3880 | 3820 | 3502 / 662 | 3.00 | 347 |
| WebKit 26 | WebSocket | 2/2 | 4126 (4094–4159) | 414 | 2862 | 1472 / 306 | 2.14 | 480 |
| WebKit 26 | WebRTC | 2/2 | 7412 (7005–7820) | 404 | 3329 | 6896 / 280 | 2.48 | 418 |

"WebSocket" stands in for WebTunnel (see below). A cold bootstrap moves
about 3.1 MB down (consensus + ~9,500 microdescriptors) and 0.57 MB up
through the bridge.

### First build (no retry, no pooling), 14 runs: 3 completed

| Browser | Carrier | Cold boot ms | Warm boot ms | RTT ms (new TLS connection each) | 1 MiB (proof.ovh.net) |
|---|---|---|---|---|---|
| Firefox 142 | WebSocket | 8344 | 3333 | 1342–2583 | 4.4 s, 233 KB/s |
| WebKit 26 | WebSocket | 4416 | 388 | 1139–1670 | 5.0 s, 205 KB/s |
| WebKit 26 | WebRTC | 10160 | 386 | 1432–4789 | 8.7 s, 117 KB/s |

The other 11 failed on exit-side errors (`timed out at exit` ×8, `END cell MISC`
×2, `tls handshake eof` / `CONNRESET` ×2). Raw: `bench/results/phase0-noretry.jsonl`.

### Phase 0 proofs per browser

| Browser | Carrier | Boot | `IsTor` | plain `fetch('https://example.com/')` | Tor fetch | `tcpbin.com:4242` echo |
|---|---|---|---|---|---|---|
| Chromium 141 | WebSocket | 5.6 s | true | `TypeError: Failed to fetch` | 200, full body | ok, 348 ms |
| Chromium 141 | WebRTC | 7.2 s | true | same | 200 | ok |
| Firefox 142 | WebSocket | 7.9 s | true | `TypeError: NetworkError when attempting to fetch resource.` | 200 | ok, 556 ms |
| Firefox 142 | WebRTC | 11.1 s | true | same | 200 | ok, 357 ms |
| WebKit 26 | WebSocket | 4.2 s | true | `TypeError: Load failed` | 200 | ok, 425 ms |
| WebKit 26 | WebRTC | 9.9 s | true | same | 200 | ok, 382 ms |
| **Safari 18.6** (real, safaridriver, worker mode) | WebSocket | 4.3 s | true | `TypeError: Load failed` | 200 | ok, 449 ms |
| **Safari 18.6** | WebRTC | 16.6 s | true | same | 200 | ok, 517 ms; 1 proxy lost and redialed mid-run |
| **Chrome 157** (installed, inline) | WebRTC | 30.9 s | true | `TypeError: Failed to fetch` | 200 | ok, 766 ms (connect 10.0 s) |

No-CORS proof: `curl -D - -H 'Origin: https://nukemichi.test'
https://example.com/` returns `HTTP/2 200` with no
`Access-Control-Allow-Origin`. Through Nukemichi the response also has no
ACAO header, and the body is read in full.

### API suite (`bench/api.mjs`, results in `bench/results/api.jsonl`)

| Browser | Carrier | Mode | Boot ms | IsTor | gzip | redirect | manual redirect | POST + custom Origin | stream upload | stream download | abort | bad cert (name) | bad cert (self-signed) | TCP echo | TLS echo | `connect` bad cert |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Chromium 141 | WebRTC | worker | 11004 | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | n/r | ✓ | ✓ | ✓ |
| Chromium 141 | WebSocket | inline | 4009 | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | n/r | ✓ | ✓ | ✓ |
| Firefox 142 | WebRTC | worker | 11108 | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | n/r | ✓ | ✓ | ✓ |
| Firefox 142 | WebSocket | inline | 8515 | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | n/r | ✓ | ✓ | ✓ |
| WebKit 26 | WebRTC | worker | 17921 | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | n/r | ✓ | ✓ | ✓ |
| WebKit 26 | WebSocket | inline | 4862 | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | n/r | ✓ | ✓ | ✓ |

n/r = badssl.com not reachable through the exits tried in that run; a
separate Chromium/WebSocket/worker run reached it and got `UnknownIssuer`.
Brotli is verified offline (`bench/decode.mjs`, local fixtures, in a worker,
3 engines). Live brotli endpoints rate-limited Tor (httpbin 429; httpbingo
has no /brotli).

### WebTunnel cannot be reached from page JavaScript

WebTunnel's server (`transport/httpupgrade/httpupgrade.go`) answers the
upgrade with a bare `101` that has no `Sec-WebSocket-Accept`, and then
carries raw bytes, not WebSocket frames. I tested a browser `WebSocket`
against WebTunnel's own `httpupgrade.Server` (`bench/webtunnel-ws.mjs`):
- Chromium 141 and WebKit 26: the server sees a good upgrade, but the browser rejects the 101 (`error`, close 1006).
- Firefox 142: sends `Connection: keep-alive, Upgrade`, and the server rejects it (`upgrade failed: unrecognized request`).

`fetch()` cannot send `Upgrade` either. WebTunnel would need a server-side
change (real WebSocket framing) or a helper proxy.

### Exit policy coverage (`bench/exit-ports.mjs`, Onionoo, consensus 2026-11-17 01:00)

How Arti picks an exit (`tor-relay-selection/src/usage.rs`, `ExitToAllPorts`):
- The relay needs the **Fast** flag.
- It also needs **Stable** if the port is in `long_lived_ports` (21, 22, 706, 1863, 5050, 5190, 5222, 5223, 6523, 6667, 6697, 8300; `tor-circmgr/src/config.rs`).
- Its microdescriptor IPv4 policy summary must allow the port.
- The Exit flag is not required.
- Among those, the choice is weighted by bandwidth (exit role).

9,569 running relays, 2,809 with Exit.

| Port | Use | Stable required | Candidate exits | Share of exit selection weight |
|---|---|---|---|---|
| 443 | HTTPS | no | 2,774 | 99.9% |
| 80 | HTTP | no | 2,771 | 99.9% |
| 9418 | git | no | 2,521 | 90.9% |
| 8333 | Bitcoin P2P | no | 1,921 | 83.7% |
| 993 | IMAPS | no | 1,712 | 70.0% |
| 5222 | XMPP | yes | 1,586 | 67.4% |
| 6697 | IRC over TLS | yes | 1,388 | 57.7% |
| 6667 | IRC | yes | 1,385 | 57.4% |
| 587 | SMTP submission | no | 1,160 | 48.2% |
| 465 | SMTPS | no | 1,147 | 47.9% |
| 22 | SSH | yes | 957 | 38.7% |
| 25565 | Minecraft | no | 894 | 37.6% |
| 4242/4243 | tcpbin echo | no | 893 | 37.5% |
| 25 | SMTP | no | 511 | 15.5% |

Arti only builds circuits through exits that allow the port, so these shares
mostly mean less exit diversity and more load on the exits that allow it,
not failure. Each of the 30 raw TCP echoes in the soak succeeded. Many
exits that allow a port still drop specific destinations: badssl.com and
proof.ovh.net failed through most circuits.

### HTTPS reach (`bench/reach.mjs`, Chromium 141, WebSocket carrier, 2 rounds, a fresh circuit per request)

| Target | Round 1 | Round 2 |
|---|---|---|
| Google | **Google "unusual traffic"** (200) | **Google "unusual traffic"** (200) |
| Wikipedia | ok (200) | ok (200) |
| Wikipedia API | ok (200) | ok (200) |
| GitHub | ok (200) | ok (200) |
| GitHub API | ok (200) | ok (200) |
| Reddit | **block page** (403) | HTTP 429 |
| Amazon | timeout (60 s) | timeout (60 s) |
| cloudflare.com | ok (200) | ok (200) |
| Stack Overflow | **Cloudflare challenge** (403) | timeout (60 s) |
| YouTube | ok (200) | ok (200) |
| X / Twitter | HTTP 403 | HTTP 403 |
| Facebook | HTTP 400 | HTTP 400 |
| Instagram | ok (200) | ok (200) |
| LinkedIn | **Cloudflare challenge** (403) | **Cloudflare challenge** (403) |
| Netflix | ok (200) | ok (200) |
| BBC | ok (200) | ok (200) |
| New York Times | ok (200) | ok (200) |
| Hacker News API (firebaseio) | TLS died | timeout (60 s) |
| npm registry | ok (200) | ok (200) |
| PyPI JSON | ok (200) | timeout (60 s) |
| crates.io API | ok (200) | timeout (60 s) |
| OpenStreetMap Nominatim | timeout (60 s) | HTTP 429 |
| Open-Meteo API | ok (200) | ok (200) |
| CoinGecko API | ok (200) | timeout (60 s) |
| Coinbase API | ok (200) | ok (200) |
| DuckDuckGo (html) | ok (202) | ok (202) |
| Archive.org | ok (200) | ok (200) |

Totals (54 requests):

| Outcome | Count | Share |
|---|---|---|
| ok | 33 | 61% |
| challenge or block page | 6 | 11% |
| HTTP error (403/400/429) | 6 | 11% |
| failed (mostly 60 s timeouts at exits) | 9 | 17% |

An earlier pass with a loose classifier (`bench/results/reach-1.json`)
falsely flagged pages that merely load reCAPTCHA; the table above uses
strict signals only.

### Reliability over 30 minutes (`bench/soak.mjs`)

One `check.torproject.org` request every 20 s, and a raw TCP echo every
third sample.

| Carrier | Samples ok | HTTP median / p90 | TCP echo median / p90 | Distinct exit IPs | Proxy carriers lost | Redials | Notes |
|---|---|---|---|---|---|---|---|
| WebRTC (Chromium 141) | **90/90** | 1628 / 2238 ms | 1163 / 1820 ms | 11 | 0 | 0 | 2 TLS handshakes died and were retried on new circuits; consensus refreshed in the background |
| WebSocket (Chromium 141) | **90/90** | 959 / 1769 ms | 772 / 1016 ms | 8 | 0 | 0 | 2 TLS handshakes died (`END` cell) and were retried on new circuits |

Proxy churn seen elsewhere: in the real-Safari WebRTC run, one proxy dropped
during bootstrap. The redial attached a new proxy with the same ClientID and
the session continued (`carriersLost: 1`, `carriersOpened: 3`). Arti opens
one Snowflake session per bridge in use (2 bridges configured).

## 5. Wasm size

| Artifact | Bytes |
|---|---|
| `nukemichi_bg.wasm` raw (wasm-opt -Oz) | 3,545,663 |
| gzip -9 | 1,531,698 |
| brotli -q 11 | 1,173,890 |
| JS glue `nukemichi.js` + `js/src/*.js` | 53 KB + 48 KB (uncompressed) |

What dominates it (twiggy on a release build with names; proportions only):

| Part | Share |
|---|---|
| Arti crates (`tor_*`, `arti_client`) | 31% |
| Read-only data: webpki roots, brotli dictionary (~120 KB), tables | 18% |
| TLS (rustls, ring, webpki) | 9% |
| core/alloc/std/hashbrown | 8% |
| wasm-bindgen / js-sys / web-sys glue | 7% |
| Tor crypto (dalek, RustCrypto, rsa) | 3.5% |
| serde/config | 2.9% |
| Decompression (zstd for Tor dir docs, brotli) | 2.3% |
| Nukemichi (Snowflake, HTTP, pool) + kcp | 2.1% |

## 6. Open risks

- **Exit blocking and CAPTCHAs.** Big sites treat Tor exits as hostile:
  Google shows "unusual traffic", LinkedIn and Stack Overflow get Cloudflare
  challenges, X and Facebook return 403/400, Reddit blocks or rate-limits.
  APIs fare better than HTML front pages. A Nukemichi page cannot solve
  challenges on the user's behalf and should not try. 17% of requests timed
  out at exits even with retries, so callers need timeouts and fallbacks.
- **Latency.**
  - Cold start: 4–16 s.
  - Warm start: about 3 s before the first response.
  - Requests reusing a pooled connection: 0.26–0.66 s; a new origin takes 1.5–7 s.
  - Throughput: 0.2–0.5 MB/s.
  - Fine for API calls and small documents; poor for anything interactive or large.
- **Snowflake capacity.** Every Nukemichi user consumes volunteer proxies
  (WebRTC) or bridge capacity (both carriers). Snowflake exists for censored
  users; a popular site embedding Nukemichi could displace them. The direct
  WebSocket carrier uses no volunteer proxy, but it is not an advertised
  client path: the Tor Project could close it, and it offers no censorship
  resistance. Ship-blocking question: get the Snowflake operators' consent
  before any public deployment. A cold bootstrap moves ~3.7 MB through the
  bridge.
- **Directory freshness.** The bridge is the only directory source, and it
  served a consensus an hour stale (fresh until 02:00 when 03:00 was current).
  Arti logged repeated `Unable to advance downloading state` warnings. It
  refreshed later in the soak, but a lagging bridge delays fresh consensus for
  every user.
- **Browser support.**
  - Works: Chromium/Chrome, Firefox, WebKit and Safari 18.6 on macOS.
  - Not tested: mobile Safari/Chrome (memory: Arti's quota defaults to 1 GiB),
    Firefox with `media.peerconnection.enabled=false`, and enterprise policies
    that block WebRTC or the Snowflake broker.
  - A page's Content-Security-Policy must allow `wasm-unsafe-eval`,
    `connect-src` to the broker and bridge, and workers.
- **Supply chain.** It depends on the voltrevo/arti fork, which carries
  ~2,000 lines of patches on 2.7.0 that are not upstream. Upstream Arti has no
  wasm runtime or wasm CI, so any upgrade may break the build.
- **Security model.**
  - Isolation keys separate circuits, but by default all requests from a
    page share circuits (and so share an exit).
  - The page itself is trusted: it can read everything.
  - Exits see plaintext HTTP. HTTPS is end-to-end with full certificate checks.
- **Legal and ethical.**
  - This is a way around CORS, the same-origin defence that protects
    third-party servers' users. CORS protects servers from being read via a
    victim's ambient credentials; Nukemichi sends no browser cookies, so it
    does not reopen CSRF-style reads. It does make any page an anonymous
    scraper and port-reacher, using volunteer infrastructure.
  - Scraping or bulk use would abuse exits and Snowflake and invites blocking
    of Tor as a whole.
  - Exit operators bear the abuse complaints for traffic a site's users generate.
  - Some jurisdictions and networks block or penalise Tor use; a site that
    silently uses Tor on a visitor's machine exposes that visitor.
    End users should be told and should opt in.
  - Terms of service of many targets forbid automated or anonymised access.
  - Recommendation: do not ship this to end users without explicit user opt-in,
    a published usage policy, rate limits in the library, and agreement from
    the Tor/Snowflake teams.

## 7. Build and run

```bash
brew install wasm-pack binaryen llvm
rustup target add wasm32-unknown-unknown
cd crates/nukemichi-wasm && wasm-pack build --target web --release --out-dir ../../js/pkg --out-name nukemichi
cd ../.. && python3 -m http.server 8766
# open http://localhost:8766/demo/index.html
```

Benchmarks: `cd bench && npm install && npx playwright install chromium firefox webkit`,
then e.g. `node bench/phase0.mjs chromium webrtc`, `bench/run-phase0.sh`,
`bench/run-api.sh`, `node bench/reach.mjs chromium websocket 2`,
`node bench/soak.mjs chromium webrtc 30`, `node bench/exit-ports.mjs`,
`node bench/safari.mjs webrtc worker` (real Safari; needs Develop → Allow Remote Automation),
`CHANNEL=chrome node bench/phase0.mjs chromium websocket` (installed Chrome).
`python3 bench/tables.py` renders the tables.
