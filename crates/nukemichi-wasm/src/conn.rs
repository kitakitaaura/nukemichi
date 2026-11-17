//! Opening streams through Tor, with retry on a fresh circuit, and a
//! keep-alive pool for HTTP connections.
//!
//! Arti keeps using a circuit after its exit fails to open a stream, so the
//! same request can fail for minutes. A [`Slot`] holds the isolation token a
//! group of requests uses; when an exit-side failure happens, the slot gets a
//! new token, which makes Arti build (or pick) a different circuit.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use arti_client::{DataStream, ErrorKind, HasKind, IsolationToken, StreamPrefs, TorClient as ArtiTorClient};
use tracing::{info, warn};

use crate::error::JsTorError;
use crate::fetch::BoxedReader;
use crate::runtime::WasmRuntime;

/// Attempts per stream before giving up (1 + retries on fresh circuits).
const MAX_ATTEMPTS: u32 = 3;
/// Idle pooled connections older than this are dropped (servers commonly
/// close idle keep-alive connections after 5-60 s).
const POOL_IDLE_MS: f64 = 30_000.0;
/// Pooled connections per origin and slot.
const POOL_PER_KEY: usize = 4;

/// The isolation token shared by a group of requests; rotated on failure.
#[derive(Clone)]
pub struct Slot(Rc<Cell<IsolationToken>>);

impl Slot {
    pub fn new() -> Self {
        Slot(Rc::new(Cell::new(IsolationToken::new())))
    }
    fn token(&self) -> IsolationToken {
        self.0.get()
    }
    fn rotate(&self) {
        self.0.set(IsolationToken::new());
    }
    fn id(&self) -> String {
        format!("{:?}", self.0.get())
    }
}

/// Failures where another exit may well succeed.
fn exit_side(kind: ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::RemoteNetworkTimeout
            | ErrorKind::RemoteStreamError
            | ErrorKind::RemoteStreamClosed
            | ErrorKind::RemoteStreamReset
            | ErrorKind::RemoteNetworkFailed
            | ErrorKind::RemoteHostResolutionFailed
            | ErrorKind::RemoteHostNotFound
            | ErrorKind::ExitPolicyRejected
            | ErrorKind::TorNetworkTimeout
            | ErrorKind::CircuitCollapse
            | ErrorKind::ExitTimeout
    )
}

/// Open a Tor stream to `host:port` on the slot's circuit, rotating to a new
/// circuit on exit-side failures.
pub async fn open_stream(
    client: &ArtiTorClient<WasmRuntime>,
    host: &str,
    port: u16,
    slot: &Slot,
) -> Result<DataStream, JsTorError> {
    let mut last = None;
    for attempt in 1..=MAX_ATTEMPTS {
        let mut prefs = StreamPrefs::new();
        prefs.set_isolation(slot.token());
        match client.connect_with_prefs((host, port), &prefs).await {
            Ok(s) => return Ok(s),
            Err(e) => {
                let kind = e.kind();
                let err = JsTorError::connect_failed(&e);
                if !exit_side(kind) || attempt == MAX_ATTEMPTS {
                    return Err(err);
                }
                warn!("stream to {host}:{port} failed ({kind:?}); retrying on a new circuit ({attempt}/{MAX_ATTEMPTS})");
                slot.rotate();
                last = Some(err);
            }
        }
    }
    Err(last.unwrap_or_else(|| JsTorError::internal("unreachable")))
}

/// Open a connection for HTTP: a Tor stream, TLS-wrapped for HTTPS. A TLS
/// handshake that dies (the exit dropped us) also moves to a new circuit.
pub async fn open_conn(
    client: &ArtiTorClient<WasmRuntime>,
    host: &str,
    port: u16,
    https: bool,
    slot: &Slot,
    tls_config: Arc<futures_rustls::rustls::ClientConfig>,
) -> Result<BoxedReader, JsTorError> {
    for attempt in 1..=MAX_ATTEMPTS {
        let stream = open_stream(client, host, port, slot).await?;
        if !https {
            return Ok(Box::new(stream));
        }
        let connector = futures_rustls::TlsConnector::from(tls_config.clone());
        let server_name = rustls_pki_types::ServerName::try_from(host.to_string())
            .map_err(|e| JsTorError::tls(format!("Invalid server name '{}': {}", host, e)))?;
        match connector.connect(server_name, stream).await {
            Ok(tls) => {
                info!("TLS connection established with {}", host);
                return Ok(Box::new(tls));
            }
            Err(e) => {
                // Certificate errors are final; I/O errors mean the stream died.
                let io_failure = e.get_ref().is_none_or(|inner| inner.downcast_ref::<futures_rustls::rustls::Error>().is_none());
                let err = JsTorError::tls(format!("TLS handshake failed with {}: {}", host, e));
                if !io_failure || attempt == MAX_ATTEMPTS {
                    return Err(err);
                }
                warn!("TLS to {host} died ({e}); retrying on a new circuit ({attempt}/{MAX_ATTEMPTS})");
                slot.rotate();
            }
        }
    }
    Err(JsTorError::internal("unreachable"))
}

thread_local! {
    static POOL: RefCell<HashMap<String, Vec<(BoxedReader, f64)>>> = RefCell::new(HashMap::new());
}

/// Pool key: scheme, host, port and the slot's current circuit token.
pub fn pool_key(https: bool, host: &str, port: u16, slot: &Slot) -> String {
    format!("{}://{}:{}#{}", if https { "https" } else { "http" }, host, port, slot.id())
}

/// Take an idle connection for `key`, if one is fresh enough.
pub fn pool_take(key: &str) -> Option<BoxedReader> {
    let now = js_sys::Date::now();
    POOL.with(|p| {
        let mut p = p.borrow_mut();
        let v = p.get_mut(key)?;
        while let Some((c, t)) = v.pop() {
            if now - t < POOL_IDLE_MS {
                return Some(c);
            }
        }
        None
    })
}

/// Return a connection whose last response was fully read.
pub fn pool_put(key: String, conn: BoxedReader) {
    POOL.with(|p| {
        let mut p = p.borrow_mut();
        let v = p.entry(key).or_default();
        if v.len() < POOL_PER_KEY {
            v.push((conn, js_sys::Date::now()));
        }
    });
}

/// Drop all pooled connections (on close).
pub fn pool_clear() {
    POOL.with(|p| p.borrow_mut().clear());
}
