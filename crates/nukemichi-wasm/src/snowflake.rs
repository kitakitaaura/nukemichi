//! Snowflake client tunnel: Turbo Tunnel encapsulation + KCP + smux v2.
//!
//! This is the client half of the protocol that the Snowflake bridge
//! (snowflake-server) speaks. The browser only provides a *carrier*: a
//! reliable, ordered byte pipe to the bridge, which is either
//!
//! - a WebRTC data channel to a volunteer Snowflake proxy (the proxy copies
//!   bytes to the bridge over WebSocket), or
//! - a WebSocket straight to the bridge's WebSocket endpoint.
//!
//! On top of the carrier we run, exactly as the Go client in
//! `snowflake/client/lib/snowflake.go` does:
//!
//! 1. Turbo Tunnel preamble: 8-byte magic token, then the 8-byte ClientID.
//! 2. Length-prefixed encapsulation of KCP packets
//!    (`snowflake/common/encapsulation`).
//! 3. KCP: conv 0, stream mode, windows 65535, nodelay(0, 0, 0, nc=1).
//! 4. smux v2 with one stream, which carries the Tor link (TLS) bytes.
//!
//! When a carrier dies (proxy went away), the driver asks JS for a new one,
//! re-sends the preamble with the *same* ClientID, and KCP retransmits
//! whatever was lost. The bridge re-attaches the new carrier to the existing
//! session, so the Tor channel survives proxy churn.

use futures::channel::{mpsc, oneshot};
use futures::{AsyncRead, AsyncWrite, AsyncWriteExt, FutureExt, StreamExt};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{self, Write};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use tracing::{debug, info, warn};

use crate::runtime::JsProxyStream;

/// Turbo Tunnel magic token (`snowflake/common/turbotunnel/consts.go`).
const TURBO_TOKEN: [u8; 8] = [0x12, 0x93, 0x60, 0x5d, 0x27, 0x81, 0x75, 0xf5];

/// KCP window, in packets (`WindowSize` in the Go client).
const KCP_WINDOW: u16 = 65535;
/// KCP tick interval in ms (Go: interval 0 is clamped to 10).
const KCP_INTERVAL_MS: u32 = 10;
/// Stop queueing into KCP once this many segments wait to be sent.
const KCP_MAX_WAITSND: usize = 4096;

/// smux protocol constants (github.com/xtaci/smux).
const SMUX_VERSION: u8 = 2;
const SMUX_SYN: u8 = 0;
const SMUX_FIN: u8 = 1;
const SMUX_PSH: u8 = 2;
const SMUX_NOP: u8 = 3;
const SMUX_UPD: u8 = 4;
const SMUX_HDR: usize = 8;
/// First stream id a smux client opens (nextStreamID starts at 1, += 2).
const SMUX_STREAM_ID: u32 = 3;
/// smux's initial peer window before the first UPD (`initialPeerWindow`).
const SMUX_INITIAL_PEER_WINDOW: u32 = 262_144;
/// Our advertised receive window (`StreamSize` in the Go client: 1 MiB).
const SMUX_RECV_WINDOW: u32 = 1_048_576;
/// Max payload per smux frame (`MaxFrameSize` default).
const SMUX_MAX_FRAME: usize = 32_768;
/// Keepalive NOP interval (`KeepAliveInterval` default 10 s).
const SMUX_KEEPALIVE_MS: f64 = 10_000.0;

/// How long to keep trying to find a new carrier before giving up.
const REDIAL_BUDGET_MS: f64 = 120_000.0;

/// Counters exposed to JS through `snowflakeStats()`.
#[derive(Default, Debug)]
pub struct SnowflakeStats {
    /// Carriers successfully opened (1 = no churn).
    pub carriers_opened: AtomicU32,
    /// Carriers that died while the session was alive.
    pub carriers_lost: AtomicU32,
    /// Redial attempts that failed.
    pub redial_failures: AtomicU32,
    /// Bytes received from the bridge (Tor link bytes, after smux).
    pub bytes_in: AtomicU64,
    /// Bytes sent to the bridge (Tor link bytes, before smux).
    pub bytes_out: AtomicU64,
}

