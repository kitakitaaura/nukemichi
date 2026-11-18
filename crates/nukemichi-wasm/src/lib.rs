//! nukemichi: cors-free http and raw tcp from a web page, through tor.
//!
//! arti (arti-client) runs in wasm and reaches the tor network through a
//! snowflake bridge. the browser only carries webrtc (or websocket) traffic to
//! snowflake; a tor exit opens the real tcp connection; tls and http run here.
//!
//! derived from tor-js (github.com/ethereum/tor-js, mit or apache-2.0). see
//! notice at the repository root for what changed.
//!
//! the js api is `js/src/index.js`; this crate is its engine.

// also compiles for the host under `cfg(test)`, so the zip parser and http
// codec run under plain `cargo test`.
#![cfg(any(target_arch = "wasm32", test))]

mod conn;
mod decode;
mod error;
mod fast_bootstrap;
mod fetch;
mod isolation;
mod runtime;
mod snowflake;
mod storage;
mod tcp;
mod tls;

pub use storage::{JsStorage, JsStorageInterface, CachedJsStorage};

use error::JsTorError;

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use arti_client::config::CfgPath;
use arti_client::{TorClient as ArtiTorClient, TorClientConfig};
use isolation::{IsolationGroups, KeyError, MAX_KEY_LEN};
use serde::Deserialize;
use crate::runtime::WasmRuntime;
use tracing::{debug, info, error};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use wasm_bindgen::prelude::*;

// wasm is single-threaded, so a thread_local is enough.
thread_local! {
    static LOG_CALLBACK: RefCell<Option<js_sys::Function>> = const { RefCell::new(None) };
    static LOG_LEVEL_HANDLE: RefCell<Option<tracing_subscriber::reload::Handle<
        tracing_subscriber::filter::LevelFilter,
        tracing_subscriber::Registry,
    >>> = const { RefCell::new(None) };
}

fn parse_level(level: &str) -> Result<tracing_subscriber::filter::LevelFilter, JsValue> {
    match level {
        "trace" => Ok(tracing_subscriber::filter::LevelFilter::TRACE),
        "debug" => Ok(tracing_subscriber::filter::LevelFilter::DEBUG),
        "info" => Ok(tracing_subscriber::filter::LevelFilter::INFO),
        "warn" => Ok(tracing_subscriber::filter::LevelFilter::WARN),
        "error" => Ok(tracing_subscriber::filter::LevelFilter::ERROR),
        other => Err(JsValue::from_str(&format!(
            "Invalid log level: {:?}. Must be trace, debug, info, warn, or error.",
            other
        ))),
    }
}

/// set up the panic hook and logging. call once before creating a torclient.
///
/// `log_level` is "trace", "debug", "info", "warn" or "error" (default
/// "debug"); change it later with `setLogLevel()`.
#[wasm_bindgen]
pub fn init(log_level: Option<String>) -> Result<(), JsValue> {
    console_error_panic_hook::set_once();

    let initial_filter = match log_level.as_deref() {
        Some(level) => parse_level(level)?,
        None => tracing_subscriber::filter::LevelFilter::DEBUG,
    };

    let (filter, reload_handle) = tracing_subscriber::reload::Layer::new(initial_filter);

    // init() may run more than once; try_init() does not panic then.
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(JsLogLayer)
        .try_init();

    LOG_LEVEL_HANDLE.with(|h| {
        *h.borrow_mut() = Some(reload_handle);
    });

    Ok(())
}

/// dynamically update the minimum log level.
///
/// called from js when the broadest requested level across all clients changes.
#[wasm_bindgen(js_name = setLogLevel)]
pub fn set_log_level(level: String) -> Result<(), JsValue> {
    let new_filter = parse_level(&level)?;
    LOG_LEVEL_HANDLE.with(|h| {
        if let Some(handle) = h.borrow().as_ref() {
            handle.modify(|filter| *filter = new_filter)
                .map_err(|e| JsValue::from_str(&format!("Failed to update log level: {}", e)))
        } else {
            Err(JsValue::from_str("init() must be called before setLogLevel()"))
        }
    })
}

/// set a callback function to receive log messages
///
/// the callback receives three arguments: (level: string, target: string, message: string)
#[wasm_bindgen(js_name = setLogCallback)]
pub fn set_log_callback(callback: js_sys::Function) {
    LOG_CALLBACK.with(|cb| {
        *cb.borrow_mut() = Some(callback);
    });
}

/// custom tracing layer that forwards logs to javascript
struct JsLogLayer;

