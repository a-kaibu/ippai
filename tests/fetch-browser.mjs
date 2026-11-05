// Real browser regression: creating a nested worker immediately before Atomics.wait can
// deadlock its startup. The network broker must be created by the unblocked page first.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
const pw = await import(process.env.PLAYWRIGHT ?? 'playwright');
const { chromium } = pw.default ?? pw;
const source = await readFile(new URL('../host/fetch-bridge.js', import.meta.url), 'utf8');
const server = createServer((req, res) => {
  res.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
  res.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
  if (req.url === '/') {res.setHeader('Content-Type','text/html');res.end('<!doctype html><title>fetch test</title>');return;}
  if (req.url === '/missing') {res.writeHead(404);res.end();return;}
  setTimeout(() => res.end(Buffer.from(Array.from({length:150000},(_,i)=>i%251))), req.url === '/slow' ? 40 : 1);
});
await new Promise(resolve => server.listen(0,'127.0.0.1',resolve));
const browser = await chromium.launch({headless:true, executablePath:process.env.CHROME, chromiumSandbox:false});
try {
  const page = await browser.newPage();
  await page.goto(`http://127.0.0.1:${server.address().port}`);
  const results = await page.evaluate(async source => {
    const connect = new Function(source + '; return ippaiFetchConnect;')();
    const request = path => new Promise((resolve,reject) => {
      const port = connect();
      const script = source + `\nself.onmessage=e=>{ippaiFetchPort=e.data.port;try{self.postMessage({bytes:Array.from(ippaiFetchSync(e.data.url))});}catch(e){self.postMessage({error:e.message});}};`;
      const blob = URL.createObjectURL(new Blob([script],{type:'text/javascript'}));
      const worker = new Worker(blob);
      const timer = setTimeout(()=>{worker.terminate();reject(new Error('worker stalled'));},10000);
      worker.onerror = reject;
      worker.onmessage = e => {clearTimeout(timer);worker.terminate();URL.revokeObjectURL(blob);resolve(e.data);};
      worker.postMessage({port,url:location.origin+path},[port]);
    });
    return await Promise.all(['/slow','/fast','/missing'].map(request));
  },source);
  const expected = Array.from({length:150000},(_,i)=>i%251);
  assert.deepEqual(results[0].bytes,expected);
  assert.deepEqual(results[1].bytes,expected);
  assert.match(results[2].error,/fetch failed/);
  console.log('browser fetch: parallel ports, binary boundaries and failure passed');
} finally {
  await browser.close();
  await new Promise(resolve=>server.close(resolve));
}