thread_local! {
    /// Stats for every Snowflake session in this wasm instance.
    pub static STATS: Arc<SnowflakeStats> = Arc::new(SnowflakeStats::default());
}

/// Returns a JS-friendly snapshot of the Snowflake counters.
pub fn stats_snapshot() -> [f64; 5] {
    STATS.with(|s| {
        [
            s.carriers_opened.load(Ordering::Relaxed) as f64,
            s.carriers_lost.load(Ordering::Relaxed) as f64,
            s.redial_failures.load(Ordering::Relaxed) as f64,
            s.bytes_in.load(Ordering::Relaxed) as f64,
            s.bytes_out.load(Ordering::Relaxed) as f64,
        ]
    })
}

/// Something that can open a fresh carrier (calls into JS).
pub type CarrierDialer =
    Rc<dyn Fn() -> Pin<Box<dyn std::future::Future<Output = io::Result<JsProxyStream>>>>>;

fn now_ms() -> f64 {
    js_sys::Date::now()
}

// Encapsulation (snowflake/common/encapsulation)

fn encapsulate(data: &[u8], out: &mut Vec<u8>) {
    let n = data.len();
    if n >> 6 == 0 {
        out.push(0x80 | (n & 0x3f) as u8);
    } else if n >> 13 == 0 {
        out.push(0xc0 | ((n >> 7) & 0x3f) as u8);
        out.push((n & 0x7f) as u8);
    } else {
        debug_assert!(n >> 20 == 0, "KCP packet too long to encapsulate");
        out.push(0xc0 | ((n >> 14) & 0x3f) as u8);
        out.push(0x80 | ((n >> 7) & 0x7f) as u8);
        out.push((n & 0x7f) as u8);
    }
    out.extend_from_slice(data);
}

/// Incremental decoder for the encapsulation stream.
#[derive(Default)]
struct Decapsulator {
    buf: Vec<u8>,
}

impl Decapsulator {
    /// Feed bytes; returns complete data packets (padding is dropped).
    fn feed(&mut self, bytes: &[u8]) -> io::Result<Vec<Vec<u8>>> {
        self.buf.extend_from_slice(bytes);
        let mut packets = Vec::new();
        let mut pos = 0;
        loop {
            let rest = &self.buf[pos..];
            if rest.is_empty() {
                break;
            }
            let b0 = rest[0];
            let is_data = b0 & 0x80 != 0;
            let mut more = b0 & 0x40 != 0;
            let mut n = (b0 & 0x3f) as usize;
            let mut hdr = 1;
            let mut i = 0;
            let mut incomplete = false;
            while more {
                if i >= 2 {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "encapsulation length too long"));
                }
                if rest.len() <= hdr {
                    incomplete = true;
                    break;
                }
                let b = rest[hdr];
                more = b & 0x80 != 0;
                n = (n << 7) | (b & 0x7f) as usize;
                hdr += 1;
                i += 1;
            }
            if incomplete || rest.len() < hdr + n {
                break;
            }
            if is_data {
                packets.push(rest[hdr..hdr + n].to_vec());
            }
            pos += hdr + n;
        }
        self.buf.drain(..pos);
        Ok(packets)
    }
}

// KCP output sink

/// KCP writes each outgoing packet here; the driver drains and encapsulates.
#[derive(Clone, Default)]
struct PacketSink(Rc<RefCell<VecDeque<Vec<u8>>>>);

impl Write for PacketSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().push_back(buf.to_vec());
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// The stream handed to Arti

/// Shared state between the [`SnowflakeStream`] and its driver task.
#[derive(Default)]
struct Shared {
    /// Bytes the application has consumed (for smux UPD).
    consumed: AtomicU32,
    /// Set by either side to tear down.
    closed: AtomicBool,
}

