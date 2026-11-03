# FlashPaste

把一段文本、Markdown 或单文件 HTML 变成一个带过期时间的链接。

场景很小：临时给同事看一份报表、把一段配置发给别人、传一个只能用一次的凭据。不做版本管理，不做协同编辑，也不打算长成 CMS。

## 能做什么

- **三种内容**：纯文本、Markdown、单文件 HTML（HTML 最大 10 MiB，其余 500 KiB）。
- **三种可见性**：公开链接、Token 授权、阅后即焚。
- **有效期**：1 小时、6 小时、1 天、2 天，或读完即毁。
- **两套运行环境**：Cloudflare Workers + D1/KV，或 Axum + SQLite；对外 HTTP 契约保持一致。

## 几个设计取舍

- **公开 HTML 不执行脚本**：无脚本 sandbox 加收紧的 CSP。需要脚本的私有 HTML 走 opaque origin，拿不到主站凭据。
- **阅后即焚不靠 GET**：消费必须是显式 `POST /consume`，聊天工具与搜索引擎的链接预取不会把内容烧掉。
- **Token 不进浏览器存储**：浏览器用一次性交换拿到的 `__Host-` HttpOnly Cookie，CLI 与 Agent 用 Bearer。
- **大正文外置**：超过 500 KiB 的正文放 Workers KV（本地放同级 `.bodies/` 目录），元数据仍留在 D1／SQLite，避开单行上限。

## 快速开始

本地运行，需要 Rust：

```bash
cargo run --no-default-features --features local
# 打开 http://127.0.0.1:3000，首次访问会生成一次性 Super Token
```

部署到边缘，需要 wrangler 与事先建好的 D1／KV：

```bash
# 先把 wrangler.toml 里的占位值换成你自己的 KV／D1 绑定与实际域名
wrangler d1 migrations apply <your-db> --remote
wrangler deploy
```

## CLI

只依赖 Python 3 标准库：

```bash
export FLASHPASTE_URL="https://your-worker.example"
export FLASHPASTE_TOKEN="<your-token>"
python3 flashpaste-cli.py create --type markdown --ttl 1d --file ./note.md
python3 flashpaste-cli.py list
```

其余子命令见 `python3 flashpaste-cli.py --help`。

## 文档

- [HTTP API 摘要](docs/API.md)
- [会话、配置与运维注意](docs/OPERATIONS.md)
- [测试与验收](docs/TESTING.md)
- [大正文支持（10 MiB）](docs/LARGE-HTML.md)

## 现状与限制

这个项目目前以自用为主，下面的边界都是已知的：

- 没做过容量与并发压测，规模化表现没有数据。
- 浏览器验收覆盖主要路径，不等于所有浏览器与边界都已验证。
- 部署链仍依赖 `wrangler`：官方 `cf` CLI 的 Rust 分支所需组件尚未发布，迁移暂时搁置，原因记在[运维注意](docs/OPERATIONS.md)。
- 大正文能力通过了本地回归，尚未在生产环境长期运行。

## 许可

[MIT](LICENSE)
