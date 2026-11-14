// This independent worker owns the WebSocket event loop while a guest pthread
// blocks. It sees only TLS records, never HTTP requests or account credentials.
function ippaiTlsSocketWorker(channel) {
  // (what may wait for the guest's reads: 64 KiB a read, the relay does not wait for them, so a
  // fast download to a busy guest thread failed at 1 MiB; 16 MiB per socket)
  const IPPAI_TLS_QUEUE=16*1024*1024;
  const node=typeof process==='object'&&!!process.versions?.node;
  const port=channel||(node?require('worker_threads').parentPort:self);
  let ws,ctl,rx,tx,pending=false,opening=true,closed=false,failed=false,queue=[],queued=0;
  const complete=(result)=>{Atomics.store(ctl,1,result);Atomics.add(ctl,0,1);Atomics.notify(ctl,0);};
  const receive=()=>{
    if(!pending)return;
    if(queued){let n=0;while(queue.length&&n<rx.length){const b=queue[0],take=Math.min(b.length,rx.length-n);rx.set(b.subarray(0,take),n);n+=take;queued-=take;if(take===b.length)queue.shift();else queue[0]=b.subarray(take);}pending=false;complete(n);}
    else if(closed){pending=false;complete(failed?-1:0);}
  };
  const message=(d)=>{
    if(d.op==='open'){
      ctl=new Int32Array(d.shared,0,4);tx=new Uint8Array(d.shared,16,65536);rx=new Uint8Array(d.shared,16+65536,65536);
      try{
        const options=node?{protocols:['ippai.tls.v1'],headers:{Origin:d.origin,...(d.token?{Authorization:'Bearer '+d.token}:{})}}:['ippai.tls.v1'];
        ws=new WebSocket(d.url,options);ws.binaryType='arraybuffer';
        ws.onopen=()=>{opening=false;if(ws.protocol!=='ippai.tls.v1'){failed=true;ws.close();complete(-1);return;}complete(1);};
        ws.onmessage=e=>{if(!(e.data instanceof ArrayBuffer)){failed=true;ws.close();return;}const b=new Uint8Array(e.data);if(queued+b.length>IPPAI_TLS_QUEUE){failed=true;ws.close();return;}if(b.length){queue.push(b);queued+=b.length;}receive();};
        ws.onerror=()=>{failed=true;};ws.onclose=()=>{closed=true;if(opening){opening=false;complete(-1);}else receive();};
      }catch{failed=closed=true;complete(-1);}return;
    }
    if(d.op==='send'){
      if(closed||!ws||ws.readyState!==1){complete(-1);return;}
      try{ws.send(tx.slice(0,d.n));const until=Date.now()+d.timeout;
        const drained=()=>{if(ws.readyState!==1){complete(-1);return;}if(ws.bufferedAmount<=65536){complete(d.n);return;}if(Date.now()>=until){failed=true;ws.close();complete(-1);return;}setTimeout(drained,5);};drained();
      }catch{complete(-1);}return;
    }
    if(d.op==='recv'){pending=true;receive();return;}
    if(d.op==='close'){closed=true;ws?.close();queue=[];queued=0;receive();}
  };
  if(node)port.on('message',message);else port.onmessage=e=>message(e.data);
}

// Chromium needs the creating worker to yield while a nested worker boots.
// Guest pthreads immediately Atomics.wait, so only the page creates workers.
// Each pthread receives a broker port; each TLS socket gets its own channel.
var ippaiTlsBroker = null;
var ippaiTlsPort = null;
function ippaiTlsConnect() {
  if (!ippaiTlsBroker) {
    const source = ippaiTlsSocketWorker.toString() +
      ';self.onmessage=e=>{const port=e.data.port;port.onmessage=m=>ippaiTlsSocketWorker(m.data.port);};';
    const blob = URL.createObjectURL(new Blob([source], {type:'text/javascript'}));
    ippaiTlsBroker = new Worker(blob);
    URL.revokeObjectURL(blob);
  }
  const channel = new MessageChannel();
  ippaiTlsBroker.postMessage({port:channel.port1}, [channel.port1]);
  return channel.port2;
}
