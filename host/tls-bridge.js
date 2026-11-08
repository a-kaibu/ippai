// Blocking Read/Write for rustls over a separately scheduled WebSocket worker.
// No plaintext request or response is given to JavaScript or the relay.
addToLibrary({
  $sakeTlsSockets: {next:1, sockets:new Map()},
  $sakeTlsWait: (socket,generation,timeout) => {
    const until=Date.now()+timeout;
    while(Atomics.load(socket.ctl,0)===generation){const left=until-Date.now();if(left<=0){socket.worker.terminate();return -1;}Atomics.wait(socket.ctl,0,generation,left);}
    return Atomics.load(socket.ctl,1);
  },
  sake_tls_open__deps:['$sakeTlsSockets','$sakeTlsWait'],
  sake_tls_open__sig:'iii',
  sake_tls_open:(target,timeout) => {
    try{
      const name=['soft-denchi','dmm-softdenchi','dmm-doujin-softdenchi'][target];if(!name)return -1;
      const address=new URL(ENVIRONMENT_IS_NODE?process.env.SAKE_TLS_RELAY:vnvmTlsRelay);if(!['ws:','wss:'].includes(address.protocol))return -1;
      address.pathname='/net/tls/'+name;address.search='';address.hash='';const url=address.href;
      if(!ENVIRONMENT_IS_NODE){const expected=new URL(vnvmTlsRelay);if(address.origin!==expected.origin)return -1;}
      const shared=new SharedArrayBuffer(16+2*65536),ctl=new Int32Array(shared,0,4);
      const source='('+ippaiTlsSocketWorker.toString()+')()';
      let worker;if(ENVIRONMENT_IS_NODE){worker=new(require('worker_threads').Worker)(source,{eval:true});worker.on('error',() => {});}else{
        if(!ippaiTlsPort)return -1;
        const channel=new MessageChannel();
        ippaiTlsPort.postMessage({port:channel.port1},[channel.port1]);
        worker=channel.port2;
        worker.terminate=() => {worker.postMessage({op:'close'});worker.close();};
      }
      const socket={worker,ctl,tx:new Uint8Array(shared,16,65536),rx:new Uint8Array(shared,16+65536,65536)};
      worker.postMessage({op:'open',url,shared,origin:address.origin.replace(/^ws/,'http'),token:ENVIRONMENT_IS_NODE?process.env.SAKE_TLS_TOKEN:undefined});
      if(sakeTlsWait(socket,0,timeout)!==1){worker.terminate();return -1;}
      const id=sakeTlsSockets.next++;sakeTlsSockets.sockets.set(id,socket);return id;
    }catch{return -1;}
  },
  sake_tls_write__deps:['$sakeTlsSockets','$sakeTlsWait'],
  sake_tls_write__sig:'iipii',
  sake_tls_write:(id,p,n,timeout) => {
    const socket=sakeTlsSockets.sockets.get(id);if(!socket||n>65536)return -1;
    socket.tx.set(HEAPU8.subarray(p>>>0,(p>>>0)+n));const generation=Atomics.load(socket.ctl,0);
    socket.worker.postMessage({op:'send',n,timeout});return sakeTlsWait(socket,generation,timeout);
  },
  sake_tls_read__deps:['$sakeTlsSockets','$sakeTlsWait'],
  sake_tls_read__sig:'iipii',
  sake_tls_read:(id,p,n,timeout) => {
    const socket=sakeTlsSockets.sockets.get(id);if(!socket||n<65536)return -1;
    const generation=Atomics.load(socket.ctl,0);socket.worker.postMessage({op:'recv'});const got=sakeTlsWait(socket,generation,timeout);
    if(got>0)HEAPU8.set(socket.rx.subarray(0,got),p>>>0);return got;
  },
  sake_tls_close__deps:['$sakeTlsSockets'],
  sake_tls_close__sig:'vi',
  sake_tls_close:(id) => {const socket=sakeTlsSockets.sockets.get(id);if(socket){socket.worker.postMessage({op:'close'});socket.worker.terminate();sakeTlsSockets.sockets.delete(id);}},
});
