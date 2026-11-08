//! S3 客户端:移植自 rust/frontend/src/oss.rs 的 SigV4 签名(纯函数部分),
//! 请求改走 reqwest blocking。凭据运行时读取(环境变量 > ~/.urlnote/oss.conf),
//! 不再编译期打进包。

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone)]
pub struct OssCreds {
    pub endpoint: String, // 不带 scheme
    pub ak: String,
    pub sk: String,
    pub bucket: String,
    pub region: String,
}

/// 环境变量 > ~/.urlnote/oss.conf(每行 KEY=VALUE)。
pub fn load_creds() -> Result<OssCreds, String> {
    let get = |k: &str| -> Option<String> {
        if let Ok(v) = std::env::var(k) {
            return Some(v);
        }
        let conf = dirs_home().join(".urlnote/oss.conf");
        if let Ok(t) = std::fs::read_to_string(&conf) {
            for line in t.lines() {
                let line = line.trim();
                if line.starts_with('#') || line.is_empty() {
                    continue;
                }
                if let Some((a, b)) = line.split_once('=') {
                    if a.trim().eq_ignore_ascii_case(k) {
                        return Some(b.trim().to_string());
                    }
                }
            }
        }
        None
    };
    let need = |k: &str| get(k).filter(|v| !v.is_empty()).ok_or_else(|| format!("缺少配置 {k}"));
    Ok(OssCreds {
        endpoint: need("OSS_ENDPOINT")?
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .to_string(),
        ak: need("OSS_AK")?,
        sk: need("OSS_SK")?,
        bucket: need("OSS_BUCKET")?,
        region: get("OSS_REGION").unwrap_or_else(|| "us-east-1".into()),
    })
}

fn dirs_home() -> std::path::PathBuf {
    std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default()
}

type HmacSha256 = Hmac<Sha256>;

fn sha256_hex(b: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(b);
    to_hex(&h.finalize())
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut m = <HmacSha256 as Mac>::new_from_slice(key).expect("hmac key");
    m.update(msg);
    m.finalize().into_bytes().into()
}

fn to_hex(b: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push(HEX[(x >> 4) as usize] as char);
        s.push(HEX[(x & 0xf) as usize] as char);
    }
    s
}

