// Nukemichi public API.
//
//   import nukemichi from 'nukemichi';
//   const res = await nukemichi.fetch('https://example.com/');   // no CORS needed
//   const sock = await nukemichi.connect('tcpbin.com', 4242);    // raw TCP
//
// By default the Tor engine runs in a dedicated worker. The page keeps only
// WebRTC signaling (workers have no RTCPeerConnection) and byte relaying.

import {
  deserializeError, pipeReadableToPort, portToReadable, relayChannelToPort, writableToPort,
} from './ports.js';
import { openDataChannel } from './snowflake.js';

const REDIRECTS = new Set([301, 302, 303, 307, 308]);
const NULL_BODY = new Set([101, 103, 204, 205, 304]);

// Content decoding happens in the engine (engine.js decodeResponse), which
// can use the wasm brotli decoder where DecompressionStream lacks brotli.
const ACCEPT_ENCODING = 'gzip, deflate, br';

// Request normalization

async function normalize(input, init = {}) {
  const base = input instanceof Request ? input : null;
  const url = new URL(base ? base.url : String(input), globalThis.location?.href);
  if (url.protocol !== 'https:' && url.protocol !== 'http:') throw new TypeError(`nukemichi.fetch: unsupported scheme ${url.protocol}`);
  const method = (init.method ?? base?.method ?? 'GET').toUpperCase();
  // A standalone Headers object has no forbidden-header guard: callers may set
  // Origin, Cookie, Referer, etc. That is the point of a CORS-free fetch.
  const headers = new Headers(init.headers ?? base?.headers ?? undefined);
  let body = init.body !== undefined ? init.body : base?.body ?? null;
  if (body != null && !(body instanceof ReadableStream) && typeof body !== 'string' && !(body instanceof Uint8Array)) {
    // Blob, FormData, URLSearchParams, ArrayBuffer, views: let the platform
    // serialize them (and pick Content-Type, e.g. the multipart boundary).
    const r = new Request('https://x.invalid/', { method: 'POST', body });
    if (!headers.has('content-type') && r.headers.get('content-type')) headers.set('content-type', r.headers.get('content-type'));
    body = new Uint8Array(await r.arrayBuffer());
  } else if (typeof body === 'string' && !headers.has('content-type')) {
    headers.set('content-type', 'text/plain;charset=UTF-8');
  }
  if (!headers.has('accept-encoding')) headers.set('accept-encoding', ACCEPT_ENCODING);
  return { url, method, headers, body, redirect: init.redirect ?? 'follow', signal: init.signal ?? base?.signal, anonOptions: init.anonOptions };
}

/**
 * fetch() semantics on top of a raw HTTP/1.1 exchange `raw(url, init)` that
 * returns a Response without following redirects or decoding bodies.
 */
async function fetchWith(raw, input, init) {
  let req = await normalize(input, init);
  let redirected = false;
  for (let hop = 0; ; hop++) {
    req.signal?.throwIfAborted();
    const res = await raw(req.url.href, {
      method: req.method,
      headers: Object.fromEntries(req.headers),
      body: req.body ?? undefined,
      signal: req.signal,
      ...(req.anonOptions ? { anonOptions: req.anonOptions } : {}),
    });
    const location = res.headers.get('location');
    if (REDIRECTS.has(res.status) && location && req.redirect !== 'manual') {
      if (req.redirect === 'error') throw new TypeError(`nukemichi.fetch: redirect to ${location} with redirect: 'error'`);
      if (hop >= 20) throw new TypeError('nukemichi.fetch: too many redirects');
      res.body?.cancel().catch(() => {});
      const next = new URL(location, req.url);
      if (next.protocol !== 'https:' && next.protocol !== 'http:') throw new TypeError(`nukemichi.fetch: redirect to unsupported scheme ${next.protocol}`);
      if ((res.status === 303 && req.method !== 'HEAD') || ((res.status === 301 || res.status === 302) && req.method === 'POST')) {
        req.method = 'GET';
        req.body = null;
        for (const h of ['content-type', 'content-length', 'content-encoding', 'content-language', 'content-location']) req.headers.delete(h);
      } else if (req.body instanceof ReadableStream) {
        throw new TypeError('nukemichi.fetch: cannot replay a streamed body for a 307/308 redirect');
      }
      if (next.origin !== req.url.origin) req.headers.delete('authorization');
      req.url = next;
      redirected = true;
      continue;
    }
    const headers = new Headers(res.headers);
    const body = res.body;
    const out = new Response(NULL_BODY.has(res.status) || req.method === 'HEAD' ? null : body, {
      status: res.status,
      statusText: res.statusText,
      headers,
    });
    Object.defineProperty(out, 'url', { value: req.url.href });
    Object.defineProperty(out, 'redirected', { value: redirected });
    return out;
  }
}

