// debug: why does the background consensus refresh fail after bootstrap?
import { chromium } from 'playwright';
const kill = setTimeout(() => { console.log('TIMEOUT'); process.exit(2); }, 200000);
const b = await chromium.launch(); const p = await b.newPage();
await p.goto('http://localhost:8766/demo/poc.html');
await p.evaluate(() => window.boot({ transport: 'websocket', storage: 'memory', logLevel: 'debug' }));
await p.waitForTimeout(40000);
console.log(await p.evaluate(() => window.logs.filter((l) => /dirmgr|dirclient|consensus/i.test(l) && !/trace/.test(l)).slice(0, 80).join('\n')));
clearTimeout(kill); await b.close();
