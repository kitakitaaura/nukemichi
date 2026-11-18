// which decompressionstream formats does each engine support?
import { chromium, firefox, webkit } from 'playwright';
for (const [name, bt] of Object.entries({ chromium, firefox, webkit })) {
  const b = await bt.launch(process.env.CHANNEL ? { channel: process.env.CHANNEL } : {}); const p = await b.newPage(); await p.goto('http://localhost:8766/demo/poc.html');
  console.log(name, b.version(), JSON.stringify(await p.evaluate(() => Object.fromEntries(['gzip', 'deflate', 'deflate-raw', 'br', 'brotli', 'zstd'].map((f) => { try { new DecompressionStream(f); return [f, true]; } catch { return [f, false]; } })))));
  await b.close();
}
