//! ONLYOFFICE Document Server(DocsAPI)集成的文档编辑/保存通道。
//!
//! - `GET /api/office/config?key=`(会话鉴权):返回 `{ds, config}`,前端加载
//!   `${ds}/web-apps/apps/api/documents/api.js` 后 `new DocsAPI.DocEditor(el, config)`。
//!   浏览器直连文档服务器(本机 Docker 或 Sealos 部署);`OFFICE_SERVER_URL` 可指向
//!   任意 ODS 实例。
//! - 打开:`config.document.url` 指向 `/wopi/files/{id}/contents?access_token=..`
//!   (下方 WOPI 子集,token 鉴权,ODS 服务端来拉);`document.key` = id。
//! - 保存:ODS 回调 `POST /api/office/callback?id=..&token=..`(Document
//!   Editing Service 协议,status 2/6 时回拉 body.url 写回 S3)。
//! - 其余 `/wopi/files/{id}` 端点(CheckFileInfo/LOCK 等)为完整 WOPI 宿主实现,
//!   Collabora 直连仍可用(把 OFFICE_SERVER_URL 指回 Collabora 时按需恢复 iframe 模式)。
//!
//! token = `v1.<exp_unix>.<hex hmac>`,密钥为进程启动时的随机值(个人单用户,
//! token 生命周期只覆盖一次编辑会话,重启失效重开即可)。id = url-safe
//! base64(S3 key)。

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::body::Body;
use axum::response::Response;
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::auth::require_user;
use crate::db::AppState;
use crate::s3ops;
use shared::ErrorResponse;

type HmacSha256 = Hmac<Sha256>;

fn token_secret() -> &'static [u8] {
    static SECRET: OnceLock<Vec<u8>> = OnceLock::new();
    SECRET.get_or_init(|| {
        use rand::RngCore;
        let mut b = vec![0u8; 32];
        rand::thread_rng().fill_bytes(&mut b);
        b
    })
}

fn mac(msg: &str) -> String {
    let mut m = <HmacSha256 as Mac>::new_from_slice(token_secret()).expect("hmac key");
    m.update(msg.as_bytes());
    m.finalize().into_bytes().iter().map(|x| format!("{x:02x}")).collect()
}

const TOKEN_TTL_SECS: u64 = 12 * 3600;

fn mint_token(id: &str) -> (String, u64) {
    let exp = chrono::Utc::now().timestamp() as u64 + TOKEN_TTL_SECS;
    (format!("v1.{exp}.{}", mac(&format!("{id}|{exp}"))), exp * 1000)
}

fn valid_token(id: &str, token: &str) -> bool {
    let mut it = token.split('.');
    if it.next() != Some("v1") {
        return false;
    }
    let Ok(exp) = it.next().unwrap_or_default().parse::<u64>() else {
        return false;
    };
    let sig = it.next().unwrap_or_default();
    if it.next().is_some() {
        return false;
    }
    exp > chrono::Utc::now().timestamp() as u64 && sig == mac(&format!("{id}|{exp}"))
}

fn id_to_key(id: &str) -> Option<String> {
    URL_SAFE_NO_PAD.decode(id.as_bytes()).ok().and_then(|b| String::from_utf8(b).ok())
}

fn key_to_id(key: &str) -> String {
    URL_SAFE_NO_PAD.encode(key.as_bytes())
}

const OFFICE_EXTS: &[&str] = &["docx", "xlsx", "pptx", "odt", "ods", "odp"];

fn valid_office_key(key: &str) -> bool {
    if key.is_empty() || key.len() > 512 || key.contains("..") || key.starts_with('/') {
        return false;
    }
    let ext = key.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    OFFICE_EXTS.contains(&ext.as_str())
}

fn json_err(status: StatusCode, msg: &str) -> (StatusCode, Json<ErrorResponse>) {
    crate::util::json_error(status, msg)
}

/// 从 WOPI 请求取 token:优先 Authorization 头(Collabora 惯例),
/// 兼容 URL query `access_token`(ONLYOFFICE 惯例)。
fn bearer(headers: &HeaderMap, query_token: Option<&str>) -> Option<String> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| query_token.map(str::to_string))
}

