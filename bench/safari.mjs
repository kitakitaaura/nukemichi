// runs demo/selftest.html's phase 0 proofs in real safari via safaridriver
// (webdriver). needs "allow remote automation" (safaridriver --enable).
// usage: node bench/safari.mjs <webrtc|websocket> [worker|inline]
import { spawn } from 'node:child_process';
const [transport = 'webrtc', mode = 'worker'] = process.argv.slice(2);
const PORT = 4799;
const drv = spawn('/usr/bin/safaridriver', ['-p', String(PORT)], { stdio: 'ignore' });
const wd = async (method, path, body) => {
  const res = await fetch(`http://localhost:${PORT}${path}`, { method, headers: { 'content-type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
  const j = await res.json();
  if (j.value?.error) throw new Error(`${j.value.error}: ${j.value.message}`);
  return j.value;
};
await new Promise((r) => setTimeout(r, 1500));
let sid;
try {
  sid = (await wd('POST', '/session', { capabilities: { alwaysMatch: { browserName: 'safari' } } })).sessionId;
  await wd('POST', `/session/${sid}/timeouts`, { script: 400000 });
  await wd('POST', `/session/${sid}/url`, { url: `http://localhost:${process.env.PORT || 8766}/demo/selftest.html` });
  await new Promise((r) => setTimeout(r, 1000));
  const r = await wd('POST', `/session/${sid}/execute/async`, {
    script: 'const done = arguments[arguments.length - 1]; window.runPhase0(arguments[0]).then(done, (e) => done({ error: String(e) }));',
    args: [{ transport, worker: mode === 'worker' }],
  });
  console.log(JSON.stringify(r, null, 1));
} catch (e) {
  console.log(JSON.stringify({ error: e.message }));
} finally {
  if (sid) await wd('DELETE', `/session/${sid}`).catch(() => {});
  drv.kill();
}
