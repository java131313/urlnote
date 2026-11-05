//! Collabora Online / ONLYOFFICE Document Server(WOPI 客户端)的 WOPI 宿主。
//!
//! - `GET /api/office/config?key=`(会话鉴权):签发 WOPI token,拼好 iframe
//!   URL 返回前端。浏览器直连文档服务器(本机 Docker 或 Sealos 部署),
//!   WOPISrc 指向本服务。默认按 ONLYOFFICE 格式拼
//!   `/hosting/wopi/{word|cell|slide}/edit?wopisrc=...`;
//!   设置 `OFFICE_SERVER_URL` 指向任意 WOPI 客户端。
//! - `GET /wopi/files/{id}`:CheckFileInfo;`.../contents` GET/PUT:下载/回写 S3。
//! - `POST /wopi/files/{id}`:X-WOPI-Override 分发(PUT 保存、LOCK/UNLOCK 等)。
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

/// GET /api/office/config?key= —— 前端(会话)换取 iframe URL。
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
    let (token, ttl_ms) = mint_token(&id);
    let (scheme, host) = host_origin(&headers);
    let wopi_src = format!("{scheme}://{host}/wopi/files/{id}");
    // 文档服务器地址:OFFICE_SERVER_URL 优先,兼容旧 COLLABORA_URL。
    let ds = std::env::var("OFFICE_SERVER_URL")
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .or_else(|| {
            std::env::var("COLLABORA_URL")
                .ok()
                .map(|v| v.trim().trim_end_matches('/').to_string())
                .filter(|v| !v.is_empty())
        })
        .unwrap_or_else(|| "http://localhost:9980".to_string());
    // ONLYOFFICE WOPI 客户端的打开格式(即 /hosting/discovery 的 urlsrc 形态):
    //   {ds}/hosting/wopi/{word|cell|slide}/edit?wopisrc=<enc>&access_token=..
    // 按扩展名选应用;WOPISrc 不带 token,由客户端回传。
    let app = match key.rsplit('.').next().unwrap_or_default().to_ascii_lowercase().as_str() {
        "xlsx" | "ods" => "cell",
        "pptx" | "odp" => "slide",
        _ => "word",
    };
    let enc_src: String = url::form_urlencoded::byte_serialize(wopi_src.as_bytes()).collect();
    let edit_url = format!(
        "{ds}/hosting/wopi/{app}/edit?wopisrc={enc_src}&access_token={token}&access_token_ttl={ttl_ms}"
    );
    let payload = serde_json::json!({ "url": edit_url });
    Ok(crate::util::json_vec(
        StatusCode::OK,
        serde_json::to_vec(&payload).unwrap(),
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

/// WOPI 路由(挂在 / 上;上传体 64MB 上限,Office 文档富余)。
pub fn router() -> Router<AppState> {
    use axum::routing::get;
    Router::new()
        .route("/api/office/config", get(config))
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
