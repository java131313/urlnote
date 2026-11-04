//! 媒体流接口:会话鉴权后代理 Sealos 对象存储的 mp3/mp4(GET)。
//! S3 凭证走服务端环境变量(OSS_AK/OSS_SK/OSS_BUCKET,端点/区域有默认值),
//! 与前端 oss 页同一个桶;签名与 react/src/oss/sigv4.ts 同一套契约
//! (SignedHeaders 仅 host;x-amz-content-sha256;x-amz-date)。

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

use crate::auth::require_user;
use crate::db::AppState;
use shared::ErrorResponse;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
struct OssCfg {
    endpoint: String,
    ak: String,
    sk: String,
    bucket: String,
    region: String,
}

fn env_non_empty(k: &str) -> Option<String> {
    std::env::var(k)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn oss_cfg() -> Option<OssCfg> {
    Some(OssCfg {
        endpoint: env_non_empty("OSS_ENDPOINT")
            .unwrap_or_else(|| "objectstorageapi.bja.sealos.run".to_string()),
        ak: env_non_empty("OSS_AK")?,
        sk: env_non_empty("OSS_SK")?,
        // 与文件库前端同一个桶;桶名本就编译在公开前端包里,非秘密
        bucket: env_non_empty("OSS_BUCKET").unwrap_or_else(|| "mpzfivcp-watchwonderful".to_string()),
        region: env_non_empty("OSS_REGION").unwrap_or_else(|| "us-east-1".to_string()),
    })
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

fn sha256_hex(b: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(b);
    hex(&h.finalize())
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut m = <HmacSha256 as Mac>::new_from_slice(key).expect("hmac key");
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

struct SignedGet {
    url: String,
    authorization: String,
    x_amz_date: String,
    x_amz_content_sha256: String,
}

/// range = 请求头 Range 的值(如 "bytes=0-1023")。有 range 时必须进签名
/// (SignedHeaders 字典序:host;range;x-amz-content-sha256;x-amz-date),
/// 否则上游按完整 GET 处理,前端播放器无法拖动进度条。
fn sign_get_at(cfg: &OssCfg, key: &str, now: DateTime<Utc>, range: Option<&str>) -> SignedGet {
    let amz = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = now.format("%Y%m%d").to_string();
    let payload_hash = sha256_hex(b"");
    // 空 key(测向量/列桶)不带尾斜杠
    let path = if key.is_empty() {
        format!("/{}", cfg.bucket)
    } else {
        format!("/{}/{}", cfg.bucket, aws_uri_encode(key, false))
    };
    let (headers_block, signed_headers) = match range {
        Some(r) => (
            format!(
                "host:{endpoint}\nrange:{r}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz}\n\nhost;range;x-amz-content-sha256;x-amz-date",
                endpoint = cfg.endpoint,
            ),
            "host;range;x-amz-content-sha256;x-amz-date",
        ),
        None => (
            format!(
                "host:{endpoint}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz}\n\nhost;x-amz-content-sha256;x-amz-date",
                endpoint = cfg.endpoint,
            ),
            "host;x-amz-content-sha256;x-amz-date",
        ),
    };
    let canonical = format!("GET\n{path}\n\n{headers_block}\n{payload_hash}");
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
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={sig}",
        cfg.ak
    );
    SignedGet {
        url: format!("https://{}{path}", cfg.endpoint),
        authorization,
        x_amz_date: amz,
        x_amz_content_sha256: payload_hash,
    }
}

fn valid_media_key(key: &str) -> bool {
    if key.is_empty() || key.len() > 512 || key.contains("..") || key.starts_with('/') {
        return false;
    }
    let ext = key.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    matches!(ext.as_str(), "mp3" | "mp4")
}

/// GET /api/media/stream/{*key} — 鉴权后的 mp3/mp4 流。支持 Range
/// (SignedHeaders 加签 range,206/Content-Range 透传),前端 H5 播放器
/// <video>/<audio> 直接引用本地址即可拖动进度条。
pub async fn stream_media(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    require_user(&st, &headers).await?;
    if !valid_media_key(&key) {
        return Err(crate::util::json_error(StatusCode::NOT_FOUND, "not found"));
    }
    let cfg = oss_cfg().ok_or_else(|| {
        crate::util::json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "media storage not configured (OSS_AK/OSS_SK)",
        )
    })?;
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let signed = sign_get_at(&cfg, &key, Utc::now(), range.as_deref());
    let mut req = reqwest::Client::new()
        .get(&signed.url)
        .header("Authorization", signed.authorization)
        .header("X-Amz-Date", signed.x_amz_date)
        .header("X-Amz-Content-Sha256", signed.x_amz_content_sha256);
    if let Some(r) = &range {
        req = req.header("Range", r);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| crate::util::json_error(StatusCode::BAD_GATEWAY, &format!("upstream: {e}")))?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(crate::util::json_error(StatusCode::NOT_FOUND, "not found"));
    }
    if !(status.is_success() || status == reqwest::StatusCode::PARTIAL_CONTENT) {
        return Err(crate::util::json_error(
            StatusCode::BAD_GATEWAY,
            &format!("upstream HTTP {status}"),
        ));
    }
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .cloned()
        .unwrap_or_else(|| HeaderValue::from_static("application/octet-stream"));
    let content_length = resp.headers().get(header::CONTENT_LENGTH).cloned();
    let content_range = resp.headers().get(header::CONTENT_RANGE).cloned();
    let stream = resp
        .bytes_stream()
        .map(|r| r.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)));
    let out_status = if status == reqwest::StatusCode::PARTIAL_CONTENT {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let mut builder = Response::builder().status(out_status);
    let h = builder.headers_mut().expect("response builder");
    h.insert(header::CONTENT_TYPE, content_type);
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if let Some(cl) = content_length {
        h.insert(header::CONTENT_LENGTH, cl);
    }
    if let Some(cr) = content_range {
        h.insert(header::CONTENT_RANGE, cr);
    }
    Ok(builder.body(Body::from_stream(stream)).expect("stream body"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cfg() -> OssCfg {
        OssCfg {
            endpoint: "objectstorageapi.bja.sealos.run".to_string(),
            ak: "mpzfivcp".to_string(),
            sk: "96p89bwhdqpmhd6z".to_string(),
            bucket: "mpzfivcp-watchwonderful".to_string(),
            region: "us-east-1".to_string(),
        }
    }

    #[test]
    fn signer_matches_ts_reference() {
        // 与 react/src/oss/sigv4.test.ts 的 Python 参考向量一致
        let cfg = test_cfg();
        let now: DateTime<Utc> = "2026-01-02T03:04:05Z".parse().unwrap();
        let signed = sign_get_at(&cfg, "", now, None);
        assert_eq!(signed.x_amz_date, "20260102T030405Z");
        assert_eq!(
            signed.x_amz_content_sha256,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            signed.authorization,
            "AWS4-HMAC-SHA256 Credential=mpzfivcp/20260102/us-east-1/s3/aws4_request, \
             SignedHeaders=host;x-amz-content-sha256;x-amz-date, \
             Signature=35716eb22305276af82b26f0e4c0b32efc35715796da8ff4f7d79d41c6d060bd"
        );
    }

    #[test]
    fn uri_encode_and_key_validation() {
        assert_eq!(aws_uri_encode("a b/中.mp4", false), "a%20b/%E4%B8%AD.mp4");
        assert_eq!(aws_uri_encode("a/b", true), "a%2Fb");
        assert!(valid_media_key("music/歌.mp3"));
        assert!(valid_media_key("clip.MP4"));
        assert!(!valid_media_key("x.md"));
        assert!(!valid_media_key("../secret.mp3"));
        assert!(!valid_media_key("/abs.mp3"));
        assert!(!valid_media_key("a/../b.mp3"));
    }
}
