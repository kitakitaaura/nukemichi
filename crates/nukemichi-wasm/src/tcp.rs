//! Raw TCP (optionally TLS) streams through Tor, exposed to JS as a
//! `{ readable, writable }` pair of WHATWG streams.

use std::sync::Arc;

use arti_client::TorClient as ArtiTorClient;
use futures::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, SinkExt, StreamExt};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

use crate::error::JsTorError;
use crate::runtime::WasmRuntime;

/// Turn an async byte stream into `{ readable, writable }` for JS.
fn to_js_pair<S>(stream: S) -> Result<JsValue, JsValue>
where
    S: AsyncRead + AsyncWrite + Unpin + 'static,
{
    let (reader, writer) = stream.split();

    // Read side: pull 16 KiB at a time on demand.
    let read_stream = futures::stream::unfold(reader, |mut r| async move {
        let mut buf = vec![0u8; 16 * 1024];
        match r.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => Some((Ok(js_sys::Uint8Array::from(&buf[..n]).into()), r)),
            Err(e) => Some((Err(JsValue::from(js_sys::Error::new(&format!("read failed: {e}")))), r)),
        }
    })
    // Stop after the first error.
    .scan(false, |errored, item: Result<JsValue, JsValue>| {
        if *errored {
            return futures::future::ready(None);
        }
        *errored = item.is_err();
        futures::future::ready(Some(item))
    });
    let readable = wasm_streams::ReadableStream::from_stream(read_stream).into_raw();

    // Write side: each chunk must be a Uint8Array; close() half-closes.
    let sink = futures::sink::unfold(writer, |mut w, chunk: JsValue| async move {
        let arr: js_sys::Uint8Array = chunk
            .dyn_into()
            .map_err(|_| JsValue::from(js_sys::TypeError::new("write() needs a Uint8Array")))?;
        w.write_all(&arr.to_vec())
            .await
            .map_err(|e| JsValue::from(js_sys::Error::new(&format!("write failed: {e}"))))?;
        w.flush()
            .await
            .map_err(|e| JsValue::from(js_sys::Error::new(&format!("flush failed: {e}"))))?;
        Ok::<_, JsValue>(w)
    });
    let writable = wasm_streams::WritableStream::from_sink(Box::pin(sink).sink_map_err(|e| e)).into_raw();

    let obj = js_sys::Object::new();
    js_sys::Reflect::set(&obj, &"readable".into(), &readable)?;
    js_sys::Reflect::set(&obj, &"writable".into(), &writable)?;
    Ok(obj.into())
}

/// Connect to `host:port` through Tor. With `tls`, run a TLS client over the
/// stream with full WebPKI validation for `host`.
pub async fn connect(
    client: &ArtiTorClient<WasmRuntime>,
    host: &str,
    port: u16,
    tls: bool,
    tls_config: Arc<futures_rustls::rustls::ClientConfig>,
    slot: &crate::conn::Slot,
) -> Result<JsValue, JsValue> {
    tracing::info!("Opening TCP stream to {}:{} via Tor (tls={})", host, port, tls);
    let stream = crate::conn::open_stream(client, host, port, slot)
        .await
        .map_err(|e| e.into_js_value())?;

    if !tls {
        return to_js_pair(stream);
    }

    // Same roots and verifier as fetch(), but no ALPN: the protocol is the
    // caller's business here.
    let mut cfg = (*tls_config).clone();
    cfg.alpn_protocols.clear();
    let connector = futures_rustls::TlsConnector::from(Arc::new(cfg));
    let server_name = rustls_pki_types::ServerName::try_from(host.to_string())
        .map_err(|e| JsTorError::new("INVALID_HOST", "validation", format!("bad TLS server name: {e}"), false).into_js_value())?;
    let tls_stream = connector
        .connect(server_name, stream)
        .await
        .map_err(|e| JsTorError::new("TLS", "tls", format!("TLS handshake with {host} failed: {e}"), false).into_js_value())?;
    to_js_pair(tls_stream)
}
