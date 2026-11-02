# FlashPaste

轻量内容分享服务：分享文本、Markdown 或单文件 HTML，支持公开链接、Token ACL、定时过期和阅后即焚。

## 部署方式

- Cloudflare Workers + D1：当前部署路径，Rust Worker，配置见 `wrangler.toml`。
- 局域网 / 本地：Axum + SQLite，运行 `cargo run`，监听 `0.0.0.0:3000`。

当前 Worker 地址：<https://flashpaste.example.workers.dev>。逐次部署版本与验收结论以 `ops/` 台账为准，本文不重复记录版本号。生产 Cron 实际触发与容量／负载仍未验收。

### 部署链路现状（含已知阻塞）

部署链当前依托 `wrangler`。官方 `cf` CLI（基线 v1.0.0-beta.12）尚不能接管本项目的 Rust Worker：

- `cf deploy` 的项目模型要求 manifest（`Cargo.toml` / `package.json` / `pyproject.toml`）内声明一个 dev-server，Rust 分支指定 `cloudflare-rs-dev-server`；
- 该 crate 在 crates.io 上**尚不存在**（API 返回 `crate does not exist`），Rust 分支因此不可用；
- 唯一不依赖 wrangler 的替代是 `cf deploy --prebuilt` 配合手写 `.cloudflare/output/`，而其官方工具包自述「not yet stable enough for external use — APIs may change without notice」。

结论：在 `cf` 发布可用的 Rust dev-server 之前，部署链保留 `wrangler`，不为对齐命令行工具而改造构建体系。上游可用后按 `cf deploy` + `cf d1 migrations apply` 迁移（官方声明后者与 `wrangler d1 migrations apply` 线兼容：同记账表、同记录名、同数字序）。

两条路径目前分别实现业务逻辑，应分别验收。

新版工作台、无标题创建和记住登录均已部署；Worker 与本地内嵌同一 `static/app.html`。桌面／手机和隔离两端回归已通过；生产另有登录／恢复、创建、分享／下载、ACL、阅后即焚与退出的有限端到端烟测，不代表所有浏览器与边界都已验收。

**后续源码差异：大 HTML 改造（Workers KV 免费方案）。** 当前源码支持 HTML 10 MiB（其他文本仍 500 KiB），大正文使用 Workers KV（Cloudflare 纯免费层、免绑定信用卡）与本地 `<数据库>.bodies/` 目录；旧小内容保留数据库内联。双端本地回归与 WASM dry-run 均已通过。详见 `docs/LARGE-HTML.md`。普通服务器场景继续独立支持。

## 登录与管理

首次访问会生成 Super Token，只显示一次。安全保存后前往首页登录。HTTPS 模式下，浏览器用 Token 换取独立的 HttpOnly 会话 Cookie；不把原始 Token 放进 URL、Cookie、sessionStorage 或 localStorage。CLI／Agent 仍使用 `Authorization: Bearer <token>`。

登录框提供“记住此设备（30 天）”，默认不勾选。不勾选时是服务端最长 12 小时的会话 Cookie；勾选时固定 30 天，不静默续期。同一浏览器打开新标签页或重新启动后，有效的持久会话会自动进入工作台。浏览器可能恢复临时会话，公共设备仍需主动退出。

页面入口：`/` 和 `/app` 都先查询服务端会话，有效则进入工作台，无效才显示登录表单；网络／存储故障保留 Cookie 并允许重试。普通成员不显示 Token 管理入口。公开接收者直接访问 `/v/:id`，不需要登录；私有内容即使带 Cookie 也不能从该公开入口访问。退出登录会撤销当前服务端会话并清 Cookie；删除／轮换 Token 撤销其全部会话。轮换当前登录 Token 时，先保存一次性新凭证，再重新登录。

Cookie 使用 `__Host-` 前缀、`Secure; HttpOnly; SameSite=Strict; Path=/`，不设 Domain；服务端仅保存会话凭证哈希。Cookie 写请求必须来自配置的准确 Origin，并使用 JSON；无效 Authorization 不会回退为 Cookie 身份。旧标签页中的 Token 不会被偷偷交换为持久会话，首页提供显式迁移确认。

Token 只有两种级别：`super`（全局管理）和 `normal`（仅查看自己创建或明确授权的内容）。

看板支持内容创建、列表、查看、删除和授权／有效期管理。公开 HTML 在无脚本 sandbox 中展示；Token 保护 HTML 可在隔离 sandbox 中运行脚本。阅后即焚仅允许 Token 保护内容。

新版工作台支持标题搜索、访问方式筛选、分享链接复制、Token 明文复制／隐藏。危险操作使用居中确认框，默认聚焦取消；请求失败保留输入，提交期间防止重复操作。创建者／Super 查看阅后即焚内容使用免焚预览，其他获授权成员须确认后读取并焚毁。

Cookie 认证的阅后即焚只能由显式 POST 消费，GET、HEAD、导航和预取不会焚毁；正文与下载共享同一次原子消费。Bearer 客户端保留既有 GET 消费兼容路径。

