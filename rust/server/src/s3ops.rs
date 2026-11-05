//! 通用 Sealos S3 操作:GET(流/全量)、Range-0 探测大小、PUT 回写。
//! 签名与 media.rs 同契约(host;x-amz-content-sha256;x-amz-date),
//! 方法/负载泛化;Collabora WOPI 宿主(office.rs)与后续服务端 S3 操作使用。

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

#[derive(Clone)]
pub struct S3Cfg {
    pub endpoint: String,
    pub ak: String,
    pub sk: String,
    pub bucket: String,
    pub region: String,
}

fn env_non_empty(k: &str) -> Option<String> {
    std::env::var(k)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

impl S3Cfg {
    /// 与文件库前端同一个桶;AK/SK 必需(服务端环境变量)。
    pub fn from_env() -> Option<S3Cfg> {
        Some(S3Cfg {
            endpoint: env_non_empty("OSS_ENDPOINT")
                .unwrap_or_else(|| "objectstorageapi.bja.sealos.run".to_string()),
            ak: env_non_empty("OSS_AK")?,
            sk: env_non_empty("OSS_SK")?,
            bucket: env_non_empty("OSS_BUCKET")
                .unwrap_or_else(|| "mpzfivcp-watchwonderful".to_string()),
            region: env_non_empty("OSS_REGION").unwrap_or_else(|| "us-east-1".to_string()),
        })
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

pub fn sha256_hex(b: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(b);
    hex(&h.finalize())
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("hmac key");
    m.update(msg);
    m.finalize().into_bytes().to_vec()
}

/// URI-encode per RFC 3986(S3 变体);encode_slash=false 用于对象 key(保留 /)。
fn aws_uri_encode(s: &str, encode_slash: bool) -> String {
    let mut out = String::new();
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b'/' if !encode_slash => out.push('/'),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

struct Signed {
    authorization: String,
    x_amz_date: String,
    x_amz_content_sha256: String,
}

fn sign(cfg: &S3Cfg, method: &str, path: &str, payload_hash: &str) -> Signed {
    use chrono::Utc;
    let now = Utc::now();
    let amz = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = now.format("%Y%m%d").to_string();
    let canonical = format!(
        "{method}\n{path}\n\nhost:{endpoint}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz}\n\nhost;x-amz-content-sha256;x-amz-date\n{payload_hash}",
        endpoint = cfg.endpoint,
    );
    let scope = format!("{date_stamp}/{}/s3/aws4_request", cfg.region);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz}\n{scope}\n{}",
        sha256_hex(canonical.as_bytes())
    );
    let mut k = hmac_sha256(format!("AWS4{}", cfg.sk).as_bytes(), date_stamp.as_bytes());
    k = hmac_sha256(&k, cfg.region.as_bytes());
    k = hmac_sha256(&k, b"s3");
    k = hmac_sha256(&k, b"aws4_request");
    let sig = hex(&hmac_sha256(&k, string_to_sign.as_bytes()));
    Signed {
        authorization: format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature={sig}",
            cfg.ak
        ),
        x_amz_date: amz,
        x_amz_content_sha256: payload_hash.to_string(),
    }
}

fn object_url(cfg: &S3Cfg, key: &str) -> (String, String) {
    let path = format!("/{}/{}", cfg.bucket, aws_uri_encode(key, false));
    (format!("https://{}{path}", cfg.endpoint), path)
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

fn signed_req(
    cfg: &S3Cfg,
    method: reqwest::Method,
    url: &str,
    signed: &Signed,
) -> reqwest::RequestBuilder {
    client()
        .request(method, url)
        .header("Authorization", &signed.authorization)
        .header("X-Amz-Date", &signed.x_amz_date)
        .header("X-Amz-Content-Sha256", &signed.x_amz_content_sha256)
}

/// 全量下载(文件编辑场景,大小受内存约束;WOPI 的 Office 文档一般几 MB)。
pub async fn object_bytes(cfg: &S3Cfg, key: &str) -> Result<Vec<u8>, String> {
    let (url, path) = object_url(cfg, key);
    let signed = sign(cfg, "GET", &path, &sha256_hex(b""));
    let resp = signed_req(cfg, reqwest::Method::GET, &url, &signed)
        .send()
        .await
        .map_err(|e| format!("upstream: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("upstream HTTP {}", resp.status()));
    }
    resp.bytes().await.map(|b| b.to_vec()).map_err(|e| e.to_string())
}

/// Range: bytes=0-0 探测对象总大小(Content-Range: bytes 0-0/TOTAL)。
pub async fn object_size(cfg: &S3Cfg, key: &str) -> Result<u64, String> {
    let (url, path) = object_url(cfg, key);
    let empty = sha256_hex(b"");
    let signed = sign(cfg, "GET", &path, &empty);
    let resp = signed_req(cfg, reqwest::Method::GET, &url, &signed)
        .header("Range", "bytes=0-0")
        .send()
        .await
        .map_err(|e| format!("upstream: {e}"))?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err("not found".to_string());
    }
    if status != reqwest::StatusCode::PARTIAL_CONTENT && !status.is_success() {
        return Err(format!("upstream HTTP {status}"));
    }
    let cr = resp
        .headers()
        .get("content-range")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let len = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());
    if let Some(cr) = cr {
        if let Some(total) = cr.rsplit('/').next().and_then(|s| s.parse::<u64>().ok()) {
            return Ok(total);
        }
    }
    len.ok_or_else(|| "size unknown".to_string())
}

/// 回写整个对象(payload 用真实内容哈希,避免 UNSIGNED-PAYLOAD 兼容性问题)。
pub async fn put_object(cfg: &S3Cfg, key: &str, body: &[u8]) -> Result<(), String> {
    let (url, path) = object_url(cfg, key);
    let hash = sha256_hex(body);
    let signed = sign(cfg, "PUT", &path, &hash);
    let resp = signed_req(cfg, reqwest::Method::PUT, &url, &signed)
        .body(body.to_vec())
        .send()
        .await
        .map_err(|e| format!("upstream: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("upstream HTTP {}", resp.status()));
    }
    Ok(())
}
