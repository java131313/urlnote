// Small shared helpers: timestamps, hashing, random ids, cookies, responses.

use axum::http::{header, HeaderMap, StatusCode};
use axum::Json;
use chrono::Utc;
use shared::ErrorResponse;
use sha2::{Digest, Sha256};

/// D1 used `datetime('now')` = "YYYY-MM-DD HH:MM:SS" UTC; the frontend
/// compares these as strings, so keep the exact format.
pub fn now_str() -> String {
    Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

pub fn password_hash(username: &str, password: &str) -> String {
    let mut h = Sha256::new();
    h.update(username.as_bytes());
    h.update(b":");
    h.update(password.as_bytes());
    to_hex(&h.finalize())
}

pub fn to_hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

pub fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut d = 0u8;
    for i in 0..a.len() {
        d |= a[i] ^ b[i];
    }
    d == 0
}

fn rand_alnum(n: usize) -> String {
    use rand::Rng;
    const ALPHABET: &[u8; 62] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::thread_rng();
    (0..n).map(|_| ALPHABET[rng.gen_range(0..62)] as char).collect()
}

pub fn new_token() -> String {
    rand_alnum(32)
}

/// 16-char random file name; returns the key including the "img/" prefix.
pub fn new_image_key() -> String {
    format!("img/{}", rand_alnum(16))
}

/// Extract a cookie value from the Cookie header (same logic as the worker).
pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let header = headers.get(header::COOKIE)?.to_str().ok()?;
    let prefix = format!("{name}=");
    header
        .split(';')
        .map(str::trim)
        .find_map(|p| p.strip_prefix(&prefix))
        .map(|s| s.to_string())
}

pub fn json_error(status: StatusCode, msg: &str) -> (StatusCode, Json<ErrorResponse>) {
    (
        status,
        Json(ErrorResponse {
            error: msg.to_string(),
        }),
    )
}

pub fn json_vec(status: StatusCode, payload: Vec<u8>) -> axum::response::Response {
    response_with(status, &[("content-type", "application/json; charset=utf-8")], payload)
}

/// Build a response with extra headers (value validated loosely; panics on
/// invalid header values, which are all constructed internally).
pub fn response_with(
    status: StatusCode,
    headers: &[(&str, &str)],
    payload: Vec<u8>,
) -> axum::response::Response {
    let mut resp = (status, payload).into_response();
    for (k, v) in headers {
        resp.headers_mut().insert(
            header::HeaderName::from_bytes(k.as_bytes()).expect("header name"),
            header::HeaderValue::from_str(v).expect("header value"),
        );
    }
    resp
}

use axum::response::IntoResponse;

pub const MAX_UPLOAD_BYTES: usize = 8 << 20;
pub const UPLOAD_HEADROOM: usize = 1 << 20;
pub const SESSION_TTL_SEC: i64 = 7 * 24 * 3600;
pub const PRESENCE_TTL_SEC: i64 = 90;
pub const INDEX_MAX_CHARS: usize = 30_000;

pub fn head_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// sniff_image returns (mime, extension) from magic bytes.
pub fn sniff_image(b: &[u8]) -> Option<(&'static str, &'static str)> {
    if b.len() >= 8 && &b[..8] == b"\x89PNG\r\n\x1a\n" {
        Some(("image/png", ".png"))
    } else if b.len() >= 3 && b[0] == 0xFF && b[1] == 0xD8 && b[2] == 0xFF {
        Some(("image/jpeg", ".jpg"))
    } else if b.len() >= 6 && (&b[..6] == b"GIF87a" || &b[..6] == b"GIF89a") {
        Some(("image/gif", ".gif"))
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some(("image/webp", ".webp"))
    } else if b.len() >= 12 && &b[4..8] == b"ftyp" && (&b[8..12] == b"avif" || &b[8..12] == b"avis") {
        Some(("image/avif", ".avif"))
    } else {
        None
    }
}

/// Escape a user-supplied string for use in an anchored MongoDB regex.
pub fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\.^$*+?()[]{}|".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

pub fn last_segment_has_dot(path: &str) -> bool {
    path.rsplit('/').next().is_some_and(|seg| seg.contains('.'))
}