/// RFC 3986(S3 变体):保留非保留字符,其余按 UTF-8 字节编码。
fn aws_uri_encode(s: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b'/' if !encode_slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

/// (amz_date, date_stamp),Hinnant civil_from_days 实现(与前端逐字节一致)。
fn fmt_times(ms: u128) -> (String, String) {
    let secs = (ms / 1000) as i64;
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

pub struct SigHeaders {
    pub authorization: String,
    pub x_amz_date: String,
    pub x_amz_content_sha256: String,
}

pub fn sign_request(
    creds: &OssCreds,
    method: &str,
    path: &str,
    query: &[(&str, &str)],
    payload: &[u8],
) -> SigHeaders {
    let (amz, date_stamp) = fmt_times(now_ms());
    let payload_hash = sha256_hex(payload);

    let mut q: Vec<(String, String)> = query
        .iter()
        .map(|(k, v)| (aws_uri_encode(k, true), aws_uri_encode(v, true)))
        .collect();
    q.sort();
    let query_str = q.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");

    let canonical = format!(
        "{method}\n{path}\n{query_str}\n\
         host:{endpoint}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz}\n\n\
         host;x-amz-content-sha256;x-amz-date\n{payload_hash}",
        endpoint = creds.endpoint,
    );
    let scope = format!("{date_stamp}/{}/s3/aws4_request", creds.region);
    let string_to_sign = format!("AWS4-HMAC-SHA256\n{amz}\n{scope}\n{}", sha256_hex(canonical.as_bytes()));

    let mut k = hmac_sha256(format!("AWS4{}", creds.sk).as_bytes(), date_stamp.as_bytes());
    k = hmac_sha256(&k, creds.region.as_bytes());
    k = hmac_sha256(&k, b"s3");
    k = hmac_sha256(&k, b"aws4_request");
    let sig = to_hex(&hmac_sha256(&k, string_to_sign.as_bytes()));

    SigHeaders {
        authorization: format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, \
             SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature={sig}",
            creds.ak
        ),
        x_amz_date: amz,
        x_amz_content_sha256: payload_hash,
    }
}

fn canonical_query(query: &[(&str, &str)]) -> String {
    let mut q: Vec<(String, String)> = query
        .iter()
        .map(|(k, v)| (aws_uri_encode(k, true), aws_uri_encode(v, true)))
        .collect();
    q.sort();
    q.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&")
}

fn signed(
    creds: &OssCreds,
    method: &str,
    path: &str,
    query: &[(&str, &str)],
    payload: &[u8],
) -> Vec<(&'static str, String)> {
    let s = sign_request(creds, method, path, query, payload);
    vec![
        ("x-amz-date", s.x_amz_date),
        ("x-amz-content-sha256", s.x_amz_content_sha256),
        ("authorization", s.authorization),
    ]
}

fn send(
    client: &reqwest::blocking::Client,
    creds: &OssCreds,
    method: &str,
    path: &str,
    query: &[(&str, &str)],
    payload: Vec<u8>,
) -> Result<reqwest::blocking::Response, String> {
    let url = format!("https://{}{path}?{}", creds.endpoint, canonical_query(query));
    let headers = signed(creds, method, path, query, &payload);
    let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
    let bodyless = payload.is_empty() && (method == reqwest::Method::GET || method == reqwest::Method::HEAD);
    let mut req = client.request(method, url);
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let req = if bodyless { req } else { req.body(payload) };
    req.send().map_err(|e| format!("网络错误: {e}"))
}

fn status_err(resp: reqwest::blocking::Response) -> String {
    let code = resp.status();
    let body = resp.text().unwrap_or_default();
    format!("HTTP {code}: {}", body.chars().take(160).collect::<String>())
}

#[derive(Clone, Debug)]
pub struct ListedFile {
    pub key: String,
    pub name: String,
    pub size: u64,
    pub modified: String,
}

#[derive(Clone, Debug, Default)]
pub struct ListResult {
    /// 当前 prefix 下的子目录名(相对,以 / 结尾)。
    pub dirs: Vec<String>,
    pub files: Vec<ListedFile>,
}

/// ListObjectsV2(prefix + delimiter=/,自动翻页)。
pub fn list_dir(
    client: &reqwest::blocking::Client,
    creds: &OssCreds,
    prefix: &str,
) -> Result<ListResult, String> {
    let path = format!("/{}", creds.bucket);
    let mut out = ListResult::default();
    let mut token: Option<String> = None;
    loop {
        let mut query: Vec<(&str, &str)> = vec![
            ("list-type", "2"),
            ("max-keys", "1000"),
            ("delimiter", "/"),
            ("prefix", prefix),
        ];
        let tok_holder;
        if let Some(t) = &token {
            tok_holder = t.clone();
            query.push(("continuation-token", &tok_holder));
        }
        let resp = send(client, creds, "GET", &path, &query, Vec::new())?;
        if !resp.status().is_success() {
            return Err(status_err(resp));
        }
        let (dirs, mut files, next, truncated) = parse_list_xml(&resp.text().map_err(|e| e.to_string())?);
        out.dirs.extend(dirs);
        out.files.append(&mut files);
        if truncated {
            token = next;
        } else {
            break;
        }
    }
    Ok(out)
}

/// 列出 prefix 下全部 key(删除目录用,不分 delimiter)。
pub fn list_all_keys(
    client: &reqwest::blocking::Client,
    creds: &OssCreds,
    prefix: &str,
) -> Result<Vec<String>, String> {
    let path = format!("/{}", creds.bucket);
    let mut keys = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut query: Vec<(&str, &str)> =
            vec![("list-type", "2"), ("max-keys", "1000"), ("prefix", prefix)];
        let tok_holder;
        if let Some(t) = &token {
            tok_holder = t.clone();
            query.push(("continuation-token", &tok_holder));
        }
        let resp = send(client, creds, "GET", &path, &query, Vec::new())?;
        if !resp.status().is_success() {
            return Err(status_err(resp));
        }
        let (_, mut files, next, truncated) = parse_list_xml(&resp.text().map_err(|e| e.to_string())?);
        keys.extend(files.drain(..).map(|f| f.key));
        if truncated {
            token = next;
        } else {
            break;
        }
    }
    Ok(keys)
}

pub fn get_object(
    client: &reqwest::blocking::Client,
    creds: &OssCreds,
    key: &str,
) -> Result<Option<String>, String> {
    let path = format!("/{}/{}", creds.bucket, aws_uri_encode(key, false));
    let resp = send(client, creds, "GET", &path, &[], Vec::new())?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        return Err(status_err(resp));
    }
    resp.text().map(Some).map_err(|e| e.to_string())
}