/// One smux stream over Snowflake, carrying the Tor link.
pub struct SnowflakeStream {
    rx: mpsc::UnboundedReceiver<io::Result<Vec<u8>>>,
    tx: mpsc::UnboundedSender<Vec<u8>>,
    leftover: Vec<u8>,
    eof: bool,
    shared: Arc<Shared>,
}

impl std::fmt::Debug for SnowflakeStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SnowflakeStream").finish()
    }
}

impl Drop for SnowflakeStream {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Relaxed);
    }
}

impl AsyncRead for SnowflakeStream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        loop {
            if !this.leftover.is_empty() {
                let n = buf.len().min(this.leftover.len());
                buf[..n].copy_from_slice(&this.leftover[..n]);
                this.leftover.drain(..n);
                this.shared.consumed.fetch_add(n as u32, Ordering::Relaxed);
                return Poll::Ready(Ok(n));
            }
            if this.eof {
                return Poll::Ready(Ok(0));
            }
            match this.rx.poll_next_unpin(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    this.eof = true;
                }
                Poll::Ready(Some(Ok(chunk))) => this.leftover = chunk,
                Poll::Ready(Some(Err(e))) => {
                    this.eof = true;
                    return Poll::Ready(Err(e));
                }
            }
        }
    }
}

impl AsyncWrite for SnowflakeStream {
    fn poll_write(self: Pin<&mut Self>, _cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        if self.shared.closed.load(Ordering::Relaxed) {
            return Poll::Ready(Err(io::Error::new(io::ErrorKind::BrokenPipe, "snowflake session closed")));
        }
        // The driver enforces the smux window and KCP queue limits; Tor's own
        // flow control (SENDME) bounds how much can be outstanding here.
        self.tx
            .unbounded_send(buf.to_vec())
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "snowflake driver gone"))?;
        Poll::Ready(Ok(buf.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.shared.closed.store(true, Ordering::Relaxed);
        Poll::Ready(Ok(()))
    }
}

// Driver

struct Driver {
    dial: CarrierDialer,
    client_id: [u8; 8],
    kcp: kcp::Kcp<PacketSink>,
    sink: PacketSink,
    decap: Decapsulator,
    /// Reassembly buffer for smux frames coming out of KCP.
    smux_in: Vec<u8>,
    /// App bytes waiting for smux window / KCP queue space.
    pending_out: VecDeque<u8>,
    /// smux v2 send-side flow control.
    num_written: u32,
    peer_consumed: u32,
    peer_window: u32,
    /// Last `consumed` value we reported with UPD.
    reported_consumed: u32,
    last_keepalive: f64,
    to_app: mpsc::UnboundedSender<io::Result<Vec<u8>>>,
    shared: Arc<Shared>,
    stats: Arc<SnowflakeStats>,
}

type CarrierWriter = futures::io::WriteHalf<JsProxyStream>;

/// Messages from the carrier reader task.
enum CarrierEvent {
    Data(Vec<u8>),
    Closed(Option<io::Error>),
}

impl Driver {
    fn smux_frame(cmd: u8, sid: u32, data: &[u8]) -> Vec<u8> {
        let mut f = Vec::with_capacity(SMUX_HDR + data.len());
        f.push(SMUX_VERSION);
        f.push(cmd);
        f.extend_from_slice(&(data.len() as u16).to_le_bytes());
        f.extend_from_slice(&sid.to_le_bytes());
        f.extend_from_slice(data);
        f
    }

    fn kcp_send(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.kcp
            .send(bytes)
            .map(|_| ())
            .map_err(|e| io::Error::other(format!("kcp send: {e:?}")))
    }

