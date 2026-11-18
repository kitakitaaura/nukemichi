//! javascript storage adapter.
//!
//! this module provides [`CachedJsStorage`], a [`KeyValueStore`] implementation
//! that bridges async javascript storage apis with rust's sync storage traits
//! using a pre-load + cache + async write-back pattern:
//!
//! 1. during client creation (async), all data is loaded from js storage
//! 2. sync reads hit the in-memory cache
//! 3. writes update the cache and schedule async persistence via spawn_local()
//!
//! [`KeyValueStore`]: tor_persist::keyvaluestore

use tor_persist::{KeyValueStore, StorageError};
use futures::FutureExt as _;
use js_sys::Promise;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

/// javascript storage interface.
///
/// this is the interface that javascript code must implement to provide
/// custom storage. all methods return promises.
/// (return values are wrapped in result to handle the possibility that js
/// throws synchronously.)
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(typescript_type = "TorStorage")]
    pub type JsStorageInterface;

    /// get a value by key. returns null if not found.
    #[wasm_bindgen(method, catch)]
    fn get(this: &JsStorageInterface, key: &str) -> Result<Promise, JsValue>;

    #[wasm_bindgen(method, catch)]
    fn set(this: &JsStorageInterface, key: &str, value: &str) -> Result<Promise, JsValue>;

    #[wasm_bindgen(method, catch)]
    fn delete(this: &JsStorageInterface, key: &str) -> Result<Promise, JsValue>;

    #[wasm_bindgen(method, catch)]
    fn keys(this: &JsStorageInterface, prefix: &str) -> Result<Promise, JsValue>;

    /// returns an array of [key, value] pairs.
    #[wasm_bindgen(method, catch, js_name = "getAll")]
    fn get_all(this: &JsStorageInterface, prefix: &str) -> Result<Promise, JsValue>;

    /// returns a boolean: true if newly acquired, false if already held.
    #[wasm_bindgen(method, catch, js_name = "tryLock")]
    fn try_lock(this: &JsStorageInterface) -> Result<Promise, JsValue>;

    #[wasm_bindgen(method, catch)]
    fn unlock(this: &JsStorageInterface) -> Result<Promise, JsValue>;
}

///
/// provides async methods that convert js promises to rust futures.
pub struct JsStorage {
    inner: send_wrapper::SendWrapper<JsStorageInterface>,
}

impl Clone for JsStorage {
    fn clone(&self) -> Self {
        let inner_clone: JsStorageInterface = (*self.inner).clone().unchecked_into();
        Self { inner: send_wrapper::SendWrapper::new(inner_clone) }
    }
}

impl JsStorage {
    pub fn new(interface: JsStorageInterface) -> Self {
        Self { inner: send_wrapper::SendWrapper::new(interface) }
    }

    pub async fn get(&self, key: &str) -> Result<Option<String>, JsValue> {
        let promise = self.inner.get(key)?;
        let result = JsFuture::from(promise).await?;
        if result.is_null() || result.is_undefined() {
            Ok(None)
        } else {
            Ok(result.as_string())
        }
    }

    pub async fn set(&self, key: &str, value: &str) -> Result<(), JsValue> {
        let promise = self.inner.set(key, value)?;
        JsFuture::from(promise).await?;
        Ok(())
    }

    pub async fn delete(&self, key: &str) -> Result<(), JsValue> {
        let promise = self.inner.delete(key)?;
        JsFuture::from(promise).await?;
        Ok(())
    }

    pub async fn keys(&self, prefix: &str) -> Result<Vec<String>, JsValue> {
        let promise = self.inner.keys(prefix)?;
        let result = JsFuture::from(promise).await?;

        let array = js_sys::Array::from(&result);
        let mut keys = Vec::with_capacity(array.length() as usize);
        for i in 0..array.length() {
            if let Some(key) = array.get(i).as_string() {
                keys.push(key);
            }
        }
        Ok(keys)
    }

    pub async fn get_all(&self, prefix: &str) -> Result<Vec<(String, String)>, JsValue> {
        let promise = self.inner.get_all(prefix)?;
        let result = JsFuture::from(promise).await?;

        let array = js_sys::Array::from(&result);
        let mut entries = Vec::with_capacity(array.length() as usize);
        for i in 0..array.length() {
            let pair = js_sys::Array::from(&array.get(i));
            if let (Some(key), Some(value)) = (pair.get(0).as_string(), pair.get(1).as_string()) {
                entries.push((key, value));
            }
        }
        Ok(entries)
    }

