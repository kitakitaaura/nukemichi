// Snowflake carriers: the byte pipes the wasm Snowflake layer runs over.
//
// - 'webrtc': ask the Snowflake broker for a volunteer proxy, open a WebRTC
//   data channel to it. The proxy relays bytes to the bridge. This is how
//   Tor Browser uses Snowflake. Needs RTCPeerConnection (main thread only).
// - 'websocket': open a WebSocket straight to the bridge's WebSocket endpoint
//   (the one proxies use). No volunteer proxy involved. Works in workers.
//
// Both carry the same Turbo Tunnel / KCP / smux stream, which the wasm side
// speaks. Redials after a carrier dies reuse the session.

import { messageChannelSocket } from './pipe.js';

/** Tor's built-in Snowflake bridges (bridges.torproject.org moat "builtin"). */
export const BRIDGES = {
  'snowflake-01': {
    line: '192.0.2.3:80 2B280B23E1107BB62ABFC40DDCC8824814F80A72',
    fingerprint: '2B280B23E1107BB62ABFC40DDCC8824814F80A72',
    websocket: 'wss://snowflake.torproject.net/',
  },
  'snowflake-02': {
    line: '192.0.2.4:80 8838024498816A039FCBBAB14E6F40A0843051FA',
    fingerprint: '8838024498816A039FCBBAB14E6F40A0843051FA',
    websocket: null,
  },
};

export const BROKER = 'https://snowflake-broker.torproject.net/';

/** STUN servers from the built-in bridge lines. */
export const ICE_SERVERS = [{
  urls: [
    'stun:stun.epygi.com:3478', 'stun:stun.uls.co.za:3478', 'stun:stun.voipgate.com:3478',
    'stun:stun.mixvoip.com:3478', 'stun:stun.telnyx.com:3478', 'stun:stun.hot-chilli.net:3478',
    'stun:stun.fitauto.ru:3478', 'stun:stun.m-online.net:3478',
  ],
}];

const withTimeout = (p, ms, what) => Promise.race([
  p,
  new Promise((_, rej) => setTimeout(() => rej(new Error(`${what} timed out after ${ms} ms`)), ms)),
]);

function bridgeByAddr(addr) {
  for (const b of Object.values(BRIDGES)) if (b.line.startsWith(addr + ' ')) return b;
  throw new Error(`unknown Snowflake bridge address ${addr}`);
}

/**
 * Rendezvous with the broker and open a WebRTC data channel to a proxy.
 * Resolves to the open RTCDataChannel and its RTCPeerConnection.
 */
export async function openDataChannel(fingerprint, { broker = BROKER, iceServers = ICE_SERVERS, log } = {}) {
  const pc = new RTCPeerConnection({ iceServers });
  const dc = pc.createDataChannel(crypto.randomUUID(), { ordered: true });
  dc.binaryType = 'arraybuffer';
  try {
    await pc.setLocalDescription(await pc.createOffer());
    // Snowflake does not trickle ICE: send the offer with all candidates.
    await withTimeout(new Promise((resolve) => {
      if (pc.iceGatheringState === 'complete') return resolve();
      pc.addEventListener('icegatheringstatechange', () => {
        if (pc.iceGatheringState === 'complete') resolve();
      });
    }), 15000, 'ICE gathering').catch(() => {}); // use what we have
    const offer = JSON.stringify({ type: 'offer', sdp: pc.localDescription.sdp });
    const body = '1.0\n' + JSON.stringify({ offer, nat: 'unknown', fingerprint });
    const t0 = performance.now();
    const res = await withTimeout(fetch(broker + 'client', { method: 'POST', body }), 60000, 'broker poll');
    const msg = await res.json();
    if (msg.error) throw new Error(`broker: ${msg.error}`);
    if (!msg.answer) throw new Error('broker: no answer');
    log?.('info', 'snowflake', `broker matched a proxy in ${Math.round(performance.now() - t0)} ms`);
    const answer = JSON.parse(msg.answer);
    await pc.setRemoteDescription({ type: 'answer', sdp: answer.sdp });
    await withTimeout(new Promise((resolve, reject) => {
      if (dc.readyState === 'open') return resolve();
      dc.onopen = () => resolve();
      dc.onerror = () => reject(new Error('data channel error'));
      dc.onclose = () => reject(new Error('data channel closed before open'));
    }), 30000, 'data channel open');
    return { dc, pc };
  } catch (e) {
    pc.close();
    throw e;
  }
}

/** Open a WebSocket to the bridge (no proxy). */
export async function openBridgeWebSocket(url) {
  const ws = new WebSocket(url);
  ws.binaryType = 'arraybuffer';
  await withTimeout(new Promise((resolve, reject) => {
    ws.onopen = () => resolve();
    ws.onerror = () => reject(new Error(`WebSocket to ${url} failed`));
  }), 30000, 'WebSocket open');
  return ws;
}

/**
 * Build the connect function the wasm runtime calls. It is only ever asked
 * for Snowflake carriers ("snowflake:<addr>"); any other target is refused,
 * because with bridges on, Arti must not dial relays directly.
 *
 * `openRtc(fingerprint)` returns an open RTCDataChannel-like object; it is
 * injected so that a worker can ask the main thread for one.
 */
export function makeConnectFn({ transport, log, openRtc, onCarrier }) {
  return async (target) => {
    if (!target.startsWith('snowflake:')) {
      throw new Error(`refusing direct connection to ${target}: Nukemichi only connects via Snowflake`);
    }
    const bridge = bridgeByAddr(target.slice('snowflake:'.length));
    const t0 = performance.now();
    let sock;
    if (transport === 'websocket') {
      if (!bridge.websocket) throw new Error('this bridge has no WebSocket endpoint');
      const ws = await openBridgeWebSocket(bridge.websocket);
      sock = messageChannelSocket(ws);
    } else {
      const ch = await openRtc(bridge.fingerprint);
      sock = messageChannelSocket(ch.dc ?? ch, { onClose: () => ch.pc?.close() });
    }
    const ms = Math.round(performance.now() - t0);
    log?.('info', 'snowflake', `${transport} carrier open in ${ms} ms`);
    onCarrier?.({ transport, ms });
    return sock;
  };
}