/// 校验 id + token;key 必须落在 office 扩展名内。返回 S3 key。
fn auth_wopi(
    id: &str,
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Result<String, (StatusCode, Json<ErrorResponse>)> {
    let key = id_to_key(id).ok_or_else(|| json_err(StatusCode::NOT_FOUND, "not found"))?;
    if !valid_office_key(&key) {
        return Err(json_err(StatusCode::NOT_FOUND, "not found"));
    }
    match bearer(headers, query_token) {
        Some(t) if valid_token(id, &t) => Ok(key),
        _ => Err(json_err(StatusCode::UNAUTHORIZED, "invalid token")),
    }
}

fn host_origin(headers: &HeaderMap) -> (String, String) {
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost")
        .to_string();
    let scheme = if host.starts_with("localhost") || host.starts_with("127.0.0.1") || host.starts_with("192.168.") {
        "http"
    } else {
        "https"
    };
    (scheme.to_string(), host)
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// 文档服务器地址:OFFICE_SERVER_URL 优先,兼容旧 COLLABORA_URL。
fn office_server_url() -> String {
    std::env::var("OFFICE_SERVER_URL")
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .or_else(|| {
            std::env::var("COLLABORA_URL")
                .ok()
                .map(|v| v.trim().trim_end_matches('/').to_string())
                .filter(|v| !v.is_empty())
        })
        .unwrap_or_else(|| "http://localhost:9980".to_string())
}

/// 扩展名 → ONLYOFFICE 的 documentType(word/cell/slide)。
fn doc_type(key: &str) -> &'static str {
    match key.rsplit('.').next().unwrap_or_default().to_ascii_lowercase().as_str() {
        "xlsx" | "ods" => "cell",
        "pptx" | "odp" => "slide",
        _ => "word",
    }
}

/// GET /api/office/config?key= —— 前端(会话)换取 ONLYOFFICE DocsAPI 配置。
/// 前端拿 {ds, config} 后加载 `${ds}/web-apps/apps/api/documents/api.js`,
/// new DocsAPI.DocEditor(el, config)。文件下载复用 /wopi/files/{id}/contents
/// (token 鉴权);保存走 editorConfig.callbackUrl(见 office_callback)。
pub async fn config(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    require_user(&st, &headers).await?;
    let key = q.get("key").cloned().unwrap_or_default();
    if !valid_office_key(&key) {
        return Err(json_err(StatusCode::BAD_REQUEST, "bad request"));
    }
    let cfg = s3ops::S3Cfg::from_env()
        .ok_or_else(|| json_err(StatusCode::SERVICE_UNAVAILABLE, "media storage not configured"))?;
    if s3ops::object_size(&cfg, &key).await.is_err() {
        return Err(json_err(StatusCode::NOT_FOUND, "not found"));
    }
    let id = key_to_id(&key);
    let (token, _ttl_ms) = mint_token(&id);
    let (scheme, host) = host_origin(&headers);
    let file_name = key.rsplit('/').next().unwrap_or(&key).to_string();
    let file_type = file_name.rsplit('.').next().unwrap_or_default().to_string();
    let base = format!("{scheme}://{host}");
    let ds = office_server_url();
    let payload = serde_json::json!({
        "ds": ds,
        "config": {
            "document": {
                "url": format!("{base}/wopi/files/{id}/contents?access_token={token}"),
                "fileType": file_type,
                "title": file_name,
                "key": id,
            },
            "documentType": doc_type(&key),
            "editorConfig": {
                "callbackUrl": format!("{base}/api/office/callback?id={id}&token={token}"),
                "mode": "edit",
                "lang": "zh-CN",
                "user": { "id": "admin", "name": "admin" },
            },
        },
    });
    Ok(crate::util::json_vec(
        StatusCode::OK,
        serde_json::to_vec(&payload).unwrap(),
    ))
}

/// POST /api/office/callback —— ONLYOFFICE Document Editing Service 回调。
/// status 2(可保存)/6(强制保存)时 body.url 为改后文件,从文档服务器回拉、
/// 回写 S3。token 与 config 签发的一致;url 限定已配置的文档服务器(防 SSRF)。
pub async fn office_callback(
    Query(q): Query<std::collections::HashMap<String, String>>,
    body: axum::body::Bytes,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    let id = q.get("id").cloned().unwrap_or_default();
    let token = q.get("token").cloned().unwrap_or_default();
    let key = id_to_key(&id).ok_or_else(|| json_err(StatusCode::UNAUTHORIZED, "invalid token"))?;
    if !valid_office_key(&key) || !valid_token(&id, &token) {
        return Err(json_err(StatusCode::UNAUTHORIZED, "invalid token"));
    }
    let v: serde_json::Value =
        serde_json::from_slice(&body).map_err(|_| json_err(StatusCode::BAD_REQUEST, "bad json"))?;
    let status = v.get("status").and_then(|s| s.as_i64()).unwrap_or(0);
    if status == 2 || status == 6 {
        let url = v
            .get("url")
            .and_then(|u| u.as_str())
            .unwrap_or_default()
            .to_string();
        let ds = office_server_url();
        if !url.starts_with(&format!("{ds}/")) {
            return Err(json_err(StatusCode::BAD_REQUEST, "bad url"));
        }
        let resp = reqwest::get(&url)
            .await
            .map_err(|e| json_err(StatusCode::BAD_GATEWAY, &e.to_string()))?;
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| json_err(StatusCode::BAD_GATEWAY, &e.to_string()))?;
        let cfg = s3ops::S3Cfg::from_env()
            .ok_or_else(|| json_err(StatusCode::SERVICE_UNAVAILABLE, "storage not configured"))?;
        s3ops::put_object(&cfg, &key, &bytes)
            .await
            .map_err(|e| json_err(StatusCode::BAD_GATEWAY, &e))?;
    }
    // 其余状态(1 编辑中/3 保存错误/4 无改动/7 强制保存错误)一律应答成功。
    Ok(crate::util::json_vec(
        StatusCode::OK,
        serde_json::to_vec(&serde_json::json!({ "error": 0 })).unwrap(),
    ))
}

