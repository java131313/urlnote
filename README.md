# urlnote

极简 URL 便签：每个网址路径就是一个便签，打开即写，自动保存。

整个应用运行在 Cloudflare 上，无需自建服务器：

- **前端** — Rust + [Leptos](https://leptos.dev/) 编译为 WebAssembly（[Trunk](https://trunkrs.dev/) 构建），由 Cloudflare Workers Static Assets 托管；Markdown 编辑器为 Vditor（JS 库，通过 wasm-bindgen 驱动）
- **后端** — Rust 编译为 WebAssembly，运行在 Cloudflare Worker 上（[workers-rs](https://github.com/cloudflare/workers-rs) SDK）
- **数据库** — Cloudflare D1（SQLite）
- **缓存/会话/图片** — Workers KV（session、presence 心跳、上传图片；响应缓存由进程内 LRU 承担）

```
浏览器 ──▶ Cloudflare Edge
            ├─ 命中静态文件 ──▶ Workers Static Assets（Leptos 应用 + Vditor 运行时资源）
            └─ /api/*、/:id ──▶ Rust(WASM) Worker ──┬─ GET：进程内 LRU 命中直接返回（miss 回源 D1 并回填）
                                                    ├─ PUT/POST：写 D1 并更新 LRU
                                                    └─ /:id ──▶ index.html（ASSETS binding）
```

**缓存一致性**：默认的 Rust Worker 用进程内 LRU（512 条，便签 TTL 30s、索引 TTL 15s，
D1 兜底）缓存响应，写操作先落 D1 再更新/失效缓存，不会出现脏读；响应头 `X-Cache: HIT/MISS`
可观察命中。绕过 API 直接改 D1（如 `wrangler d1 execute`）时，最坏要等一个 TTL 才会看到新值。

## 部署

### 生产架构(Sealos + GitHub Pages,无需本地部署)

```
浏览器 ──▶ github.io 静态前端(Leptos WASM,.github/workflows/deploy-pages.yml)
              │  fetch(API_BASE, 跨域带 Cookie)
              ▼
         Sealos:urlnote-server 应用(镜像 ghcr.io/java131313/urlnote-server)
              │  MONGODB_URI
              ▼
         Sealos:MongoDB 应用 ── GridFS:便签正文/标记/图片;集合:users/sessions/dict
```

两个镜像由 `.github/workflows/docker-ghcr.yml` 在推送 `go-back` 时自动构建并
推到 GitHub Packages(:latest 与提交 SHA 两个标签):

| 镜像 | 内容 |
|---|---|
| `ghcr.io/java131313/urlnote-frontend` | Leptos WASM 前端 + 词表/IPADIC 词典(nginx,:80);备选托管方式,生产用 github.io 即可 |
| `ghcr.io/java131313/urlnote-server` | urlnote-server 纯 API(+ IPADIC 日文分词,:8080) |

**1. Sealos:部署 MongoDB**

应用商店安装 MongoDB(或新建 mongo:7 应用),记录连接信息
(用户名/密码/集群内主机名,形如 `mongo-xxx.ns.svc`)。

**2. Sealos:部署后端 urlnote-server**

新建应用 → 镜像 `ghcr.io/java131313/urlnote-server:latest` → 环境变量:

| 变量 | 示例 | 说明 |
|---|---|---|
| `MONGODB_URI` | `mongodb://root:密码@<mongo主机>:27017/?authSource=admin` | 指向 Sealos 的 MongoDB |
| `MONGO_DB` | `urlnote` | 数据库名 |
| `ADMIN_PASSWORD` | 强密码 | 首次启动创建 admin(默认 root@123,务必改) |
| `CORS_ORIGINS` | `https://java131313.github.io` | 跨域来源,多个用逗号;**必须**含 Pages 地址 |
| `PORT` | `8080` | 监听端口 |

暴露 8080,Sealos 会分配 https 域名(跨站登录要求后端 HTTPS,平台已提供,
无需改代码)。首次启动后在应用终端执行一次(幂等,可重复):

```bash
/app/urlnote-server seed     # 导入四语词典(70 组对照 + 6 万高频词)
```

**3. GitHub:配置并自动部署前端到 github.io**

推送 `go-back` 分支即触发 `.github/workflows/deploy-pages.yml`:
worker/frontend WASM 编译检查 → trunk 构建(注入 API_BASE)→ 推 `gh-pages`
分支 → `https://java131313.github.io/urlnote`。

仓库变量(Settings → Secrets and variables → Actions → **Variables**):

| 变量 | 示例 | 说明 |
|---|---|---|
| `API_BASE` | `https://urlnote-server.xxx.sealosbja.site` | 第 2 步 Sealos 分配的后端域名,不带尾斜杠;改后任意推送即重新生效 |
| `BASE_PATH` | `/urlnote` | 子路径部署才需要,默认可留空 |

首次登录 `admin / <ADMIN_PASSWORD>`。

> 备选:`docker-compose.yml`(mongo+server+web)仅作本地/其他主机自托管参考,
> 生产环境按上面三节部署即可。

### 多前端来源(CORS 配置)

后端 `CORS_ORIGINS` 是**逗号分隔的精确 Origin 列表**(自动去空白和尾斜杠),
要放行几个前端就填几个:

```bash
CORS_ORIGINS=https://urlnote.idiom.ccwu.cc,https://java131313.github.io,https://urlnote-frontend.xxx.sealosbja.site
```

| 前端托管 | Origin(填进 CORS_ORIGINS) | 该前端的 API 指向 |
|---|---|---|
| Cloudflare(wrangler) | `https://urlnote.idiom.ccwu.cc` | 默认同源 → 走 **Worker+D1,独立数据**;要让这个前端也连 MongoDB 后端,需用 `API_BASE=https://<sealos server>` 重新构建前端再部署 |
| GitHub Pages | `https://java131313.github.io` | 仓库变量 `API_BASE`(deploy-pages.yml 构建期注入) |
| Sealos 前端应用 | `https://<sealos 分配的域名>` | 同源部署可留空;跨端口/域名时填 server 地址 |

注意:CORS 只管"浏览器允许跨域访问";每个前端最终连哪个后端由各自的
`API_BASE` 决定,两套后端(Cloudflare D1 / MongoDB)的数据互不相通。

## 本地开发

```bash
make init-db-local   # 首次：初始化本地 D1
make dev             # wrangler dev，http://localhost:8787
```

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
| GET | `/api/dict/langs` | 语言表 `{langs:[{code,name,badge}]}`（编辑器语言选择器选项） |
| GET | `/api/dict/lookup?q=&lang=` | 词典查询：`lang` 指定在某语言下前缀匹配（留空=不限语言），精确命中优先（≤8 条），返回 `{words:[{id,group_id,lang,word,trans:{语言code:译词}}]}`（编辑器提示用） |
| POST | `/api/dict/langs` | 新增语言 `{code,name,badge}`（admin，code 为小写字母/数字/下划线） |
| GET | `/api/dict/words?q=&lang=` | 词表列表/过滤（admin，≤500 条） |
| POST | `/api/dict/words` | 新增词 `{lang,word,group_id?,trans?}`（admin；不带 group_id 时新建词义组，`trans` 可附带其它语言译词） |
| PUT | `/api/dict/words/{id}` | 改词/移动到其它词义组（admin） |
| DELETE | `/api/dict/words/{id}` | 删除词（空组自动清理）（admin） |
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
  shared/        # 前后端共用的 API 类型与校验逻辑
  worker/        # Rust Cloudflare Worker（workers-rs，编译为 WASM）
  frontend/      # Rust + Leptos 前端（Trunk 构建；Vditor 运行时资源从 npm 包拷入 static/）
                 #   src/suggest.rs 四语词典提示（便签与 /oss 文件库编辑器共用）
  server/        # 独立后端（axum + MongoDB）：正文/标记/图片走 GridFS，结构数据走集合
                 #   cargo run -p urlnote-server [seed]；docker-compose 一键起
frontend/        # 旧 Vue 3 前端（留档）
schema.sql       # D1 建表语句（含 editor settings 列与四语词典 dict 表）
migrations/      # 已部署数据库的增量迁移
scripts/gen_dict_seed.py  # 生成 dict_seed.sql（70 组四语对照 + 6 万静态高频词入库）
scripts/dict_seed.sql     # 生成的种子（INSERT OR IGNORE + 稳定 key，可重复执行）
scripts/dict_translations.json  # 70 组四语对照（SQL 种子与 MongoDB 播种共用）
.github/workflows/deploy-pages.yml  # 构建前端 WASM 并部署到 github.io
Dockerfile         # 多阶段：前端 WASM + 后端 + IPADIC 词典 → 单镜像
docker-compose.yml # mongo + app 一键栈（含健康检查、数据卷）
wrangler.toml      # 备选：Rust Worker / D1 / Assets 配置
Makefile         # build（Rust）/ dev / deploy / 数据库初始化
archive/         # 旧版代码留档（文件存储版 / MySQL+Docker 版）
```
