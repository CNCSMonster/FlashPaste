// Shared real HTTPS browser/API acceptance for Axum and local Worker/D1.
// Only localhost test servers are accepted. Never use production credentials.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {chromium,request} = require('playwright-core');

module.exports = async function exercise(origin, directory) {
  const url = new URL(origin);
  assert(url.protocol==='https:' && ['localhost','127.0.0.1'].includes(url.hostname),'Only isolated local HTTPS servers are permitted');
  const output=process.env.FLASHPASTE_UI_OUTPUT || '/tmp/opencode/flashpaste-ux';fs.mkdirSync(output,{recursive:true});
  const profile=path.join(directory,'chrome-profile');
  const launch=()=>chromium.launchPersistentContext(profile,{executablePath:process.env.CHROME_PATH || '/usr/bin/google-chrome',headless:true,ignoreHTTPSErrors:true,viewport:{width:1365,height:1000},locale:'zh-CN'});
  let context=await launch(), memberContext, client;
  const errors=[];
  const watch=page=>page.on('pageerror',error=>errors.push(error.message));
  async function login(page,token,remember) {
    await page.goto(origin+'/');await page.locator('#login').waitFor();
    await page.locator('#tokenInput').fill(token);await page.locator('#rememberDevice').setChecked(remember);
    const response=page.waitForResponse(r=>r.url()===origin+'/api/auth/login' && r.request().method()==='POST');
    await page.locator('#loginBtn').click();const result=await response;assert.equal(result.status(),200);
    await page.waitForURL(origin+'/app');await page.locator('#app').waitFor();
  }
  function cookieHeader(cookie){return cookie.name+'='+cookie.value;}
  try {
    let page=context.pages()[0];watch(page);
    await page.goto(origin+'/');
    // A fresh test database is mandatory; do not interact with another server's account.
    const superToken=(await page.locator('#tokenVal').innerText()).trim();
    await page.locator('#enterWorkspace').click();
    await page.locator('#login').waitFor();
    assert.equal(await page.locator('#rememberDevice').isChecked(),false);
    await page.screenshot({path:path.join(output,'login-desktop.png'),fullPage:true});
    await page.setViewportSize({width:390,height:844});await page.screenshot({path:path.join(output,'login-mobile.png'),fullPage:true});await page.setViewportSize({width:1365,height:1000});
    await login(page,superToken,true);
    let cookies=await context.cookies(origin), saved=cookies.find(c=>c.name==='__Host-flashpaste_session');
    assert(saved && saved.httpOnly && saved.secure && saved.sameSite==='Strict');
    assert(Math.abs(saved.expires-Date.now()/1000-30*86400)<10);
    assert.equal(await page.evaluate(()=>sessionStorage.getItem('flashpaste_token')),null);
    assert.equal(await page.evaluate(()=>localStorage.getItem('flashpaste_token')),null);
    assert.equal(await page.evaluate(()=>document.cookie.includes('__Host-flashpaste_session')),false);
    const tab=await context.newPage();watch(tab);await tab.goto(origin+'/');await tab.waitForURL(origin+'/app');await tab.locator('#app').waitFor();await tab.close();
    // Restart Chrome with its actual profile: no injected storageState or Token.
    await context.close();context=await launch();page=context.pages()[0];watch(page);
    await page.goto(origin+'/');await page.waitForURL(origin+'/app');await page.locator('#app').waitFor();
    assert.equal(await page.evaluate(()=>sessionStorage.getItem('flashpaste_token')),null);
    await page.route('**/api/auth/session',route=>route.fulfill({status:503,json:{error:'test outage'}}));
    await page.goto(origin+'/');await page.locator('#restoreError').waitFor();
    assert.equal(await page.locator('#app').isVisible(),false);
    assert((await context.cookies(origin)).some(c=>c.name==='__Host-flashpaste_session'));
    await page.unroute('**/api/auth/session');await page.locator('#retrySession').click();await page.waitForURL(origin+'/app');await page.locator('#app').waitFor();
    const headers={Origin:origin};
    const api=context.request;
    await require('./core-regression.cjs')(origin,api,superToken,directory);
    await require('./large-html.cjs')(origin,page,api,superToken,directory);
    const created=await api.post(origin+'/api/tokens',{headers,data:{name:'session-member',level:'normal'}});assert.equal(created.status(),201);
    const member=await created.json(), memberToken=member.token_plaintext, memberId=member.token_id;
    const privateResponse=await api.post(origin+'/api/contents',{headers,data:{title:null,content_type:'text',content:'私有会话验收正文',visibility:'token',ttl:3600,allowed_tokens:[memberId]}});
    assert.equal(privateResponse.status(),201);const privateId=(await privateResponse.json()).id;
    const burnResponse=await api.post(origin+'/api/contents',{headers,data:{title:'浏览器单次读取验收',content_type:'text',content:'single-use-test',visibility:'token',ttl:604800,burn_after_read:true,allowed_tokens:[memberId]}});
    assert.equal(burnResponse.status(),201);const burnId=(await burnResponse.json()).id;
    await page.locator('#refresh').click();await page.locator('#items .item').filter({hasText:'无标题 · '}).waitFor();
    await page.screenshot({path:path.join(output,'cookie-workspace.png'),fullPage:true});
    for(const source of ['null','https://evil.test',origin+':444']) {
      const denied=await api.post(origin+'/api/contents',{headers:{Origin:source},data:{content_type:'text',content:'blocked',visibility:'public',ttl:3600}});assert.equal(denied.status(),403);
    }
    assert.equal((await api.post(origin+'/api/auth/login',{data:{token:superToken,remember_device:true}})).status(),403);
    assert.equal((await api.get(origin+'/api/dashboard',{headers:{Authorization:'Bearer invalid'}})).status(),401);
    assert.equal((await api.get(origin+'/api/dashboard',{headers:{Authorization:'Basic invalid'}})).status(),401);
    assert.equal((await api.get(origin+'/v/'+burnId)).status(),404);
    memberContext=await context.browser().newContext({ignoreHTTPSErrors:true,viewport:{width:390,height:844}});
    const memberPage=await memberContext.newPage();watch(memberPage);await login(memberPage,memberToken,false);
    const memberCookie=(await memberContext.cookies(origin)).find(c=>c.name==='__Host-flashpaste_session');assert(memberCookie && memberCookie.expires===-1);
    assert.equal(await memberPage.locator('#tokensCard').isVisible(),false);
    await memberPage.screenshot({path:path.join(output,'member-mobile.png'),fullPage:true});
    await memberPage.setViewportSize({width:1365,height:1000});await memberPage.screenshot({path:path.join(output,'member-desktop.png'),fullPage:true});await memberPage.setViewportSize({width:390,height:844});
    assert.equal((await memberContext.request.get(origin+'/api/contents/'+privateId+'/body')).status(),200);
    const blocked=await memberContext.request.get(origin+'/api/contents/'+burnId+'/body');assert.equal(blocked.status(),409);assert.equal((await blocked.json()).code,'CONSUME_REQUIRES_POST');
    assert.equal((await memberContext.request.head(origin+'/api/contents/'+burnId+'/body')).status(),405);
    const blockedDownload=await memberContext.request.get(origin+'/d/'+burnId);assert.equal(blockedDownload.status(),405);assert.equal(blockedDownload.headers().allow,'POST');
    assert.equal((await api.get(origin+'/api/contents/'+burnId+'/body?consume=0')).status(),200);
    const row=memberPage.locator('#items .item').filter({hasText:'[阅后即焚]'});
    await row.getByRole('button',{name:'读取并焚毁'}).click();await memberPage.locator('#dialogCancel').click();
    assert.equal((await memberContext.request.get(origin+'/api/contents/'+burnId+'/body')).status(),409);
    await row.getByRole('button',{name:'读取并焚毁'}).click();
    const consumed=memberPage.waitForResponse(r=>r.url()===origin+'/api/contents/'+burnId+'/consume');
    await memberPage.locator('#dialogSubmit').click();assert.equal((await consumed).request().method(),'POST');
    await memberPage.locator('#viewer').waitFor();assert.equal(await memberPage.locator('#rendered pre').innerText(),'single-use-test');
    assert.equal((await memberContext.request.get(origin+'/api/contents/'+burnId+'/body')).status(),404);
    const competingBurn=await (await api.post(origin+'/api/contents',{headers,data:{content_type:'text',content:'competing-test',visibility:'token',ttl:604800,burn_after_read:true,allowed_tokens:[memberId]}})).json();
    const competing=await Promise.all([memberContext.request.post(origin+'/api/contents/'+competingBurn.id+'/consume',{headers,data:{}}),memberContext.request.post(origin+'/d/'+competingBurn.id,{headers,data:{}})]);
    assert.equal(competing.filter(response=>response.status()===200).length,1);assert.equal(competing.filter(response=>response.status()===404).length,1);
    const rotated=await api.post(origin+'/api/tokens/'+memberId+'/rotate',{headers,data:{}});assert.equal(rotated.status(),200);
    const newMemberToken=(await rotated.json()).token_plaintext;
    assert.equal((await memberContext.request.get(origin+'/api/auth/session')).status(),401);
    client=await request.newContext({ignoreHTTPSErrors:true});
    assert.equal((await client.get(origin+'/api/auth/session',{headers:{Cookie:cookieHeader(memberCookie)}})).status(),401);
    assert.equal((await client.post(origin+'/api/auth/login',{headers,data:{token:memberToken,remember_device:true}})).status(),401);
    const renewed=await client.post(origin+'/api/auth/login',{headers,data:{token:newMemberToken,remember_device:true}});assert.equal(renewed.status(),200);
    await api.delete(origin+'/api/tokens/'+memberId,{headers,data:{}});
    assert.equal((await client.get(origin+'/api/auth/session')).status(),401);
    // Old-credential login racing a rotation may complete before it, never after it with a valid new epoch.
    const raceToken=await (await api.post(origin+'/api/tokens',{headers,data:{name:'race-member',level:'normal'}})).json();let raw=raceToken.token_plaintext;
    for(let i=0;i<8;i++) {
      const [signed,changed]=await Promise.all([client.post(origin+'/api/auth/login',{headers,data:{token:raw,remember_device:true}}),api.post(origin+'/api/tokens/'+raceToken.token_id+'/rotate',{headers,data:{}})]);
      assert.equal(changed.status(),200);raw=(await changed.json()).token_plaintext;
      if(signed.status()===200) {const credential=signed.headers()['set-cookie'].split(';')[0];assert.equal((await client.get(origin+'/api/auth/session',{headers:{Cookie:credential}})).status(),401);}else assert.equal(signed.status(),401);
    }
    // Self rotation must expose the new credential before asking the user to reauthenticate.
    // A delayed background 401 must not redirect away from the one-time secret.
    let releaseRefresh;
    const heldRefresh=new Promise(resolve=>releaseRefresh=resolve);
    await page.route('**/api/dashboard',async route=>{await heldRefresh;await route.fulfill({status:401,json:{error:'test stale session'}})});
    await page.locator('#refresh').click();
    await page.route('**/api/tokens/*/rotate',async route=>{const actual=await route.fetch();releaseRefresh();await page.locator('#listError').waitFor();await route.fulfill({response:actual})});
    await page.locator('#tokensCard > summary').click();await page.locator('#refreshTokens').click();await page.locator('#tokens .item').filter({hasText:'当前登录'}).locator('details.row-menu summary').click();await page.locator('#tokens .item').filter({hasText:'当前登录'}).getByRole('button',{name:'轮换',exact:true}).click();
    await page.locator('#dialogSubmit').click();await page.locator('#tokenResult .secret').waitFor();
    await page.unroute('**/api/dashboard');await page.unroute('**/api/tokens/*/rotate');
    const newSuperToken=await page.locator('#tokenResult .secret').innerText();assert(newSuperToken && newSuperToken!==superToken);
    assert(await page.locator('#logout').isDisabled());assert(await page.getByRole('button',{name:'复制 Token',exact:true}).isEnabled());
    await page.getByRole('button',{name:'已保存，重新登录'}).click();await page.waitForURL(origin+'/');await page.locator('#login').waitFor();
    await login(page,newSuperToken,true);
    saved=(await context.cookies(origin)).find(c=>c.name==='__Host-flashpaste_session');
    await page.route('**/api/auth/logout',route=>route.fulfill({status:503,json:{error:'test outage'}}));
    await page.locator('#logout').click();await page.locator('#toast.error').waitFor();assert(await page.locator('#app').isVisible());
    assert.equal((await api.get(origin+'/api/auth/session')).status(),200);
    await page.unroute('**/api/auth/logout');await page.locator('#logout').click();await page.waitForURL(origin+'/');await page.locator('#login').waitFor();
    assert.equal((await context.cookies(origin)).filter(c=>c.name==='__Host-flashpaste_session').length,0);
    assert.equal((await client.get(origin+'/api/auth/session',{headers:{Cookie:cookieHeader(saved)}})).status(),401);
    assert.equal(errors.length,0,errors.join('\n'));
    console.log('PASS HTTPS session acceptance:',origin,'— real Cookie attributes, browser restart, cross-tab restore, no Web Storage token, CSRF, burn POST, rotation/races, self rotation and logout.');
  } finally {if(client)await client.dispose();if(memberContext)await memberContext.close();await context.close();}
};
