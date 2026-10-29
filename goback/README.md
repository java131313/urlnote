# urlnote goback — Go 重写（后端 + WASM 前端）

将 Cloudflare 部署（Rust worker + D1 + KV）移植为单容器 Go 应用：

- **后端** — `net/http` + GORM/PostgreSQL，直连 Seaos 的 Postgres
  （迁移自 D1，见根目录 `migrate/`）
- **前端** — Go 编译为 WebAssembly（裸 `syscall/js`，无框架），Vditor
  仍是同一个 JS 编辑器；OSS 文件库（`/oss`）是浏览器内 SigV4 直连 S3
- **静态资源** — 单容器 all-in-one：API、WASM、Vditor/mammoth/turndown/
  pdfjs、上传图片全部由这一个服务提供

API 契约与 Rust 实现逐字段一致（`shared/dto.go` 对照
`rust/shared/src/lib.rs`），时间戳沿用 D1 的 TEXT 格式
（UTC `YYYY-MM-DD HH:MM:SS`），与迁移数据完全兼容。

## 目录

```
cmd/server/       服务入口（环境变量见下）
internal/api/     HTTP 处理器（26 个路由，对照 rust/worker/src/lib.rs）
internal/store/   GORM 模型（7 张 D1 迁移表 + notes_fts 全文索引表）
internal/token/   分词（gojieba 中文 + kagome/IPADIC 日语），token.rs 移植
internal/analyze/ 语法规则分析，analyze.rs 移植
internal/cache/   进程内 TTL 缓存（对照 worker 的 LRU）
shared/           前后端共用 API DTO
web/              Go WASM 前端（Leptos 应用的 syscall/js 移植）
static/           index.html / app.css / vditor / 转换库 / 词表 / app.wasm
Dockerfile        多阶段 all-in-one 镜像
```

## 运行

```bash
# 本地开发（需要本地 Postgres；macOS 上 brew 的 postgresql@17 即可）
export DATABASE_URL="postgres://$(whoami)@localhost:5432/urlnote_test?sslmode=disable"
make build && make dev          # http://localhost:8080

# 测试
make test                       # 分词/分析单元测试

# Docker（本地）
export DATABASE_URL="postgres://$(whoami)@host.docker.internal:5432/urlnote_test?sslmode=disable"
make run                        # build 镜像 + 容器,数据卷 urlnote-data

# Docker（Seaos / 生产）
docker build -t urlnote-goback .
docker run -d --name urlnote -p 8080:8080 \
  -e DATABASE_URL="postgres://postgres:<密码>@dbconn.sealosbja.site:39671/urlnote?sslmode=require" \
  -v urlnote-data:/data \
  urlnote-goback
```

### 环境变量

| 变量 | 默认 | 说明 |
|---|---|---|
| `DATABASE_URL` | —（必填） | GORM Postgres DSN |
| `ADDR` | `:8080` | 监听地址 |
| `IMAGE_DIR` | 镜像内 `/data/images` | 上传图片存储目录 |
| `STATIC_DIR` | 镜像内 `/app/static` | 前端静态资源 |
| `CORS_ORIGINS` | 空（仅同源） | 逗号分隔的允许跨域来源 |
| `SWEEP_INTERVAL` | `20m` | notes_mark 一致性巡检周期（替代 CF cron） |

### 构建参数（WASM 前端的部署配置）

`web/` 的这些变量用 `-ldflags -X` 在 **wasm 编译时**注入：

| 变量 | 默认 | 说明 |
|---|---|---|
| `main.API_BASE` | 空 | API 源（前端与 API 不同域时，如 GitHub Pages 前端） |
| `main.BASE_PATH` | 空 | 前端挂载前缀（如 `/urlnote` 项目页） |
| `main.OSSAK` / `main.OSSSK` / `main.OSSBucket` | 空 | S3 签名凭据（**会打进公开的 wasm 包**，仅适合个人使用） |
| `main.OSSEndpoint` / `main.OSSRegion` | sealos 默认值 | S3 端点/区域 |

```bash
GOOS=js GOARCH=wasm go build -o static/app.wasm \
  -ldflags "-X main.OSSAK=xxx -X main.OSSSK=yyy -X main.OSSBucket=zzz" ./web
```

## 与 Rust/CF 实现的差异

- **notes_fts** — SQLite FTS5 虚拟表 → 真实表 + `to_tsvector('simple')`；
  保存时重新分词灌入，搜索用 ts_rank 排序（对照 bm25）
- **sessions / presence** — KV → PG 表（迁移时已在库里）
- **图片** — KV → 本地磁盘（`IMAGE_DIR`），URL 格式 `/i/{name}` 不变
- **缓存** — Workers 每 isolate 的 LRU → 单实例进程内 TTL 缓存，
  `X-Cache: HIT/MISS` 行为保留
- **一致性巡检** — CF cron（每 20 分钟）→ 进程内 ticker
- **gojieba Tag() 恒开 HMM** — Rust `zh_tag(false)` 是 MP-only；OOV 词的
  POS 覆盖略有差异，分析器另有显式 HMM 通道兜底
- **analyze 的 ABAB 动词豁免** — Rust 原实现把 char 偏移与 byte 偏移混比，
  对中文文本实际不生效；Go 版逐字保留该行为以维持结果一致

## macOS 本地构建注意

这台机器的 CLT 把 C++ 头文件只放在 SDK 里，cgo（gojieba）需要：

```bash
export SDK=/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk
export CGO_CXXFLAGS="-stdlib=libc++ -nostdinc++ -I$SDK/usr/include/c++/v1"
```

Makefile 已自动探测并设置；Linux/Docker 构建不需要。
