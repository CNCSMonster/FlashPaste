# 更新日志

记录对外可见的行为变化。格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循语义化版本。

## [未发布]

### 变更

- 部署链迁移到 `cf` CLI 的方案暂时搁置：上游 Rust dev-server 组件尚未发布，当前仍使用 `wrangler`。原因与替代路径见[运维注意](docs/OPERATIONS.md)。

## [0.1.0] - 2026-11-03

首个公开版本。

### 新增

- 三种内容类型：纯文本、Markdown、单文件 HTML。
- 三种可见性：公开链接、Token 授权、阅后即焚。
- 有效期：1 小时、6 小时、1 天、2 天，或读完即毁。
- Token 两级权限（`super` / `normal`），支持创建、列出、轮换与删除。
- 浏览器登录用一次性 Token 换取 `__Host-` HttpOnly 会话 Cookie；命令行与 Agent 使用 Bearer。
- 大正文外置：HTML 上限 10 MiB，其余 500 KiB，超出部分存 Workers KV 或本地 `.bodies/` 目录。
- 两套运行环境：Cloudflare Workers + D1/KV，以及 Axum + SQLite，对外 HTTP 契约一致。
- 命令行工具只依赖 Python 3 标准库。

### 安全

- 公开 HTML 以无脚本 sandbox 渲染，并配套收紧的 CSP。
- 需要脚本的私有 HTML 运行在 opaque origin，读不到主站凭据。
- 阅后即焚在 Cookie 认证下只能由显式 `POST` 消费，链接预取与页面预导航不会触发焚毁。
- Cookie 写操作校验 Origin 并要求 JSON 请求体；原始 Token 不写入浏览器存储。

### 限制

- 未做容量与并发压测，规模化表现无量化数据。
- 浏览器验收覆盖主要路径，不等于全浏览器与全边界验证。
- 大正文能力通过本地回归，尚未在生产环境长期运行。