// Engine hosts

// A worker script must be same-origin, so when this module comes from a CDN,
// start a blob worker that imports the real one (the CDN must allow CORS).
function makeWorker(url) {
  const u = new URL(url, location.href);
  if (u.origin === location.origin) return new Worker(u, { type: 'module' });
  const src = URL.createObjectURL(new Blob([`import ${JSON.stringify(u.href)};`], { type: 'text/javascript' }));
  const w = new Worker(src, { type: 'module' });
  setTimeout(() => URL.revokeObjectURL(src), 60000);
  return w;
}

/** Run the engine in a dedicated worker; relay WebRTC from the main thread. */
class WorkerHost {
  constructor(opts) {
    this.worker = makeWorker(opts.workerUrl ?? new URL('./worker.js', import.meta.url));
    this.seq = 0;
    this.pending = new Map();
    this.log = opts.log;
    this.worker.onmessage = (ev) => this.onMessage(ev.data);
    // A worker that fails to load (file:// pages, a CSP without blob:) reports
    // only this event, with no message; fail every waiting call instead of hanging.
    this.worker.onerror = (e) => {
      this.failed = new Error(`nukemichi: worker failed${e.message ? `: ${e.message}` : ' to start'}`);
      for (const p of this.pending.values()) p.reject(this.failed);
      this.pending.clear();
    };
  }
  onMessage(m) {
    if (m.log) { this.log?.(...m.log); return; }
    if (m.rtc) { this.openRtc(m.rtc); return; }
    const p = this.pending.get(m.id);
    if (!p) return;
    this.pending.delete(m.id);
    m.ok ? p.resolve(m.value) : p.reject(deserializeError(m.error));
  }
  async openRtc({ rid, fingerprint }) {
    try {
      const { dc, pc } = await openDataChannel(fingerprint, { log: this.log });
      const ch = new MessageChannel();
      relayChannelToPort(dc, pc, ch.port1);
      this.worker.postMessage({ rtcResult: { rid, port: ch.port2 } }, [ch.port2]);
    } catch (e) {
      this.worker.postMessage({ rtcResult: { rid, error: e.message } });
    }
  }
  call(op, args, transfer = []) {
    const id = ++this.seq;
    return { id, promise: new Promise((resolve, reject) => {
      if (this.failed) { reject(this.failed); return; }
      this.pending.set(id, { resolve, reject });
      this.worker.postMessage({ id, op, args }, transfer);
    }) };
  }
  create(opts) {
    const { log, workerUrl, worker, ...plain } = opts;
    return this.call('create', plain).promise;
  }
  async raw(url, init) {
    const { signal, body, ...rest } = init;
    let bodyPort = null;
    const transfer = [];
    let plainBody = body;
    if (body instanceof ReadableStream) {
      const ch = new MessageChannel();
      pipeReadableToPort(body, ch.port1);
      bodyPort = ch.port2;
      transfer.push(bodyPort);
      plainBody = undefined;
    }
    const { id, promise } = this.call('fetch', { url, init: { ...rest, body: plainBody }, bodyPort }, transfer);
    const onAbort = () => this.call('abort', { target: id });
    signal?.addEventListener('abort', onAbort, { once: true });
    try {
      const r = await promise;
      return new Response(r.port ? portToReadable(r.port) : null, { status: r.status, headers: r.headers });
    } finally {
      signal?.removeEventListener('abort', onAbort);
    }
  }
  async connect(host, port, options) {
    const r = await this.call('connect', { host, port, options }).promise;
    return { readable: portToReadable(r.readPort), writable: writableToPort(r.writePort) };
  }
  stats() { return this.call('stats').promise; }
  async close() { await this.call('close').promise; this.worker.terminate(); }
}

