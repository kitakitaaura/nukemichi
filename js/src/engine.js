// The Tor engine: loads the wasm, builds an Arti client that reaches Tor via
// Snowflake, and exposes fetch/connect. Runs in a dedicated worker (default)
// or on the main thread.

import init, { init as initLogging, setLogCallback, setLogLevel, TorClient, TorClientOptions, snowflakeStats, BrotliDecoder } from '../pkg/nukemichi.js';
import { BRIDGES, makeConnectFn, openDataChannel } from './snowflake.js';
import { IndexedDBStorage, MemoryStorage } from './storage.js';

let wasmReady = null;

// --- content decoding (runs where the wasm lives: the worker by default) ----

function nativeFormat(...names) {
  for (const n of names) { try { new DecompressionStream(n); return n; } catch {} }
  return null;
}
const NATIVE = { gzip: nativeFormat('gzip'), 'x-gzip': nativeFormat('gzip'), deflate: nativeFormat('deflate'), br: nativeFormat('brotli', 'br') };

/** TransformStream that undoes brotli with the wasm decoder. */
function wasmBrotli() {
  const d = new BrotliDecoder();
  return new TransformStream({
    transform(chunk, ctl) { const out = d.push(chunk); if (out.length) ctl.enqueue(out); },
    flush(ctl) { const out = d.finish(); if (out.length) ctl.enqueue(out); d.free(); },
  });
}

const DECODABLE = new Set(['gzip', 'x-gzip', 'deflate', 'br']);
export const ACCEPT_ENCODING = 'gzip, deflate, br';

/** Decode a Response per Content-Encoding (codings undone in reverse order). */
export function decodeResponse(res) {
  const ce = res.headers.get('content-encoding');
  if (!ce || !res.body) return res;
  const codings = ce.split(',').map((s) => s.trim().toLowerCase()).filter((c) => c && c !== 'identity');
  if (!codings.every((c) => DECODABLE.has(c))) return res;
  let body = res.body;
  for (const c of codings.reverse()) {
    body = body.pipeThrough(NATIVE[c] ? new DecompressionStream(NATIVE[c]) : wasmBrotli());
  }
  const headers = new Headers(res.headers);
  headers.delete('content-encoding');
  headers.delete('content-length');
  return new Response(body, { status: res.status, statusText: res.statusText, headers });
}

export async function loadWasm(wasmUrl) {
  if (!wasmReady) {
    wasmReady = init(wasmUrl ? { module_or_path: wasmUrl } : undefined);
  }
  await wasmReady;
}

/**
 * Create a bootstrapped engine.
 * @param {object} o
 * @param {'webrtc'|'websocket'} o.transport
 * @param {'indexeddb'|'memory'} o.storage
 * @param {string[]} o.bridges  names from BRIDGES
 * @param {(level,target,msg)=>void} o.log
 * @param {(fingerprint)=>Promise<{dc,pc}|RTCDataChannel>} o.openRtc  WebRTC carrier factory
 */
export async function createEngine(o) {
  const t0 = performance.now();
  await loadWasm(o.wasmUrl);
  const tWasm = performance.now();
  initLogging(o.logLevel ?? 'info');
  setLogCallback((level, target, msg) => o.log?.(level, target, msg));

  const carriers = [];
  const connect = makeConnectFn({
    transport: o.transport ?? 'webrtc',
    log: o.log,
    openRtc: o.openRtc ?? ((fp) => openDataChannel(fp, { log: o.log })),
    onCarrier: (c) => carriers.push({ ...c, at: performance.now() - t0 }),
  });

  let options = new TorClientOptions(connect);
  const store = o.storage === 'memory' ? new MemoryStorage() : new IndexedDBStorage(o.dbName ?? 'nukemichi');
  if (o.clearStorage) await store.clear();
  options = options.withStorage(store);
  for (const name of o.bridges ?? ['snowflake-01', 'snowflake-02']) {
    // Direct-WebSocket mode can only reach bridges that publish an endpoint.
    if (o.transport === 'websocket' && !BRIDGES[name].websocket) continue;
    options = options.withSnowflakeBridge(BRIDGES[name].line);
  }
  const client = await TorClient.create(options);
  const tBoot = performance.now();
  return {
    client,
    timings: { wasmMs: tWasm - t0, bootstrapMs: tBoot - tWasm, totalMs: tBoot - t0, carriers },
    fetch: async (url, init) => decodeResponse(await client.fetch(url, init ?? {})),
    connect: (host, port, opts = {}) => client.connect(host, port, !!opts.tls, opts),
    status: () => client.bootstrapStatus(),
    stats: () => {
      const [carriersOpened, carriersLost, redialFailures, bytesIn, bytesOut] = snowflakeStats();
      return { carriersOpened, carriersLost, redialFailures, bytesIn, bytesOut };
    },
    setLogLevel,
    close: () => client.close(),
  };
}
