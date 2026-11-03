const fs=require('node:fs');
const os=require('node:os');
const path=require('node:path');
const net=require('node:net');
const http=require('node:http');
const https=require('node:https');
const {spawn,execFileSync}=require('node:child_process');
const exercise=require('./session-browser.cjs');

(async()=>{
  for(const port of [3041,3443])await new Promise((resolve,reject)=>{const server=net.createServer();server.once('error',()=>reject(Error('Test port occupied: '+port)));server.listen(port,'127.0.0.1',()=>server.close(resolve));});
  const directory=fs.mkdtempSync(path.join(os.tmpdir(),'flashpaste-session-local-'));
  execFileSync('openssl',['req','-x509','-newkey','rsa:2048','-nodes','-keyout',path.join(directory,'key.pem'),'-out',path.join(directory,'cert.pem'),'-days','1','-subj','/CN=localhost','-addext','subjectAltName=IP:127.0.0.1,DNS:localhost'],{stdio:'ignore'});
  const child=spawn(path.join(__dirname,'../target/debug/flashpaste'),[],{cwd:directory,env:{...process.env,FLASHPASTE_BIND:'127.0.0.1:3041',FLASHPASTE_ORIGIN:'https://127.0.0.1:3443'},stdio:['ignore','pipe','pipe']});
  let log='';child.stdout.on('data',chunk=>log+=chunk);child.stderr.on('data',chunk=>log+=chunk);
  const proxy=https.createServer({key:fs.readFileSync(path.join(directory,'key.pem')),cert:fs.readFileSync(path.join(directory,'cert.pem'))},(req,res)=>{const upstream=http.request({hostname:'127.0.0.1',port:3041,path:req.url,method:req.method,headers:req.headers},response=>{res.writeHead(response.statusCode,response.headers);response.pipe(res);});upstream.on('error',()=>{res.writeHead(503);res.end();});req.pipe(upstream);});
  try{
    await new Promise(resolve=>proxy.listen(3443,'127.0.0.1',resolve));
    for(let i=0;i<100;i++){if(child.exitCode!==null)throw Error(log);try{if((await fetch('http://127.0.0.1:3041/health')).ok)break;}catch{}await new Promise(resolve=>setTimeout(resolve,50));}
    await exercise('https://127.0.0.1:3443',directory);
  }finally{proxy.closeAllConnections();proxy.close();child.kill('SIGTERM');}
})().catch(error=>{console.error(error);process.exitCode=1});