默认不需要标题：粘贴内容或导入文件即可直接创建。标题入口默认折叠，只有需要时才展开填写；留空时存储为无标题，不会提取正文或文件名补标题。看板以「无标题 · 短 ID」作展示标识，不会把该标识写入标题。API 可省略 `title`，CLI 不传 `--title` 即创建无标题内容；已有记录不会自动更改。

## CLI

需要 Python 3，无第三方依赖。Token 通过环境变量提供，不要放进 shell 历史或命令参数：

```bash
export FLASHPASTE_URL="https://your-worker.example"
export FLASHPASTE_TOKEN="<your-token>"
python3 flashpaste-cli.py list
python3 flashpaste-cli.py create --type markdown --visibility public --ttl 1d --file ./note.md
python3 flashpaste-cli.py create --type html --visibility token --grant team --file ./report.html
python3 flashpaste-cli.py get <content-id>
python3 flashpaste-cli.py delete <content-id>
```

`create` 不带 `--file` 时从 stdin 读取正文。`--grant` 可重复指定 Token 名称。阅后即焚内容可用 `--ttl burn --visibility token` 创建；`get --preview` 仅创建者或 Super 可用于预览而不焚毁。

## API 摘要

除公开内容的正文／查看／下载外，管理 API 均需要有效 Cookie 会话或 Bearer Token。

- `POST /api/auth/verify`：验证 Token。
- `GET /api/auth/config`：返回当前站点是否启用 HTTPS Cookie 登录。
- `POST /api/auth/login`：JSON `{token, remember_device}`，签发会话 Cookie；响应仅返回身份和到期时间。
- `GET /api/auth/session`、`POST /api/auth/logout`：恢复／退出当前会话。无 Cookie 的身份查询不发送清除 Cookie 的响应。
- `POST /api/contents`：创建内容，支持 `allowed_tokens`、`ttl`、`burn_after_read`。
- `GET /api/dashboard`：返回当前 Token 可管理或被授权的活动内容。
- `GET /api/contents/:id`、`PATCH /api/contents/:id`、`DELETE /api/contents/:id`：查看元数据、修改授权／可见性／到期时间、删除。
- `GET /api/contents/:id/body`：读取正文；阅后即焚可选 `?consume=0`（仅创建者／Super）。
- `POST /api/contents/:id/consume`：浏览器显式消费阅后即焚；Cookie GET 的消费请求返回 `409 CONSUME_REQUIRES_POST`。
- `GET /v/:id`：公开内容浏览器页面。
- `GET /d/:id`：下载原始内容。
- `POST /d/:id`：浏览器显式下载并消费阅后即焚；Cookie GET 下载 burn 内容返回 `405 Allow: POST`。
- `GET /api/tokens/lookup?name=...`、`POST /api/tokens`、`GET /api/tokens`、`DELETE /api/tokens/:id`、`POST /api/tokens/:id/rotate`：Token 查找和管理（仅 Super，查找除外）。

源码内容上限：HTML 10 MiB，text／markdown 500 KiB；JSON 请求封装另限 16 MiB。生产当前仍为全部 500 KiB。支持 1 小时、6 小时、1 天、2 天或阅后即焚。大 HTML 在 Worker 使用 Workers KV、本地使用 `<数据库>.bodies/`；旧小内容不搬迁。小时任务在元数据到期清理后回收失去引用的对象，宽限 1 小时；删除和消费先撤销元数据访问，再清理正文。备份新数据时必须同时备份数据库与文件／对象，不能只保存 SQLite 文件。

## 会话配置与迁移

- Worker：`wrangler.toml` 的 `FLASHPASTE_ORIGIN` 必须是实际访问的 HTTPS origin，不能根据任意请求 Host 自动信任新站点。更换域名时也要修改此值，Cookie 不跨域共享。
- Axum：设置 `FLASHPASTE_ORIGIN=https://你的域名`，由可信 HTTPS 反向代理转发到后端；配置后默认仅监听 `127.0.0.1:3000`，可用 `FLASHPASTE_BIND` 显式修改。不要将未加密的代理后端直接暴露到外网。
- 纯 HTTP 调试：未配置 HTTPS origin 时仅保留原有标签页 Bearer 登录，界面明确禁用“记住此设备”；不会降级签发不安全的持久 Cookie。
- SQLite：启动时检测 `auth_version`；既有库升级前用 SQLite backup API 保存包含已提交 WAL 的私密备份，再新增会话结构。备份名为 `<数据库>.pre-sessions-<随机ID>.sqlite3`，Unix 权限为 `0600`。原有 Token／内容／ACL 保留；既有本地旧明文列清理逻辑仍单独执行，备份保留其执行前的数据。
- D1：会话版本曾新增 `0002_token_auth_version.sql`、`0003_sessions.sql`；大 HTML 源码新增 `0004_content_bodies.sql`，尚未在生产执行。新迁移不自动修改远端数据库。正式更新前须先导出到私密位置、核对 migration 历史，再获授权执行远端迁移及部署；迁移工具只执行一次。新增迁移不操作旧 `token_value` 列。

