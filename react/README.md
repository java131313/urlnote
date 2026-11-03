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

## 开发

```bash
npm ci
API_TARGET=https://yyuqmndqpnjr.sealosbja.site npm run dev   # http://localhost:5173
# API_TARGET 缺省为 http://localhost:8080(本地 server)
```

`/api` 与 `/i` 由 Vite 代理到 API_TARGET(同源,cookie 无跨域问题)。

## 生产构建

```bash
VITE_API_BASE=https://yyuqmndqpnjr.sealosbja.site npm run build   # 产物在 dist/
```

跨域部署时后端需把前端来源加入 `CORS_ORIGINS`。`VITE_BASE_PATH` 可设子路径。
`public/vditor` 由 `prebuild` 自动从 node_modules 拷贝(Vditor 运行时资源)。

## 目录

```
src/api.ts       # 全部接口封装(Note/NoteMeta/DictLang/DictWord)
src/App.tsx      # 登录态 + 哈希路由(#/<noteId>)
src/Editor.tsx   # Vditor 封装:装载/自动保存/字号颜色/语言选择
src/suggest.tsx  # 词典提示:片段提取、防抖查询、键盘导航、锚点替换
src/Picker.tsx   # 便签列表 + 搜索
src/Login.tsx    # 登录表单
```
