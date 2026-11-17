// Worker entry: runs the Tor engine (wasm Arti, TLS, HTTP) off the main
// thread. The main thread only does what workers cannot: WebRTC signaling.

import { createEngine } from './engine.js';
import {
  pipeReadableToPort, portToReadable, portToWritableSink, writableToPort,
  portChannel, serializeError,
} from './ports.js';

let engine = null;
const aborts = new Map();
let rtcSeq = 0;
const rtcWaiters = new Map();

const reply = (id, value, transfer) => postMessage({ id, ok: true, value }, transfer ?? []);
const fail = (id, e) => postMessage({ id, ok: false, error: serializeError(e) });
const log = (level, target, msg) => postMessage({ log: [level, target, msg] });

// Ask the main thread for a WebRTC data channel; bytes come over a port.
function openRtcViaMain(fingerprint) {
  const rid = ++rtcSeq;
  return new Promise((resolve, reject) => {
    rtcWaiters.set(rid, { resolve, reject });
    postMessage({ rtc: { rid, fingerprint } });
  });
}

const ops = {
  async create(id, opts) {
    engine = await createEngine({ ...opts, log, openRtc: openRtcViaMain });
    reply(id, engine.timings);
  },
  async fetch(id, { url, init, bodyPort }) {
    const ac = new AbortController();
    aborts.set(id, ac);
    try {
      const body = bodyPort ? portToReadable(bodyPort) : init.body;
      const res = await engine.fetch(url, { ...init, body, signal: ac.signal });
      let port = null;
      if (res.body) {
        const ch = new MessageChannel();
        pipeReadableToPort(res.body, ch.port1);
        port = ch.port2;
      }
      reply(id, { status: res.status, headers: [...res.headers], port }, port ? [port] : []);
    } finally {
      aborts.delete(id);
    }
  },
  async connect(id, { host, port, options }) {
    const sock = await engine.connect(host, port, options);
    const r = new MessageChannel();
    const w = new MessageChannel();
    pipeReadableToPort(sock.readable, r.port1);
    portToWritableSink(w.port1, sock.writable);
    reply(id, { readPort: r.port2, writePort: w.port2 }, [r.port2, w.port2]);
  },
  async abort(id, { target }) { aborts.get(target)?.abort(); reply(id); },
  async stats(id) { reply(id, { ...engine.stats(), status: engine.status() }); },
  async close(id) { await engine?.close(); engine = null; reply(id); },
};

onmessage = async (ev) => {
  const m = ev.data;
  if (m.rtcResult) {
    const w = rtcWaiters.get(m.rtcResult.rid);
    rtcWaiters.delete(m.rtcResult.rid);
    if (m.rtcResult.error) w?.reject(new Error(m.rtcResult.error));
    else w?.resolve(portChannel(m.rtcResult.port));
    return;
  }
  const op = ops[m.op];
  if (!op) return fail(m.id, new Error(`unknown op ${m.op}`));
  try { await op(m.id, m.args ?? {}); } catch (e) { fail(m.id, e); }
};

// Exported only so bundlers keep the module; unused.
export { writableToPort };
