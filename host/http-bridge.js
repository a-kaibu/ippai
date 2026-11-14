// Synchronous guest API over an authenticated loopback relay. Never log the
// request, response or token. The relay performs the actual verified HTTPS.
addToLibrary({
  sake_http__deps: ['malloc'],
  sake_http__sig: 'pppp',
  sake_http: (request, length, outLength) => {
    let bytes;
    try {
      let relay=ENV.SAKE_HTTP_RELAY, token=ENV.SAKE_HTTP_TOKEN;
      if(ENVIRONMENT_IS_NODE && ENV.SAKE_HTTP_CONFIG){const config=JSON.parse(require('fs').readFileSync(ENV.SAKE_HTTP_CONFIG,'utf8'));relay=config.relay;token=config.token;}
      const url=new URL(relay);
      if(url.protocol!=='http:'||!['127.0.0.1','[::1]','localhost'].includes(url.hostname)||!token||token.length<32)return 0;
      const data=HEAPU8.slice(request>>>0,(request>>>0)+length);
      if(ENVIRONMENT_IS_NODE){
        const shared=new SharedArrayBuffer(24*1024*1024+8), ctl=new Int32Array(shared,0,2);
        const worker=new (require('worker_threads').Worker)(`
          const {workerData:d}=require('worker_threads');const http=require('http');const ctl=new Int32Array(d.shared,0,2);
          const done=(bytes)=>{if(bytes.length> d.shared.byteLength-8){Atomics.store(ctl,0,-1);}else{new Uint8Array(d.shared,8,bytes.length).set(bytes);Atomics.store(ctl,1,bytes.length);Atomics.store(ctl,0,1);}Atomics.notify(ctl,0);};
          const req=http.request(new URL('/request',d.relay),{method:'POST',headers:{'Authorization':'Bearer '+d.token,'Content-Type':'application/json'},timeout:125000},res=>{const chunks=[];let n=0;res.on('data',b=>{n+=b.length;if(n>d.shared.byteLength-8){req.destroy();return}chunks.push(b)});res.on('end',()=>{if(res.statusCode!==200&&res.statusCode!==502){done(Buffer.from('{"error":12029}'));return}done(Buffer.concat(chunks))});res.on('error',()=>done(Buffer.from('{"error":12029}')));});
          req.on('error',()=>done(Buffer.from('{"error":12029}')));req.on('timeout',()=>req.destroy());req.end(Buffer.from(d.data));
        `,{eval:true,workerData:{shared,relay:relay,token:token,data:data}});
        // This pthread can block; the independent worker owns its network loop.
        const until=Date.now()+130000;while(Atomics.load(ctl,0)===0){const left=until-Date.now();if(left<=0)break;Atomics.wait(ctl,0,0,left);}
        // (the worker ends once its answer is read: it lived on with its socket and 24 MiB buffer)
        const ok=Atomics.load(ctl,0)===1;if(ok)bytes=new Uint8Array(shared,8,Atomics.load(ctl,1)).slice();worker.terminate();if(!ok)return 0;
      }else{
        const xhr=new XMLHttpRequest();xhr.open('POST',new URL('/request',relay).href,false);xhr.responseType='arraybuffer';
        // (a guest thread is a worker, where a synchronous request may have a time limit: a stalled
        // relay no longer holds it for good)
        try{xhr.timeout=130000;}catch{}xhr.setRequestHeader('Authorization','Bearer '+token);xhr.setRequestHeader('Content-Type','application/json');xhr.send(data);if(xhr.status!==200&&xhr.status!==502)return 0;bytes=new Uint8Array(xhr.response);
      }
      const p=_malloc(Math.max(bytes.length,1));if(!p)return 0;HEAPU8.set(bytes,p>>>0);HEAPU32[outLength>>>2]=bytes.length;return p;
    }catch{return 0;}
  },
});