impl<S> tracing_subscriber::Layer<S> for JsLogLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let level = match *event.metadata().level() {
            tracing::Level::TRACE => "trace",
            tracing::Level::DEBUG => "debug",
            tracing::Level::INFO => "info",
            tracing::Level::WARN => "warn",
            tracing::Level::ERROR => "error",
        };
        let target = event.metadata().target();
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);

        LOG_CALLBACK.with(|cb| {
            if let Some(callback) = cb.borrow().as_ref() {
                let _ = callback.call3(
                    &JsValue::NULL,
                    &JsValue::from_str(level),
                    &JsValue::from_str(target),
                    &JsValue::from_str(&visitor.message),
                );
            } else {
                // fall back to console.log if no callback set
                web_sys::console::log_1(&format!("[{}] {}: {}", level, target, visitor.message).into());
            }
        });
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: String,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{:?}", value);
            // remove surrounding quotes if present
            self.message = self.message.trim_matches('"').to_string();
        } else if self.message.is_empty() {
            self.message = format!("{} = {:?}", field.name(), value);
        } else {
            self.message.push_str(&format!(", {} = {:?}", field.name(), value));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else if self.message.is_empty() {
            self.message = format!("{} = {}", field.name(), value);
        } else {
            self.message.push_str(&format!(", {} = {}", field.name(), value));
        }
    }
}

/// options for creating a torclient
#[wasm_bindgen]
pub struct TorClientOptions {
    /// js callback: `(addr: string) => Promise<{ readable, writable, close() }>`.
    connect: js_sys::Function,
    /// custom storage implementation (optional)
    storage: Option<JsStorageInterface>,
    /// optional callback returning bootstrap.zip bytes for fast directory pre-population
    fast_bootstrap: Option<js_sys::Function>,
    /// bridge lines (`"192.0.2.3:80 <FINGERPRINT>"`), all reached via snowflake.
    bridges: Vec<String>,
}

#[wasm_bindgen]
impl TorClientOptions {
    /// create options with a connect function.
    ///
    /// the connect function receives a target address string (e.g. "198.51.100.1:9001")
    /// and must return a promise of `{ readable, writable, close() }` (whatwg
    /// byte streams; `js/src/snowflake.js` builds this function).
    #[wasm_bindgen(constructor)]
    pub fn new(connect: js_sys::Function) -> Self {
        Self {
            connect,
            storage: None,
            fast_bootstrap: None,
            bridges: Vec::new(),
        }
    }

    /// add a bridge reached through snowflake.
    ///
    /// `line` is `"<addr>:<port> <RSA fingerprint>"`, e.g. the built-in
    /// `"192.0.2.3:80 2B280B23E1107BB62ABFC40DDCC8824814F80A72"`. the address
    /// is a placeholder that never hits the network: the runtime routes it to
    /// snowflake, and the connect function is asked for a carrier with the
    /// target `"snowflake:<addr>:<port>"`.
    #[wasm_bindgen(js_name = withSnowflakeBridge)]
    pub fn with_snowflake_bridge(mut self, line: String) -> Self {
        self.bridges.push(line);
        self
    }

    /// set a custom storage implementation for persistent state.
    ///
    /// when set, the tor client will persist guard selection and other state
    /// to this storage, allowing faster reconnection across page reloads.
    ///
    /// if not set, in-memory storage is used (state lost on page reload).
    ///
    /// `storage` implements the torstorage interface (see `js/src/storage.js`).
    #[wasm_bindgen(js_name = withStorage)]
    pub fn with_storage(mut self, storage: JsStorageInterface) -> Self {
        self.storage = Some(storage);
        self
    }

    /// set a callback that provides bootstrap.zip bytes for fast directory pre-population.
    ///
    /// the callback should be `() => Promise<Uint8Array>` returning the
    /// bootstrap archive from a tor-js-gateway server — either raw zip bytes
    /// or zstd-compressed (`bootstrap.zip.zst`); compression is auto-detected.
    ///
    /// when set and storage has no cached consensus, the zip is parsed and the
    /// directory cache is pre-populated before bootstrap begins.
    #[wasm_bindgen(js_name = withFastBootstrap)]
    pub fn with_fast_bootstrap(mut self, callback: js_sys::Function) -> Self {
        self.fast_bootstrap = Some(callback);
        self
    }
}

