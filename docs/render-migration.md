# Render 免费档部署 + Mongo→PG(Supabase)数据迁移

架构(单 Pod,全免费):

```
浏览器 ──HTTPS──► urlnote (Render 免费 Web,Oregon;单容器)
                   ├─ FerretDB 127.0.0.1:27017(Mongo 线协议 → PG)
                   ├─ urlnote-server :8080(API + /i 图片 + 词典分词)
                   └─ 静态托管 Rust/Leptos 前端(STATIC_DIR,SPA 回退)
                            │
                            ▼  postgres wire over IPv4(Session Pooler)
                   Supabase 免费 PostgreSQL(500MB,数据真身)
```

服务端代码零改动:仍用 mongodb driver + GridFS,FerretDB 负责翻译。

**已实测通过**:FerretDB 1.24 ↔ Supabase Session Pooler 全链路
(建索引/GridFS 读写/token 搜索/登录会话/词典/分词)。

⚠️ 踩过的坑(都验证过):
- **必须 Session Pooler 串**(`*.pooler.supabase.com:5432`,用户名带 `.项目ref` 后缀)。
  Direct 串(`db.*.supabase.co:5432`)是 IPv6-only,Render 出站只有 IPv4,连不上。
- 密码含 `@` 必须百分号编码:`Root@131313@super` → `Root%40131313%40super`。
- **Neon 与 FerretDB 不兼容**(其代理对 Go/pgx 客户端握手无响应,超时;libpq 正常)。已排除。
- Supabase 项目区域以控制台为准(本项目实际在东京 ap-northeast-1,aws-1 集群)。

## 0. 准备(已完成)

- Supabase 项目:`pyrpqmfwzyvbcozqqozx`(东京),Session Pooler 串已配置
- Render:免费 web 服务,仓库 `java131313/urlnote` 分支 `go-back`

## 一、部署(Render API / Dashboard 二选一)

### 方式 A:Dashboard → New → Web Service

- 仓库/分支同上,Runtime: Docker,Dockerfile path: `Dockerfile.render`
- Plan: Free,Region: Oregon(免费只有这个)
- 环境变量(照 render.yaml):
  `FERRETDB_POSTGRESQL_URL` = Session Pooler 串(密码编码后)
  `MONGO_DB=urlnote` `ADMIN_PASSWORD=<强密码>` `CORS_ORIGINS=https://java131313.github.io`
  `OFFICE_SERVER_URL=https://qkrvktsrtwmd.sealosbja.site`

### 方式 B:REST API

```bash
curl -s -X POST -H "Authorization: Bearer $RENDER_API_KEY" -H 'Content-Type: application/json' \
  -d '{
    "type":"web","name":"urlnote","ownerId":"<workspaceId>",
    "repo":"https://github.com/java131313/urlnote","branch":"go-back",
    "serviceDetails":{
      "region":"oregon","plan":"free","runtime":"docker",
      "dockerfilePath":"Dockerfile.render","healthCheckPath":"/",
      "envVars":[{"key":"FERRETDB_POSTGRESQL_URL","value":"<pooler 串>"},
                 {"key":"MONGO_DB","value":"urlnote"},
                 {"key":"ADMIN_PASSWORD","value":"<强密码>"},
                 {"key":"CORS_ORIGINS","value":"https://java131313.github.io"},
                 {"key":"OFFICE_SERVER_URL","value":"https://qkrvktsrtwmd.sealosbja.site"}]
    }}' https://api.render.com/v1/services
```

首次构建约 20~30 分钟(rust + wasm 全量编译)。

## 二、数据迁移(Sealos Mongo → Supabase PG)

一次性,本地跑。需要 `brew install mongodb-database-tools`。

```bash
# 1. 从 Sealos 拉全量 dump(内网域名不通就先 kubectl 端口转发到 27099)
mongodump --uri="mongodb://root:64LFF652zQ2I52DZ@127.0.0.1:27099/?authSource=admin" \
  --out=./urlnote-dump

# 2. 本地起一次性 FerretDB 指向 Supabase,灌库
docker run -d --name fdb-mig -p 127.0.0.1:27099:27017 \
  -e FERRETDB_POSTGRESQL_URL='<pooler 串>' \
  -e FERRETDB_LISTEN_ADDR=0.0.0.0:27017 \
  ghcr.io/ferretdb/ferretdb:1.24

# 3. 灌库(--drop:同名集合先清再导,幂等可重跑)
mongorestore --uri="mongodb://127.0.0.1:27099/urlnote" --drop ./urlnote-dump/urlnote

# 4. 核对后清场
docker rm -f fdb-mig
```

注意:若本机 DNS 解析不了 `*.pooler.supabase.com`(国内 ISP DNS 污染),
给 fdb-mig 加 `--add-host "<pooler 主机>:<IP>"`(IP 用 `curl 'https://dns.google/resolve?name=<host>&type=A'` 查)。

## 三、迁移后验证

- [ ] `https://urlnote-xxxx.onrender.com/` 打开能登录(admin / ADMIN_PASSWORD)
- [ ] 旧笔记列表完整、正文(走 GridFS)能打开、全文搜索有结果
- [ ] Supabase Dashboard → Database → 有 `urlnote` schema 和一堆 `*_xxxxxxxx` 表
- [ ] 词典 seed(幂等):Dashboard → urlnote → Shell → `/app/urlnote-server seed`
- [ ] 冷启动体验:闲置 15 分钟后首访 ~10s(Render 容器唤醒),Neon 侧无此问题

## 四、免费档红线与对策

| 限制 | 对策 |
|---|---|
| Render 免费 Web 15 分钟无访问休眠 | 首访 ~10s 冷启动;或 UptimeRobot 定时 ping(灰色,慎用) |
| Supabase 500MB | 文本笔记随便装;**图片别走 GridFS**,继续用 Sealos S3(/oss 页本来就连 S3) |
| Render→东京 DB 延迟 ~120ms | 已实测可接受;页面加载多 1-2s |
| Render 免费 750 小时/月 | 单服务全月 744h,刚好够;别开第二个免费服务 |
| 无备份 | pg_dump 定期导出存 GitHub 私有仓库,见下 |

pg_dump 备份:

```bash
docker run --rm postgres:16 pg_dump "postgresql://postgres.<ref>:<pwd>@<pooler>:5432/postgres" \
  -n urlnote -Fc -f /dev/stdout > urlnote-$(date +%F).dump 2>/dev/null
# (走 pooler 时需要先加 --add-host;或从有 IPv6 的机器走 direct 串)
```

## 五、升级路径(以后想付费/换区域)

- 数据在 Supabase PG,想换部署平台/区域随时 `pg_dump`/`mongorestore` 搬走
- 加卡后可把 Render 服务升 Starter(选新加坡),延迟降到 ~30ms
- FerretDB 独立 pserv 方案(与 Render 付费 PG 搭配)见 git 历史(`render.yaml` @ `86c4ef1`)
