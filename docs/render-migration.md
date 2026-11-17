# Render(新加坡)部署与 Mongo→PG 数据迁移

整套栈跑在 Render,区域 `singapore`:

```
浏览器 ──HTTPS──► urlnote (Web,单 Pod)
                   ├─ 静态托管 Rust/Leptos 前端(STATIC_DIR,/api 之外 SPA 回退)
                   ├─ urlnote-server(API + /i 图片 + 词典分词)
                   └─ mongodb driver ──► ferretdb (私有服务, :27017)
                                           └─ FerretDB 把文档写进 ──► urlnote-pg (托管 PG 16)
```

服务端代码零改动:仍用 mongodb driver + GridFS(notecontents / markcontents / images),
FerretDB 负责把线协议翻译成 PG 表。兼容性已实测通过(GridFS 读写、token 搜索、TTL/唯一索引)。

## 一、部署(Render Dashboard)

1. **New → Blueprint** 选本仓库,Apply `render.yaml`。会创建:
   - `urlnote-pg` — 托管 PostgreSQL 16(Starter,数据真身)
   - `ferretdb` — 私有服务,镜像 `ghcr.io/ferretdb/ferretdb:1.24`
   - `urlnote` — Web 服务,Dockerfile.render 单 Pod(前端 + 后端)
2. 部署完成后记下 `https://urlnote-xxxx.onrender.com`(或绑自定义域名)。
3. `ADMIN_PASSWORD` 在 Blueprint 里是 `generateValue` 自动生成的随机串,
   到 Dashboard → urlnote → Environment 里改成你想要的(只影响**首次**建 admin)。
4. (可选)把 GitHub Pages 那份前端的 API_BASE 指过来 → CORS_ORIGINS 里加
   `https://java131313.github.io` 即可(render.yaml 已默认带上)。

## 二、数据迁移(Sealos Mongo → FerretDB/PG)

一次性,本地跑。前提:本机有 `mongodump`/`mongorestore`
(`brew install mongodb-database-tools`)。

```bash
# 1. 从 Sealos Mongo 拉全量 dump
#    内网域名连不上就先用 kubectl/Sealos 控制台做端口转发到本地 27099
mongodump \
  --uri="mongodb://root:64LFF652zQ2I52DZ@127.0.0.1:27099/?authSource=admin" \
  --out=./urlnote-dump
#  dump 目录结构:urlnote-dump/urlnote/<collection>.bson + .metadata.json
```

**导入通道三选一**(FerretDB 在 Render 私网,本机不可直达):

### 方案 A:临时开公网隧道(最快,迁完即关)

1. Dashboard → `ferretdb` → 临时改成 **Web** 类型服务(或新建一个指向同镜像的
   Web 服务 `ferretdb-migrate`,环境变量同 ferretdb),Render 会给出
   `https://ferretdb-migrate-xxxx.onrender.com`。
2. Render 只代理 HTTP,所以本地用 `mongorestore --ssl` 不行——
   改用 WebSocket/HTTP 隧道工具(如 ` bore ` 不适用;推荐 **本地 SSH 隧道到任一
   Render Web Pod 再跳私网**,见方案 B)。⚠️ 实践中方案 B 更稳,优先 B。

### 方案 B:SSH 进 urlnote Pod 转发(推荐)

Render 支持 `ssh` 直连 Web 服务实例(实例详情页有 SSH 地址):

```bash
# 本地 27017 转发到 Render 私网里的 ferretdb:27017
ssh -L 27017:ferretdb:27017 <urlnote-pod-ssh地址> -N
# 另一个终端:
mongorestore --uri="mongodb://127.0.0.1:27017/urlnote" --drop ./urlnote-dump/urlnote
```

`render ssh` CLI 也支持同样转发:`render ssh <service-id> -- -L 27017:ferretdb:27017 -N`。

### 方案 C:导入完成后清场

- 方案 A 用的临时 Web 服务删掉。
- 验证数据:`docker run --rm -it mongo:7 mongosh "mongodb://..."` 或直接开站看笔记。

## 三、迁移后验证清单

- [ ] `https://<域名>/` 打开,能登录(admin / 你设的密码)
- [ ] 旧笔记列表完整、正文(走 GridFS)能打开、全文搜索有结果
- [ ] 图片 `/i/...` 正常(图片在 KV,若仍走 Cloudflare Worker 则不受影响)
- [ ] Dashboard → `urlnote-pg` → 看表:`notecontents.files/chunks`、`notes`、`dict_word`…
- [ ] 词典 seed(迁移后建议补一次,幂等):
      Dashboard → urlnote → Shell:
      `/app/urlnote-server seed`

## 四、费用与注意

- 新加坡没有免费档:`urlnote-pg` + `ferretdb` + `urlnote` 三个 Starter ≈ $21/月。
- FerretDB 1.24 是 1.x 末版(2.x 需要 PG 装 DocumentDB 扩展,Render 托管 PG 不支持)。
  组件只在 Render 私网内监听,不暴露公网;锁定不升级即可长期稳定。
- 单 Pod 意味着发新版时有几秒不可用;介意的话把 `urlnote` 升 Standard 开多实例
  (无状态,可以水平扩;会话在 Mongo 侧即 PG 侧)。
