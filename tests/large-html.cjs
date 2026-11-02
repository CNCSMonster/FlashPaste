// Private local-only large HTML regression, shared by both real HTTPS backends.
const assert=require('node:assert/strict');
const fs=require('node:fs');
const path=require('node:path');
const crypto=require('node:crypto');
const {promisify}=require('node:util');
const execFile=promisify(require('node:child_process').execFile);
const digest=bytes=>crypto.createHash('sha256').update(bytes).digest('hex');
const LIMIT=10*1024*1024;

module.exports=async function largeHTML(origin,page,api,token,directory) {
  assert(['localhost','127.0.0.1'].includes(new URL(origin).hostname),'Never upload fixtures to production');
  const headers={Origin:origin}, ids=[];
  const localOnly=route=>{const url=new URL(route.request().url());return ['localhost','127.0.0.1'].includes(url.hostname)?route.continue():route.abort();};
  await page.route('**/*',localOnly);
  const call=(method,suffix,data)=>api[method](origin+suffix,{headers,...(data===undefined?{}:{data})});
  const sql=async query=>{
    if(origin.endsWith(':3443')) {
      const result=await execFile('python3',['-c','import sqlite3,json,sys;c=sqlite3.connect(sys.argv[1]);c.row_factory=sqlite3.Row;print(json.dumps([dict(r) for r in c.execute(sys.argv[2])]))',path.join(directory,'flashpaste.db'),query]);return JSON.parse(result.stdout);
    }
    const result=await execFile('wrangler',['d1','execute','flashpaste-db','--local','--persist-to',path.join(directory,'state'),'--json','--command',query],{cwd:path.join(__dirname,'..'),env:{...process.env,CI:'1',WRANGLER_SEND_METRICS:'false'},maxBuffer:1024*1024});return JSON.parse(result.stdout)[0].results;
  };
  const create=async(content,extra={})=>{
    const result=await call('post','/api/contents',{content_type:'html',content,visibility:'token',ttl:3600,...extra});
    assert.equal(result.status(),201,'Large HTML create');const item=await result.json();ids.push(item.id);return item.id;
  };
  const removeObject=async key=>{
    if(origin.endsWith(':3443')) fs.unlinkSync(path.join(directory,'flashpaste.db.bodies',key.slice(7)));
    else await execFile('wrangler',['kv','key','delete','--binding=flashpaste_bodies',key,'--local','--persist-to',path.join(directory,'state')],{cwd:path.join(__dirname,'..'),env:{...process.env,CI:'1',WRANGLER_SEND_METRICS:'false'}});
  };
  try {
    const prefix='<!doctype html><h1>Boundary</h1>';
    const exact=prefix+'x'.repeat(LIMIT-Buffer.byteLength(prefix));
    assert.equal(Buffer.byteLength(exact),LIMIT);
    const id=await create(exact);
    const stored=await sql(`SELECT length(c.content) AS inline_length,b.size_bytes,b.sha256,b.object_key FROM contents c JOIN content_bodies b ON b.content_id=c.id WHERE c.id='${id}'`);
    assert.equal(stored[0].inline_length,0);assert.equal(stored[0].size_bytes,LIMIT);assert.equal(stored[0].sha256,digest(exact));
    assert.equal((await call('get','/d/'+id)).status(),200);
    const bytes=await (await call('get','/d/'+id)).body();assert.equal(digest(bytes),digest(exact));
    const invalid=await call('post','/api/contents',{content_type:'html',content:exact+'a',visibility:'public',ttl:3600});assert.equal(invalid.status(),413);
    const envelope=await call('post','/api/contents',{content_type:'html',content:'small',visibility:'public',ttl:3600,unused:'x'.repeat(16*1024*1024)});assert.equal(envelope.status(),413);
    await call('patch','/api/contents/'+id,{visibility:'public'});
    const publicResponse=await call('get','/v/'+id);assert.equal(publicResponse.status(),200);assert((await publicResponse.text()).includes('sandbox=""'));
    assert((await publicResponse.body()).length<15000,'Large public HTML must use a small shell, not escaped body attributes');
    const publicTab=await page.context().newPage();
    try {
      await publicTab.goto(origin+'/v/'+id);await publicTab.locator('#publicFrame').waitFor();
      assert.equal(await publicTab.frameLocator('#publicFrame').locator('h1').innerText(),'Boundary');
      assert.equal(await publicTab.locator('#publicFrame').getAttribute('sandbox'),'');
    } finally {await publicTab.close();}
    await call('patch','/api/contents/'+id,{visibility:'token'});
    const anon=await require('playwright-core').request.newContext({ignoreHTTPSErrors:true});
    try {assert.equal((await anon.get(origin+'/d/'+id)).status(),404);assert.equal((await anon.get(origin+'/v/'+id)).status(),404);}finally{await anon.dispose();}
    // Real UI import preserves BOM + CRLF rather than normalizing via textarea.
    const imported=Buffer.from('\uFEFF<!doctype html>\r\n<h1>Preserve bytes</h1>\r\n<!--'+'x'.repeat(600000)+'-->');
    async function importAndCheck(buffer,name,inspectPreview=false) {
      await page.locator('#file').setInputFiles({name,mimeType:'text/html',buffer});
      await page.waitForFunction(()=>document.querySelector('#type').value==='html'&&document.querySelector('#content').value.length>500000);
      assert((await page.locator('#contentSize').innerText()).includes('10 MiB'));
      // Switching type must immediately revalidate against the smaller text limit.
      await page.locator('#type').selectOption('text');assert.equal(await page.locator('#content').evaluate(el=>el.validity.valid),false);
      await page.locator('#type').selectOption('html');assert.equal(await page.locator('#content').evaluate(el=>el.validity.valid),true);
      await page.locator('#visibility').selectOption('token');
      const response=page.waitForResponse(r=>r.url()===origin+'/api/contents'&&r.request().method()==='POST');
      await page.locator('#create').click();const created=await response;assert.equal(created.status(),201);const item=await created.json();ids.push(item.id);
      const download=await call('get','/d/'+item.id);assert.equal(digest(await download.body()),digest(buffer));
      const metadata=await (await call('get','/api/contents/'+item.id)).json();assert.equal(metadata.title,'');
      if(inspectPreview) {
        const row=page.locator('#items .item').filter({hasText:item.id.slice(0,8)});await row.getByRole('button',{name:'查看内容'}).click();await page.locator('#viewer iframe').waitFor();
        await page.frameLocator('#viewer iframe').locator('body').waitFor();
        assert((await page.frameLocator('#viewer iframe').locator('body').innerText()).trim().length>0,'Imported preview must visibly contain text');
        await page.locator('#closeView').click();
      }
      return item.id;
    }
    await importAndCheck(imported,'preserve-original.html',true);
    const sample=process.env.FLASHPASTE_HTML_SAMPLE;
    if(sample) {
      const buffer=fs.readFileSync(sample);
      assert(buffer.length>512000&&buffer.length<=LIMIT);
      await importAndCheck(buffer,path.basename(sample),true);
      console.log('PASS private local user HTML sample:',buffer.length,'bytes; real UI import/isolated preview/download SHA-256 identical.');
    }
    const burnBody='<!doctype html><h1>Burn test</h1>'+'y'.repeat(600000);
    const burn=await create(burnBody,{burn_after_read:true,ttl:604800});
    assert.equal((await call('get','/api/contents/'+burn+'/body?consume=0')).status(),200);
    assert.equal((await call('get','/api/contents/'+burn+'/body')).status(),409);
    const competitors=await Promise.all([call('post','/api/contents/'+burn+'/consume',{}),call('post','/d/'+burn,{})]);
    assert.deepEqual(competitors.map(r=>r.status()).sort(),[200,404]);
    // Object unavailable must not consume the metadata or return an empty success.
    const missing=await create(burnBody,{burn_after_read:true,ttl:604800});
    const reference=(await sql(`SELECT object_key FROM content_bodies WHERE content_id='${missing}'`))[0];
    await removeObject(reference.object_key);
    const failed=await call('post','/api/contents/'+missing+'/consume',{});assert([500,503].includes(failed.status()));
    assert.equal((await call('get','/api/contents/'+missing)).status(),200);
    assert.equal((await sql(`SELECT count(*) AS n FROM contents WHERE id='${missing}'`))[0].n,1);
    const wrongMethod=await create(burnBody);
    const methodRef=(await sql(`SELECT object_key FROM content_bodies WHERE content_id='${wrongMethod}'`))[0];
    await removeObject(methodRef.object_key);
    assert.equal((await call('post','/d/'+wrongMethod,{})).status(),400,'Reject non-burn POST before object read');
    assert.equal((await sql(`SELECT count(*) AS n FROM contents WHERE id='${wrongMethod}'`))[0].n,1);
    // Exercise the actual Python CLI with an object-backed file as well.
    const file=path.join(directory,'large-cli.html');fs.writeFileSync(file,imported);
    const cli=async args=>(await execFile('python3',[path.join(__dirname,'../flashpaste-cli.py'),...args],{env:{...process.env,FLASHPASTE_URL:origin,FLASHPASTE_TOKEN:token,SSL_CERT_FILE:path.join(directory,'cert.pem')},maxBuffer:16*1024*1024})).stdout;
    const cliItem=JSON.parse(await cli(['create','--file',file,'--visibility','token']));ids.push(cliItem.id);
    const output=path.join(directory,'large-download.html');await cli(['download',cliItem.id,'--output',output]);assert.equal(digest(fs.readFileSync(output)),digest(imported));
    console.log('PASS large HTML:',origin,'— 10 MiB/16 MiB boundaries, external refs, preview/ACL/download, raw BOM/CRLF, burn race, missing object non-consumption, real CLI.');
  } finally {
    for(const id of ids) await call('delete','/api/contents/'+id,{});
    await page.locator('#refresh').click();
    await page.unroute('**/*',localOnly);
  }
};
