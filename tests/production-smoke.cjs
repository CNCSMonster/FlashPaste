// Explicit opt-in release smoke. Only creates/rotates/deletes its own temporary
// normal Tokens and content; never rotates the supplied production Super Token.
const assert=require('node:assert/strict');
const fs=require('node:fs');
const {chromium,request}=require('playwright-core');

(async()=>{
  assert.equal(process.env.FLASHPASTE_PRODUCTION_SMOKE,'YES','Explicit production smoke opt-in required');
  const origin='https://flashpaste.example.workers.dev';
  const secret=process.env.FLASHPASTE_API_TOKEN;assert(secret,'Production credential required in environment');
  const root='/tmp/opencode';process.umask(0o077);
  const directory=fs.mkdtempSync(root+'/flashpaste-production-smoke-');
  const admin=await request.newContext({baseURL:origin,extraHTTPHeaders:{Authorization:'Bearer '+secret}});
  const anon=await request.newContext({baseURL:origin});
  let context, browser, peerContext;const tokens=[], contents=[], errors=[];
  const check=async(response,expected)=>{assert.equal(response.status(),expected,new URL(response.url()).pathname);return response;};
  const makeToken=async name=>{const result=await check(await admin.post('/api/tokens',{data:{name,level:'normal'}}),201);const token=await result.json();tokens.push(token.token_id);return token;};
  const launch=()=>chromium.launchPersistentContext(directory+'/profile',{executablePath:process.env.CHROME_PATH||'/usr/bin/google-chrome',headless:true});
  try {
    await check(await anon.get('/health'),200);
    const config=await (await anon.get('/api/auth/config')).json();assert.equal(config.cookie_login,true);assert.equal(config.debug_bearer,false);
    const empty=await anon.get('/api/auth/session');await check(empty,401);assert(!empty.headers()['set-cookie']);
    const id=Date.now().toString(36), owner=await makeToken('release-'+id+'-owner'), peer=await makeToken('release-'+id+'-peer');
    context=await launch();let page=context.pages()[0];page.on('pageerror',e=>errors.push(e.message));
    await page.goto(origin+'/');await page.locator('#login').waitFor();assert.equal(await page.locator('#rememberDevice').isChecked(),false);
    await page.locator('#tokenInput').fill(owner.token_plaintext);await page.locator('#rememberDevice').check();await page.locator('#loginBtn').click();
    await page.waitForURL(origin+'/app');await page.locator('#app').waitFor();
    const saved=(await context.cookies(origin)).find(c=>c.name==='__Host-flashpaste_session');assert(saved&&saved.httpOnly&&saved.secure&&saved.sameSite==='Strict'&&saved.expires>Date.now()/1000);
    assert.equal(await page.evaluate(()=>sessionStorage.getItem('flashpaste_token')),null);
    assert.equal(await page.evaluate(()=>localStorage.getItem('flashpaste_token')),null);
    const tab=await context.newPage();await tab.goto(origin+'/');await tab.waitForURL(origin+'/app');await tab.close();
    await context.close();context=await launch();page=context.pages()[0];page.on('pageerror',e=>errors.push(e.message));
    await page.goto(origin+'/');await page.waitForURL(origin+'/app');await page.locator('#app').waitFor();
    // Exercise actual body-only UI creation over production HTTPS/D1.
    const body='Release smoke '+id+'\n测试正文 😀';
    await page.locator('#content').fill(body);
    const created=page.waitForResponse(r=>r.url()===origin+'/api/contents'&&r.request().method()==='POST');
    await page.locator('#create').click();const response=await created;assert.equal(response.status(),201);
    const item=await response.json();contents.push(item.id);
    await page.locator('#items .item').filter({hasText:'无标题 · '}).waitFor();
    const metadata=await (await admin.get('/api/contents/'+item.id)).json();assert.equal(metadata.title,'');
    const publicPage=await anon.get('/v/'+item.id);await check(publicPage,200);assert.match(publicPage.headers()['content-type'],/text\/html/);
    const download=await anon.get('/d/'+item.id);await check(download,200);assert.equal((await download.body()).toString(),body);
    const headers={Origin:origin}, api=context.request;
    const privateResponse=await check(await api.post(origin+'/api/contents',{headers,data:{content_type:'text',content:'private test',visibility:'token',ttl:3600,allowed_tokens:[peer.token_id]}}),201);
    const privateId=(await privateResponse.json()).id;contents.push(privateId);
    await check(await anon.get('/api/contents/'+privateId+'/body'),404);await check(await anon.get('/v/'+privateId),404);
    browser=await chromium.launch({executablePath:process.env.CHROME_PATH||'/usr/bin/google-chrome',headless:true});peerContext=await browser.newContext();
    await check(await peerContext.request.post(origin+'/api/auth/login',{headers,data:{token:peer.token_plaintext,remember_device:false}}),200);
    await check(await peerContext.request.get(origin+'/api/contents/'+privateId+'/body'),200);
    await check(await api.patch(origin+'/api/contents/'+privateId,{headers,data:{allowed_tokens:[]}}),200);
    await check(await peerContext.request.get(origin+'/api/contents/'+privateId+'/body'),404);
    await check(await api.post(origin+'/api/contents',{headers:{Origin:'https://example.invalid'},data:{content_type:'text',content:'blocked',visibility:'public',ttl:3600}}),403);
    await check(await api.get(origin+'/api/dashboard',{headers:{Authorization:'Bearer invalid'}}),401);
    const burnResponse=await check(await api.post(origin+'/api/contents',{headers,data:{content_type:'text',content:'burn test',visibility:'token',ttl:604800,burn_after_read:true,allowed_tokens:[peer.token_id]}}),201);
    const burnId=(await burnResponse.json()).id;contents.push(burnId);
    await check(await peerContext.request.get(origin+'/api/contents/'+burnId+'/body'),409);
    await check(await peerContext.request.head(origin+'/api/contents/'+burnId+'/body'),405);
    await check(await peerContext.request.get(origin+'/d/'+burnId),405);
    const consumed=await Promise.all([peerContext.request.post(origin+'/api/contents/'+burnId+'/consume',{headers,data:{}}),peerContext.request.post(origin+'/d/'+burnId,{headers,data:{}})]);
    assert.deepEqual(consumed.map(r=>r.status()).sort(),[200,404]);
    const rotated=await check(await admin.post('/api/tokens/'+owner.token_id+'/rotate',{data:{}}),200);
    const newToken=(await rotated.json()).token_plaintext;
    await check(await api.get(origin+'/api/auth/session'),401);
    await check(await api.post(origin+'/api/auth/login',{headers,data:{token:owner.token_plaintext,remember_device:true}}),401);
    await check(await api.post(origin+'/api/auth/login',{headers,data:{token:newToken,remember_device:true}}),200);
    const loggedIn=(await context.cookies(origin)).find(c=>c.name==='__Host-flashpaste_session');
    await check(await api.post(origin+'/api/auth/logout',{headers,data:{}}),204);
    await check(await anon.get('/api/auth/session',{headers:{Cookie:loggedIn.name+'='+loggedIn.value}}),401);
    await check(await admin.post('/api/auth/verify',{data:{token:secret}}),200);
    assert.deepEqual(errors,[]);
    console.log('PASS production HTTPS: browser login/restart/cross-tab, untitled UI create, share/download, ACL revoke, CSRF, Bearer priority, D1 atomic burn, temporary Token rotation, logout.');
  } finally {
    const failures=[];
    // Recover any create response lost during navigation, scoped by test Token IDs.
    if(tokens.length){const response=await admin.get('/api/dashboard');if(response.ok()){for(const item of (await response.json()).items) if(tokens.includes(item.created_by)&&!contents.includes(item.id))contents.push(item.id);}}
    for(const id of contents){const response=await admin.delete('/api/contents/'+id);if(![200,404].includes(response.status()))failures.push('content cleanup');}
    for(const id of tokens){const response=await admin.delete('/api/tokens/'+id);if(![200,404].includes(response.status()))failures.push('token cleanup');}
    if(peerContext)await peerContext.close();if(browser)await browser.close();if(context)await context.close();
    await admin.dispose();await anon.dispose();assert.deepEqual(failures,[],'Temporary production data cleanup failed');
    console.log('PASS production smoke cleanup: only this run’s temporary data removed; production Super Token unchanged.');
  }
})().catch(error=>{console.error(error);process.exitCode=1});
