//! 媒体流接口(与 rust/server/src/media.rs 同契约):会话鉴权后代理
//! Sealos 对象存储的 mp3/mp4(GET)。S3 凭证走 Worker 环境变量/secret
//! (OSS_AK / OSS_SK / OSS_BUCKET;端点/区域有默认值),不入库不上链。
//! wasm 环境没有 chrono:时间用 worker::Date + civil_from_days 格式化,
//! 与 rust/frontend/src/oss.rs 同一套实现。

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use worker::*;

type HmacSha256 = Hmac<Sha256>;

struct OssCfg {
    endpoint: String,
    ak: String,
    sk: String,
    bucket: String,
    region: String,
}

fn oss_cfg(env: &Env) -> Option<OssCfg> {
    let v = |k: &str| {
        env.var(k)
            .ok()
            .map(|x| x.to_string())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    Some(OssCfg {
        endpoint: v("OSS_ENDPOINT").unwrap_or_else(|| "objectstorageapi.bja.sealos.run".into()),
        ak: v("OSS_AK")?,
        sk: v("OSS_SK")?,
        bucket: v("OSS_BUCKET")?,
        region: v("OSS_REGION").unwrap_or_else(|| "us-east-1".into()),
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

/// 反转上式:对路径分段的 %XX 解码(pathname 是编码形态)。
fn pct_decode(seg: &str) -> String {
    let b = seg.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let h = |c: u8| -> Option<u8> {
                match c {
                    b'0'..=b'9' => Some(c - b'0'),
                    b'a'..=b'f' => Some(c - b'a' + 10),
                    b'A'..=b'F' => Some(c - b'A' + 10),
                    _ => None,
                }
            };
            if let (Some(hi), Some(lo)) = (h(b[i + 1]), h(b[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// (amz_date, date_stamp) —— epoch 毫秒 → UTC,Howard Hinnant civil_from_days
/// (与 rust/frontend/src/oss.rs 逐行一致)。
fn fmt_times(ms: f64) -> (String, String) {
    let secs = (ms / 1000.0) as i64;
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (hh, mi, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (
        format!("{y:04}{m:02}{d:02}T{hh:02}{mi:02}{ss:02}Z"),
        format!("{y:04}{m:02}{d:02}"),
    )
}

struct SignedGet {
    url: String,
    authorization: String,
    x_amz_date: String,
    x_amz_content_sha256: String,
}

fn sign_get(cfg: &OssCfg, key: &str, now_ms: f64) -> SignedGet {
    let (amz, date_stamp) = fmt_times(now_ms);
    let payload_hash = sha256_hex(b"");
    // 空 key(列桶)不带尾斜杠
    let path = if key.is_empty() {
        format!("/{}", cfg.bucket)
    } else {
        format!("/{}/{}", cfg.bucket, aws_uri_encode(key, false))
    };
    let canonical = format!(
        "GET\n{path}\n\nhost:{endpoint}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz}\n\nhost;x-amz-content-sha256;x-amz-date\n{payload_hash}",
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
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature={sig}",
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

/// GET /api/media/stream/{*key} —— 鉴权后的 mp3/mp4 流,透传 S3 响应。
pub async fn stream_media(req: &Request, env: &Env, key: &str) -> Result<Response> {
    // require_user! 宏只在 lib.rs 内可见,这里用底层 current_user(同样逻辑)
    let _user = match crate::current_user(req, env).await {
        Some(u) => u,
        None => return crate::json_error(401, "unauthorized"),
    };
    if !valid_media_key(key) {
        return Response::error("not found", 404);
    }
    let cfg = match oss_cfg(env) {
        Some(c) => c,
        None => {
            return Response::error(
                "media storage not configured (OSS_AK/OSS_SK/OSS_BUCKET)",
                503,
            )
        }
    };
    let signed = sign_get(&cfg, key, Date::now().as_millis() as f64);
    let mut init = RequestInit::new();
    init.with_method(Method::Get);
    let headers = Headers::new();
    headers.set("Authorization", &signed.authorization)?;
    headers.set("X-Amz-Date", &signed.x_amz_date)?;
    headers.set("X-Amz-Content-Sha256", &signed.x_amz_content_sha256)?;
    init.with_headers(headers);
    let mut upstream = Fetch::Request(Request::new_with_init(&signed.url, &init)?)
        .send()
        .await?;
    let status = upstream.status_code();
    if status == 404 {
        return Response::error("not found", 404);
    }
    if !(200..300).contains(&status) {
        return Response::error(&format!("upstream HTTP {status}"), 502);
    }
    let out_headers = Headers::new();
    if let Ok(Some(ct)) = upstream.headers().get("Content-Type") {
        out_headers.set("Content-Type", &ct)?;
    }
    if let Ok(Some(cl)) = upstream.headers().get("Content-Length") {
        out_headers.set("Content-Length", &cl)?;
    }
    let stream = upstream.stream()?;
    let resp = Response::from_stream(stream)?;
    Ok(resp.with_headers(out_headers))
}

/// 路由匹配取出的原始分段(join 后仍是 %XX 形态)还原成对象 key。
pub fn decode_key(segs: &[&str]) -> String {
    segs.iter().map(|s| pct_decode(s)).collect::<Vec<_>>().join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cfg() -> OssCfg {
        OssCfg {
            endpoint: "objectstorageapi.bja.sealos.run".to_string(),
            ak: "TESTACCESSKEY".to_string(),
            sk: "fake-secret-key-for-unit-tests".to_string(),
            bucket: "test-bucket".to_string(),
            region: "us-east-1".to_string(),
        }
    }

    #[test]
    fn signer_matches_reference_vector() {
        // 与 rust/frontend、react 的 signer 共用同一参考向量
        let signed = sign_get(&test_cfg(), "", 1767323045000.0);
        assert_eq!(signed.x_amz_date, "20260102T030405Z");
        assert_eq!(
            signed.x_amz_content_sha256,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert!(signed
            .authorization
            .starts_with("AWS4-HMAC-SHA256 Credential=TESTACCESSKEY/20260102/us-east-1/s3/aws4_request, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature=61df9983846c3a678d1499dd8b7057c2c9ead369bf3e266b292146bacae7314f"));
    }

    #[test]
    fn pct_decode_and_key_validation() {
        assert_eq!(decode_key(&["music", "%E6%AD%8C.mp3"]), "music/歌.mp3");
        assert!(valid_media_key("music/歌.mp3"));
        assert!(valid_media_key("clip.MP4"));
        assert!(!valid_media_key("x.md"));
        assert!(!valid_media_key("a/../b.mp3"));
    }
}
