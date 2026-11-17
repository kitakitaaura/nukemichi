//! Streaming brotli decoder for JS, for engines whose DecompressionStream
//! lacks brotli (Chromium and Firefox as of late 2026).

use std::io::Write;
use wasm_bindgen::prelude::*;

/// Feed compressed chunks with `push`, collect output, then call `finish`.
#[wasm_bindgen]
pub struct BrotliDecoder {
    inner: Option<brotli_decompressor::DecompressorWriter<Vec<u8>>>,
}

#[wasm_bindgen]
impl BrotliDecoder {
    #[wasm_bindgen(constructor)]
    pub fn new() -> BrotliDecoder {
        BrotliDecoder { inner: Some(brotli_decompressor::DecompressorWriter::new(Vec::new(), 16 * 1024)) }
    }

    /// Decompress a chunk; returns whatever output is ready.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<u8>, JsValue> {
        let w = self.inner.as_mut().ok_or_else(|| JsValue::from_str("brotli: decoder finished"))?;
        w.write_all(chunk).map_err(|e| JsValue::from_str(&format!("brotli: {e}")))?;
        Ok(std::mem::take(w.get_mut()))
    }

    /// Finish the stream; errors if it was truncated.
    pub fn finish(&mut self) -> Result<Vec<u8>, JsValue> {
        let mut w = self.inner.take().ok_or_else(|| JsValue::from_str("brotli: decoder finished"))?;
        w.close().map_err(|e| JsValue::from_str(&format!("brotli: truncated stream ({e})")))?;
        Ok(std::mem::take(w.get_mut()))
    }
}

impl Default for BrotliDecoder {
    fn default() -> Self {
        Self::new()
    }
}