    pub async fn try_lock(&self) -> Result<bool, JsValue> {
        let promise = self.inner.try_lock()?;
        let result = JsFuture::from(promise).await?;
        Ok(result.as_bool().unwrap_or(false))
    }

    pub async fn unlock(&self) -> Result<(), JsValue> {
        let promise = self.inner.unlock()?;
        JsFuture::from(promise).await?;
        Ok(())
    }
}

/// cached javascript storage implementing [`KeyValueStore`].
///
/// bridges async javascript storage apis with the sync [`KeyValueStore`] trait
/// using a pre-load + cache + async write-back pattern. all keys from js
/// storage are loaded into an in-memory cache during construction, then:
///
/// - **reads** hit the cache directly (sync)
/// - **writes** update the cache and schedule async persistence via `spawn_local()`
/// - **locking**: the js-side lock is acquired once during construction and
///   held for the client's lifetime. the sync `try_lock()`/`unlock()` methods
///   only manage local state for arti's internal protocol.
///
/// # locking limitations
///
/// because [`KeyValueStore`] lock methods are sync but js locking is async,
/// we can't cycle locks at runtime. the js lock is acquired once in [`new()`]
/// and never released (until the tab/worker closes).
pub struct CachedJsStorage {
    /// the underlying js storage (for async write-back).
    js_storage: JsStorage,
    /// in-memory cache for sync reads.
    cache: Arc<RwLock<HashMap<String, String>>>,
    /// whether we currently hold the write lock.
    locked: Arc<RwLock<bool>>,
    /// fires after the js lock is released in `Drop`. `Option` so `Drop` can take it.
    drop_tx: Option<tor_async_utils::oneshot::Sender<()>>,
    /// cloneable handle that resolves when the js lock is released.
    drop_rx: futures::future::Shared<tor_async_utils::oneshot::Receiver<()>>,
}

// cachedjsstorage is send/sync because jsstorage uses sendwrapper internally.

impl CachedJsStorage {
    /// create a new cachedjsstorage, preloading all data from js storage.
    ///
    /// acquires the js-side lock and holds it for the client's lifetime.
    /// this validates that `tryLock()` and `unlock()` exist on the js object,
    /// then preloads all `"state:"` and `"dir:"` prefixed keys into the
    /// in-memory cache (necessary because [`KeyValueStore`] is sync but js
    /// storage apis are async).
    pub async fn new(js_storage: JsStorage) -> Result<Self, JsValue> {
        js_storage.try_lock().await.map_err(|e| {
            JsValue::from_str(&format!(
                "Storage must implement tryLock(): {:?}",
                e
            ))
        })?;

        // validate that unlock() exists (we'll need it for shutdown).
        // call it, then re-acquire immediately.
        js_storage.unlock().await.map_err(|e| {
            JsValue::from_str(&format!(
                "Storage must implement unlock(): {:?}",
                e
            ))
        })?;
        js_storage.try_lock().await.map_err(|e| {
            JsValue::from_str(&format!(
                "Storage.tryLock() failed on re-acquire: {:?}",
                e
            ))
        })?;

        let (drop_tx, drop_rx) = tor_async_utils::oneshot::channel();
        let drop_rx = drop_rx.shared();
        let storage = Self {
            js_storage,
            cache: Arc::new(RwLock::new(HashMap::new())),
            locked: Arc::new(RwLock::new(true)),
            drop_tx: Some(drop_tx),
            drop_rx,
        };

        storage.preload_all().await?;

        Ok(storage)
    }

    /// pre-load all data from js storage into the cache.
    ///
    /// uses bulk `getAll()` to fetch all key-value pairs per prefix in a single
    /// js call, avoiding thousands of sequential wasm-js round-trips.
    async fn preload_all(&self) -> Result<(), JsValue> {
        let state_entries = self.js_storage.get_all("state:").await?;
        let dir_entries = self.js_storage.get_all("dir:").await?;

        let mut cache = self
            .cache
            .write()
            .map_err(|_| JsValue::from_str("cache lock poisoned"))?;
        for (key, value) in state_entries {
            cache.insert(key, value);
        }
        for (key, value) in dir_entries {
            cache.insert(key, value);
        }

        tracing::debug!("CachedJsStorage: preloaded {} entries", cache.len());
        Ok(())
    }