/// tor client for making http requests through the tor network
#[wasm_bindgen]
pub struct TorClient {
    inner: Option<Arc<ArtiTorClient<WasmRuntime>>>,
    tls: tls::TlsConfigs,
    /// `anonOptions.isolationKey` → arti isolation group, with the key's own tls
    /// session cache (see isolation.rs, tls.rs).
    isolation: RefCell<IsolationGroups<(Arc<futures_rustls::rustls::ClientConfig>, conn::Slot)>>,
    /// circuit slot for requests without an isolation key.
    default_slot: conn::Slot,
}

#[wasm_bindgen]
impl TorClient {
    /// bootstrap a client: `const client = await TorClient.create(options);`
    pub fn create(options: TorClientOptions) -> js_sys::Promise {
        wasm_bindgen_futures::future_to_promise(async move {
            let client = create_client(options).await?;
            Ok(JsValue::from(client))
        })
    }

    /// make an http fetch request through tor
    ///
    /// returns a promise that resolves to a standard browser `Response` object
    /// as soon as response headers are received. the body is a `ReadableStream`
    /// that reads from the tor circuit on demand.
    #[wasm_bindgen(js_name = fetch, skip_typescript)]
    pub fn fetch(&self, url: String, init: JsValue) -> js_sys::Promise {
        let client = match &self.inner {
            Some(c) => Arc::clone(c),
            None => {
                return wasm_bindgen_futures::future_to_promise(async {
                    Err(JsTorError::not_initialized().into_js_value())
                });
            }
        };

        // resolve the isolation group now: the map lives on this client, and the
        // future below must not borrow `self`.
        let (slot, tls_config) = match isolation_key_from_init(&init) {
            Ok(Some(key)) => match self.isolation.borrow_mut().group_for(&key, || (self.tls.for_new_key(), conn::Slot::new())) {
                Ok(group) => (group.state.1, group.state.0),
                Err(e) => {
                    let message = match e {
                        KeyError::Empty => "anonOptions.isolationKey must not be empty".to_string(),
                        KeyError::TooLong => format!("anonOptions.isolationKey is longer than {} bytes", MAX_KEY_LEN),
                    };
                    return wasm_bindgen_futures::future_to_promise(async move {
                        Err(JsTorError::new("INVALID_OPTIONS", "validation", message, false).into_js_value())
                    });
                }
            },
            Ok(None) => (self.default_slot.clone(), self.tls.unkeyed()),
            Err(e) => return wasm_bindgen_futures::future_to_promise(async move { Err(e) }),
        };

        wasm_bindgen_futures::future_to_promise(async move {
            fetch_impl(&client, &url, init, tls_config, slot).await
        })
    }

    /// wait until the client is ready for traffic (connection usable + valid directory).
    #[wasm_bindgen(js_name = ready)]
    pub fn ready(&self) -> js_sys::Promise {
        let client = match &self.inner {
            Some(c) => Arc::clone(c),
            None => {
                return wasm_bindgen_futures::future_to_promise(async {
                    Err(JsTorError::not_initialized().into_js_value())
                });
            }
        };

        wasm_bindgen_futures::future_to_promise(async move {
            use futures::StreamExt;

            if client.bootstrap_status().ready_for_traffic() {
                return Ok(JsValue::undefined());
            }

            let mut events = client.bootstrap_events();
            while let Some(status) = events.next().await {
                if status.ready_for_traffic() {
                    return Ok(JsValue::undefined());
                }
            }

            Err(JsTorError::bootstrap(
                "Client failed to become ready for traffic"
            ).into_js_value())
        })
    }

    /// open a raw tcp stream through tor, optionally wrapped in tls.
    ///
    /// resolves to `{ readable, writable }` (a `ReadableStream<Uint8Array>` and
    /// a `WritableStream<Uint8Array>`). with `tls`, the server certificate is
    /// checked against the mozilla roots (webpki-roots) for `host`; there is no
    /// way to turn that check off.
    #[wasm_bindgen(js_name = connect)]
    pub fn connect(&self, host: String, port: u16, tls: bool, init: JsValue) -> js_sys::Promise {
        let client = match &self.inner {
            Some(c) => Arc::clone(c),
            None => {
                return wasm_bindgen_futures::future_to_promise(async {
                    Err(JsTorError::not_initialized().into_js_value())
                });
            }
        };
        let (slot, tls_config) = match isolation_key_from_init(&init) {
            Ok(Some(key)) => match self.isolation.borrow_mut().group_for(&key, || (self.tls.for_new_key(), conn::Slot::new())) {
                Ok(group) => (group.state.1, group.state.0),
                Err(_) => {
                    return wasm_bindgen_futures::future_to_promise(async move {
                        Err(JsTorError::new("INVALID_OPTIONS", "validation", "bad isolationKey".to_string(), false).into_js_value())
                    });
                }
            },
            Ok(None) => (self.default_slot.clone(), self.tls.unkeyed()),
            Err(e) => return wasm_bindgen_futures::future_to_promise(async move { Err(e) }),
        };
        wasm_bindgen_futures::future_to_promise(async move {
            tcp::connect(&client, &host, port, tls, tls_config, &slot).await
        })
    }

