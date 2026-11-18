// offline check of engine.decoderesponse (br via wasm or native, gzip native),
// in a worker as in production. uses local fixtures; no tor traffic.
import { chromium, firefox, webkit } from 'playwright';
for (const [name, bt] of Object.entries({ chromium, firefox, webkit })) {
  const b = await bt.launch(); const p = await b.newPage();
  await p.goto(`http://localhost:${process.env.PORT || 8766}/demo/poc.html`);
  const r = await p.evaluate(async () => {
    const base = location.origin;
    const src = `
      import { loadWasm, decodeResponse } from '${base}/js/src/engine.js';
      onmessage = async () => {
        await loadWasm();
        const out = {};
        for (const [enc, file] of [['br', 'sample.json.br'], ['gzip', 'sample.json.gz']]) {
          const raw = await fetch('${base}/bench/fixtures/' + file);
          const res = decodeResponse(new Response(raw.body, { headers: { 'content-encoding': enc } }));
          const j = await res.json();
          out[enc] = { brotli: j.brotli, chars: j.pad.length, ce: res.headers.get('content-encoding') };
        }
        postMessage(out);
      };
      onerror = (e) => postMessage({ error: String(e.message || e) });
      onunhandledrejection = (e) => postMessage({ error: String(e.reason) });`;
    const w = new Worker(URL.createObjectURL(new Blob([src], { type: 'text/javascript' })), { type: 'module' });
    w.postMessage(0);
    return await new Promise((res) => { w.onmessage = (e) => res(e.data); w.onerror = (e) => res({ error: e.message }); });
  });
  console.log(name, b.version(), JSON.stringify(r));
  await b.close();
}
