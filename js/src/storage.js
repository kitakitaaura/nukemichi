// persistent storage for arti's state and directory cache.
// implements the interface crates/nukemichi-wasm/src/storage.rs expects:
// get, set, delete, keys, getall, trylock, unlock (all async).
// adapted from tor-js (mit or apache-2.0).

function req(r) {
  return new Promise((resolve, reject) => {
    r.onsuccess = () => resolve(r.result);
    r.onerror = () => reject(r.error);
  });
}

/** IndexedDB key/value store, one object store per database name. */
export class IndexedDBStorage {
  constructor(name = 'nukemichi') {
    this.name = name;
    this.dbp = null;
    this.release = null;
  }
  db() {
    if (!this.dbp) {
      const open = indexedDB.open(this.name, 1);
      open.onupgradeneeded = () => open.result.createObjectStore('kv');
      this.dbp = req(open);
    }
    return this.dbp;
  }
  async store(mode) {
    return (await this.db()).transaction('kv', mode).objectStore('kv');
  }
  async get(key) {
    const v = await req((await this.store('readonly')).get(key));
    return v === undefined ? null : v;
  }
  async set(key, value) { await req((await this.store('readwrite')).put(value, key)); }
  async delete(key) { await req((await this.store('readwrite')).delete(key)); }
  async keys(prefix) {
    const all = await req((await this.store('readonly')).getAllKeys());
    return all.filter((k) => k.startsWith(prefix)).sort();
  }
  async getAll(prefix) {
    const s = await this.store('readonly');
    const range = IDBKeyRange.bound(prefix, prefix + '￿');
    const [keys, vals] = await Promise.all([req(s.getAllKeys(range)), req(s.getAll(range))]);
    return keys.map((k, i) => [k, vals[i]]);
  }
  // one writer per origin: arti's state must not be written by two tabs.
  async tryLock() {
    if (this.release) return false;
    if (!globalThis.navigator?.locks) { this.release = () => {}; return true; }
    return new Promise((resolve) => {
      navigator.locks.request(`${this.name}:write`, { ifAvailable: true }, (lock) => {
        if (!lock) { resolve(false); return; }
        return new Promise((done) => { this.release = done; resolve(true); });
      });
    });
  }
  async unlock() { const r = this.release; this.release = null; if (r) r(); }
  /** Delete everything (used by benchmarks to force a cold start). */
  async clear() { await req((await this.store('readwrite')).clear()); }
}

/** In-memory store: nothing survives a reload. */
export class MemoryStorage {
  constructor() { this.m = new Map(); this.locked = false; }
  async get(k) { return this.m.has(k) ? this.m.get(k) : null; }
  async set(k, v) { this.m.set(k, v); }
  async delete(k) { this.m.delete(k); }
  async keys(p) { return [...this.m.keys()].filter((k) => k.startsWith(p)).sort(); }
  async getAll(p) { return [...this.m.entries()].filter(([k]) => k.startsWith(p)); }
  async tryLock() { if (this.locked) return false; this.locked = true; return true; }
  async unlock() { this.locked = false; }
  async clear() { this.m.clear(); }
}