    /// arti's bootstrap status as text (for progress displays).
    #[wasm_bindgen(js_name = bootstrapStatus)]
    pub fn bootstrap_status(&self) -> String {
        match &self.inner {
            Some(c) => c.bootstrap_status().to_string(),
            None => "closed".to_string(),
        }
    }

    /// close the torclient and release resources
    #[wasm_bindgen(js_name = close)]
    pub fn close(&mut self) -> js_sys::Promise {
        self.inner = None;
        conn::pool_clear();
        wasm_bindgen_futures::future_to_promise(async {
            info!("TorClient closed");
            Ok(JsValue::undefined())
        })
    }
}

async fn create_client(options: TorClientOptions) -> Result<TorClient, JsValue> {
    debug!("nukemichi {}", env!("CARGO_PKG_VERSION"));
    info!("Creating TorClient");

    // with bridges (the normal case), every tor channel goes to a snowflake
    // bridge; arti never dials relays itself.
    let mut config_builder = TorClientConfig::builder();
    let mut snowflake_addrs = Vec::new();
    if !options.bridges.is_empty() {
        use arti_client::config::{BoolOrAuto, BridgeConfigBuilder};
        config_builder.bridges().enabled(BoolOrAuto::Explicit(true));
        for line in &options.bridges {
            let b: BridgeConfigBuilder = line
                .parse()
                .map_err(|e| JsTorError::config(format!("Bad bridge line {line:?}: {e}")).into_js_value())?;
            let addr: std::net::SocketAddr = line
                .split_whitespace()
                .next()
                .and_then(|a| a.parse().ok())
                .ok_or_else(|| JsTorError::config(format!("Bridge line needs an ip:port first: {line:?}")).into_js_value())?;
            snowflake_addrs.push(addr);
            config_builder.bridges().bridges().push(b);
        }
    }

    // storage paths (required by config validation, but not used on wasm)
    config_builder
        .storage()
        .cache_dir(CfgPath::new("/wasm/cache".to_owned()))
        .state_dir(CfgPath::new("/wasm/state".to_owned()));

    let config = config_builder
        .build()
        .map_err(|e| JsTorError::config(format!("Failed to build config: {}", e)).into_js_value())?;

    let mut runtime = WasmRuntime::default();
    runtime.set_connect_fn(options.connect);
    runtime.set_snowflake_addrs(snowflake_addrs);

    let mut builder = ArtiTorClient::with_runtime(runtime).config(config);

    if let Some(js_storage_interface) = options.storage {
        info!("Initializing JS storage...");
        let js_storage = JsStorage::new(js_storage_interface);

        let cached_storage = CachedJsStorage::new(js_storage)
            .await
            .map_err(|e| {
                JsTorError::internal(format!("Failed to initialize storage: {:?}", e)).into_js_value()
            })?;

        // pre-populate directory cache from fast bootstrap if available and storage is empty
        if let Some(callback) = options.fast_bootstrap {
            if let Err(e) = fast_bootstrap::maybe_fast_bootstrap(&cached_storage, callback).await {
                tracing::warn!("Fast bootstrap failed (continuing normally): {:?}", e);
            }
        }

        builder = builder.storage(cached_storage);
        info!("Storage configured (state + directory cache)");
    } else {
        error!("Storage not configured");
    }

    let tor_client = builder
        .create_unbootstrapped()
        .map_err(|e| JsTorError::internal(format!("Failed to create client: {}", e)).into_js_value())?;

    info!("TorClient created (unbootstrapped)");

    info!("Bootstrapping Tor client...");
    tor_client
        .bootstrap()
        .await
        .map_err(|e| JsTorError::bootstrap(format!("Bootstrap failed: {}", e)).into_js_value())?;
    info!("Bootstrap complete!");

    Ok(TorClient {
        inner: Some(tor_client),
        tls: tls::TlsConfigs::new(make_tls_config()),
        isolation: RefCell::new(IsolationGroups::default()),
        default_slot: conn::Slot::new(),
    })
}

/// snowflake counters: `[carriersOpened, carriersLost, redialFailures, bytesIn, bytesOut]`.
#[wasm_bindgen(js_name = snowflakeStats)]
pub fn snowflake_stats() -> Vec<f64> {
    snowflake::stats_snapshot().to_vec()
}