/// GET /wopi/files/{id} —— CheckFileInfo。
pub async fn check_file_info(
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    let key = auth_wopi(&id, &headers, q.get("access_token").map(String::as_str))?;
    let cfg = s3ops::S3Cfg::from_env()
        .ok_or_else(|| json_err(StatusCode::SERVICE_UNAVAILABLE, "storage not configured"))?;
    let size = s3ops::object_size(&cfg, &key)
        .await
        .map_err(|_| json_err(StatusCode::NOT_FOUND, "not found"))?;
    let file_name = key.rsplit('/').next().unwrap_or(&key).to_string();
    let payload = serde_json::json!({
        "BaseFileName": file_name,
        "Size": size,
        "OwnerId": "admin",
        "UserId": "admin",
        "UserFriendlyName": "admin",
        "UserCanWrite": true,
        "UserCanNotWriteRelative": true,
        "ReadOnly": false,
        "SupportsUpdate": true,
        "SupportsLock": true,
        "SupportsGetLock": true,
        "SupportsExtendedLockLength": true,
        "PostMessageOrigin": "*",
        "LastModifiedTime": now_rfc3339(),
    });
    Ok(crate::util::json_vec(
        StatusCode::OK,
        serde_json::to_vec(&payload).unwrap(),
    ))
}

/// GET /wopi/files/{id}/contents —— 文档服务器拉文件。
pub async fn get_contents(
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    let key = auth_wopi(&id, &headers, q.get("access_token").map(String::as_str))?;
    let cfg = s3ops::S3Cfg::from_env()
        .ok_or_else(|| json_err(StatusCode::SERVICE_UNAVAILABLE, "storage not configured"))?;
    let bytes = s3ops::object_bytes(&cfg, &key)
        .await
        .map_err(|e| json_err(StatusCode::BAD_GATEWAY, &e))?;
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/octet-stream")
        .header("Content-Length", bytes.len())
        .body(Body::from(bytes))
        .expect("body"))
}

thread_local! {
    static LOCKS: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

fn lock_get(id: &str) -> Option<String> {
    LOCKS.with(|l| l.borrow().get(id).cloned())
}

fn lock_set(id: &str, lock: &str) {
    LOCKS.with(|l| {
        l.borrow_mut().insert(id.to_string(), lock.to_string());
    });
}

fn lock_del(id: &str) {
    LOCKS.with(|l| {
        l.borrow_mut().remove(id);
    });
}

fn wopi_json(payload: serde_json::Value) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::to_vec(&payload).unwrap()))
        .expect("body")
}

