// Moving byte streams across a MessagePort.
//
// WebKit cannot transfer ReadableStream/WritableStream to a worker, and only
// Chromium can transfer an RTCDataChannel (and only in the task that created
// it). So every byte that crosses the worker boundary goes over a
// MessagePort, with credit-based flow control so a slow reader stalls the
// writer instead of filling memory.

const CREDIT = 16; // chunks in flight per stream

function toBuffer(chunk) {
  const u8 = chunk instanceof Uint8Array ? chunk : new Uint8Array(chunk);
  // Copy unless the view covers its whole buffer, so we can transfer it.
  return u8.byteOffset === 0 && u8.byteLength === u8.buffer.byteLength ? u8.buffer : u8.slice().buffer;
}

export function serializeError(e) {
  return { name: e?.name ?? 'Error', message: e?.message ?? String(e), code: e?.code, kind: e?.kind, retryable: e?.retryable, torKind: e?.torKind };
}

export function deserializeError(o) {
  const e = new Error(o.message);
  e.name = o.name;
  for (const k of ['code', 'kind', 'retryable', 'torKind']) if (o[k] !== undefined) e[k] = o[k];
  return e;
}

/** Send `readable` over `port`. The far side uses portToReadable(). */
export function pipeReadableToPort(readable, port) {
  const reader = readable.getReader();
  let credit = 0;
  let pumping = false;
  let finished = false;
  const pump = async () => {
    if (pumping || finished) return;
    pumping = true;
    try {
      while (credit > 0 && !finished) {
        const { value, done } = await reader.read();
        if (done) { finished = true; port.postMessage({ t: 'end' }); break; }
        credit--;
        const buf = toBuffer(value);
        port.postMessage({ t: 'chunk', buf }, [buf]);
      }
    } catch (e) {
      finished = true;
      port.postMessage({ t: 'error', error: serializeError(e) });
    }
    pumping = false;
  };
  port.onmessage = (ev) => {
    const m = ev.data;
    if (m.t === 'credit') { credit += m.n; pump(); }
    else if (m.t === 'cancel') { finished = true; reader.cancel(m.reason).catch(() => {}); port.close(); }
  };
}

/** The receiving end of pipeReadableToPort(). */
export function portToReadable(port) {
  let outstanding = 0;
  return new ReadableStream({
    start(c) {
      port.onmessage = (ev) => {
        const m = ev.data;
        if (m.t === 'chunk') { outstanding--; c.enqueue(new Uint8Array(m.buf)); }
        else if (m.t === 'end') { c.close(); port.close(); }
        else if (m.t === 'error') { c.error(deserializeError(m.error)); port.close(); }
      };
      outstanding = CREDIT;
      port.postMessage({ t: 'credit', n: CREDIT });
    },
    pull() {
      // Top up credit as the consumer drains the queue.
      if (outstanding < CREDIT / 2) {
        const n = CREDIT - outstanding;
        outstanding += n;
        port.postMessage({ t: 'credit', n });
      }
    },
    cancel(reason) { port.postMessage({ t: 'cancel', reason: String(reason ?? '') }); },
  }, { highWaterMark: CREDIT });
}

/** Make a WritableStream whose chunks go over `port` (see portToWritableSink). */
export function writableToPort(port) {
  let seq = 0;
  const waiting = new Map();
  port.onmessage = (ev) => {
    const m = ev.data;
    const w = waiting.get(m.id);
    if (!w) return;
    waiting.delete(m.id);
    m.error ? w.reject(deserializeError(m.error)) : w.resolve();
  };
  const call = (msg, transfer) => new Promise((resolve, reject) => {
    const id = ++seq;
    waiting.set(id, { resolve, reject });
    port.postMessage({ ...msg, id }, transfer ?? []);
  });
  return new WritableStream({
    write(chunk) { const buf = toBuffer(chunk); return call({ t: 'write', buf }, [buf]); },
    close() { return call({ t: 'close' }); },
    abort(reason) { return call({ t: 'abort', reason: String(reason ?? '') }); },
  });
}

/** The receiving end of writableToPort(): forwards into `writable`. */
export function portToWritableSink(port, writable) {
  const writer = writable.getWriter();
  port.onmessage = async (ev) => {
    const m = ev.data;
    try {
      if (m.t === 'write') await writer.write(new Uint8Array(m.buf));
      else if (m.t === 'close') await writer.close();
      else if (m.t === 'abort') await writer.abort(m.reason);
      port.postMessage({ id: m.id });
    } catch (e) {
      port.postMessage({ id: m.id, error: serializeError(e) });
    }
  };
}

/**
 * A DataChannel-like object over a MessagePort, for the worker side of a
 * WebRTC carrier whose real RTCDataChannel lives on the main thread.
 */
export function portChannel(port) {
  const ch = {
    bufferedAmount: 0,
    onmessage: null,
    onclose: null,
    onerror: null,
    send(data) { const buf = toBuffer(data); port.postMessage({ t: 'data', buf }, [buf]); },
    close() { port.postMessage({ t: 'close' }); port.close(); },
  };
  port.onmessage = (ev) => {
    const m = ev.data;
    if (m.t === 'data') ch.onmessage?.({ data: m.buf });
    else if (m.t === 'close') { ch.onclose?.(); port.close(); }
    else if (m.t === 'error') ch.onerror?.(new Error(m.message));
  };
  return ch;
}

/** Main-thread side: relay an open RTCDataChannel over `port`. */
export function relayChannelToPort(dc, pc, port) {
  dc.binaryType = 'arraybuffer';
  dc.onmessage = (e) => { const buf = e.data; port.postMessage({ t: 'data', buf }, [buf]); };
  dc.onclose = () => { port.postMessage({ t: 'close' }); pc?.close(); };
  dc.onerror = () => port.postMessage({ t: 'error', message: 'data channel error' });
  port.onmessage = (ev) => {
    const m = ev.data;
    if (m.t === 'data') { if (dc.readyState === 'open') dc.send(m.buf); }
    else if (m.t === 'close') { dc.close(); pc?.close(); }
  };
}