/// rustls client config: mozilla roots (webpki-roots), ring crypto, http/1.1 alpn.
fn make_tls_config() -> futures_rustls::rustls::ClientConfig {
    use futures_rustls::rustls;

    let provider = futures_rustls::rustls::crypto::ring::default_provider();
    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let mut config = rustls::ClientConfig::builder_with_provider(provider.into())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    config
}

/// fetch init options from javascript
#[derive(Debug, Default, Deserialize)]
struct FetchInit {
    method: Option<String>,
    headers: Option<HashMap<String, String>>,
    // `body` is a js object (bytes or a readablestream), not serde-decodable —
    // extracted separately via extract_body_from_js.
}

/// perform a fetch request, returning a real browser `Response` object.
///
/// the response is created as soon as headers arrive. its body is a
/// `ReadableStream` that reads decoded body bytes from the tor circuit
/// on demand (handling content-length, chunked, and eof framing).
async fn fetch_impl(
    client: &ArtiTorClient<WasmRuntime>,
    url_str: &str,
    init: JsValue,
    tls_config: Arc<futures_rustls::rustls::ClientConfig>,
    slot: conn::Slot,
) -> Result<JsValue, JsValue> {
    let url = url::Url::parse(url_str)
        .map_err(|e| JsTorError::new("INVALID_URL", "validation", e.to_string(), false).into_js_value())?;

    let fetch_init: FetchInit = if init.is_undefined() || init.is_null() {
        FetchInit::default()
    } else {
        serde_wasm_bindgen::from_value(init.clone())
            .map_err(|e| JsTorError::new("INVALID_OPTIONS", "validation", e.to_string(), false).into_js_value())?
    };

    // extract body and signal separately (js objects; not handled by serde).
    let request_body = if init.is_undefined() || init.is_null() {
        fetch::RequestBody::None
    } else {
        extract_body_from_js(&init)?
    };
    let signal = extract_signal_from_js(&init)?;

    // parse method (normalize to uppercase per fetch spec)
    let method_upper = fetch_init.method.as_deref().map(str::to_ascii_uppercase);
    let method = match method_upper.as_deref() {
        Some("GET") | None => http::Method::GET,
        Some("POST") => http::Method::POST,
        Some("PUT") => http::Method::PUT,
        Some("DELETE") => http::Method::DELETE,
        Some("HEAD") => http::Method::HEAD,
        Some("OPTIONS") => http::Method::OPTIONS,
        Some("PATCH") => http::Method::PATCH,
        Some(other) => {
            return Err(JsTorError::new(
                "INVALID_METHOD",
                "validation",
                format!("Unsupported HTTP method: {}", other),
                false,
            )
            .into_js_value());
        }
    };

    let headers = fetch_init.headers.unwrap_or_default();

    let host = url
        .host_str()
        .ok_or_else(|| JsTorError::new("INVALID_URL", "validation", "No host in URL", false).into_js_value())?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| JsTorError::new("INVALID_URL", "validation", "No port in URL", false).into_js_value())?;
    let is_https = url.scheme() == "https";

    // log only the origin: the full url can carry credentials or api keys.
    info!("Fetching {}://{}:{} via Tor", url.scheme(), host, port);

    // reuse a pooled keep-alive connection when the request can be replayed
    // (a streamed body cannot); otherwise open one, retrying on new circuits.
    let key = conn::pool_key(is_https, host, port, &slot);
    let replayable = !matches!(request_body, fetch::RequestBody::Stream(_));
    let mut request_body = Some(request_body);
    let mut tries = 0;
    let (result, keep_alive) = loop {
        tries += 1;
        let pooled = if replayable && tries == 1 { conn::pool_take(&key) } else { None };
        let reused = pooled.is_some();
        let c = match pooled {
            Some(c) => {
                debug!("Reusing pooled connection to {}:{}", host, port);
                c
            }
            None => abortable(signal.as_ref(), async {
                conn::open_conn(client, host, port, is_https, &slot, tls_config.clone())
                    .await
                    .map_err(|e| e.into_js_value())
            })
            .await?,
        };
        let body = if reused {
            // keep a copy in case the pooled connection turns out to be stale.
            match request_body.as_ref().unwrap() {
                fetch::RequestBody::Bytes(b) => fetch::RequestBody::Bytes(b.clone()),
                _ => fetch::RequestBody::None,
            }
        } else {
            request_body.take().unwrap_or(fetch::RequestBody::None)
        };
        let r = abortable(signal.as_ref(), async {
            fetch::fetch_on_conn(c, &url, method.clone(), headers.clone(), body)
                .await
                .map_err(|e| e.into_js_value())
        })
        .await;
        match r {
            Ok(v) => break v,
            // a pooled connection the server already closed: open a new one.
            Err(_) if reused => {
                debug!("Pooled connection was stale; opening a new one");
                continue;
            }
            Err(e) => return Err(e),
        }
    };
    let mut result = result;
    if keep_alive {
        let key = conn::pool_key(is_https, host, port, &slot);
        result.body_reader.on_reusable(Box::new(move |c| conn::pool_put(key, c)));
    }

    let js_headers = web_sys::Headers::new()
        .map_err(|e| JsTorError::internal(format!("Failed to create Headers: {:?}", e)).into_js_value())?;
    for (key, value) in &result.headers {
        js_headers.append(key, value)
            .map_err(|e| JsTorError::internal(format!("Failed to set header {}: {:?}", key, e)).into_js_value())?;
    }

    // the signal is threaded through the unfold state so abort interrupts each chunk read.
    let body_stream = futures::stream::unfold((result.body_reader, signal), |(mut reader, sig)| async move {
        let chunk = abortable(sig.as_ref(), async { reader.read_chunk().await.map_err(|e| e.into_js_value()) }).await;
        match chunk {
            Ok(Some(chunk)) => {
                let arr = js_sys::Uint8Array::from(&chunk[..]);
                Some((Ok(arr.into()), (reader, sig)))
            }
            Ok(None) => None,
            Err(e) => Some((Err(e), (reader, sig))),
        }
    });
    let readable = wasm_streams::ReadableStream::from_stream(body_stream);
    let raw_readable: web_sys::ReadableStream = readable.into_raw();

    let response_init = web_sys::ResponseInit::new();
    response_init.set_status(result.status);
    response_init.set_headers(&js_headers.into());

    let response = web_sys::Response::new_with_opt_readable_stream_and_init(
        Some(&raw_readable),
        &response_init,
    )
    .map_err(|e| JsTorError::internal(format!("Failed to create Response: {:?}", e)).into_js_value())?;

    Ok(response.into())
}

