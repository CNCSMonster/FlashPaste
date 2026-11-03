// Run with playwright-core installed outside the repository:
// NODE_PATH=<临时目录>/flashpaste-ux-tools/node_modules node tests/ui-smoke.cjs
// Uses only synthetic API fixtures. No production credentials or data.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os=require('node:os');
const path = require('node:path');
const { chromium } = require('playwright-core');

const html = fs.readFileSync(path.join(__dirname, '../static/app.html'), 'utf8');
const output = process.env.FLASHPASTE_UI_OUTPUT || path.join(os.tmpdir(),'flashpaste-ux');
fs.mkdirSync(output, { recursive: true });
const csp = "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; frame-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'";
const now = Math.floor(Date.now() / 1000);
const initialItems = [
  { id:'private', title:'本周项目进展与技术方案', content_type:'markdown', visibility:'token', created_by:'super-id', created_at:now-30, expires_at:now+7207 },
  { id:'public', title:'产品发布页 · HTML 预览', content_type:'html', visibility:'public', created_by:'super-id', created_at:now-30, expires_at:now+18000 },
  { id:'burn', title:'一次性协作资料', content_type:'text', visibility:'token', burn_after_read:1, created_by:'super-id', created_at:now-30, expires_at:now+3600 },
];

async function fixture(browser, width = 1365, role = 'super') {
  const context = await browser.newContext({ viewport:{width,height:1000}, locale:'zh-CN', timezoneId:'Asia/Shanghai' });
  const page = await context.newPage(), errors = [], calls = [];
  const state = { items:structuredClone(initialItems), verifyStatus:200, dashboardStatus:200, patchStatus:200, deleteStatus:200, delayDelete:false, gate:null };
  page.on('pageerror', error => errors.push(error.message));
  page.on('dialog', async dialog => { errors.push('Unexpected native dialog: ' + dialog.message()); await dialog.dismiss(); });
  await page.route('http://flashpaste.test/**', async route => {
    const request = route.request(), url = new URL(request.url()), method = request.method();
    calls.push({ path:url.pathname, search:url.search, method, body:request.postDataJSON(), auth:request.headers().authorization });
    if (url.pathname === '/' || url.pathname === '/app') return route.fulfill({contentType:'text/html',body:html,headers:{'Content-Security-Policy':csp}});
    if (url.pathname === '/favicon.ico') return route.fulfill({status:204});
    let data = {}, status = 200;
    if (url.pathname === '/api/auth/config') data = {cookie_login:false,debug_bearer:true};
    else if (url.pathname === '/api/auth/verify') { data = { token_id:role === 'super' ? 'super-id' : 'member-id', name:'产品协作空间', level:role }; status = state.verifyStatus; }
    else if (url.pathname === '/api/dashboard') { data = {items:state.items}; status = state.dashboardStatus; }
    else if (url.pathname === '/api/tokens' && method === 'GET') data = {tokens:[{id:'super-id',name:'我的工作台',level:'super'},{id:'tok_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',name:'设计协作组',level:'normal'}]};
    else if (url.pathname === '/api/tokens' && method === 'POST') { data = { token_plaintext:'test-only-new-secret', level:request.postDataJSON().level }; status = 201; }
    else if (url.pathname.endsWith('/rotate')) data = {token_plaintext:'test-only-rotated-secret'};
    else if (url.pathname === '/api/contents' && method === 'POST') { data = {id:'new-content',url:request.postDataJSON().visibility === 'public' ? '/v/new-content' : null}; status = 201; }
    else if (url.pathname.endsWith('/body')) { const id = url.pathname.split('/')[3]; data = {content_type:id === 'public' ? 'html' : 'text',content:id === 'public' ? '<h1>静态分享</h1><script>parent.postMessage("bad-script", "*")<\/script>' : '预览测试内容',visibility:id === 'public' ? 'public' : 'token'}; if(id === 'burn' && url.searchParams.get('consume') !== '0') state.items = state.items.filter(item => item.id !== id); }
    else if (url.pathname.startsWith('/api/contents/') && method === 'GET') { data = {...state.items.find(item => item.id === url.pathname.split('/')[3]), allowed_tokens:['tok_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa']}; }
    else if (method === 'PATCH') status = state.patchStatus;
    else if (method === 'DELETE') { if(state.delayDelete) await new Promise(resolve => state.gate = resolve); status = state.deleteStatus; if(status === 200) state.items = state.items.filter(item => item.id !== url.pathname.split('/')[3]); }
    else { status = 404; }
    return route.fulfill({status,json:data});
  });
  await page.goto('http://flashpaste.test/');
  await page.locator('#login').waitFor();
  return {context,page,state,calls,errors};
}
async function login(page) { await page.locator('#tokenInput').fill('test-only-login-secret'); await page.locator('#tokenInput').press('Enter'); await page.locator('#items .item').first().waitFor(); }
async function openRowMenu(row) { const menu = row.locator('details.row-menu'); if (await menu.count() && !(await menu.evaluate(node => node.open))) await menu.locator('summary').click(); }
async function openTokens(page) { const card = page.locator('#tokensCard'); if (!(await card.evaluate(node => node.open))) await page.locator('#tokensCard > summary').click(); }
async function waitFor(check) { for (let i=0;i<100;i++) { if(await check()) return; await new Promise(resolve => setTimeout(resolve,20)); } throw Error('Timed out'); }
async function noOverflow(page) { assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'Page must not overflow horizontally'); }

