# 测试与验收

## CI 覆盖什么

`.github/workflows/ci.yml` 在每次 push 与 PR 上执行：

```bash
cargo fmt --check
cargo clippy --no-default-features --features local -- -D warnings
cargo test --no-default-features --features local
cargo check --lib --target wasm32-unknown-unknown --no-default-features --features worker
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -p 'test_cli.py'
```

浏览器类验收依赖本机 Chrome 与 `playwright-core`，不纳入 CI，按下面手工执行。

## 手工验收

依赖放在临时目录，不写入项目：

```bash
npm install --prefix "${TMPDIR:-/tmp}/flashpaste-ux-tools" playwright-core --no-audit --no-fund
export NODE_PATH="${TMPDIR:-/tmp}/flashpaste-ux-tools/node_modules"

node tests/ui-smoke.cjs        # 合成数据，跑首页／工作台／弹窗／响应式布局
cargo build --no-default-features --features local
node tests/ui-local.cjs        # 真实 Axum + SQLite，需 3000 端口空闲
node tests/session-local.cjs   # 会话与安全回归，起本地 HTTPS 反代（3041／3443）
node tests/session-worker.cjs  # 同上，跑在本地 Worker／D1（8789／9241）
```

`ui-smoke.cjs` 用合成数据覆盖页面跳转、桌面与手机布局、弹窗、错误反馈与预览安全，截图输出到 `${TMPDIR:-/tmp}/flashpaste-ux/`；初始化截图会用演示文本替换凭证。Chrome 路径可用 `CHROME_PATH` 覆盖。

`session-local.cjs` 与 `session-worker.cjs` 复用 `session-browser.cjs` 和 `core-regression.cjs`，覆盖：真实重启与跨标签页恢复、Cookie 属性、Web Storage 不含 Token、CSRF、POST 消费、轮换竞态、自轮换与退出，以及三种格式原样下载、UTF-8 的 500 KiB 边界、全部 TTL、非法输入与角色、ACL 授权撤销／替换、读取时过期，并调用真实 Python CLI。Worker 侧还需要 wrangler 与 Rust Worker 工具链。

两个 session 脚本同时调用 `tests/large-html.cjs`：可用 `FLASHPASTE_HTML_SAMPLE` 指定仅用于本地测试的 HTML，脚本限定 loopback 并阻断外部网络。该用例覆盖 10 MiB 边界、外部正文引用、文件导入／私有预览／原样下载、BOM 与 CRLF、大正文消费竞争和正文缺失不消费。

## 线上烟测（需单独授权）

`tests/production-smoke.cjs` 不在默认测试中执行。它要求环境变量 `FLASHPASTE_API_TOKEN` 且显式设置 `FLASHPASTE_PRODUCTION_SMOKE=YES`，只创建与轮换临时普通 Token 和测试内容，结束时清理，不动原有 Super Token。

## 这些测试不证明什么

功能回归与烟测都不是容量测试，也不是全浏览器安全证明。项目的并发表现与长期稳定性目前没有量化数据。
