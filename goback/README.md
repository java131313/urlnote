# urlnote goback — Go 后端（Cloudflare Worker）+ Go WASM 前端

Go 重写版现在作为 **Cloudflare Worker** 运行：Go 代码编译为 WebAssembly
（`GOOS=js GOARCH=wasm`，由 [syumai/workers-go](https://github.com/syumai/workers-go)
驱动），直连 **D1**（SQLite），不再使用 PostgreSQL / GORM，也没有独立服务器进程。

- **后端** — `cmd/worker`：D1 绑定 `DB` + KV 绑定 `CACHE`（响应缓存与上传图片）+
  Static Assets 绑定 `ASSETS`（Go WASM 前端），cron 触发 `notes_mark` 一致性巡检
- **数据访问** — `internal/store`：纯 `database/sql`，全部为 SQLite 方言
  （`?` 占位符、`ON CONFLICT` 原子 upsert、`substr()`、FTS5 `MATCH`/`bm25`）
- **前端** — `web/` 编译为 WASM（裸 `syscall/js`），产物 `static/app.wasm`；
  Vditor 等运行时资源在 `static/`
- **分词** — 原生构建用 gojieba + kagome/IPADIC；WASM 构建无法链接 cgo，
  `internal/token/token_wasm.go` 提供纯 Go 的启发式分词（见下方差异）

API 契约与 Rust 实现逐字段一致（`shared/dto.go` 对照 `rust/shared/src/lib.rs`），
时间戳沿用 D1 的 TEXT 格式（UTC `YYYY-MM-DD HH:MM:SS`），与既有数据完全兼容。

> **定位：备选实现，不是生产默认。** 仓库默认部署的仍是 Rust Worker
> （`wrangler.toml`），本项目保留 Go 版是为了"用 Go 也能维护同一套后端"，
> 配置在根目录的 `wrangler-go.toml`（Worker 名 `urlnote-go`，可与 Rust 版并行部署）。
> 实测差距：包体 11.31 MiB vs Rust 7.22 MiB；wasm 下中文分词是启发式（无词典）、
> 日文分词不可用；响应缓存只能走 KV write-through（Rust 用进程内 LRU）。
> 切换/回滚步骤见仓库根 `README.md` 的「Go Worker（备选）」。

## 目录

```
cmd/worker/        Worker 入口（D1 / KV / ASSETS / cron）
internal/api/      HTTP 处理器（26 个路由，对照 rust/worker/src/lib.rs）
internal/store/    D1 (SQLite) 数据访问层（替代 GORM/PostgreSQL）
internal/token/    分词：原生 gojieba+kagome / wasm 纯 Go 启发式
internal/analyze/  语法规则分析，analyze.rs 移植
internal/cache/    进程内 TTL 缓存（原生实现的参考，Worker 用 KV 实现同接口）
shared/            前后端共用 API DTO
web/               Go WASM 前端（Leptos 应用的 syscall/js 移植）
static/            index.html / app.css / vditor / 转换库 / 词表 / app.wasm
```

## 构建与部署

在**仓库根目录**执行：

```bash
make go-wasm      # 生成 goback/build/{worker.mjs,app.wasm,runtime.mjs,wasm_exec.js}
make dev-go       # wrangler dev --config wrangler-go.toml（本地 D1）
make deploy-go    # wrangler deploy --config wrangler-go.toml
```

首次需要初始化 D1 表结构（与 Rust worker 共用同一个库）：

```bash
make create-db        # 仅第一次：npx wrangler d1 create urlnote，把 id 填进配置
make init-db          # npx wrangler d1 execute urlnote --remote --file=./schema.sql
make init-db-local    # 本地开发库
```

`wrangler-go.toml` 与默认的 `wrangler.toml`(Rust) 共用 D1 与 KV，可并行灰度；
API 契约、KV key（`v4/notes/*`、`v4/index.json`、`img/*`）完全一致。

也可以在本目录执行 `make worker / make dev / make deploy`（会调用根 Makefile）。

## 与 Rust / Cloudflare 实现的差异

- **分词（仅 wasm）** — gojieba 依赖 cgo，无法编译进 WASM。Worker 版中文分词为
  纯 Go 的 2 字切分启发式（搜索仍保留词级语义，但召回/排序与 jieba 不同，
  词性标注类别也更粗略）。默认的 Rust worker 仍使用完整 jieba + lindera。
- **缓存** — 原进程内 TTL 缓存改为 Workers KV write-through（接口不变）；
  KV 的 `expiration_ttl` 下限为 60 秒，原 30s/15s 的 TTL 会被提升到 60s。
- **图片** — 原先本地磁盘 `IMAGE_DIR`，Worker 版存 KV `img/*`；URL `/i/{name}` 不变。
- **一致性巡检** — 原进程内 ticker，Worker 版由 cron 触发（`7,27,47 * * * *`）。

## 测试

```bash
make test    # internal/token + internal/analyze（原生 cgo 构建）
```

WASM 构建不会编译 gojieba/kagome，因此 `GOOS=js GOARCH=wasm go build ./cmd/worker`
无需 C++ 工具链。
