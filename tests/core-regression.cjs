// Same real API + CLI regression on Axum and Miniflare/D1, never production.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {promisify} = require('node:util');
const execFile = promisify(require('node:child_process').execFile);

module.exports = async function regression(origin, api, superToken, directory) {
  assert(['localhost','127.0.0.1'].includes(new URL(origin).hostname));
  const headers = {Origin:origin};
  const call = (method, suffix, data, token) => api[method](origin+suffix, {
    headers: token ? {Authorization:'Bearer '+token} : headers, ...(data === undefined ? {} : {data}),
  });
  const status = async (response, expected) => {
    assert.equal(response.status(),expected, new URL(response.url()).pathname);
    return response;
  };
  const make = async (extra={}, token) => {
    const response=await call('post','/api/contents',{content_type:'text',content:'原文\n<&> 😀',visibility:'public',ttl:3600,...extra},token);
    await status(response,201);return response.json();
  };
  const token = async name => {
    const response=await call('post','/api/tokens',{name,level:'normal'});await status(response,201);return response.json();
  };
  const owner=await token('回归 owner + 名称'), reader=await token('回归 reader'), outsider=await token('回归 outsider');
  const created=[];
  try {
    // Normal downloads: all formats, UTF-8 bytes, anonymous/public and ACL/private.
    for (const content_type of ['text','markdown','html']) {
      const content=content_type==='html' ? '<h1>下载原文</h1><script>window.bad=true</script>' : '# 正文\n<&> 😀\r\n';
      const item=await make({content_type,content,title:'中文 " / 标题'},owner.token_plaintext);created.push(item.id);
      const download=await call('get','/d/'+item.id);await status(download,200);
      assert.deepEqual(await download.body(),Buffer.from(content));
      assert.match(download.headers()['content-disposition'],/attachment/);
      assert.match(download.headers()['content-type'],/text\/plain|application\/octet-stream/);
      assert.equal(download.headers()['x-content-type-options'],'nosniff');
      assert.equal(download.headers()['cache-control'],'no-store');
      const viewed=await call('get','/v/'+item.id);await status(viewed,200);
      assert.match(viewed.headers()['content-type'],/text\/html/);
      if(content_type==='html') assert.match(await viewed.text(),/sandbox=""/);
      // Non-burn reads/downloads are repeatable.
      await status(await call('get','/d/'+item.id),200);
      const raw=await call('get','/api/contents/'+item.id+'/body');assert.equal((await raw.json()).content,content);
    }
    // No title autofill; every supported TTL; validation and exact UTF-8 byte boundary.
    for(const ttl of [3600,21600,86400,172800]) {const item=await make({ttl,title:'   '});created.push(item.id);const metadata=await (await call('get','/api/contents/'+item.id)).json();assert.equal(metadata.title,'');assert(Math.abs(metadata.expires_at-metadata.created_at-ttl)<1);}
    for(const extra of [{ttl:0},{ttl:604800},{content_type:'pdf'},{visibility:'secret'},{burn_after_read:true},{burn_after_read:true,ttl:604800},{allowed_tokens:[reader.token_id]},{visibility:'token',allowed_tokens:['missing']},{visibility:'token',allowed_tokens:[reader.token_id,reader.token_id]}]) {
      const response=await call('post','/api/contents',{content_type:'text',content:'invalid',visibility:'public',ttl:3600,...extra});
      assert(response.status()>=400 && response.status()<500,'Invalid input must be a client error');
    }
    const exact='中'.repeat(170666)+'ab';assert.equal(Buffer.byteLength(exact),500*1024);
    const boundary=await make({content:exact});created.push(boundary.id);
    assert.equal((await (await call('get','/api/contents/'+boundary.id+'/body')).json()).content,exact);
    const tooLarge=await call('post','/api/contents',{content_type:'text',content:exact+'a',visibility:'public',ttl:3600});
    await status(tooLarge,413);
    const malformed=await api.post(origin+'/api/contents',{headers:{...headers,'Content-Type':'application/json'},data:'{invalid'});
    assert(malformed.status()>=400 && malformed.status()<500,'Malformed JSON must not become a service outage');
    for(const level of ['admin','user','super']) await status(await call('post','/api/tokens',{name:'forbidden-'+level,level},owner.token_plaintext),403);
    for(const level of ['admin','user']) await status(await call('post','/api/tokens',{name:'invalid-'+level,level}),400);
    await status(await call('get','/api/tokens',undefined,owner.token_plaintext),403);
    assert([400,409].includes((await call('post','/api/tokens',{name:owner.name,level:'normal'})).status()));
    // Standard form encoding (+ means space), Chinese and literal plus are supported.
    const lookup=await call('get','/api/tokens/lookup?'+new URLSearchParams({name:owner.name}));
    await status(lookup,200);assert.equal((await lookup.json()).id,owner.token_id);
    const privateItem=await make({visibility:'token',allowed_tokens:[reader.token_id]},owner.token_plaintext);created.push(privateItem.id);
    for(const endpoint of ['/body','']) await status(await call('get','/api/contents/'+privateItem.id+endpoint,undefined,outsider.token_plaintext),404);
    await status(await call('get','/d/'+privateItem.id,undefined,outsider.token_plaintext),404);
    await status(await call('get','/d/'+privateItem.id,undefined,reader.token_plaintext),200);
    await status(await call('get','/v/'+privateItem.id),404);
    await status(await call('patch','/api/contents/'+privateItem.id,{allowed_tokens:[]},reader.token_plaintext),404);
    await status(await call('delete','/api/contents/'+privateItem.id,{},reader.token_plaintext),404);
    const before=await (await call('get','/api/contents/'+privateItem.id,undefined,owner.token_plaintext)).json();
    await status(await call('patch','/api/contents/'+privateItem.id,{allowed_tokens:['missing']},owner.token_plaintext),400);
    await status(await call('get','/api/contents/'+privateItem.id+'/body',undefined,reader.token_plaintext),200);
    await status(await call('patch','/api/contents/'+privateItem.id,{allowed_tokens:[]},owner.token_plaintext),200);
    await status(await call('get','/api/contents/'+privateItem.id+'/body',undefined,reader.token_plaintext),404);
    await status(await call('patch','/api/contents/'+privateItem.id,{allowed_tokens:[reader.token_id]},owner.token_plaintext),200);
    await status(await call('get','/api/contents/'+privateItem.id+'/body',undefined,reader.token_plaintext),200);
    await status(await call('patch','/api/contents/'+privateItem.id,{expires_at:before.expires_at+3600},owner.token_plaintext),400);
    await status(await call('patch','/api/contents/'+privateItem.id,{visibility:'public'},owner.token_plaintext),200);
    await status(await call('get','/v/'+privateItem.id),200);
    await status(await call('patch','/api/contents/'+privateItem.id,{visibility:'token'},owner.token_plaintext),200);
    await status(await call('get','/d/'+privateItem.id,undefined,reader.token_plaintext),404);
    // Expiry enforced on request, with no wait for hourly cleanup.
    await status(await call('patch','/api/contents/'+privateItem.id,{expires_at:Math.floor(Date.now()/1000)+2},owner.token_plaintext),200);
    await new Promise(resolve=>setTimeout(resolve,2200));
    await status(await call('get','/api/contents/'+privateItem.id+'/body'),404);
    await status(await call('get','/d/'+privateItem.id),404);
    const dashboard=await (await call('get','/api/dashboard')).json();assert(!dashboard.items.some(item=>item.id===privateItem.id));
    // Bearer legacy burn compatibility is exercised against both actual backends.
    const burn=await make({visibility:'token',ttl:604800,burn_after_read:true},owner.token_plaintext);
    await status(await call('get','/api/contents/'+burn.id+'/body?consume=0',undefined,owner.token_plaintext),200);
    await status(await call('get','/d/'+burn.id,undefined,owner.token_plaintext),200);
    await status(await call('get','/api/contents/'+burn.id+'/body',undefined,owner.token_plaintext),404);
    // Real Python CLI, not mocks. Trust only the isolated test certificate.
    const cli=async(args, credential=owner.token_plaintext)=>{
      const result=await execFile('python3',[path.join(__dirname,'../flashpaste-cli.py'),...args],{env:{...process.env,FLASHPASTE_URL:origin,FLASHPASTE_TOKEN:credential,SSL_CERT_FILE:path.join(directory,'cert.pem')},maxBuffer:2*1024*1024});
      return result.stdout;
    };
    const file=path.join(directory,'测试.md'), content='# CLI 正文\n原样下载 😀\n';fs.writeFileSync(file,content);
    assert.equal(JSON.parse(await cli(['lookup-token',reader.name])).id,reader.token_id);
    const imported=JSON.parse(await cli(['create','--file',file,'--visibility','token','--grant',reader.name]));created.push(imported.id);
    assert.equal(await cli(['get',imported.id],reader.token_plaintext),content);
    const output=path.join(directory,'download.md');await cli(['download',imported.id,'--output',output]);assert.equal(fs.readFileSync(output,'utf8'),content);
    assert(JSON.parse(await cli(['list'])).items.some(item=>item.id===imported.id));
    await cli(['delete',imported.id]);await status(await call('get','/d/'+imported.id,undefined,owner.token_plaintext),404);
    console.log('PASS core regression:',origin,'— formats/download bytes, TTL/size/input boundaries, roles, ACL replacement, expiry, Bearer burn, real Python CLI.');
  } finally {
    for(const id of created) await call('delete','/api/contents/'+id,{});
    for(const item of [reader,outsider,owner]) await call('delete','/api/tokens/'+item.token_id,{});
  }
};