fn extract_body_from_js(init: &JsValue) -> Result<fetch::RequestBody, JsValue> {
    let body = js_sys::Reflect::get(init, &JsValue::from_str("body"))
        .map_err(|e| JsTorError::new("INVALID_OPTIONS", "validation", format!("Failed to get body: {:?}", e), false).into_js_value())?;

    if body.is_undefined() || body.is_null() {
        return Ok(fetch::RequestBody::None);
    }

    if let Some(s) = body.as_string() {
        return Ok(fetch::RequestBody::Bytes(s.into_bytes()));
    }

    if let Ok(arr) = body.clone().dyn_into::<js_sys::Uint8Array>() {
        return Ok(fetch::RequestBody::Bytes(arr.to_vec()));
    }

    if let Ok(buf) = body.clone().dyn_into::<js_sys::ArrayBuffer>() {
        let arr = js_sys::Uint8Array::new(&buf);
        return Ok(fetch::RequestBody::Bytes(arr.to_vec()));
    }

    // handle a readablestream<uint8array> — streamed with chunked encoding, so
    // a large upload isn't buffered. backpressure rides the underlying stream.
    if let Ok(rs) = body.clone().dyn_into::<web_sys::ReadableStream>() {
        use futures::StreamExt;
        let chunks = wasm_streams::ReadableStream::from_raw(rs)
            .into_stream()
            .map(|item| match item {
                Ok(chunk) => chunk
                    .dyn_into::<js_sys::Uint8Array>()
                    .map(|a| a.to_vec())
                    .map_err(|_| JsTorError::http_request(
                        "request body stream must yield Uint8Array chunks",
                    )),
                Err(e) => Err(JsTorError::http_request(format!(
                    "request body stream error: {:?}",
                    e
                ))),
            });
        return Ok(fetch::RequestBody::Stream(Box::pin(chunks)));
    }

    Err(JsTorError::new(
        "INVALID_BODY",
        "validation",
        "Body must be a string, Uint8Array, ArrayBuffer, or ReadableStream",
        false,
    )
    .into_js_value())
}

