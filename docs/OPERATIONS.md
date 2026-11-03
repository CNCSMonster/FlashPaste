# 会话、配置与运维注意

## 会话与鉴权

首次访问会生成 Super Token，只显示一次。HTTPS 模式下浏览器用 Token 换取独立的 HttpOnly 会话 Cookie，原始 Token 不进入 URL、Cookie、sessionStorage 或 localStorage；CLI／Agent 直接用 `Authorization: Bearer <token>`。

登录框的"记住此设备"默认不勾选：不勾选是服务端最长 12 小时的会话 Cookie，勾选固定 30 天且不静默续期。Cookie 使用 `__Host-` 前缀与 `Secure; HttpOnly; SameSite=Strict; Path=/`，不设 Domain，服务端只保存会话凭证哈希。

Cookie 写请求必须来自配置的准确 Origin 且使用 JSON；无效的 `Authorization` 不会回退成 Cookie 身份。退出登录撤销当前会话；删除或轮换 Token 会撤销其全部会话。

Token 只有两级：`super`（全局管理）与 `normal`（仅自己创建或被授权的内容）。

## 阅后即焚

Cookie 认证下只能由显式 `POST` 消费：`GET`、`HEAD`、页面导航与链接预取都不会焚毁内容。正文读取与下载共享同一次原子消费。Bearer 客户端保留既有的 GET 消费兼容路径。

看板上，创建者／super 查看阅后即焚内容使用免焚预览；其他被授权成员需确认后才读取并焚毁。

## 部署配置

- **Worker**：`wrangler.toml` 的 `FLASHPASTE_ORIGIN` 必须是实际访问的 HTTPS origin，不能依据请求 Host 自动信任新站点；换域名时同步修改，Cookie 不跨域共享。
- **Axum**：设置 `FLASHPASTE_ORIGIN=https://你的域名`，由可信 HTTPS 反向代理转发；配置后默认只监听 `127.0.0.1:3000`，可用 `FLASHPASTE_BIND` 覆盖。不要把未加密的后端直接暴露到外网。
- **纯 HTTP 调试**：未配置 HTTPS origin 时只保留标签页 Bearer 登录，界面禁用"记住此设备"，不会降级签发不安全的持久 Cookie。

## 为什么仍在使用 wrangler

官方 `cf` CLI（基线 v1.0.0-beta.12）目前接管不了这个项目的 Rust Worker：

- `cf deploy` 的项目模型要求 manifest（`Cargo.toml` / `package.json` / `pyproject.toml`）里声明一个 dev-server，Rust 分支指定 `cloudflare-rs-dev-server`；
- 该 crate 在 crates.io 上尚不存在（API 返回 `crate does not exist`），Rust 分支因此不可用；
- 唯一不依赖 wrangler 的替代是 `cf deploy --prebuilt` 配合手写 `.cloudflare/output/`，而其官方工具包自述 "not yet stable enough for external use"。

因此部署链暂时保留 `wrangler`，不为对齐命令行工具而改造构建体系。上游补齐后，可按 `cf deploy` + `cf d1 migrations apply` 迁移（官方声明后者与 `wrangler d1 migrations apply` 线兼容：同记账表、同记录名、同数字序）。

## 数据库迁移

- SQLite：启动时检测 `auth_version`；既有库升级前用 SQLite backup API 保存包含已提交 WAL 的私密备份，再新增会话结构。备份名形如 `<数据库>.pre-sessions-<随机ID>.sqlite3`，Unix 权限 `0600`。原有 Token／内容／ACL 保留。
- D1：会话相关迁移为 `0002_token_auth_version.sql`、`0003_sessions.sql`；大正文源码新增 `0004_content_bodies.sql`。迁移不会自动作用于远端库，正式更新前需先导出备份、核对 migration 历史，再执行迁移与部署。

## 日志与历史遗留

Worker 开启 10% 采样的日志与 traces，日志剔除查询字符串；错误只记录通用故障码，不输出请求凭证或正文。

旧版本曾可能把 Token 明文写入 D1。当前远端仍保留空的 `token_value` 列，检查到非空记录为 0。如需移除该列，先备份再执行 `src/sql/drop_legacy_plaintext_token.sql`；列不存在时不要运行。
