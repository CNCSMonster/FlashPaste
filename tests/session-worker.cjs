// Runs exclusively against Miniflare/local D1 in a fresh directory.
const fs=require('node:fs');
const path=require('node:path');
const net=require('node:net');
const https=require('node:https');
const {spawn,execFileSync}=require('node:child_process');
const exercise=require('./session-browser.cjs');
const assert=require('node:assert/strict');

(async()=>{
  for(const port of [8789,9241])await new Promise((resolve,reject)=>{const server=net.createServer();server.once('error',()=>reject(Error('Test port occupied: '+port)));server.listen(port,'127.0.0.1',()=>server.close(resolve));});
  const root=path.join(__dirname,'..'), directory=fs.mkdtempSync('/tmp/opencode/flashpaste-session-worker-'), state=path.join(directory,'state');
  const env={...process.env,CI:'1',WRANGLER_SEND_METRICS:'false'};
  delete env.FLASHPASTE_API_TOKEN;delete env.CLOUDFLARE_API_TOKEN;
  execFileSync('openssl',['req','-x509','-newkey','rsa:2048','-nodes','-keyout',path.join(directory,'key.pem'),'-out',path.join(directory,'cert.pem'),'-days','1','-subj','/CN=localhost','-addext','subjectAltName=IP:127.0.0.1,DNS:localhost'],{stdio:'ignore'});
  // Rehearse an upgrade, not only a clean schema: existing token/content/ACL,
  // plus the production legacy empty plaintext column, must survive unchanged.
  const sql=(command)=>JSON.parse(execFileSync('wrangler',['d1','execute','flashpaste-db','--local','--persist-to',state,'--json','--command',command],{cwd:root,env,encoding:'utf8'}));
  execFileSync('wrangler',['d1','execute','flashpaste-db','--local','--persist-to',state,'--file',path.join(root,'migrations/0001_init.sql')],{cwd:root,env,stdio:'pipe'});
  sql("ALTER TABLE tokens ADD COLUMN token_value TEXT; INSERT INTO tokens(id,token_hash,name,level,created_at) VALUES('upgrade-token','synthetic-hash','upgrade-test','user',1); INSERT INTO contents(id,content_type,content,visibility,created_by,created_at,expires_at,max_expires_at,burn_after_read) VALUES('upgrade-content','text','retained-body','token','upgrade-token',1,9999999999,9999999999,0); INSERT INTO access_control(content_id,token_id,granted_at,granted_by) VALUES('upgrade-content','upgrade-token',1,'upgrade-token');");
  for(let i=0;i<2;i++) execFileSync('wrangler',['d1','migrations','apply','flashpaste-db','--local','--persist-to',state],{cwd:root,env,input:'y\n',stdio:['pipe','pipe','pipe']});
  const retained=sql("SELECT t.auth_version,t.token_value,c.content,(SELECT COUNT(*) FROM access_control) AS acl FROM tokens t JOIN contents c ON c.created_by=t.id WHERE t.id='upgrade-token';")[0].results[0];
  assert.deepEqual(retained,{auth_version:0,token_value:null,content:'retained-body',acl:1});
  sql("DELETE FROM contents WHERE id='upgrade-content'; DELETE FROM tokens WHERE id='upgrade-token';");
  console.log('PASS local D1 upgrade: Token/content/ACL/legacy column retained; migration runner restart is idempotent.');
  const dev=spawn('wrangler',['dev','--local','--ip','127.0.0.1','--port','8789','--inspector-port','9241','--local-protocol','https','--https-key-path',path.join(directory,'key.pem'),'--https-cert-path',path.join(directory,'cert.pem'),'--var','FLASHPASTE_ORIGIN:https://127.0.0.1:8789','--persist-to',state,'--show-interactive-dev-session=false','--log-level','error'],{cwd:root,env,detached:true,stdio:['ignore','pipe','pipe']});
  let log='';const collect=data=>log=(log+data).slice(-24000);dev.stdout.on('data',collect);dev.stderr.on('data',collect);
  try{
    let ready=false;
    for(let i=0;i<300;i++){
      if(dev.exitCode!==null)throw Error('Worker test server exited: '+log);
      ready=await new Promise(resolve=>{const req=https.get('https://127.0.0.1:8789/health',{rejectUnauthorized:false},response=>{response.resume();resolve(response.statusCode===200)});req.on('error',()=>resolve(false));req.setTimeout(1000,()=>{req.destroy();resolve(false)});});
      if(ready)break;await new Promise(resolve=>setTimeout(resolve,200));
    }
    if(!ready)throw Error('Local Worker did not start: '+log);
    await exercise('https://127.0.0.1:8789',directory);
    console.log('Isolated local D1 directory:',state);
  }catch(error){console.error(log);throw error;}finally{try{process.kill(-dev.pid,'SIGTERM')}catch{}dev.stdout.destroy();dev.stderr.destroy();}
})().catch(error=>{console.error(error);process.exitCode=1});
