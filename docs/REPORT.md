# Nukemichi feasibility report (interim, 2026-11-16)

Status: Phase 0 done. Phase 1 code written but not yet re-measured
(the API test suite `bench/api.mjs` has not run yet). Phase 2 scripts written, not run.

## Verdicts so far

| Phase | Verdict | Evidence |
|---|---|---|
| 0: proof of concept | **Works** (Snowflake). WebTunnel **does not work** from a page. | All three proofs pass in Chromium 141, Firefox 142, WebKit 26 over both Snowflake carriers. |
| 1: library | In progress | worker/inline hosts, fetch semantics, connect, pool, retry written; Rust compiles; not yet re-measured |
| 2: reach | Not run | scripts ready |

## Prior art (checked 2026-11-16)

| Project | Licence | Maintenance | Wasm / transports | Verdict |
|---|---|---|---|---|
| webtor-rs (privacy-ethereum) v0.5.7 | MIT | Last human commit 2026-01-30; since then only a daily bot that updates the cached consensus | Vendors a patched Arti 1.8 (tor-proto only; its own path selection). Own TLS stack (`subtle-tls`). Snowflake WebRTC + WebSocket in wasm. **WebTunnel is native-only** (`"WebTunnel is not supported in WASM"` in client.rs), even though the README says otherwise. | Rejected. Loads its consensus from `privacy-ethereum.github.io/webtor-rs/consensus.txt.br`, which now returns 404. It also never checks consensus signatures (`dangerously_assume_timely()` and then reads `.consensus` directly). At HEAD, with the consensus served locally, it never opened a channel (`Channel not established`). |
| tor-js (ethereum/tor-js, "TorJS") v0.4.4 | MIT OR Apache-2.0 | Active (release 2026-10-29) | Full `arti-client` from the voltrevo/arti fork; IndexedDB storage; streaming fetch. Browsers **require a gateway server** (KPS over WebRTC + HTTP CONNECT); no Snowflake/WebTunnel. TLS via `rustls-rustcrypto 0.0.2-alpha`. | Used as the base: its wasm runtime/storage/fetch are good; the gateway contradicts the "no proxy server" goal, so it is replaced with Snowflake. |
| Arti upstream (2.7.0, crates 0.48) | MIT OR Apache-2.0 | Active | `arti-client --no-default-features` builds for wasm32 since spring 2026 (MRs 3816, 3819, 3820). There is no upstream wasm runtime or storage backend: MRs 4160–4166 (in-memory state, BlobStore seam, wasm CI) were closed unmerged 2026-06-28. Issues #2445 and #2446 (no wasm in CI) are open. | Not usable alone; the voltrevo fork adds ~2,000 lines (custom dir/state storage, time fixes) on top of 2.7.0. |

## Phase 0 evidence

No-CORS endpoint: `curl -D - -H 'Origin: https://nukemichi.test' https://example.com/`
returns `HTTP/2 200` with no `Access-Control-Allow-Origin` (also www.iana.org,
www.kernel.org).

Chromium 141, Snowflake via WebSocket (`bench/phase0.mjs`):
- `check.torproject.org/api/ip` returned `{"IsTor":true,"IP":"109.70.100.7"}`
- plain `fetch('https://example.com/')` threw `TypeError: Failed to fetch`
- `nukemichi` fetch: 200, full body, title "Example Domain", no ACAO header
- `tcpbin.com:4242` raw TCP echo: sent == received, connect 1095 ms, echo RTT 348 ms

| Browser | Carrier | Cold boot | IsTor | plain fetch | Tor fetch | TCP echo |
|---|---|---|---|---|---|---|
| Chromium 141 | WebSocket | 5.6 s | true | `TypeError: Failed to fetch` | 200 | ok, 348 ms |
| Chromium 141 | WebRTC | 7.2 s (28.7 s on an earlier run) | true | — | 200 | — |
| Firefox 142 | WebSocket | 7.9 s | true | `TypeError: NetworkError…` | 200 | ok, 556 ms |
| Firefox 142 | WebRTC | 11.1 s | true | same | 200 | ok, 357 ms |
| WebKit 26 | WebSocket | 4.2 s | true | `TypeError: Load failed` | 200 | ok, 425 ms |
| WebKit 26 | WebRTC | 9.9 s | true | same | 200 | ok, 382 ms |

### WebTunnel cannot be used from a page