    fn schedule_persist(&self, key: String, value: String) {
        let js_storage = self.js_storage.clone();
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(e) = js_storage.set(&key, &value).await {
                tracing::warn!("CachedJsStorage: failed to persist key {}: {:?}", key, e);
            }
        });
    }

    /// insert many key-value pairs into the cache and schedule a single async
    /// batch persist to js storage.  much faster than calling `set()` in a loop
    /// because it acquires the rwlock once and spawns one `spawn_local()` task.
    pub fn set_many(&self, entries: Vec<(String, String)>) -> Result<(), StorageError> {
        {
            let mut cache = self
                .cache
                .write()
                .map_err(|_| -> StorageError { "cache lock poisoned".into() })?;
            for (k, v) in &entries {
                cache.insert(k.clone(), v.clone());
            }
        }

        let js_storage = self.js_storage.clone();
        wasm_bindgen_futures::spawn_local(async move {
            for (key, value) in entries {
                if let Err(e) = js_storage.set(&key, &value).await {
                    tracing::warn!("CachedJsStorage: failed to persist key {}: {:?}", key, e);
                }
            }
        });
        Ok(())
    }

    fn schedule_delete(&self, key: String) {
        let js_storage = self.js_storage.clone();
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(e) = js_storage.delete(&key).await {
                tracing::warn!("CachedJsStorage: failed to delete key {}: {:?}", key, e);
            }
        });
    }
}

impl Drop for CachedJsStorage {
    fn drop(&mut self) {
        let js_storage = self.js_storage.clone();
        let drop_tx = self.drop_tx.take();
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(e) = js_storage.unlock().await {
                tracing::warn!("CachedJsStorage: failed to release JS lock on drop: {:?}", e);
            }
            // signal waiters *after* the js lock is released.
            drop(drop_tx);
        });
    }
}

impl KeyValueStore for CachedJsStorage {
    fn get(&self, key: &str) -> Result<Option<String>, StorageError> {
        let cache = self
            .cache
            .read()
            .map_err(|_| -> StorageError { "cache lock poisoned".into() })?;
        Ok(cache.get(key).cloned())
    }

    fn set(&self, key: &str, value: &str) -> Result<(), StorageError> {
        {
            let mut cache = self
                .cache
                .write()
                .map_err(|_| -> StorageError { "cache lock poisoned".into() })?;
            cache.insert(key.to_string(), value.to_string());
        }

        self.schedule_persist(key.to_string(), value.to_string());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), StorageError> {
        {
            let mut cache = self
                .cache
                .write()
                .map_err(|_| -> StorageError { "cache lock poisoned".into() })?;
            cache.remove(key);
        }

        self.schedule_delete(key.to_string());
        Ok(())
    }

    fn keys(&self, prefix: &str) -> Result<Vec<String>, StorageError> {
        let cache = self
            .cache
            .read()
            .map_err(|_| -> StorageError { "cache lock poisoned".into() })?;
        Ok(cache
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect())
    }

    fn try_lock(&self) -> Result<bool, StorageError> {
        // local state only: the js lock is held from new() for the client's lifetime.
        let mut locked = self
            .locked
            .write()
            .map_err(|_| -> StorageError { "lock state poisoned".into() })?;
        if *locked {
            return Ok(false);
        }
        *locked = true;
        Ok(true)
    }

    fn can_store(&self) -> Result<bool, StorageError> {
        Ok(*self.locked.read().map_err(|e| e.to_string())?)
    }

    fn unlock(&self) -> Result<(), StorageError> {
        // local-only: the js lock stays held for the client's lifetime.
        let mut locked = self
            .locked
            .write()
            .map_err(|_| -> StorageError { "lock state poisoned".into() })?;
        *locked = false;
        Ok(())
    }

    fn wait_for_unlock(
        &self,
    ) -> Pin<Box<dyn futures::Future<Output = ()> + Send + Sync + 'static>> {
        Box::pin(self.drop_rx.clone().map(|_| ()))
    }
}