/** Run the engine on the calling thread. */
class InlineHost {
  constructor(opts) { this.opts = opts; }
  async create(opts) {
    const { createEngine } = await import('./engine.js');
    this.engine = await createEngine(opts);
    return this.engine.timings;
  }
  raw(url, init) { return this.engine.fetch(url, init); }
  connect(host, port, options) { return this.engine.connect(host, port, options); }
  async stats() { return { ...this.engine.stats(), status: this.engine.status() }; }
  close() { return this.engine.close(); }
}

/**
 * Create a Nukemichi client. Bootstrapping starts immediately.
 *
 * @param {object} [opts]
 * @param {'webrtc'|'websocket'} [opts.transport='webrtc'] Snowflake carrier.
 *   'webrtc' goes through volunteer Snowflake proxies (as Tor Browser does).
 *   'websocket' connects straight to the bridge; it uses no volunteer proxy,
 *   but the Tor Project could close that path at any time.
 * @param {boolean} [opts.worker=true] Run Tor in a dedicated worker.
 * @param {'indexeddb'|'memory'} [opts.storage='indexeddb'] Where Arti keeps
 *   its guard state and directory cache (enables fast warm starts).
 * @param {(level: string, target: string, msg: string) => void} [opts.log]
 * @param {'trace'|'debug'|'info'|'warn'|'error'} [opts.logLevel='warn']
 */
export function createClient(opts = {}) {
  const o = { transport: 'webrtc', worker: true, storage: 'indexeddb', logLevel: 'warn', ...opts };
  let host;
  const ready = (async () => {
    if (o.worker) {
      let w;
      try {
        w = new WorkerHost(o);
        host = w;
        return await w.create(o);
      } catch (e) {
        if (w && !w.failed) throw e;
        w?.worker.terminate();
        o.log?.('warn', 'nukemichi', `${e.message}; running Tor on the main thread`);
      }
    }
    host = new InlineHost(o);
    return host.create(o);
  })();
  const client = {
    /** Resolves with bootstrap timings once Tor is usable. */
    ready: () => ready,
    /** fetch() through Tor: no CORS, redirects followed, gzip/deflate/br decoded. */
    fetch: async (input, init) => { await ready; return fetchWith((u, i) => host.raw(u, i), input, init); },
    /**
     * Raw TCP through Tor. Resolves to { readable, writable }.
     * @param {{tls?: boolean, anonOptions?: {isolationKey?: string}}} [options]
     *   tls: run TLS with full certificate checks for `hostname`.
     */
    connect: async (hostname, port, options = {}) => { await ready; return host.connect(hostname, port, options); },
    stats: async () => { await ready; return host.stats(); },
    close: async () => { await ready.catch(() => {}); return host.close(); },
  };
  return client;
}

let shared = null;
let sharedOpts = {};

/** Default client, created on first use. */
const nukemichi = {
  configure(opts) { if (shared) throw new Error('nukemichi: configure() must come before first use'); sharedOpts = opts; },
  client() { return (shared ??= createClient(sharedOpts)); },
  fetch: (input, init) => nukemichi.client().fetch(input, init),
  connect: (host, port, options) => nukemichi.client().connect(host, port, options),
  ready: () => nukemichi.client().ready(),
  createClient,
};

export default nukemichi;
export { fetchWith as _fetchWith };