(async () => {
  const browser = await chromium.launch({executablePath:process.env.CHROME_PATH || '/usr/bin/google-chrome',headless:true});
  try {
    const {page,context,state,calls,errors} = await fixture(browser);
    await page.screenshot({path:path.join(output,'login-desktop.png'),fullPage:true});
    assert.equal(calls.filter(call => call.path.startsWith('/api/') && call.path !== '/api/auth/config').length,0,'Homepage never loads authenticated content');
    await page.goto('http://flashpaste.test/app');
    await page.waitForURL('http://flashpaste.test/');
    assert.equal(await page.locator('#app').isVisible(),false,'Anonymous direct navigation is redirected to login');
    state.verifyStatus = 401;
    await page.locator('#tokenInput').fill('invalid-test-token');
    await page.locator('#loginBtn').click();
    await waitFor(() => page.locator('#loginError').innerText().then(text => text.includes('Token 无效')));
    await page.screenshot({path:path.join(output,'login-error.png'),fullPage:true});
    assert.equal(calls.filter(call => call.path === '/api/dashboard').length,0);
    state.verifyStatus = 200;
    await login(page);
    assert.equal(page.url(),'http://flashpaste.test/app');
    await waitFor(() => page.locator('#tokens .item').count().then(count => count > 0));
    assert.equal(await page.locator('#tokenLevel option').count(),2);
    assert.equal(await page.locator('#title').isVisible(),true);
    assert.equal(await page.locator('#title').inputValue(),'');
    assert.equal(await page.locator('#grantsField').isVisible(),false);
    await noOverflow(page);
    await page.screenshot({path:path.join(output,'desktop.png'),fullPage:true});
    await page.locator('#search').fill('不存在');
    assert.match(await page.locator('#items').innerText(), /没有匹配/);
    await page.getByRole('button',{name:'清除筛选',exact:true}).click();
    assert.equal(await page.locator('#items .item').count(),3);
    const privateRow = page.locator('#items .item').filter({hasText:'本周项目进展'});
    await openRowMenu(privateRow);
    await privateRow.getByRole('button',{name:'管理',exact:true}).click();
    const originalExpiry = await page.locator('[name=expiry]').inputValue();
    await page.screenshot({path:path.join(output,'manage-dialog.png')});
    state.patchStatus = 500;
    await page.locator('#dialogSubmit').click();
    await waitFor(() => page.locator('#dialogError').innerText().then(text => text.includes('暂时不可用')));
    assert(await page.locator('#appDialog').isVisible());
    assert.equal(await page.locator('[name=expiry]').inputValue(),originalExpiry);
    state.patchStatus = 200;
    await page.locator('#dialogSubmit').click();
    await page.locator('#appDialog').waitFor({state:'hidden'});
    assert.equal(calls.filter(call => call.method === 'PATCH').at(-1).body.expires_at,initialItems[0].expires_at,'No accidental rounding when changing only ACL');
    await openRowMenu(privateRow);
    await privateRow.getByRole('button',{name:'删除',exact:true}).click();
    assert.equal(await page.evaluate(() => document.activeElement.id),'dialogCancel');
    let rect = await page.locator('#appDialog').boundingBox();
    assert(Math.abs(rect.y+rect.height/2-500)<3,'Dialog is vertically centered');
    await page.mouse.click(rect.x+5,rect.y+5);
    assert(await page.locator('#appDialog').isVisible(),'Dialog padding must not act as backdrop');
    await page.screenshot({path:path.join(output,'dialog-desktop.png'),fullPage:false});
    await page.keyboard.press('Escape');
    assert.equal(calls.filter(call => call.method === 'DELETE').length,0);
    await openRowMenu(privateRow);
    await privateRow.getByRole('button',{name:'删除',exact:true}).click();
    await page.mouse.click(4,4);
    await page.locator('#appDialog').waitFor({state:'hidden'});
    await openRowMenu(privateRow);
    await privateRow.getByRole('button',{name:'删除',exact:true}).click();
    state.delayDelete = true;
    await page.locator('#dialogSubmit').click();
    await waitFor(() => state.gate !== null);
    assert(await page.locator('#dialogSubmit').isDisabled());
    await page.keyboard.press('Escape');
    assert(await page.locator('#appDialog').isVisible(),'Cannot dismiss a pending mutation');
    state.gate(); state.delayDelete = false;
    await page.locator('#appDialog').waitFor({state:'hidden'});
    assert.equal(calls.filter(call => call.method === 'DELETE').length,1);
    await page.locator('#items .item').filter({hasText:'一次性协作资料'}).getByRole('button',{name:'免焚预览'}).click();
    await page.locator('#viewer').waitFor();
    await page.locator('#viewer').screenshot({path:path.join(output,'viewer-text.png')});
    assert.equal(calls.filter(call => call.path.endsWith('/body')).at(-1).search,'?consume=0');
    await page.locator('#closeView').click();
    await page.evaluate(() => { window.badScript=false; addEventListener('message', event => {if(event.data==='bad-script') window.badScript=true;}); });
    await page.locator('#items .item').filter({hasText:'产品发布页'}).getByRole('button',{name:'查看内容'}).click();
    assert.equal(await page.locator('#rendered iframe').getAttribute('sandbox'),'');
    assert.match(await page.locator('#rendered iframe').getAttribute('srcdoc'),/img-src data: blob:/);
    await page.frameLocator('#rendered iframe').getByRole('heading',{name:'静态分享'}).waitFor();
    await page.locator('#viewer').screenshot({path:path.join(output,'viewer-html.png')});
    assert.equal(await page.evaluate(() => window.badScript),false);
    await page.locator('#closeView').click();
    assert.equal(await page.locator('#rendered iframe').count(),0);
    await page.locator('#ttl').selectOption('burn');
    assert.equal(await page.locator('#visibility').inputValue(),'token');
    assert(await page.locator('#visibility option[value=public]').evaluate(option => option.disabled));
    await page.locator('#ttl').selectOption('3600');
    await page.locator('#visibility').selectOption('public');
    await page.locator('#content').fill('测试分享内容');
    await page.locator('#create').click();
    await page.locator('#createResult a').waitFor();
    assert.equal(await page.locator('#createResult a').getAttribute('href'),'http://flashpaste.test/v/new-content');
    assert.equal(await page.locator('#content').inputValue(),'');
    assert.equal(calls.filter(call => call.path === '/api/contents' && call.method === 'POST').at(-1).body.title,null);
    await page.locator('#file').setInputFiles({name:'不该用作标题.md',mimeType:'text/markdown',buffer:Buffer.from('# 文件正文')});
    await waitFor(() => page.locator('#content').inputValue().then(text => text === '# 文件正文'));
    assert.equal(await page.locator('#title').inputValue(),'');
    await page.locator('#create').click();
    await waitFor(() => calls.filter(call => call.path === '/api/contents' && call.method === 'POST').length === 2);
    assert.equal(calls.filter(call => call.path === '/api/contents' && call.method === 'POST').at(-1).body.title,null);
    await page.locator('#title').fill('  显式标题  ');
    await page.locator('#content').fill('正文');
    await page.locator('#create').click();
    await waitFor(() => calls.filter(call => call.path === '/api/contents' && call.method === 'POST').length === 3);
    assert.equal(calls.filter(call => call.path === '/api/contents' && call.method === 'POST').at(-1).body.title,'显式标题');
    await waitFor(() => page.locator('#title').inputValue().then(value => value === ''));
    await openTokens(page);
    await page.locator('#tokenName').fill('测试成员');
    await page.locator('#createToken').click();
    await page.locator('#tokenResult .secret').waitFor();
    await page.locator('#tokensCard').screenshot({path:path.join(output,'token-created.png')});
    assert.equal(await page.locator('#tokenResult .secret').innerText(),'test-only-new-secret');
    await page.getByRole('button',{name:'已保存，隐藏凭证'}).click();
    assert.equal(await page.locator('#tokenResult .secret').count(),0);
    assert(await page.getByRole('button',{name:'最后一个 Super',includeHidden:true}).isDisabled());
    // Self rotation updates only session storage, never the URL.
    const selfRow = page.locator('#tokens .item').filter({hasText:'我的工作台'});
    await openRowMenu(selfRow);
    await selfRow.getByRole('button',{name:'轮换',exact:true}).click();
    await page.locator('#dialogSubmit').click();
    await page.locator('#appDialog').waitFor({state:'hidden'});
    assert.equal(await page.evaluate(() => sessionStorage.getItem('flashpaste_token')),'test-only-rotated-secret');
    assert.equal(page.url(),'http://flashpaste.test/app');
    state.dashboardStatus = 500;
    await page.locator('#refresh').click();
    await page.locator('#listError').waitFor();
    await page.locator('#board').screenshot({path:path.join(output,'list-error.png')});
    assert.equal(await page.evaluate(() => sessionStorage.getItem('flashpaste_token')),'test-only-rotated-secret');
    assert(await page.locator('#app').isVisible());
    // Returning to the homepage always shows the login entry, even with a saved session.
    await page.goto('http://flashpaste.test/');
    assert(await page.locator('#login').isVisible());
    assert.equal(await page.locator('#app').isVisible(),false);
    assert.equal(await page.evaluate(() => sessionStorage.getItem('flashpaste_token')),'test-only-rotated-secret');
    state.verifyStatus = 500;
    await page.goto('http://flashpaste.test/app');
    await waitFor(() => page.locator('#restoreError').innerText().then(text => text.includes('暂时不可用')));
    assert.equal(await page.locator('#app').isVisible(),false);
    assert.equal(await page.evaluate(() => sessionStorage.getItem('flashpaste_token')),'test-only-rotated-secret');
    await page.screenshot({path:path.join(output,'session-retry.png'),fullPage:true});
    state.verifyStatus = 401;
    await page.locator('#retrySession').click();
    await page.waitForURL('http://flashpaste.test/');
    assert.equal(await page.evaluate(() => sessionStorage.getItem('flashpaste_token')),null);
    assert.equal(await page.locator('#app').isVisible(),false);
    assert.equal(errors.length,0,errors.join('\n'));
    await context.close();

    const mobile = await fixture(browser,390);
    await mobile.page.screenshot({path:path.join(output,'login-mobile.png'),fullPage:true});
    await login(mobile.page);
    await waitFor(() => mobile.page.locator('#tokens .item').count().then(count => count > 0));
    await noOverflow(mobile.page);
    await mobile.page.screenshot({path:path.join(output,'mobile.png'),fullPage:true});
    await openTokens(mobile.page);
    const mobileTokenRow = mobile.page.locator('#tokens .item').filter({hasText:'设计协作组'});
    await openRowMenu(mobileTokenRow);
    await mobileTokenRow.getByRole('button',{name:'撤销',exact:true}).click();
    rect = await mobile.page.locator('#appDialog').boundingBox();
    assert(rect.width<=358 && Math.abs(rect.y+rect.height/2-500)<3);
    await mobile.page.screenshot({path:path.join(output,'dialog-mobile.png')});
    await mobile.page.keyboard.press('Escape');
    await mobile.page.setViewportSize({width:320,height:640});
    await noOverflow(mobile.page);
    assert.equal(mobile.errors.length,0,mobile.errors.join('\n'));
    await mobile.context.close();

    const member = await fixture(browser,390,'normal');
    await login(member.page);
    await member.page.screenshot({path:path.join(output,'member-mobile.png'),fullPage:true});
    await member.page.setViewportSize({width:1365,height:1000});
    await member.page.screenshot({path:path.join(output,'member-desktop.png'),fullPage:true});
    assert.equal(await member.page.locator('#tokensCard').isVisible(),false);
    const burn = member.page.locator('#items .item').filter({hasText:'一次性协作资料'});
    await burn.getByRole('button',{name:'读取并焚毁'}).click();
    await member.page.screenshot({path:path.join(output,'burn-confirm.png')});
    assert.equal(member.calls.filter(call => call.path.endsWith('/body')).length,0);
    await member.page.locator('#dialogCancel').click();
    assert.equal(member.calls.filter(call => call.path.endsWith('/body')).length,0);
    await burn.getByRole('button',{name:'读取并焚毁'}).click();
    await member.page.locator('#dialogSubmit').click();
    await member.page.locator('#viewer').waitFor();
    await waitFor(() => member.page.locator('#items .item').count().then(count => count===2));
    assert.equal(member.calls.filter(call => call.path.endsWith('/body')).length,1);
    assert.match(await member.page.locator('#viewerHelp').innerText(),/服务端焚毁/);
    await member.page.locator('#viewer').screenshot({path:path.join(output,'burned-view.png')});
    assert.equal(member.errors.length,0,member.errors.join('\n'));
    await member.context.close();
    console.log('PASS: desktop/mobile layout, modal focus/cancel/pending/error, expiry preservation, burn safety, HTML sandbox, search, create, credential lifecycle, network failure.');
    fs.copyFileSync(path.join(__dirname,'ui-gallery.html'),path.join(output,'index.html'));
    console.log('Screenshots and gallery:',output);
  } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode=1; });
