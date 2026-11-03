# urlnote-react

基于 `rust/server`(urlnote-server)API 的 React 界面。Vite + React 18 + TypeScript,
编辑器为 Vditor(与 Rust 前端相同的 JS 库),契约逐字段一致。

## 已实现

- 登录 / 退出(会话 cookie,`whoami` 探测)
- 便签编辑器:打开即写、800ms 防抖自动保存、标题/标签、字号(A−/A+,CSS zoom)
  与文字颜色(`--note-color`,存 markdown 的是 class 语义由 Vditor 保留 fc- span)
- 便签选择器:全部列表 + 全文搜索(`/api/search` 词级匹配)
- 词典提示:输入防抖 200ms 查 `/api/dict/lookup?q=&lang=`,语言下拉框
  (localStorage 持久化,默认 `VITE_SUGGEST_LANG`),↑↓/Enter/Tab/Esc 键盘导航,
  接受后替换锚点文字;精确命中额外给出其它语言的可插行
- 图片上传走 `/api/upload`(Vditor 协议,field `file[]`)
- 文档库 `#/oss`(与 Rust 前端 /oss 同功能):Sealos 对象存储文件浏览 +
  markdown 编辑,浏览器内 SigV4 直连 S3(列表分页/增删改/目录树),
  图片灯箱预览、PDF 经 `/api/pdf2md` 转 md 打开、字号/颜色存
  `<key>.settings.json` sidecar、工具栏文字颜色(fc- span)与批注
  (锚定文字 + 侧栏,数据走 `/api/comments`,doc 为 `oss/<key>`)

## 开发

```bash
npm ci
API_TARGET=https://yyuqmndqpnjr.sealosbja.site npm run dev   # http://localhost:5173
# API_TARGET 缺省为 http://localhost:8080(本地 server)
```

`/api` 与 `/i` 由 Vite 代理到 API_TARGET(同源,cookie 无跨域问题)。

对象存储凭据放进 `react/.env.local`(Vite 只暴露 `VITE_` 前缀变量,
构建期静态替换,与 Rust 前端的编译期 `option_env!` 同机制):

```
VITE_OSS_ENDPOINT=objectstorageapi.bja.sealos.run   # 缺省值
VITE_OSS_AK=...        # 不设则 /oss 页显示“未配置”
VITE_OSS_SK=...
VITE_OSS_BUCKET=...
VITE_OSS_REGION=us-east-1                           # 缺省值
```

注意:AK/SK 会编译进公开的前端包(与 Rust 版同样的安全模型,仅个人使用)。

## 测试

```bash
npm test     # vitest:SigV4 签名(与 Rust 版共用 Python 参考实现生成的测试
             # 向量,逐字节一致)+ SHA-256/HMAC RFC 向量 + 目录树构建
```

## 生产构建

```bash
VITE_API_BASE=https://yyuqmndqpnjr.sealosbja.site npm run build   # 产物在 dist/
```

跨域部署时后端需把前端来源加入 `CORS_ORIGINS`。`VITE_BASE_PATH` 可设子路径。
`public/vditor` 由 `prebuild` 自动从 node_modules 拷贝(Vditor 运行时资源)。

## 目录

```
src/api.ts           # 全部接口封装(便签/词典/批注/PDF 转换)
src/App.tsx          # 登录态 + 哈希路由(#/<noteId>,保留字 #/oss → 文档库)
src/Editor.tsx       # 便签 Vditor 封装:装载/自动保存/字号颜色/语言选择
src/suggest.tsx      # 词典提示:片段提取、防抖查询、键盘导航、锚点替换
src/Picker.tsx       # 便签列表 + 搜索
src/Login.tsx        # 登录表单
src/oss/             # 文档库(rust/frontend/src/oss.rs + comment.rs + vditor.rs 的移植)
  config.ts          # VITE_OSS_* 配置与 OSS_ENABLED 门
  sigv4.ts           # 纯 TS SHA-256/HMAC + SigV4 签名(同步、可单测)
  s3.ts              # GET/PUT/DELETE/HEAD/LIST(分页)+ ListObjectsV2 XML 解析
  tree.ts            # 扁平对象列表 → 目录树(展开状态按前缀继承)
  editorTools.ts     # 选区暂存/文字着色(fc- span)/批注锚点包装 + 两个工具栏按钮
  comments.tsx       # 批注控制器(选区偏移定位/跳转/闪烁)+ 侧栏组件
  OssPage.tsx        # 页面:树/编辑器/文件操作/图片灯箱/PDF 转 md/设置 sidecar
```
