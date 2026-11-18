import { sendInPieces } from './ports.js';

// byte-pipe helpers. a "socket" here is { readable, writable, close() }:
// the shape the wasm runtime (jsproxystream) consumes.

/**
 * Wrap a message-oriented channel (WebSocket or RTCDataChannel) as a socket.
 * `ch` must already be open and use binaryType 'arraybuffer'.
 */
export function messageChannelSocket(ch, { highWater = 1 << 20, onClose } = {}) {
  let closed = false;
  let rctl;
  const finish = (err) => {
    if (closed) return;
    closed = true;
    try { err ? rctl.error(err) : rctl.close(); } catch {}
    onClose?.(err);
  };
  const readable = new ReadableStream({
    start(c) { rctl = c; },
    cancel() { try { ch.close(); } catch {} },
  });
  ch.onmessage = (e) => {
    if (closed) return;
    const d = e.data;
    rctl.enqueue(d instanceof ArrayBuffer ? new Uint8Array(d) : new Uint8Array(d.buffer, d.byteOffset, d.byteLength));
  };
  ch.onclose = () => finish();
  ch.onerror = () => finish(new Error('carrier error'));

  // backpressure: wait while too much is buffered in the channel.
  const drain = () => new Promise((resolve) => {
    if (ch.bufferedAmount < highWater || closed) return resolve();
    const t = setInterval(() => {
      if (ch.bufferedAmount < highWater || closed) { clearInterval(t); resolve(); }
    }, 5);
  });
  const writable = new WritableStream({
    async write(chunk) {
      if (closed) throw new Error('carrier closed');
      await drain();
      if (typeof RTCDataChannel !== 'undefined' && ch instanceof RTCDataChannel) sendInPieces(ch, chunk);
      else ch.send(chunk);
    },
    close() { try { ch.close(); } catch {} },
    abort() { try { ch.close(); } catch {} },
  });
  return { readable, writable, close() { try { ch.close(); } catch {} finish(); } };
}