    /// Move app bytes into smux PSH frames as window and KCP queue allow.
    fn pump_out(&mut self) -> io::Result<()> {
        while !self.pending_out.is_empty() && self.kcp.wait_snd() < KCP_MAX_WAITSND {
            let inflight = self.num_written.wrapping_sub(self.peer_consumed);
            let win = self.peer_window.saturating_sub(inflight) as usize;
            if win == 0 {
                break;
            }
            let n = self.pending_out.len().min(win).min(SMUX_MAX_FRAME);
            let chunk: Vec<u8> = self.pending_out.drain(..n).collect();
            let frame = Self::smux_frame(SMUX_PSH, SMUX_STREAM_ID, &chunk);
            self.kcp_send(&frame)?;
            self.num_written = self.num_written.wrapping_add(n as u32);
            self.stats.bytes_out.fetch_add(n as u64, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Pull reassembled bytes out of KCP and parse smux frames.
    fn pump_in(&mut self) -> io::Result<()> {
        let mut tmp = vec![0u8; 65536];
        loop {
            match self.kcp.recv(&mut tmp) {
                Ok(n) => self.smux_in.extend_from_slice(&tmp[..n]),
                Err(kcp::Error::RecvQueueEmpty) => break,
                Err(e) => return Err(io::Error::other(format!("kcp recv: {e:?}"))),
            }
        }
        let mut pos = 0;
        while self.smux_in.len() - pos >= SMUX_HDR {
            let h = &self.smux_in[pos..pos + SMUX_HDR];
            let cmd = h[1];
            let len = u16::from_le_bytes([h[2], h[3]]) as usize;
            let sid = u32::from_le_bytes([h[4], h[5], h[6], h[7]]);
            if self.smux_in.len() - pos < SMUX_HDR + len {
                break;
            }
            let body = &self.smux_in[pos + SMUX_HDR..pos + SMUX_HDR + len];
            match cmd {
                SMUX_PSH if sid == SMUX_STREAM_ID => {
                    self.stats.bytes_in.fetch_add(len as u64, Ordering::Relaxed);
                    let _ = self.to_app.unbounded_send(Ok(body.to_vec()));
                }
                SMUX_UPD if sid == SMUX_STREAM_ID && len >= 8 => {
                    self.peer_consumed = u32::from_le_bytes([body[0], body[1], body[2], body[3]]);
                    self.peer_window = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
                }
                SMUX_FIN if sid == SMUX_STREAM_ID => {
                    info!("snowflake: bridge closed the smux stream");
                    self.shared.closed.store(true, Ordering::Relaxed);
                }
                _ => {}
            }
            pos += SMUX_HDR + len;
        }
        self.smux_in.drain(..pos);
        Ok(())
    }

    /// Send window updates and keepalives as needed.
    fn housekeeping(&mut self) -> io::Result<()> {
        let consumed = self.shared.consumed.load(Ordering::Relaxed);
        if consumed.wrapping_sub(self.reported_consumed) >= SMUX_RECV_WINDOW / 4 {
            let mut body = [0u8; 8];
            body[..4].copy_from_slice(&consumed.to_le_bytes());
            body[4..].copy_from_slice(&SMUX_RECV_WINDOW.to_le_bytes());
            let f = Self::smux_frame(SMUX_UPD, SMUX_STREAM_ID, &body);
            self.kcp_send(&f)?;
            self.reported_consumed = consumed;
        }
        let now = now_ms();
        if now - self.last_keepalive >= SMUX_KEEPALIVE_MS {
            let f = Self::smux_frame(SMUX_NOP, 0, &[]);
            self.kcp_send(&f)?;
            self.last_keepalive = now;
        }
        Ok(())
    }

    /// Run KCP's clock and return encapsulated output for the carrier.
    fn tick(&mut self) -> io::Result<Vec<u8>> {
        self.kcp
            .update(now_ms() as u64 as u32)
            .map_err(|e| io::Error::other(format!("kcp update: {e:?}")))?;
        let mut out = Vec::new();
        for pkt in self.sink.0.borrow_mut().drain(..) {
            encapsulate(&pkt, &mut out);
        }
        Ok(out)
    }

    /// Open a carrier and send the Turbo Tunnel preamble. Retries until the
    /// redial budget is spent.
    async fn open_carrier(
        &self,
    ) -> io::Result<(CarrierWriter, mpsc::UnboundedReceiver<CarrierEvent>, oneshot::Sender<()>)> {
        let start = now_ms();
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            match (self.dial)().await {
                Ok(carrier) => {
                    return Ok(self.attach(carrier).await?);
                }
                Err(e) => {
                    self.stats.redial_failures.fetch_add(1, Ordering::Relaxed);
                    warn!("snowflake: carrier dial #{attempt} failed: {e}");
                    if now_ms() - start > REDIAL_BUDGET_MS || self.shared.closed.load(Ordering::Relaxed) {
                        return Err(e);
                    }
                    let backoff = (500u32 << attempt.min(4)).min(8_000);
                    gloo_timers::future::TimeoutFuture::new(backoff).await;
                }
            }
        }
    }

    /// Send the preamble on a fresh carrier and start its reader task.
    async fn attach(
        &self,
        carrier: JsProxyStream,
    ) -> io::Result<(CarrierWriter, mpsc::UnboundedReceiver<CarrierEvent>, oneshot::Sender<()>)> {
        use futures::AsyncReadExt;
        let (reader_half, mut writer) = carrier.split();
        let mut pre = Vec::with_capacity(16);
        pre.extend_from_slice(&TURBO_TOKEN);
        pre.extend_from_slice(&self.client_id);
        writer.write_all(&pre).await?;
        self.stats.carriers_opened.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded();
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        wasm_bindgen_futures::spawn_local(async move {
            use futures::AsyncReadExt;
            let mut reader = reader_half;
            let mut stop = stop_rx.fuse();
            let mut buf = vec![0u8; 65536];
            loop {
                let mut read = reader.read(&mut buf).fuse();
                futures::select! {
                    _ = stop => break,
                    r = read => match r {
                        Ok(0) => { let _ = tx.unbounded_send(CarrierEvent::Closed(None)); break; }
                        Ok(n) => { let _ = tx.unbounded_send(CarrierEvent::Data(buf[..n].to_vec())); }
                        Err(e) => { let _ = tx.unbounded_send(CarrierEvent::Closed(Some(e))); break; }
                    }
                }
            }
        });
        Ok((writer, rx, stop_tx))
    }

    async fn run(mut self, mut from_app: mpsc::UnboundedReceiver<Vec<u8>>) {
        let result = self.run_inner(&mut from_app).await;
        if let Err(e) = &result {
            warn!("snowflake: session ended: {e}");
        }
        self.shared.closed.store(true, Ordering::Relaxed);
        let _ = self.to_app.unbounded_send(Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "snowflake session ended",
        )));
        self.to_app.close_channel();
    }

