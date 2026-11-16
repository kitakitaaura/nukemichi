// The Tor engine: loads the wasm, builds an Arti client that reaches Tor via
// Snowflake, and exposes fetch/connect. Runs in a dedicated worker (default)
// or on the main thread.

import init, { init as initLogging, setLogCallback, setLogLevel, TorClient, TorClientOptions, snowflakeStats } from '../pkg/nukemichi.js';
import { BRIDGES, makeConnectFn, openDataChannel } from './snowflake.js';
import { IndexedDBStorage, MemoryStorage } from './storage.js';

let wasmReady = null;

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
    fetch: (url, init) => client.fetch(url, init ?? {}),
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