/// read `anonOptions.isolationKey` from a javascript fetchinit object, if present.
///
/// read with reflect rather than serde so a bad type is a clear validation
/// error instead of a generic deserialization failure.
fn isolation_key_from_init(init: &JsValue) -> Result<Option<String>, JsValue> {
    let invalid = |message: &str| JsTorError::new("INVALID_OPTIONS", "validation", message, false).into_js_value();
    if init.is_undefined() || init.is_null() {
        return Ok(None);
    }
    let options = js_sys::Reflect::get(init, &JsValue::from_str("anonOptions"))
        .map_err(|_| invalid("unreadable anonOptions"))?;
    if options.is_undefined() {
        return Ok(None);
    }
    let value = js_sys::Reflect::get(&options, &JsValue::from_str("isolationKey"))
        .map_err(|_| invalid("anonOptions must be an object"))?;
    if value.is_undefined() {
        return Ok(None);
    }
    // ill-formed utf-16 is refused: conversion to utf-8 would map distinct
    // keys (a lone surrogate, u+fffd) to one group.
    value
        .dyn_ref::<js_sys::JsString>()
        .filter(|key| key.is_valid_utf16())
        .map(|key| Some(String::from(key)))
        .ok_or_else(|| invalid("anonOptions.isolationKey must be a well-formed string"))
}

/// extract an abortsignal from a javascript fetchinit object.
fn extract_signal_from_js(init: &JsValue) -> Result<Option<web_sys::AbortSignal>, JsValue> {
    if init.is_undefined() || init.is_null() {
        return Ok(None);
    }
    let signal = js_sys::Reflect::get(init, &JsValue::from_str("signal"))
        .map_err(|e| JsTorError::new("INVALID_OPTIONS", "validation", format!("Failed to get signal: {:?}", e), false).into_js_value())?;
    if signal.is_undefined() || signal.is_null() {
        return Ok(None);
    }
    signal
        .dyn_into::<web_sys::AbortSignal>()
        .map(Some)
        .map_err(|_| JsTorError::new("INVALID_OPTIONS", "validation", "signal must be an AbortSignal", false).into_js_value())
}

/// return an abort error if the signal has already been triggered.
fn check_aborted(signal: Option<&web_sys::AbortSignal>) -> Result<(), JsValue> {
    if let Some(s) = signal {
        if s.aborted() {
            return Err(JsTorError::aborted().into_js_value());
        }
    }
    Ok(())
}

/// run `fut` unless `signal` aborts first, in which case `fut` is dropped
/// (closing whatever stream it owns) and an abort error is returned.
async fn abortable<T>(
    signal: Option<&web_sys::AbortSignal>,
    fut: impl std::future::Future<Output = Result<T, JsValue>>,
) -> Result<T, JsValue> {
    let Some(signal) = signal else {
        return fut.await;
    };
    check_aborted(Some(signal))?;

    let (tx, rx) = futures::channel::oneshot::channel::<()>();
    let _listener = AbortListener::new(signal, Closure::once(move || {
        let _ = tx.send(());
    }))?;

    unless_aborted(rx, fut)
        .await
        .unwrap_or_else(|| Err(JsTorError::aborted().into_js_value()))
}

/// run `fut` unless `abort` resolves first, in which case `fut` is dropped and
/// `None` is returned. `abort` is polled first, so an abort wins over i/o that
/// completes in the same wakeup.
async fn unless_aborted<F: std::future::Future>(
    abort: impl std::future::Future,
    fut: F,
) -> Option<F::Output> {
    futures::pin_mut!(abort, fut);
    match futures::future::select(abort, fut).await {
        futures::future::Either::Left(_) => None,
        futures::future::Either::Right((output, _)) => Some(output),
    }
}

/// an `abort` listener that unregisters itself before its closure is freed,
/// including when the future holding it is dropped mid-await.
struct AbortListener<'a> {
    signal: &'a web_sys::AbortSignal,
    callback: Closure<dyn FnMut()>,
}

impl<'a> AbortListener<'a> {
    fn new(signal: &'a web_sys::AbortSignal, callback: Closure<dyn FnMut()>) -> Result<Self, JsValue> {
        signal.add_event_listener_with_callback("abort", callback.as_ref().unchecked_ref())?;
        Ok(Self { signal, callback })
    }
}

impl Drop for AbortListener<'_> {
    fn drop(&mut self) {
        let _ = self
            .signal
            .remove_event_listener_with_callback("abort", self.callback.as_ref().unchecked_ref());
    }
}