fn header_val(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

/// POST /wopi/files/{id} —— X-WOPI-Override 分发:保存 / LOCK / UNLOCK 等。
pub async fn wopi_post(
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<std::collections::HashMap<String, String>>,
    body: axum::body::Bytes,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    let key = auth_wopi(&id, &headers, q.get("access_token").map(String::as_str))?;
    let ov = header_val(&headers, "x-wopi-override").unwrap_or_default();
    match ov.to_ascii_uppercase().as_str() {
        "PUT" | "PUT_CONTENT" | "PATCH_CONTENT" => save_contents(&key, &body).await,
        "LOCK" | "REFRESH_LOCK" => {
            let lock = header_val(&headers, "x-wopi-lock").unwrap_or_default();
            if lock.is_empty() {
                return Err(json_err(StatusCode::BAD_REQUEST, "missing lock"));
            }
            match lock_get(&id) {
                Some(cur) if cur != lock => {
                    let mut resp = wopi_json(serde_json::json!({}));
                    *resp.status_mut() = StatusCode::CONFLICT;
                    resp.headers_mut().insert("X-WOPI-Lock", cur.parse().unwrap());
                    Ok(resp)
                }
                _ => {
                    lock_set(&id, &lock);
                    Ok(wopi_json(serde_json::json!({})))
                }
            }
        }
        "UNLOCK" => {
            let lock = header_val(&headers, "x-wopi-lock").unwrap_or_default();
            match lock_get(&id) {
                Some(cur) if cur == lock => {
                    lock_del(&id);
                    Ok(wopi_json(serde_json::json!({})))
                }
                Some(cur) => {
                    let mut resp = wopi_json(serde_json::json!({}));
                    *resp.status_mut() = StatusCode::CONFLICT;
                    resp.headers_mut().insert("X-WOPI-Lock", cur.parse().unwrap());
                    Ok(resp)
                }
                None => Err(json_err(StatusCode::CONFLICT, "not locked")),
            }
        }
        "GET_LOCK" => {
            let mut resp = wopi_json(serde_json::json!({}));
            if let Some(cur) = lock_get(&id) {
                resp.headers_mut().insert("X-WOPI-Lock", cur.parse().unwrap());
            }
            Ok(resp)
        }
        // RENAME/DELETE 等不支持
        other => Err(json_err(StatusCode::NOT_IMPLEMENTED, &format!("unsupported {other}"))),
    }
}

async fn save_contents(key: &str, body: &[u8]) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    let cfg = s3ops::S3Cfg::from_env()
        .ok_or_else(|| json_err(StatusCode::SERVICE_UNAVAILABLE, "storage not configured"))?;
    s3ops::put_object(&cfg, key, body)
        .await
        .map_err(|e| json_err(StatusCode::BAD_GATEWAY, &e))?;
    Ok(wopi_json(serde_json::json!({ "LastModifiedTime": now_rfc3339() })))
}

/// PUT/POST /wopi/files/{id}/contents —— 保存正文。
pub async fn put_contents(
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<std::collections::HashMap<String, String>>,
    body: axum::body::Bytes,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    let key = auth_wopi(&id, &headers, q.get("access_token").map(String::as_str))?;
    save_contents(&key, &body).await
}

/// 会话鉴权的 S3 直写(文件库的服务端写入通道,浏览器/桌面可免 SigV4
/// 凭据)。key 限制扩展名与安全字符,大小 64MB 内。
const OSS_WRITE_EXTS: &[&str] = &["md", "markdown", "txt", "svg", "json"];

fn valid_oss_write_key(key: &str) -> bool {
    if key.is_empty() || key.len() > 512 || key.contains("..") || key.starts_with('/') {
        return false;
    }
    if !key.chars().all(|c| c.is_alphanumeric() || " ._-+/()（）·、".contains(c)) {
        return false;
    }
    let ext = key.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    OSS_WRITE_EXTS.contains(&ext.as_str())
}

/// PUT /api/oss/{*key} —— body 原样写入桶。
pub async fn oss_put(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(key): Path<String>,
    body: axum::body::Bytes,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    require_user(&st, &headers).await?;
    if !valid_oss_write_key(&key) {
        return Err(json_err(StatusCode::BAD_REQUEST, "bad key"));
    }
    let cfg = s3ops::S3Cfg::from_env()
        .ok_or_else(|| json_err(StatusCode::SERVICE_UNAVAILABLE, "storage not configured"))?;
    s3ops::put_object(&cfg, &key, &body)
        .await
        .map_err(|e| json_err(StatusCode::BAD_GATEWAY, &e))?;
    Ok(crate::util::json_vec(
        StatusCode::OK,
        serde_json::to_vec(&serde_json::json!({ "ok": true, "key": key })).unwrap(),
    ))
}

/// DELETE /api/oss/{*key} —— 会话鉴权的 S3 直删(文件库的服务端删除通道)。
pub async fn oss_delete(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    require_user(&st, &headers).await?;
    if !valid_oss_write_key(&key) {
        return Err(json_err(StatusCode::BAD_REQUEST, "bad key"));
    }
    let cfg = s3ops::S3Cfg::from_env()
        .ok_or_else(|| json_err(StatusCode::SERVICE_UNAVAILABLE, "storage not configured"))?;
    s3ops::delete_object(&cfg, &key)
        .await
        .map_err(|e| json_err(StatusCode::BAD_GATEWAY, &e))?;
    Ok(crate::util::json_vec(
        StatusCode::OK,
        serde_json::to_vec(&serde_json::json!({ "ok": true })).unwrap(),
    ))
}

/// WOPI 路由(挂在 / 上;上传体 64MB 上限,Office 文档富余)。
pub fn router() -> Router<AppState> {
    use axum::routing::{delete, get, post, put};
    Router::new()
        .route("/api/office/config", get(config))
        .route("/api/office/callback", post(office_callback))
        .route("/api/oss/{*key}", put(oss_put).delete(oss_delete))
        .route(
            "/wopi/files/{id}",
            get(check_file_info).post(wopi_post),
        )
        .route(
            "/wopi/files/{id}/contents",
            get(get_contents).put(put_contents).post(put_contents),
        )
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024))
}