WebTunnel's server (`transport/httpupgrade/httpupgrade.go`) answers with a bare
`101` that has no `Sec-WebSocket-Accept`, then sends raw bytes rather than
WebSocket frames. I tested a browser `WebSocket` against WebTunnel's real
`httpupgrade.Server` (`bench/webtunnel-ws.mjs`):
- Chromium 141 and WebKit 26: the upgrade succeeds on the server, then the
  browser rejects the response (`error`, close 1006).
- Firefox 142: sends `Connection: keep-alive, Upgrade`, and the server
  rejects that header (`upgrade failed: unrecognized request`).

So the WebTunnel row of the measurements is replaced by "Snowflake bridge via
direct WebSocket".

### Measurements (first build, no retry)

| Browser | Carrier | Cold boot | Warm boot (IndexedDB) | First request after warm boot | RTT, new TLS conn each | 1 MiB download |
|---|---|---|---|---|---|---|
| Chromium 141 | WebSocket | 5.6 s | 0.40 s | 3.6 s | 0.9–1.6 s | 4.5 s (226 KB/s) |
| Firefox 142 | WebSocket | 8.3 s | 3.3 s | 3.5 s | 1.3–2.6 s | 4.4 s (233 KB/s) |
| WebKit 26 | WebSocket | 4.4 s | 0.39 s | 2.9 s | 1.1–1.7 s | 5.0 s (205 KB/s) |
| WebKit 26 | WebRTC | 10.2 s | 0.39 s | 9.5 s | 1.4–4.8 s | 8.7 s (117 KB/s) |

Cold bootstrap downloads about 3.1 MB through the bridge (consensus +
~9,450 microdescriptors) and uploads about 0.57 MB.

**Reliability without retry: 3 of 14 batch runs completed**
(`bench/results/phase0-noretry.jsonl`). All 11 failures were on the exit side:
- `timed out at exit`: 8
- `END cell MISC`: 2
- `tls handshake eof` / `CONNRESET`: 2

Arti keeps reusing a circuit whose exit failed, so one bad exit failed every
later request. Fix (`conn.rs`): on exit-side errors, rotate the isolation
token so the next attempt uses a fresh circuit (3 attempts in total).

With keep-alive pooling, repeated requests to the same origin dropped from
about 1.5 s to 0.33–0.42 s.

## Phase 1 design evidence

API availability in a dedicated worker (`bench`, Playwright 1.56):

| | Chromium 141 | Firefox 142 | WebKit 26 |
|---|---|---|---|
| `RTCPeerConnection` in worker | no | no | no |
| `RTCDataChannel` in worker | yes | no | yes |
| Transfer `RTCDataChannel` to worker | only in the same task as `createDataChannel` | no (`DataCloneError`) | no |
| Transfer `ReadableStream`/`WritableStream` | yes | yes | no ("can not be cloned") |
| `WebSocket`, IndexedDB, OPFS, `navigator.locks` in worker | yes | yes | yes |

Main-thread cost of running Tor inline (Chromium, `bench/mainthread.mjs`): 2
long tasks during bootstrap (148 ms max, 200 ms total); none during a 1 MiB
download.

Crypto backends for `wasm32-unknown-unknown` (rustls ClientConfig only):

| Backend | Builds? | Toolchain | Size after wasm-opt -Oz | gzip |
|---|---|---|---|---|
| ring 0.17 | yes, with feature `wasm32_unknown_unknown_js` | clang with the wasm32 target (Homebrew LLVM); Apple clang fails ("No available targets are compatible with triple") | 687 KB | 210 KB |
| aws-lc-rs | no | `aws-lc-sys` needs libc headers (`stdlib.h` not found) with either clang | – | – |
| rustls-rustcrypto 0.0.2-alpha | yes, with `rustls-pki-types/web` | pure Rust | 665 KB | 182 KB |

Chosen: ring (mature, BoringSSL-derived). Certificate validation: rustls +
webpki-roots, and there is no option to turn it off.

Wasm size, current build (`js/pkg/nukemichi_bg.wasm`): 3,375,713 bytes raw
(gzip size still to measure).

## Next steps

1. Run `bench/api.mjs` across 3 browsers × 2 carriers × worker/inline.
2. Re-run the Phase 0 batch with retry and pooling (`bench/run-phase0.sh`).
3. Phase 2: `bench/reach.mjs`, `bench/exit-ports.mjs`, `bench/soak.mjs` (30 min).
4. Size breakdown (twiggy), gzip/brotli size, final report.
