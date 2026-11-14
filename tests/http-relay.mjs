import {test} from 'node:test';import assert from 'node:assert/strict';import http from 'node:http';import {createRelay} from '../host/http-relay.mjs';
const listen=s=>new Promise(r=>s.listen(0,'127.0.0.1',r));const close=s=>new Promise(r=>s.close(r));
test('authenticated relay preserves actual bytes, status, sessions and redirects',async()=>{
 let visits=0;const upstream=http.createServer(async(req,res)=>{visits++;if(req.url==='/redirect'){res.writeHead(302,{Location:'/binary'}).end();return;}if(req.url==='/cookie'){res.end(req.headers.cookie||'');return;}const chunks=[];for await(const b of req)chunks.push(b);if(req.method==='POST'){res.writeHead(201,{'X-Actual':'server','Set-Cookie':'test=one; Path=/; HttpOnly'});res.end(Buffer.concat(chunks));}else{res.end(Buffer.from([0,0xff,42]));}});
 const token='a'.repeat(64);const relay=createRelay({token,origins:['http://localhost:5173'],hosts:['127.0.0.1'],allowHttpForTests:true});await listen(upstream);await listen(relay);
 const url=`http://127.0.0.1:${relay.address().port}/request`,target=`http://127.0.0.1:${upstream.address().port}`;
 const call=(r,auth=token,origin)=>fetch(url,{method:'POST',headers:{Authorization:'Bearer '+auth,'Content-Type':'application/json',...(origin?{Origin:origin}:{})},body:JSON.stringify({url:target+'/binary',method:'POST',headers:[['Content-Type','application/octet-stream']],body:[0,255,128,42],session:'first',...r})});
 try{
 assert.equal((await call({},'wrong')).status,403);assert.equal((await call({},token,'https://untrusted.invalid')).status,403);assert.equal(visits,0);
 const result=await(await call({})).json();assert.equal(result.status,201);assert.deepEqual(result.body,[0,255,128,42]);assert.ok(result.headers.some(([n,v])=>n.toLowerCase()==='x-actual'&&v==='server'));
 const cookie=await(await call({url:target+'/cookie',method:'GET',body:[]})).json();assert.equal(Buffer.from(cookie.body).toString(),'test=one');
 const separate=await(await call({url:target+'/cookie',method:'GET',body:[],session:'second'})).json();assert.equal(separate.body.length,0);
 let before=visits;const redirect=await(await call({url:target+'/redirect'})).json();assert.equal(redirect.status,302);assert.equal(visits,before+1);
 const follow=await(await call({url:target+'/redirect',method:'GET',body:[]})).json();assert.deepEqual(follow.body,[0,255,42]);
 before=visits;assert.equal((await call({url:'http://example.invalid/'})).status,502);assert.equal(visits,before);
 const preflight=await fetch(url,{method:'OPTIONS',headers:{Origin:'http://localhost:5173'}});assert.equal(preflight.status,204);assert.equal(preflight.headers.get('access-control-allow-origin'),'http://localhost:5173');
 }finally{await close(relay);await close(upstream);}
});
test('production relay refuses plaintext destinations',async()=>{const token='b'.repeat(64);const relay=createRelay({token,hosts:['127.0.0.1']});await listen(relay);try{const res=await fetch(`http://127.0.0.1:${relay.address().port}/request`,{method:'POST',headers:{Authorization:'Bearer '+token},body:JSON.stringify({url:'http://127.0.0.1:9/',session:'test',body:[]})});assert.equal(res.status,502);}finally{await close(relay);}});

test('untrusted HTTPS certificates are rejected',async()=>{
 const {mkdtempSync,readFileSync,rmSync}=await import('node:fs');const {tmpdir}=await import('node:os');const {join}=await import('node:path');const {execFileSync}=await import('node:child_process');const https=await import('node:https');
 const dir=mkdtempSync(join(tmpdir(),'sake-tls-test-'));let upstream,relay;
 try{execFileSync('openssl',['req','-x509','-newkey','rsa:2048','-nodes','-keyout',join(dir,'key.pem'),'-out',join(dir,'cert.pem'),'-subj','/CN=localhost','-days','1'],{stdio:'ignore'});
 upstream=https.createServer({key:readFileSync(join(dir,'key.pem')),cert:readFileSync(join(dir,'cert.pem'))},(req,res)=>res.end('must not accept'));
 const token='c'.repeat(64);relay=createRelay({token,hosts:['127.0.0.1']});await listen(upstream);await listen(relay);
 const response=await fetch(`http://127.0.0.1:${relay.address().port}/request`,{method:'POST',headers:{Authorization:'Bearer '+token},body:JSON.stringify({url:`https://127.0.0.1:${upstream.address().port}/`,method:'GET',session:'tls',body:[]})});assert.equal(response.status,502);assert.equal((await response.json()).error,12045);
 }finally{if(relay)await close(relay);if(upstream)await close(upstream);rmSync(dir,{recursive:true,force:true});}
});

// review b11: the 129th session no longer fails every request; the one unused longest goes
test('sessions past 128 evict the one unused longest',async()=>{
 const upstream=http.createServer((req,res)=>res.end(req.headers.cookie||'none'));
 let jars;const token='d'.repeat(64);const relay=createRelay({token,hosts:['127.0.0.1'],allowHttpForTests:true,onJars:j=>jars=j});await listen(upstream);await listen(relay);
 const call=(session)=>fetch(`http://127.0.0.1:${relay.address().port}/request`,{method:'POST',headers:{Authorization:'Bearer '+token},body:JSON.stringify({url:`http://127.0.0.1:${upstream.address().port}/`,method:'GET',session,body:[]})});
 try{
  for(let i=0;i<300;i++){const r=await call('s'+(i%200));assert.equal(r.status,200);await r.json();assert.ok(jars.size<=128);}
  // (the last 128 used are the ones kept, in use order)
  assert.deepEqual([...jars.keys()].slice(-3),['s97','s98','s99']);
  assert.ok(!jars.has('s100'));
 }finally{await close(relay);await close(upstream);}
});