    async fn run_inner(&mut self, from_app: &mut mpsc::UnboundedReceiver<Vec<u8>>) -> io::Result<()> {
        let (mut writer, mut events, mut stop) = self.open_carrier().await?;
        // smux SYN for our single stream.
        let syn = Self::smux_frame(SMUX_SYN, SMUX_STREAM_ID, &[]);
        self.kcp_send(&syn)?;

        loop {
            if self.shared.closed.load(Ordering::Relaxed) {
                let fin = Self::smux_frame(SMUX_FIN, SMUX_STREAM_ID, &[]);
                let _ = self.kcp_send(&fin);
                let out = self.tick()?;
                let _ = writer.write_all(&out).await;
                let _ = stop.send(());
                return Ok(());
            }

            let mut carrier_lost: Option<Option<io::Error>> = None;
            {
                let mut timer = gloo_timers::future::TimeoutFuture::new(KCP_INTERVAL_MS).fuse();
                futures::select! {
                    ev = events.next() => match ev {
                        Some(CarrierEvent::Data(bytes)) => {
                            for pkt in self.decap.feed(&bytes)? {
                                let _ = self.kcp.input(&pkt);
                            }
                        }
                        Some(CarrierEvent::Closed(e)) => carrier_lost = Some(e),
                        None => carrier_lost = Some(None),
                    },
                    data = from_app.next() => match data {
                        Some(d) => self.pending_out.extend(d),
                        None => self.shared.closed.store(true, Ordering::Relaxed),
                    },
                    _ = timer => {}
                }
            }
            // Drain anything else already queued without waiting.
            while let Ok(d) = from_app.try_recv() {
                self.pending_out.extend(d);
            }

            if let Some(err) = carrier_lost {
                self.stats.carriers_lost.fetch_add(1, Ordering::Relaxed);
                warn!("snowflake: carrier lost ({err:?}); redialing with same ClientID");
                let _ = stop.send(());
                self.decap = Decapsulator::default();
                let (w, e, s) = self.open_carrier().await?;
                writer = w;
                events = e;
                stop = s;
                info!("snowflake: new carrier attached");
                continue;
            }

            self.pump_in()?;
            self.housekeeping()?;
            self.pump_out()?;
            let out = self.tick()?;
            if !out.is_empty() {
                if let Err(e) = writer.write_all(&out).await {
                    debug!("snowflake: carrier write failed: {e}");
                    // The reader task will report the close; KCP retransmits.
                }
            }
        }
    }
}

