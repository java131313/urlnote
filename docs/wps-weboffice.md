# WPS 云盘接入方案:Office 在线编辑(WebOffice)

> 状态:待实施的设计文档。目标读者:维护者本人(个人单用户部署)。
> 所有 WebOffice API 细节(签名算法、回调字段、额度)以 wwo.wps.cn 官方文档为准,
> 实施前先注册开发者账号核实。

## 1. 目标与范围

urlnote 文件库(`#/oss`)里的 **docx / xlsx / pptx** 点击后,在编辑器区域直接
用 WPS 在线预览/编辑(iframe),保存后内容**回写到 S3 原文件**。

- 范围:Office 三件套在线编辑;不做 WPS 云盘目录浏览(那是另一套
  open.wps.cn 云盘 API,本期不做)。
- 用户:个人单用户,不做多账号绑定。

## 2. 总体架构

```
浏览器(react / rust 前端)
   │  ① POST /api/office/open   {key, mode: preview|edit}
   ▼
urlnote-server(rust 新模块 office.rs,持有 appid/secret)
   │  ② S3 GET 原文件(复用 media.rs 的 SigV4 代理逻辑)
   │  ③ 上传 WebOffice 云存储(wwo file/upload,签名)
   │  ④ 生成预览/编辑链接(wwo file/link,签名,短链有过期)
   ▼
前端拿到 {url} → ⑤ <iframe src=url> 占满编辑器区域
   │
   │  ⑥ 用户在 WPS 编辑器里保存
   ▼
WebOffice 回调 POST /api/office/callback(验签)
   │  ⑦ 按 file_id 下载最新版本内容
   │  ⑧ 写回 S3 原 key(putObject)
   ▼
⑨ 前端轮询状态,提示"已保存新版本"(或自动刷新编辑器内容)
```

要点:**appid/secret 只存服务端环境变量**,前端只拿到一次性编辑链接;
所有 wwo 请求由后端发出。回调公网地址用 Sealos 后端域名。

## 3. 后端接口设计(rust/server,worker 后续对齐)

| 方法 | 路径 | 说明 |
|---|---|---|
| POST | `/api/office/open` | body: `{key, mode}`。key→S3 object key。返回 `{url}`。内部:file_id 缓存命中直接 ④,否则 ②③④ |
| GET | `/api/office/status?key=` | 返回 `{version, saved_at}`,前端轮询感知"WPS 已保存" |
| POST | `/api/office/callback` | WebOffice webhook,验签后按 file_id 拉新版本回写 S3,更新 mongo 记录 |

### 存储(mongo)

新集合 `office_files`:

```json
{ "_id": "oss/<key>", "file_id": "<wwo file id>", "version": 3,
  "updated_at": "...", "size": 12345 }
```

key ↔ file_id 映射缓存,避免每次打开都重复上传;文件在 S3 被改动后
(编辑器保存/上传覆盖)比较 size/updated_at 或直接删除映射强制重传。

### 环境变量

```
WPS_OFFICE_APPID=        # wwo.wps.cn 应用
WPS_OFFICE_SECRET=
```

### 签名与回调(实施时对照官方文档核实)

- wwo 请求的 `_w_sign`:参数按 key 排序拼接后 SHA1(算法细节以文档为准),
  建议先在 `office.rs` 写单测对齐官方给出的签名示例。
- 回调同样验 `_w_sign`;回调体里取 file_id,再调下载接口拿内容。

## 4. 前端交互(react 与 rust 对齐,复用媒体播放器面板模式)

1. `fileKind` 增加 `office`(docx/xlsx/pptx),图标 📘。
2. 文件树点击 office 文件 → 编辑器区域**隐藏不卸载**,换成编辑面板:
   - 顶栏:文件名 + 模式切换(预览/编辑)+ 保存状态灯 + ✕ 返回编辑;
   - 主体:`<iframe src={url}>` 占满。
3. 面板打开时 `POST /api/office/open`;iframe 加载失败(链接过期)时
   提供"重新打开"按钮重新走一遍。
4. 状态轮询:编辑模式下 5s 轮询 `/api/office/status`;version 变化 →
   顶栏提示"WPS 已保存新版本":
   - 若该文件同时打开在 markdown 编辑器且**未脏** → 静默重载内容;
   - 已脏 → 提示冲突,由人决定覆盖方向(初期简单处理)。

## 5. 里程碑

| 里程碑 | 内容 | 预估 |
|---|---|---|
| M0 资质 | wwo.wps.cn 注册个人开发者、实名、建应用、配回调域名 | 半天(含审核等待) |
| M1 预览 | 上传 + 预览 link + iframe 面板(只读) | 1~2 天 |
| M2 编辑回写 | 编辑 link + 回调验签 + 下载回写 S3 + 状态轮询 | 2~3 天 |
| M3 打磨 | 去重缓存、脏冲突提示、worker(CF)对齐、文档 | 1 天 |

## 6. 风险与退路

- **个人开发者权限/额度未知**:WebOffice 历史上偏企业向,个人开发者能否
  开通、免费额度多少,需要 M0 先验证。若个人不可开通 → 退路:
  - 退到只读预览(额度要求低);
  - 或改用 open.wps.cn 云盘 API 管理文件 + 其 WebOffice 能力;
  - 或放弃 WPS,评估 OnlyOffice Document Server 自托管(社区版免费,
    但要自己跑一个服务,Sealos 上可部署)。
- **回调可达性**:Sealos 免费域名随应用重建会变,回调域名变了要去
  wwo 后台改并等待生效;CF worker 域名稳定但接 webhook 要另一套。
- **大文件**:wwo 对单文件大小/格式有限制,超大 docx 可能传不上去,
  错误要友好提示。
- **链接过期**:file/link 是短链有过期时间,面板长时间挂着会失效,
  靠"重新打开"兜底。