2026-10-18 已获授权执行生产备份、迁移和部署。生产原有表已存在但 migration 历史为空，因此迁移工具执行了幂等的 `0001_init.sql` 并登记历史，再执行 `0002`／`0003`。升级前导出、恢复校验和副本演练均通过；迁移后与烟测清理后逐字段比较原有 Token／内容／ACL，完全不变，旧 `token_value` 列保留。详见 `ops/RELEASE-2026-10-18.md`。

## 验证

```bash
cargo test --no-default-features --features local
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -p 'test_cli.py'
cargo check --lib --target wasm32-unknown-unknown --no-default-features --features worker
wrangler deploy --dry-run
```

`wrangler deploy --dry-run` 仅构建和验证，不会部署。dry-run 通过不等于已上线：远端迁移、实际部署与生产烟测是三件彼此独立、需分别授权并留证的事。

浏览器回归测试需要本机 Chrome 和 `playwright-core`；测试依赖可放在临时目录，不写入项目：

```bash
npm install --prefix /tmp/opencode/flashpaste-ux-tools playwright-core --no-audit --no-fund
export NODE_PATH=/tmp/opencode/flashpaste-ux-tools/node_modules
node tests/ui-smoke.cjs
cargo build --no-default-features --features local
node tests/ui-local.cjs
node tests/session-local.cjs
node tests/session-worker.cjs
```

`ui-smoke.cjs` 使用合成 API 数据覆盖首页／工作台跳转、桌面／手机布局、弹窗、错误反馈及预览安全，截图保存在 `/tmp/opencode/flashpaste-ux/`。`ui-local.cjs` 使用临时目录中的独立 SQLite 库启动真实 Axum 服务，覆盖初始化、登录、创建 Token、私有 ACL、公开分享与阅后即焚；需要端口 3000 空闲。两者均不读取生产凭证或修改生产数据，Chrome 路径可用 `CHROME_PATH` 覆盖。依次运行两项后，目录中的 `index.html` 为全页面截图预览入口；初始化截图会使用演示文本替换凭证。

`session-local.cjs` 使用隔离 Axum、SQLite 和本地 HTTPS 反向代理（3041／3443 端口）；`session-worker.cjs` 只使用 `--local` 和独立持久化目录，演练旧数据迁移后启动本地 Worker／D1 HTTPS 服务（8789／9241 端口）。需要 OpenSSL、Chrome、playwright-core；Worker 测试还需 Wrangler／Rust Worker 工具链。两端复用浏览器会话验收和 `core-regression.cjs`：真实重启／跨标签页恢复、Cookie 属性、Web Storage 无 Token、CSRF、POST 消费、轮换竞态、自轮换与退出，以及三种格式原样下载、UTF-8 的 500 KiB 边界、全部 TTL、非法输入／角色、ACL 授权撤销／替换、读取时过期和真实 Python CLI。Rust 测试另覆盖到期、存储故障、SQLite WAL 备份和原子竞争。`test_api.sh` 已替换为隔离测试入口，不再对不明来源的 localhost 服务执行过时示例。

`tests/production-smoke.cjs` 是另行授权使用的线上烟测，不在默认本地测试中执行。要求环境中提供生产管理凭证 `FLASHPASTE_API_TOKEN`，并显式设置 `FLASHPASTE_PRODUCTION_SMOKE=YES`；只创建／轮换临时普通 Token 和测试内容，结束时清理，不轮换原有 Super Token。本轮执行通过并核对测试数据全部清理。此烟测、功能回归均不是容量测试或全浏览器安全证明。

两项 session HTTPS 脚本现也调用 `tests/large-html.cjs`。可用 `FLASHPASTE_HTML_SAMPLE` 指定仅供本地测试的 HTML，脚本限定 loopback 且阻断测试页面的外部网络，不会上传到生产。该用例包含 10 MiB 边界、外部正文引用、真实文件导入／私有预览／原样下载、BOM／CRLF、大正文 burn 竞争及正文缺失不消费。CLI `--file` 保留原有换行；浏览器导入要求 UTF-8，未编辑时也保留 BOM／换行原始字节。

Worker 开启 10% 采样的日志与 traces，日志剔除查询字符串；错误只记通用故障码，不输出请求凭证或正文。

旧版曾可能将 Token 明文写入 D1。当前远端仍有 `token_value` 空列，检查到非空记录为 0。若要移除此列，先备份，再执行 `ops/drop_legacy_plaintext_token.sql`；若列不存在，不要运行该 SQL。此操作不会由 Worker 自动执行。

## 持续集成与许可

CI（`.github/workflows/ci.yml`）在每次 push 与 PR 上执行四项：`cargo fmt --check`、`cargo clippy -D warnings`、`cargo test`（local 特性栈）、`cargo check`（wasm32 Worker 目标），另加 Python CLI 单测。浏览器类验收依赖本机 Chrome 与 `playwright-core`，不纳入 CI，按上文「验证」一章手工执行并留证。

本项目以 [MIT 许可](LICENSE) 发布。
