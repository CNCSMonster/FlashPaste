// Real Axum/SQLite UI integration in a fresh temporary directory.
// cargo build --no-default-features --features local
// NODE_PATH=<临时目录>/flashpaste-ux-tools/node_modules node tests/ui-local.cjs
// Port 3000 must be free. Does not touch flashpaste.db in the project directory.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os=require('node:os');
const path = require('node:path');
const net = require('node:net');
const { spawn } = require('node:child_process');
const { chromium } = require('playwright-core');

(async () => {
  await new Promise((resolve,reject) => {
    const probe = net.createServer(); probe.once('error', () => reject(Error('Port 3000 is occupied; stop the test rather than touching another server.')));
    probe.listen(3000,'0.0.0.0', () => probe.close(resolve));
  });
  const directory = fs.mkdtempSync(path.join(os.tmpdir(),'flashpaste-ui-local-'));
  const output = process.env.FLASHPASTE_UI_OUTPUT || path.join(os.tmpdir(),'flashpaste-ux');
  fs.mkdirSync(output,{recursive:true});
  const server = spawn(path.join(__dirname,'../target/debug/flashpaste'), [], {cwd:directory,stdio:['ignore','pipe','pipe']});
  let log = ''; server.stdout.on('data',data => log+=data); server.stderr.on('data',data => log+=data);
  let browser;
  try {
    for(let attempt=0;attempt<100;attempt++) {
      if(server.exitCode!==null) throw Error('Local server exited: '+log);
      try { if((await fetch('http://127.0.0.1:3000/health')).ok) break; } catch {}
      await new Promise(resolve => setTimeout(resolve,50));
    }
    browser = await chromium.launch({executablePath:process.env.CHROME_PATH || '/usr/bin/google-chrome',headless:true});
    const context = await browser.newContext({viewport:{width:1365,height:1000},locale:'zh-CN'});
    const page = await context.newPage(), errors = [];
    page.on('pageerror',error => errors.push(error.message));
    await page.goto('http://127.0.0.1:3000/');
    const superToken = (await page.locator('#tokenVal').innerText()).trim();
    await page.locator('#tokenVal').evaluate(node => node.textContent='演示凭证 · 仅初始化时显示一次，请安全保存');
    await page.screenshot({path:path.join(output,'bootstrap.png'),fullPage:true});
    await page.locator('#tokenVal').evaluate((node,token) => node.textContent=token,superToken);
    await page.getByRole('button',{name:'已保存，前往登录'}).click();
    await page.locator('#tokenInput').fill(superToken);
    await page.locator('#loginBtn').click();
    await page.locator('#tokensCard > summary').click();
    await page.locator('#tokens .item').waitFor();
    await page.locator('#tokensCard > summary').click();
    assert.equal(page.url(),'http://127.0.0.1:3000/app');
    assert.match(await page.locator('#items').innerText(),/还没有有效内容/);
    await page.screenshot({path:path.join(output,'empty-workspace.png'),fullPage:true});
    await page.locator('#tokensCard > summary').click();
    await page.locator('#tokenName').fill('e2e-member');
    await page.locator('#createToken').click();
    await page.locator('#tokenResult .secret').waitFor();
    const memberToken = await page.locator('#tokenResult .secret').innerText();
    await page.getByRole('button',{name:'已保存，隐藏凭证'}).click();
    // Default flow: paste content and create, without opening the title field.
    assert.equal(await page.locator('#title').isVisible(),true);
    assert.equal(await page.locator('#title').inputValue(),'');
    await page.locator('#content').fill('不要把这段正文自动转成标题');
    await page.locator('#create').click();
    const unnamedRow = page.locator('#items .item').filter({hasText:'无标题 · '});
    await unnamedRow.waitFor();
    assert.equal(await page.locator('#items').innerText().then(text => text.includes('不要把这段正文')),false);
    // Verify actual storage, not only a frontend placeholder.
    const untitledData = await (await fetch('http://127.0.0.1:3000/api/dashboard',{headers:{Authorization:'Bearer '+superToken}})).json();
    assert.equal(untitledData.items[0].title,'');
    const { execFileSync } = require('node:child_process');
    assert.equal(execFileSync('python3',['-c','import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); print(c.execute("SELECT title IS NULL FROM contents WHERE id=?", (sys.argv[2],)).fetchone()[0])',path.join(directory,'flashpaste.db'),untitledData.items[0].id],{encoding:'utf8'}).trim(),'1');
    const anonymousPage = await browser.newPage({viewport:{width:1365,height:1000}});
    const publicResponse = await anonymousPage.goto('http://127.0.0.1:3000/v/'+untitledData.items[0].id);
    assert.match(publicResponse.headers()['content-type'],/text\/html/);
    await anonymousPage.screenshot({path:path.join(output,'public-share.png'),fullPage:true});
    const missingResponse = await anonymousPage.goto('http://127.0.0.1:3000/v/missing-content');
    assert.equal(missingResponse.status(),404);
    await anonymousPage.screenshot({path:path.join(output,'unavailable.png'),fullPage:true});
    await anonymousPage.close();
    await page.locator('#title').fill('真实 API 私有分享');
    await page.locator('#content').fill('真实 SQLite 内容，测试后不保留在项目目录。');
    await page.locator('#visibility').selectOption('token');
    await page.locator('#grants').fill('e2e-member');
    await page.locator('#create').click();
    const privateRow = page.locator('#items .item').filter({hasText:'真实 API 私有分享'});
    await privateRow.waitFor();
    const data = await (await fetch('http://127.0.0.1:3000/api/dashboard',{headers:{Authorization:'Bearer '+superToken}})).json();
    const privateItem = data.items.find(item => item.title === '真实 API 私有分享');
    const expiry = privateItem.expires_at;
    await privateRow.locator('details.row-menu summary').click();
    await privateRow.getByRole('button',{name:'管理',exact:true}).click();
    await page.locator('#dialogSubmit').click();
    await page.locator('#appDialog').waitFor({state:'hidden'});
    const metadata = await (await fetch('http://127.0.0.1:3000/api/contents/'+privateItem.id,{headers:{Authorization:'Bearer '+superToken}})).json();
    assert.equal(metadata.expires_at,expiry);
    await page.locator('#title').fill('真实 API 阅后即焚');
    await page.locator('#content').fill('读取一次');
    await page.locator('#ttl').selectOption('burn');
    await page.locator('#create').click();
    const burnRow = page.locator('#items .item').filter({hasText:'真实 API 阅后即焚'});
    await burnRow.getByRole('button',{name:'免焚预览'}).click();
    await page.locator('#viewer').waitFor();
    await page.locator('#closeView').click();
    await burnRow.waitFor();
    await page.locator('#logout').click();
    await page.waitForURL('http://127.0.0.1:3000/');
    await page.locator('#tokenInput').fill(memberToken);
    await page.locator('#loginBtn').click();
    await page.locator('#items .item').filter({hasText:'真实 API 私有分享'}).waitFor();
    assert.equal(await page.locator('#tokensCard').isVisible(),false);
    // Non-owner burn metadata intentionally hides the title.
    const memberBurn = page.locator('#items .item').filter({hasText:'[阅后即焚]'});
    await memberBurn.getByRole('button',{name:'读取并焚毁'}).click();
    await page.locator('#dialogSubmit').click();
    await page.locator('#viewer').waitFor();
    assert.equal(await page.locator('#rendered pre').innerText(),'读取一次');
    await memberBurn.waitFor({state:'detached'});
    await page.locator('#closeView').click();
    await page.locator('#items .item').filter({hasText:'真实 API 私有分享'}).getByRole('button',{name:'查看内容'}).click();
    await page.locator('#viewer').waitFor();
    assert.match(await page.locator('#rendered').innerText(),/真实 SQLite/);
    assert.equal(errors.length,0,errors.join('\n'));
    await context.close();
    console.log('PASS: real local bootstrap/login, untitled creation and NULL storage, token creation, private ACL, unchanged expiry, owner burn preview, member burn confirmation and consumption.');
    fs.copyFileSync(path.join(__dirname,'ui-gallery.html'),path.join(output,'index.html'));
    console.log('Isolated test database:',directory);
  } finally { if(browser) await browser.close(); server.kill('SIGTERM'); }
})().catch(error => {console.error(error);process.exitCode=1;});