#[wasm_bindgen(typescript_custom_section)]
const TS_TYPES: &str = r#"
/**
 * Storage interface for persisting Tor client state.
 *
 * Implement this interface to provide custom storage (IndexedDB, filesystem, etc.).
 * All methods must return Promises.
 *
 * When storage is provided, the Tor client will persist guard selection and other
 * state, allowing faster reconnection across page reloads.
 *
 * @example
 * ```typescript
 * class IndexedDBStorage implements TorStorage {
 *   async get(key: string): Promise<string | null> {
 *     // Load from IndexedDB
 *   }
 *   async set(key: string, value: string): Promise<void> {
 *     // Save to IndexedDB
 *   }
 *   async delete(key: string): Promise<void> {
 *     // Delete from IndexedDB
 *   }
 *   async keys(prefix: string): Promise<string[]> {
 *     // List keys matching prefix
 *   }
 *   async tryLock(): Promise<boolean> {
 *     // addLocking is available in tor-js to solve locking with in-memory
 *     // overlay
 *     // true:   newly acquired
 *     // false:  already held
 *     // reject: couldn't lock
 *   }
 *   async unlock(): Promise<void> {
 *   }
 * }
 *
 * const options = new TorClientOptions(gatewayUrl)
 *   .withStorage(new IndexedDBStorage());
 * const client = await TorClient.create(options);
 * ```
 */
export interface TorStorage {
    /**
     * Get a value by key.
     * @param key - The storage key
     * @returns The stored value as a string, or null if not found
     */
    get(key: string): Promise<string | null>;

    /**
     * Get all key-value pairs matching a prefix.
     * @param prefix - The key prefix to match
     * @returns Array of [key, value] pairs
     */
    getAll(prefix: string): Promise<[string, string][]>;

    /**
     * Set a value by key.
     * @param key - The storage key
     * @param value - The value to store (JSON string)
     */
    set(key: string, value: string): Promise<void>;

    /**
     * Delete a value by key.
     * @param key - The storage key
     */
    delete(key: string): Promise<void>;

    /**
     * List all keys with a given prefix.
     * @param prefix - The key prefix to match
     * @returns Array of matching keys
     */
    keys(prefix: string): Promise<string[]>;

    /**
     * Try to acquire an exclusive write lock.
     * @returns true if newly acquired, false if already held.
     * Implement using Web Locks API (browser) or lock files (Node.js).
     */
    tryLock(): Promise<boolean>;

    /**
     * Release the write lock.
     */
    unlock(): Promise<void>;
}

export interface FetchInit {
    method?: string;
    headers?: Record<string, string>;
    body?: string | Uint8Array | ArrayBuffer | ReadableStream<Uint8Array>;
    signal?: AbortSignal;
    /** Per-call options, as anon-rpc spec 0.4.0 (§9.3) defines them (see FetchInit in types.ts). */
    anonOptions?: { isolationKey?: string };
}

export interface TorClient {
    /** Make an HTTP fetch request through Tor. Returns a standard Response. */
    fetch(url: string, init?: FetchInit): Promise<Response>;
    close(): Promise<void>;
}

export interface TorClientOptions {
    /**
     * Set a custom storage implementation for persistent state.
     * If not provided, in-memory storage is used (state lost on page reload).
     */
    withStorage(storage: TorStorage): TorClientOptions;
}
"#;

#[cfg(test)]
mod tests {
    use super::unless_aborted;
    use futures::channel::oneshot;
    use futures::executor::block_on;
    use futures::future::{pending, ready};
    use std::cell::Cell;
    use std::rc::Rc;

    /// A future that never completes and records when it is dropped.
    fn pending_until_dropped(dropped: Rc<Cell<bool>>) -> impl std::future::Future<Output = u8> {
        struct OnDrop(Rc<Cell<bool>>);
        impl Drop for OnDrop {
            fn drop(&mut self) {
                self.0.set(true);
            }
        }
        let guard = OnDrop(dropped);
        async move {
            let _guard = guard;
            pending().await
        }
    }

    #[test]
    fn the_operation_completes_when_no_abort_comes() {
        assert_eq!(block_on(unless_aborted(pending::<()>(), ready(7))), Some(7));
    }

    #[test]
    fn an_abort_ends_the_wait_and_drops_the_operation() {
        let dropped = Rc::new(Cell::new(false));
        let (tx, rx) = oneshot::channel::<()>();
        tx.send(()).unwrap();
        let output = block_on(unless_aborted(rx, pending_until_dropped(dropped.clone())));
        assert_eq!(output, None);
        assert!(
            dropped.get(),
            "the interrupted operation, and the stream it owns, is dropped"
        );
    }

    #[test]
    fn an_abort_wins_over_an_operation_ready_in_the_same_wakeup() {
        assert_eq!(block_on(unless_aborted(ready(()), ready(7))), None);
    }
}
