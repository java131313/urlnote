# urlnote

极简 URL 便签：每个网址路径就是一个便签，打开即写，自动保存。

整个应用运行在 Cloudflare 上，无需自建服务器：

- **前端** — Rust + [Leptos](https://leptos.dev/) 编译为 WebAssembly（[Trunk](https://trunkrs.dev/) 构建），由 Cloudflare Workers Static Assets 托管；Markdown 编辑器为 Vditor（JS 库，通过 wasm-bindgen 驱动）
- **后端** — Rust 编译为 WebAssembly，运行在 Cloudflare Worker 上（[workers-rs](https://github.com/cloudflare/workers-rs) SDK）
- **数据库** — Cloudflare D1（SQLite）
- **缓存/会话/图片** — Workers KV（响应缓存 write-through、session、presence 心跳、上传图片）
- **旧实现保留** — Go 后端（`main.go`，workers-go）与 Vue 前端（`frontend/`）完整保留，可一键回滚（见下文）

```
浏览器 ──▶ Cloudflare Edge
            ├─ 命中静态文件 ──▶ Workers Static Assets（Leptos 应用 + Vditor 运行时资源）
            └─ /api/*、/:id ──▶ Rust(WASM) Worker ──┬─ GET：Workers KV 命中直接返回（miss 回源 D1 并回填）
                                                    ├─ PUT/POST：先写 D1，再 write-through 同步 KV
                                                    └─ /:id ──▶ index.html（ASSETS binding）
```

**缓存一致性**：Workers KV 保存每个便签 API 响应的副本（key 为 `v4/notes/{id}.json`），
通过 write-through 与 D1 保持一致；若 KV 写入失败会删除对应 key 使其回源 D1，
不会出现脏读。响应头 `X-Cache: HIT/MISS` 可观察缓存状态。
注意：绕过 API 直接改 D1（如 wrangler d1 execute）不会自动同步缓存，
需同步删除对应 KV key（`wrangler kv key delete v4/notes/{id}.json --namespace-id <id>`）。
Rust 与 Go 两个 Worker 使用相同的 KV key 布局与 JSON 结构，灰度切换期间缓存可以互相命中。

## 部署

前置：Node.js、Rust 工具链（`rustup`，含 `wasm32-unknown-unknown` target）、`cargo install trunk worker-build`、`npx wrangler login` 已登录。

```bash
# 首次：创建 D1 数据库与 KV 命名空间，把输出的 id 填进 wrangler.toml
make create-db
npx wrangler kv namespace create "urlnote-cache"

# 初始化数据库表（远程 + 可选的本地开发库）
make init-db
make init-db-local

# 部署（自动执行 make build：Rust 前端 Trunk 构建 + Rust Worker 编译为 WASM）
make deploy
```

部署后得到 `https://urlnote.<你的子域>.workers.dev`。

## 本地开发

```bash
make init-db-local   # 首次：初始化本地 D1
make dev             # wrangler dev，http://localhost:8787
```

## 回滚到 Go 实现

Go 后端（`main.go` → `build/app.wasm`）与 Vue 前端（`frontend/dist`）的构建目标保留在 Makefile 中：

```bash
make build-go        # 构建 Go Worker 与 Vue 前端
```

然后把 `wrangler.toml` 改回旧配置即可：

```toml
main = "./build/worker.mjs"
[build]
command = "make build-go"
[assets]
directory = "./frontend/dist"
binding = "ASSETS"
```

数据库、KV、API 契约完全一致，回滚无数据迁移成本。

## 使用

- 打开 `/` 会由前端生成一个新随机便签 ID 并跳转，如 `/u0yYk`
- 直接访问 `/<id>` 即可打开/创建对应便签（前端从 D1 加载内容与编辑设置）
- **需要登录**（内置账号，无注册页）：`admin / root@123`、`guest / guest`、`amigo / amigo`；
  Session 为 32 位随机 token 存于 Workers KV（`sess/<token>` → 用户名，7 天过期），
  通过 HttpOnly Cookie 携带；顶栏显示当前用户与本文最后编辑者（✍️），可一键退出
- **多用户协作感知**：前端每 15 秒上报心跳（`presence/<note>/<user>`，KV 90 秒 TTL），
  顶栏显示"👀 正在查看"的其他用户；每 10 秒轮询便签，他人保存后若无本地未保存修改则
  **自动同步**进编辑器，若正在编辑则显示"已被更新"横幅手动载入，避免互相覆盖；
  保存响应回显 `updated_at`，客户端据此判断是否有新变化
- 顶栏可直接编辑**标题**和**标签**（逗号分隔，自动去重，最多 10 个），随内容一起自动保存
- 顶栏「☰ 便签」打开**可搜索下拉框**：按标题 / 标签 / ID 过滤，列出所有便签
  （含标签 chips、内容摘要、编辑者、更新日期，按最近更新排序），点击即跳转到对应便签
- 输入内容后不会立即保存：顶栏「💾 保存」按钮或 `Ctrl/Cmd + S` 立即保存；
  有未保存更改时**每 3 分钟自动保存一次**；关闭页面时通过 sendBeacon 兜底保存；
  状态栏显示"有未保存的更改 / 已保存 / 没有更改需要保存"
- 顶栏「＋ 新建」可再开一个便签
- **插入图片**：编辑器工具栏图片按钮 / 粘贴 / 拖拽，图片上传到 Worker 存于 KV
  （按文件头魔数校验真实格式，限 png/jpg/gif/webp/avif，单张 ≤ 8MB），
  以 `/i/<随机名>.<扩展名>` 插入 Markdown 并即时渲染
- **Markdown 编辑器**（[Vditor](https://b3log.org/vditor/)，MarkText 同款所见即所得范式）：
  - 默认**所见即所得**模式：内容以渲染后的效果呈现（标题、加粗、列表、表格、代码高亮…），点击任意处直接编辑
  - 工具栏「编辑模式」📝 可切换到**即时渲染**或**分屏编辑**（左侧原始 Markdown 源码、右侧实时预览）
  - 工具栏「预览」👁 切换为只读预览
  - 数据库保存的是**原始 Markdown 文本**，旧便签（纯文本）可直接当 Markdown 继续编辑
  - 支持粘贴图片（以 base64 内联进 Markdown）
- **编辑器显示设置**（随便签保存，分享链接任何人打开显示效果一致）：
  - `A−` / `A+` 缩放整个编辑区（10–40px 基准，默认 16），快捷键 `Ctrl/Cmd + -` / `Ctrl/Cmd + =`，`Ctrl/Cmd + 0` 复位
  - 取色器设置文字颜色，「默认」恢复主题默认色（跟随亮/暗模式，代码块高亮不受影响）

## API

| 方法 | 路径 | 说明 |
|---|---|---|
| POST | `/api/login` | 登录 `{username,password}`，成功设置 HttpOnly Cookie（session 存 KV） |
| POST | `/api/logout` | 登出，删除服务端 session 与 Cookie |
| GET | `/api/whoami` | 当前登录用户，未登录 401 |
| GET | `/api/note/{id}` | 读取便签，返回 `{"id","content","font_size","color","title","tags","author","updated_at"}` |
| PUT | `/api/note/{id}` | 保存便签（`author` 由服务端取当前登录用户），响应回显完整便签 JSON |
| POST | `/api/note/{id}` | 同上（sendBeacon 兼容） |
| GET | `/api/notes` | 列出所有便签元数据 `{notes:[{id,title,tags,updated_at,excerpt,author}]}`，按更新时间倒序 |
| POST | `/api/presence` | 协作心跳 `{note_id}`，返回该便签当前在线用户 `{users:[...]}`（KV 90s TTL） |
| POST | `/api/upload` | 图片上传（multipart 字段 `file[]`），返回 Vditor 协议 `{code,data:{succMap,errFiles}}` |
| GET | `/i/{name}` | 提供已上传图片（需登录；`Cache-Control: immutable`） |

- ID 规则：`[a-zA-Z0-9_-]{1,64}`，其余路径返回 404
- 除登录/登出/whoami 外所有 API（含图片）都需要登录，未认证返回 401
- `content`：原始 Markdown 字符串，无格式上限（建议 < 100KB）
- `font_size`：整数，越界自动收敛到 10–40，缺省为 16
- `color`：`#rrggbb` 十六进制，非法值视为默认（空串 = 跟随主题）
- `title`：字符串，最长 200 字符（自动去首尾空格）
- `tags`：字符串数组，自动去重/去空白，最多 10 个、单个最长 24 字符
- 用户密码哈希：`sha256("用户名:密码")` 十六进制，由 migrations/0003-users.sql 内置

## 项目结构

```
rust/
  shared/        # 前后端共用的 API 类型与校验逻辑（JSON 契约与 Go 版一致）
  worker/        # Rust Cloudflare Worker（workers-rs，编译为 WASM）
  frontend/      # Rust + Leptos 前端（Trunk 构建；Vditor 运行时资源从 npm 包拷入 static/）
main.go          # 旧 Go Worker：API 路由 + SPA 回退（保留，make build-go 可构建）
frontend/        # 旧 Vue 3 前端（保留，供回滚）
schema.sql       # D1 建表语句（含 editor settings 列）
migrations/      # 已部署数据库的增量迁移
wrangler.toml    # Worker / D1 / Assets 配置
Makefile         # build（Rust）/ build-go（Go）/ dev / deploy / 数据库初始化
archive/         # 旧版代码留档（文件存储版 / MySQL+Docker 版）
```
