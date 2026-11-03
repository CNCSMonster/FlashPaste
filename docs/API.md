# HTTP API 摘要

除公开内容的读取外，管理类接口都需要有效的会话 Cookie 或 `Authorization: Bearer <token>`。

## 认证

| 接口 | 说明 |
| :--- | :--- |
| `POST /api/auth/verify` | 校验 Token |
| `GET /api/auth/config` | 当前站点是否启用 HTTPS Cookie 登录 |
| `POST /api/auth/login` | JSON `{token, remember_device}`，签发会话 Cookie；响应只返回身份与到期时间 |
| `GET /api/auth/session` | 恢复当前会话 |
| `POST /api/auth/logout` | 退出并撤销当前会话 |

## 内容

| 接口 | 说明 |
| :--- | :--- |
| `POST /api/contents` | 创建内容，支持 `allowed_tokens`、`ttl`、`burn_after_read` |
| `GET /api/dashboard` | 当前 Token 可管理或被授权的活动内容 |
| `GET /api/contents/:id` | 读取元数据 |
| `PATCH /api/contents/:id` | 修改授权、可见性与到期时间 |
| `DELETE /api/contents/:id` | 删除 |
| `GET /api/contents/:id/body` | 读取正文；阅后即焚可选 `?consume=0`（仅创建者或 super） |
| `POST /api/contents/:id/consume` | 浏览器显式消费阅后即焚；Cookie GET 消费返回 `409 CONSUME_REQUIRES_POST` |

## 公开读取与下载

| 接口 | 说明 |
| :--- | :--- |
| `GET /v/:id` | 公开内容的浏览器页面 |
| `GET /d/:id` | 下载原始内容 |
| `POST /d/:id` | 浏览器显式下载并消费阅后即焚；Cookie GET 下载 burn 内容返回 `405 Allow: POST` |

## Token 管理

`GET /api/tokens/lookup?name=...`（各级别可用）、`POST /api/tokens`、`GET /api/tokens`、`DELETE /api/tokens/:id`、`POST /api/tokens/:id/rotate`（仅 super）。

## 容量与到期

- 正文上限：HTML 10 MiB，text／markdown 500 KiB；JSON 请求封装另限 16 MiB。
- 有效期：1 小时、6 小时、1 天、2 天，或阅后即焚。
- 大正文在 Worker 侧存 Workers KV，本地存 `<数据库>.bodies/`；早期的小内容不迁移。
- 每小时任务在清理过期元数据后回收失去引用的正文对象，宽限期 1 小时；删除与消费先撤销元数据访问，再清理正文。
- 备份新数据时必须同时备份数据库与正文文件／对象，只保存 SQLite 文件是不够的。
