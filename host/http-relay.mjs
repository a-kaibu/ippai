// Local HTTPS transport for guest WinINet. Authorization stays in the original
// vendor program. No request bodies, cookies or credentials are logged or saved.
import http from 'node:http';
import https from 'node:https';
import {randomBytes, timingSafeEqual} from 'node:crypto';
import {writeFileSync} from 'node:fs';
import {pathToFileURL} from 'node:url';
const MAX = 4 * 1024 * 1024;
export function createRelay({token, origins = [], hosts = [], allowHttpForTests = false}) {
  if (!token || token.length < 32) throw new Error('A random relay token is required');
  const allowed = new Set(hosts.map(h => h.toLowerCase()));
  // Cookies belong to one InternetOpen session and are never exposed to the UI.
  const jars = new Map();
  function target(value) {
    const u = new URL(value);
    if ((!allowHttpForTests && u.protocol !== 'https:') || !['http:','https:'].includes(u.protocol) || u.username || u.password || !allowed.has(u.hostname.toLowerCase())) throw new Error('destination refused');
    return u;
  }
  async function perform(r, signal) {
    let u=target(r.url); const method=String(r.method||'GET').toUpperCase();
    if (!/^[A-Z]+$/.test(method) || !Array.isArray(r.body) || r.body.length>MAX || !r.body.every(b=>Number.isInteger(b)&&b>=0&&b<=255)) throw new Error('invalid request');
    const body=Buffer.from(r.body);let session=String(r.session||'');if(!/^[a-zA-Z0-9:-]{1,100}$/.test(session))throw new Error('invalid session');
    if(!jars.has(session)){if(jars.size>=128)throw new Error('too many sessions');jars.set(session,[]);}
    for(let redirects=0;;redirects++) {
      const headers={};for(const pair of r.headers||[]){if(!Array.isArray(pair)||pair.length!==2)throw new Error('invalid header');const[k,v]=pair;if(!/^[!#$%&'*+.^_`|~0-9A-Za-z-]+$/.test(k)||/[\r\n\0]/.test(v))throw new Error('invalid header');if(!['host','connection','content-length','transfer-encoding','proxy-authorization'].includes(k.toLowerCase()))headers[k]=v;}
      if(r.agent)headers['User-Agent']=String(r.agent);
      const jar=jars.get(session);const now=Date.now();const cookies=jar.filter(c=>c.expires>now&&c.domain===u.hostname&&u.pathname.startsWith(c.path)&&(!c.secure||u.protocol==='https:'));
      if(cookies.length)headers.Cookie=cookies.map(c=>c.value).join('; ');
      const response=await new Promise((resolve,reject)=>{
        const req=(u.protocol==='https:'?https:http).request(u,{method,headers,signal,timeout:Math.min(Math.max(Number(r.timeout)||30000,100),120000)},res=>{
          const chunks=[];let size=0;res.on('data',b=>{size+=b.length;if(size>MAX){req.destroy(new Error('response too large'));return}chunks.push(b)});res.on('end',()=>resolve({status:res.statusCode,statusText:res.statusMessage,headers:res.headers,raw:res.rawHeaders,body:Buffer.concat(chunks)}));res.on('error',reject);
        });req.on('timeout',()=>req.destroy(Object.assign(new Error('timeout'),{code:'ETIMEDOUT'})));req.on('error',reject);req.end(body.length?body:undefined);
      });
      for(const set of response.headers['set-cookie']||[]){const [value,...attrs]=set.split(';');const name=value.split('=',1)[0];if(!name)continue;const cookie={value,domain:u.hostname,path:u.pathname.slice(0,u.pathname.lastIndexOf('/')+1)||'/',secure:false,expires:Infinity};let valid=true;
        for(const attr of attrs){const[k,...values]=attr.trim().split('=');const v=values.join('=');switch(k.toLowerCase()){case'domain':if(v.replace(/^\./,'').toLowerCase()!==u.hostname){valid=false;}break;case'path':if(v.startsWith('/'))cookie.path=v;break;case'secure':cookie.secure=true;break;case'max-age':cookie.expires=now+Number(v)*1000;break;case'expires':if(cookie.expires===Infinity)cookie.expires=Date.parse(v);break;}}
        if(valid){const i=jar.findIndex(c=>c.domain===cookie.domain&&c.path===cookie.path&&c.value.split('=',1)[0]===name);if(i>=0)jar.splice(i,1);if(cookie.expires>now)jar.push(cookie);}
      }
      if([301,302,303,307,308].includes(response.status)&&response.headers.location&&!r.noRedirect){if(redirects>=5)throw new Error('redirect limit');
        // Never repeat a credential-bearing POST automatically. Its caller must
        // decide how to handle the redirect returned by the original server.
        if(method==='GET'||method==='HEAD') {
        const next=target(new URL(response.headers.location,u).href);if(next.origin!==u.origin){r={...r,headers:(r.headers||[]).filter(([k])=>!['authorization','cookie'].includes(k.toLowerCase()))};}u=next;continue;
        }
      }
      const pairs=[];for(let i=0;i<response.raw.length;i+=2)pairs.push([response.raw[i],response.raw[i+1]]);
      return {status:response.status,statusText:response.statusText,headers:pairs,body:Array.from(response.body)};
    }
    throw new Error('unsupported redirect');
  }
  return http.createServer(async(req,res)=>{
    const origin=req.headers.origin;
    res.setHeader('Cache-Control','no-store');res.setHeader('X-Content-Type-Options','nosniff');
    if(origin){if(!origins.includes(origin)){res.writeHead(403).end();return}res.setHeader('Access-Control-Allow-Origin',origin);res.setHeader('Vary','Origin');}
    if(req.method==='OPTIONS'){res.setHeader('Access-Control-Allow-Methods','POST');res.setHeader('Access-Control-Allow-Headers','authorization,content-type');res.writeHead(204).end();return;}
    const auth=Buffer.from(req.headers.authorization||'');const expected=Buffer.from('Bearer '+token);
    if(req.url!=='/request'||req.method!=='POST'||auth.length!==expected.length||!timingSafeEqual(auth,expected)){res.writeHead(403).end();return;}
    let size=0;const chunks=[];const abort=new AbortController();res.on('close',()=>abort.abort());
    try{for await(const b of req){size+=b.length;if(size>MAX*5)throw new Error('request too large');chunks.push(b)}const request=JSON.parse(Buffer.concat(chunks).toString('utf8'));
      const result=await perform(request,abort.signal);res.setHeader('Content-Type','application/json');res.end(JSON.stringify(result));
    }catch(error){if(!res.destroyed){const code=error?.code;const internetError=code==='ENOTFOUND'?12007:code==='ETIMEDOUT'?12002:code==='CERT_HAS_EXPIRED'?12037:code==='ERR_TLS_CERT_ALTNAME_INVALID'?12038:['DEPTH_ZERO_SELF_SIGNED_CERT','SELF_SIGNED_CERT_IN_CHAIN','UNABLE_TO_VERIFY_LEAF_SIGNATURE','UNABLE_TO_GET_ISSUER_CERT_LOCALLY'].includes(code)?12045:code==='ABORT_ERR'?12017:12029;res.setHeader('Content-Type','application/json');res.writeHead(502).end(JSON.stringify({error:internetError}));}}
  });
}
if(process.argv[1]&&import.meta.url===pathToFileURL(process.argv[1]).href){
 const args=process.argv.slice(2);const option=(name)=>{const i=args.indexOf(name);return i<0?undefined:args[i+1]};
 const config=option('--config');if(!config)throw new Error('Usage: node host/http-relay.mjs --config PRIVATE_FILE [--origin http://localhost:5173] [--hosts host1,host2]');
 const token=randomBytes(32).toString('hex');const server=createRelay({token,origins:[option('--origin')||'http://localhost:5173'],hosts:(option('--hosts')||'charge1.soft-denchi.jp').split(',')});
 server.listen(Number(option('--port')||0),'127.0.0.1',()=>{const relay=`http://127.0.0.1:${server.address().port}`;writeFileSync(config,JSON.stringify({relay,token})+'\n',{mode:0o600,flag:'wx'});console.log('HTTPS relay ready; private connection settings written to '+config);});
}