/// Start a Snowflake session and return the stream for the Tor link.
pub async fn connect(dial: CarrierDialer) -> io::Result<SnowflakeStream> {
    let mut client_id = [0u8; 8];
    getrandom_03::fill(&mut client_id).map_err(|e| io::Error::other(format!("rng: {e}")))?;

    let sink = PacketSink::default();
    let mut kcp = kcp::Kcp::new_stream(0, sink.clone());
    kcp.set_wndsize(KCP_WINDOW, KCP_WINDOW);
    kcp.set_nodelay(false, 0, 0, true);
    // Never declare the link dead on retransmit count: carriers come and go.
    kcp.set_maximum_resend_times(u32::MAX);

    let (to_app, rx) = mpsc::unbounded();
    let (tx, from_app) = mpsc::unbounded();
    let shared = Arc::new(Shared::default());
    let stats = STATS.with(|s| s.clone());

    let driver = Driver {
        dial,
        client_id,
        kcp,
        sink,
        decap: Decapsulator::default(),
        smux_in: Vec::new(),
        pending_out: VecDeque::new(),
        num_written: 0,
        peer_consumed: 0,
        peer_window: SMUX_INITIAL_PEER_WINDOW,
        reported_consumed: 0,
        last_keepalive: now_ms(),
        to_app,
        shared: shared.clone(),
        stats,
    };
    info!("snowflake: starting session");
    wasm_bindgen_futures::spawn_local(driver.run(from_app));

    Ok(SnowflakeStream { rx, tx, leftover: Vec::new(), eof: false, shared })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encapsulation_roundtrip() {
        for n in [0usize, 1, 63, 64, 1400, 8191, 8192, 70000] {
            let data: Vec<u8> = (0..n).map(|i| i as u8).collect();
            let mut out = Vec::new();
            encapsulate(&data, &mut out);
            // Add a padding frame of 5 bytes (prefix 0x05 + 5 bytes).
            out.extend_from_slice(&[0x05, 0, 0, 0, 0, 0]);
            let mut d = Decapsulator::default();
            // Feed byte by byte to exercise partial frames.
            let mut got = Vec::new();
            for b in &out {
                got.extend(d.feed(std::slice::from_ref(b)).unwrap());
            }
            assert_eq!(got, vec![data]);
        }
    }

    #[test]
    fn encapsulation_prefix_matches_go() {
        // Values from snowflake/common/encapsulation/encapsulation_test.go.
        let mut out = Vec::new();
        encapsulate(&[0u8; 0], &mut out);
        assert_eq!(out, vec![0x80]);
        out.clear();
        encapsulate(&[0u8; 64], &mut out);
        assert_eq!(&out[..2], &[0xc0, 0x40]);
        out.clear();
        encapsulate(&[0u8; 8192], &mut out);
        assert_eq!(&out[..3], &[0xc0, 0xc0, 0x00]);
    }
}