pub fn put_object(
    client: &reqwest::blocking::Client,
    creds: &OssCreds,
    key: &str,
    content: &[u8],
) -> Result<(), String> {
    let path = format!("/{}/{}", creds.bucket, aws_uri_encode(key, false));
    let resp = send(client, creds, "PUT", &path, &[], content.to_vec())?;
    if !resp.status().is_success() {
        return Err(status_err(resp));
    }
    Ok(())
}

pub fn delete_object(
    client: &reqwest::blocking::Client,
    creds: &OssCreds,
    key: &str,
) -> Result<(), String> {
    let path = format!("/{}/{}", creds.bucket, aws_uri_encode(key, false));
    let resp = send(client, creds, "DELETE", &path, &[], Vec::new())?;
    if !resp.status().is_success() && resp.status() != reqwest::StatusCode::NOT_FOUND {
        return Err(status_err(resp));
    }
    Ok(())
}

/// 解析 ListObjectsV2 响应 → (CommonPrefixes, Contents, NextContinuationToken, IsTruncated)。
fn parse_list_xml(body: &str) -> (Vec<String>, Vec<ListedFile>, Option<String>, bool) {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    #[derive(Default)]
    struct Acc {
        in_common_prefix: bool,
        in_contents: bool,
        in_key: bool,
        in_prefix: bool,
        in_size: bool,
        in_lm: bool,
        in_trunc: bool,
        in_token: bool,
        cur_key: String,
        cur_size: String,
        cur_lm: String,
        token: String,
    }
    let mut a = Acc::default();
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let mut next: Option<String> = None;
    let mut truncated = false;

    let mut reader = Reader::from_str(body);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                b"CommonPrefixes" => a.in_common_prefix = true,
                b"Contents" => a.in_contents = true,
                b"Key" => a.in_key = true,
                b"Prefix" => a.in_prefix = true,
                b"Size" => a.in_size = true,
                b"LastModified" => a.in_lm = true,
                b"IsTruncated" => a.in_trunc = true,
                b"NextContinuationToken" => a.in_token = true,
                _ => {}
            },
            Ok(Event::Text(t)) => {
                let raw = reader.decoder().decode(t.as_ref()).unwrap_or_default();
                let s = quick_xml::escape::unescape(&raw)
                    .map(|c| c.into_owned())
                    .unwrap_or_else(|_| raw.into_owned());
                if a.in_key || a.in_prefix {
                    a.cur_key = s;
                } else if a.in_size {
                    a.cur_size = s;
                } else if a.in_lm {
                    a.cur_lm = s;
                } else if a.in_trunc {
                    truncated = s.trim() == "true";
                } else if a.in_token {
                    a.token.push_str(&s);
                }
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"CommonPrefixes" => a.in_common_prefix = false,
                b"Contents" => {
                    a.in_contents = false;
                    let key = std::mem::take(&mut a.cur_key);
                    if !key.is_empty() {
                        files.push(ListedFile {
                            name: key.rsplit('/').next().unwrap_or(&key).to_string(),
                            size: a.cur_size.parse().unwrap_or(0),
                            modified: std::mem::take(&mut a.cur_lm),
                            key,
                        });
                    }
                    a.cur_size.clear();
                }
                b"Key" => a.in_key = false,
                b"Prefix" => {
                    if a.in_common_prefix {
                        dirs.push(std::mem::take(&mut a.cur_key));
                    }
                    a.in_prefix = false;
                }
                b"Size" => a.in_size = false,
                b"LastModified" => a.in_lm = false,
                b"IsTruncated" => a.in_trunc = false,
                b"NextContinuationToken" => {
                    a.in_token = false;
                    if !a.token.is_empty() {
                        next = Some(std::mem::take(&mut a.token));
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    // CommonPrefixes 返回完整 prefix(如 a/b/)。
    (dirs, files, next, truncated)
}

#[cfg(test)]
mod tests {
    #[test]
    fn oss_conf_and_list() {
        let c = super::load_creds().expect("load_creds");
        assert!(c.endpoint.contains("sealos"));
        let client = reqwest::blocking::Client::new();
        let r = super::list_dir(&client, &c, "").expect("list_dir");
        println!("dirs={} files={}", r.dirs.len(), r.files.len());
    }
}
