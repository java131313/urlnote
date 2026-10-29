//! Object-storage file browser + markdown editor page (route /oss).
//!
//! Lists the bucket's directory structure, opens markdown files in a
//! standalone Vditor instance, and saves back to the bucket — bypassing
//! D1 entirely. S3 requests are SigV4-signed in the browser against the
//! Sealos object-storage endpoint (MinIO-compatible).
//!
//! SECURITY: OSS_AK/OSS_SK are compile-time constants, so they ship inside
//! the public WASM bundle. Anyone can extract them and access the bucket.
//! Fine for personal use; if that ever matters, move signing behind the
//! CF worker (this module keeps it in one place on purpose).

use crate::vditor::VditorEditor;
use crate::{current_time, window, BASE_PATH};
use hmac::{Hmac, Mac};
use leptos::html::Div;
use leptos::prelude::*;
use leptos::task::spawn_local;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

// ---- compile-time config ----

pub const OSS_ENDPOINT: &str = match option_env!("OSS_ENDPOINT") {
    Some(v) => v,
    None => "objectstorageapi.bja.sealos.run",
};
pub const OSS_AK: &str = match option_env!("OSS_AK") {
    Some(v) => v,
    None => "",
};
pub const OSS_SK: &str = match option_env!("OSS_SK") {
    Some(v) => v,
    None => "",
};
pub const OSS_BUCKET: &str = match option_env!("OSS_BUCKET") {
    Some(v) => v,
    None => "",
};
pub const OSS_REGION: &str = match option_env!("OSS_REGION") {
    Some(v) => v,
    None => "us-east-1",
};
/// Requests are only made when the page is actually used; false just shows
/// a "not configured" hint.
pub const OSS_ENABLED: bool = !OSS_AK.is_empty() && !OSS_BUCKET.is_empty();

// ---- SigV4 ----

type HmacSha256 = Hmac<Sha256>;

fn sha256_hex(b: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(b);
    to_hex(&h.finalize())
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut m = HmacSha256::new_from_slice(key).expect("hmac key");
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

/// URI-encode per RFC 3986 (S3 variant): keep unreserved chars, encode
/// everything else as UTF-8 bytes. `encode_slash` controls whether `/`
/// becomes %2F (query components) or stays (object keys).
fn aws_uri_encode(s: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b'/' if !encode_slash => out.push('/'),
            _ => {
                out.push('%');
                out.push_str(&format!("{b:02X}"));
            }
        }
    }
    out
}

/// (amz_date, date_stamp) formatted from epoch milliseconds.
fn fmt_times(ms: f64) -> (String, String) {
    let secs = (ms / 1000.0) as i64;
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (hh, mi, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Howard Hinnant's civil_from_days
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

/// Credentials + endpoint, threaded as parameters so signing is a pure
/// function (testable, and reusable from a future server-side proxy).
pub struct OssCreds {
    pub ak: &'static str,
    pub sk: &'static str,
    pub region: &'static str,
    pub endpoint: &'static str,
}

fn compiled_creds() -> OssCreds {
    OssCreds {
        ak: OSS_AK,
        sk: OSS_SK,
        region: OSS_REGION,
        endpoint: OSS_ENDPOINT,
    }
}

/// Sign a request. `path` must already be URI-encoded (slashes kept);
/// query keys/values are encoded here. Only the three SigV4-required
/// headers are signed; anything the browser adds (Accept, Content-Type…)
/// stays outside the signature, as SigV4 allows.
pub fn sign_request(
    method: &str,
    path: &str,
    query: &[(&str, &str)],
    payload: &[u8],
    now_ms: f64,
) -> SigHeaders {
    sign_request_with(&compiled_creds(), method, path, query, payload, now_ms)
}

pub fn sign_request_with(
    creds: &OssCreds,
    method: &str,
    path: &str,
    query: &[(&str, &str)],
    payload: &[u8],
    now_ms: f64,
) -> SigHeaders {
    let (amz, date_stamp) = fmt_times(now_ms);
    let payload_hash = sha256_hex(payload);

    let mut q: Vec<(String, String)> = query
        .iter()
        .map(|(k, v)| (aws_uri_encode(k, true), aws_uri_encode(v, true)))
        .collect();
    q.sort();
    let query_str = q
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");

    let canonical = format!(
        "{method}\n{path}\n{query_str}\n\
         host:{endpoint}\nx-amz-content-sha256:{ph}\nx-amz-date:{amz}\n\n\
         host;x-amz-content-sha256;x-amz-date\n{ph}",
        endpoint = creds.endpoint,
        ph = payload_hash,
        amz = amz,
    );
    let scope = format!("{date_stamp}/{}/s3/aws4_request", creds.region);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz}\n{scope}\n{}",
        sha256_hex(canonical.as_bytes())
    );

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

fn now_ms() -> f64 {
    js_sys::Date::new_0().get_time()
}

/// Canonical (sorted, fully encoded) query string — used verbatim both in
/// the signature and in the request URL.
fn canonical_query(query: &[(&str, &str)]) -> String {
    let mut q: Vec<(String, String)> = query
        .iter()
        .map(|(k, v)| (aws_uri_encode(k, true), aws_uri_encode(v, true)))
        .collect();
    q.sort();
    q.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn apply_sig(
    mut b: gloo_net::http::RequestBuilder,
    sig: &SigHeaders,
) -> gloo_net::http::RequestBuilder {
    b = b
        .header("x-amz-date", &sig.x_amz_date)
        .header("x-amz-content-sha256", &sig.x_amz_content_sha256)
        .header("Authorization", &sig.authorization);
    b
}

// ---- S3 operations ----

#[derive(Clone, PartialEq)]
pub struct OssEntry {
    /// file name relative to the listed prefix
    pub name: String,
    pub key: String,
    pub size: u64,
    pub modified: String,
}

pub async fn get_object(key: &str) -> Result<String, String> {
    let path = format!("/{OSS_BUCKET}/{}", aws_uri_encode(key, false));
    let sig = sign_request("GET", &path, &[], b"", now_ms());
    let url = format!("https://{OSS_ENDPOINT}{path}");
    let resp = apply_sig(gloo_net::http::Request::get(&url), &sig)
        .send()
        .await
        .map_err(|e| format!("网络错误: {e}"))?;
    match resp.status() {
        200 => resp.text().await.map_err(|e| format!("读取失败: {e}")),
        404 => Err("文件不存在".to_string()),
        s => Err(format!("下载失败 (HTTP {s})")),
    }
}

pub async fn delete_object(key: &str) -> Result<(), String> {
    let path = format!("/{OSS_BUCKET}/{}", aws_uri_encode(key, false));
    let sig = sign_request("DELETE", &path, &[], b"", now_ms());
    let url = format!("https://{OSS_ENDPOINT}{path}");
    let resp = apply_sig(gloo_net::http::Request::delete(&url), &sig)
        .send()
        .await
        .map_err(|e| format!("网络错误: {e}"))?;
    match resp.status() {
        204 | 200 => Ok(()),
        404 => Err("文件不存在".to_string()),
        s => Err(format!("删除失败 (HTTP {s})")),
    }
}

/// True if an object already exists under `key` (for overwrite confirmation).
/// gloo-net has no HEAD constructor; a GET whose body we drop is equivalent
/// for a name-collision check (the checked key is almost always absent).
pub async fn object_exists(key: &str) -> bool {
    get_object(key).await.is_ok()
}

pub async fn put_object(key: &str, content: &str) -> Result<(), String> {
    let path = format!("/{OSS_BUCKET}/{}", aws_uri_encode(key, false));
    let payload = content.as_bytes().to_vec();
    let sig = sign_request("PUT", &path, &[], &payload, now_ms());
    let url = format!("https://{OSS_ENDPOINT}{path}");
    let body: JsValue = js_sys::Uint8Array::from(content.as_bytes()).into();
    let resp = apply_sig(gloo_net::http::Request::put(&url), &sig)
        .body(body)
        .map_err(|e| format!("请求构建失败: {e}"))?
        .send()
        .await
        .map_err(|e| format!("网络错误: {e}"))?;
    if resp.ok() {
        Ok(())
    } else {
        Err(format!("保存失败 (HTTP {})", resp.status()))
    }
}

// ---- standalone editor ----

// ---- tree ----

/// A directory node. S3 has no real directories — these come from splitting
/// object keys on `/`, plus zero-byte "name/" marker objects.
#[derive(Clone, Default, PartialEq)]
struct TreeNode {
    /// display name ("" for the synthetic root)
    name: String,
    /// full prefix with trailing "/" ("" for root)
    prefix: String,
    open: bool,
    dirs: Vec<TreeNode>,
    files: Vec<OssEntry>,
}

impl TreeNode {
    fn find(&self, prefix: &str) -> Option<&TreeNode> {
        if self.prefix == prefix {
            return Some(self);
        }
        self.dirs.iter().find_map(|d| d.find(prefix))
    }
    fn find_mut(&mut self, prefix: &str) -> Option<&mut TreeNode> {
        if self.prefix == prefix {
            return Some(self);
        }
        self.dirs.iter_mut().find_map(|d| d.find_mut(prefix))
    }
}

/// Build the directory tree from a flat listing. Expansion state is carried
/// over from the previous tree (matched by prefix) so refreshes keep the
/// user's view.
fn build_tree(entries: &[OssEntry], old: Option<&TreeNode>) -> TreeNode {
    let mut root = TreeNode::default();
    for e in entries {
        let parts: Vec<&str> = e.key.split('/').collect();
        let mut node = &mut root;
        let mut acc = String::new();
        for seg in &parts[..parts.len().saturating_sub(1)] {
            if seg.is_empty() {
                continue;
            }
            acc.push_str(seg);
            acc.push('/');
            if !node.dirs.iter().any(|d| d.prefix == acc) {
                let open = old
                    .and_then(|o| o.find(&acc))
                    .map(|n| n.open)
                    .unwrap_or(false);
                node.dirs.push(TreeNode {
                    name: (*seg).to_string(),
                    prefix: acc.clone(),
                    open,
                    dirs: Vec::new(),
                    files: Vec::new(),
                });
            }
            node = node.dirs.iter_mut().find(|d| d.prefix == acc).unwrap();
        }
        let last = parts[parts.len() - 1];
        if last.is_empty() {
            // a folder marker object — the dir itself was created above
        } else {
            node.files.push(OssEntry {
                name: last.to_string(),
                key: e.key.clone(),
                size: e.size,
                modified: e.modified.clone(),
            });
        }
    }
    fn sort_rec(n: &mut TreeNode) {
        n.dirs.sort_by(|a, b| a.name.cmp(&b.name));
        n.files.sort_by(|a, b| a.name.cmp(&b.name));
        for d in &mut n.dirs {
            sort_rec(d);
        }
    }
    sort_rec(&mut root);
    root
}

/// List ALL objects under `prefix` (no delimiter), following pagination.
pub async fn list_all_entries_under(prefix: &str) -> Result<Vec<OssEntry>, String> {
    let mut out = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut query: Vec<(&str, &str)> = vec![("list-type", "2"), ("max-keys", "1000")];
        if prefix.is_empty() {
            query.push(("prefix", ""));
        } else {
            query.push(("prefix", prefix));
        }
        let tok_storage;
        if let Some(t) = &token {
            tok_storage = t.clone();
            query.push(("continuation-token", &tok_storage));
        }
        let path = format!("/{OSS_BUCKET}");
        let sig = sign_request("GET", &path, &query, b"", now_ms());
        let qs = canonical_query(&query);
        let url = format!("https://{OSS_ENDPOINT}{path}?{qs}");
        let resp = apply_sig(gloo_net::http::Request::get(&url), &sig)
            .send()
            .await
            .map_err(|e| format!("网络错误: {e}"))?;
        if !resp.ok() {
            return Err(format!("列举失败 (HTTP {})", resp.status()));
        }
        let text = resp.text().await.map_err(|e| format!("响应读取失败: {e}"))?;
        let (entries, truncated, next) = parse_full_list(&text)?;
        out.extend(entries);
        if truncated {
            token = next;
        } else {
            break;
        }
    }
    Ok(out)
}

/// Parse an unpaginated-style ListObjectsV2 response: entries, IsTruncated,
/// NextContinuationToken.
fn parse_full_list(xml: &str) -> Result<(Vec<OssEntry>, bool, Option<String>), String> {
    use quick_xml::events::Event;
    let mut entries = Vec::new();
    let mut truncated = false;
    let mut next_token: Option<String> = None;
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut stack: Vec<String> = Vec::new();
    let mut cur: Option<OssEntry> = None;
    loop {
        let ev = reader
            .read_event()
            .map_err(|e| format!("目录解析失败: {e}"))?;
        match ev {
            Event::Start(e) => stack.push(String::from_utf8_lossy(e.name().as_ref()).into_owned()),
            Event::Text(t) => {
                let parent = stack
                    .len()
                    .checked_sub(2)
                    .and_then(|i| stack.get(i))
                    .map(|s| s.as_str())
                    .unwrap_or("");
                let tag = stack.last().map(|s| s.as_str()).unwrap_or("");
                let s = t
                    .unescape()
                    .map_err(|e| format!("目录解析失败: {e}"))?;
                match (parent, tag) {
                    ("Contents", "Key") => {
                        let key = s.into_owned();
                        cur = Some(OssEntry {
                            name: key.clone(),
                            key,
                            size: 0,
                            modified: String::new(),
                        });
                    }
                    ("Contents", "Size") => {
                        if let Some(ref mut c) = cur {
                            c.size = s.parse().unwrap_or(0);
                        }
                    }
                    ("Contents", "LastModified") => {
                        if let Some(ref mut c) = cur {
                            c.modified = s.get(..10).unwrap_or(&s).to_string();
                        }
                    }
                    ("ListBucketResult", "IsTruncated") => {
                        truncated = s.trim() == "true";
                    }
                    ("ListBucketResult", "NextContinuationToken") => {
                        next_token = Some(s.into_owned());
                    }
                    _ => {}
                }
            }
            Event::End(e) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if name == "Contents" {
                    if let Some(c) = cur.take() {
                        entries.push(c);
                    }
                }
                stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok((entries, truncated, next_token))
}


struct OssEditor {
    editor: VditorEditor,
    // keep the input callback alive
    #[allow(dead_code)]
    cbs: Vec<JsValue>,
}

thread_local! {
    static OSS_EDITOR: RefCell<Option<OssEditor>> = const { RefCell::new(None) };
}

fn init_editor(mount: &web_sys::Element, on_input: Closure<dyn FnMut()>) -> Result<(), String> {
    let dark = window()
        .match_media("(prefers-color-scheme: dark)")
        .ok()
        .flatten()
        .map(|m| m.matches())
        .unwrap_or(false);
    let options = js_sys::Object::new();
    let set = |k: &str, v: &JsValue| {
        js_sys::Reflect::set(&options, &JsValue::from_str(k), v).unwrap();
    };
    set("value", &JsValue::from_str(""));
    set("mode", &"wysiwyg".into());
    set("lang", &"zh_CN".into());
    set("cdn", &JsValue::from_str(&format!("{BASE_PATH}/vditor")));
    set("theme", &JsValue::from_str(if dark { "dark" } else { "classic" }));
    let preview = js_sys::Object::new();
    js_sys::Reflect::set(
        &preview,
        &"theme".into(),
        &JsValue::from_str(if dark { "dark" } else { "light" }),
    )
    .unwrap();
    set("preview", &preview.into());
    set(
        "placeholder",
        &"从左侧选择一个 Markdown 文件开始编辑，编辑后点右上角保存…".into(),
    );
    let cache = js_sys::Object::new();
    js_sys::Reflect::set(&cache, &"enable".into(), &false.into()).unwrap();
    set("cache", &cache.into());
    set("input", on_input.as_ref().unchecked_ref());
    // no upload integration on this page (view/edit only): no upload button
    let toolbar = js_sys::Array::new();
    for t in [
        "headings",
        "bold",
        "italic",
        "strike",
        "|",
        "list",
        "ordered-list",
        "check",
        "quote",
        "line",
        "code",
        "inline-code",
        "link",
        "table",
        "|",
        "undo",
        "redo",
        "|",
        "edit-mode",
        "preview",
    ] {
        toolbar.push(&JsValue::from_str(t));
    }
    set("toolbar", &toolbar.into());

    let editor = VditorEditor::new(mount, options.into());
    // into_js_value leaks the closure — dropping a Closure invalidates its JS
    // function, which would silently kill the input callback (and with it
    // dirty tracking, leaving the save button permanently disabled)
    let cb = on_input.into_js_value();
    OSS_EDITOR.with(|c| {
        *c.borrow_mut() = Some(OssEditor {
            editor,
            cbs: vec![cb],
        });
    });
    Ok(())
}

fn set_editor_value(v: &str) {
    OSS_EDITOR.with(|c| {
        if let Some(e) = c.borrow().as_ref() {
            e.editor.set_value(v);
        }
    });
}

fn editor_value() -> String {
    OSS_EDITOR.with(|c| {
        c.borrow()
            .as_ref()
            .map(|e| e.editor.get_value())
            .unwrap_or_default()
    })
}

// ---- page ----

#[derive(Clone, Copy)]
struct OssCtx {
    tree: RwSignal<TreeNode>,
    current: RwSignal<Option<String>>,
    current_name: RwSignal<String>,
    dirty: RwSignal<bool>,
    suppress: RwSignal<f64>,
    status: RwSignal<String>,
}

thread_local! {
    static CTX: RefCell<Option<OssCtx>> = const { RefCell::new(None) };
}

/// The page has exactly one instance, so handlers live as free functions
/// reading the installed context — no callback threading through the tree.
fn ctx() -> OssCtx {
    CTX.with(|c| *c.borrow().as_ref().expect("oss ctx installed"))
}

fn sanitize_name(raw: &str) -> Option<String> {
    let name: String = raw
        .chars()
        .filter(|c| !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\n' | '\r'))
        .take(120)
        .collect::<String>()
        .trim()
        .to_string();
    (!name.is_empty() && name != "." && name != "..").then_some(name)
}

/// (Re)load the whole bucket into the tree, preserving expansion state.
fn refresh_tree() {
    let ctx = ctx();
    ctx.status.set("加载目录…".to_string());
    spawn_local(async move {
        match list_all_entries_under("").await {
            Ok(entries) => {
                ctx.tree.update(|t| {
                    let old = std::mem::take(t);
                    *t = build_tree(&entries, Some(&old));
                });
                ctx.status.set(String::new());
            }
            Err(e) => ctx.status.set(e),
        }
    });
}

/// Copy everything under `old_prefix` to `new_prefix`, then delete the
/// originals. S3 has no rename — GET+PUT per object (markdown scale).
async fn copy_prefix(old_prefix: &str, new_prefix: &str) -> Result<(), String> {
    let ctx = ctx();
    let entries = list_all_entries_under(old_prefix).await?;
    let total = entries.len();
    for (i, e) in entries.iter().enumerate() {
        let Some(rest) = e.key.strip_prefix(old_prefix) else { continue };
        let target = format!("{new_prefix}{rest}");
        ctx.status.set(format!("重命名中 ({}/{total})…", i + 1));
        let content = get_object(&e.key).await?;
        put_object(&target, &content).await?;
        delete_object(&e.key).await?;
    }
    if total == 0 {
        // folder had no objects at all — leave a marker so it still exists
        put_object(new_prefix, "").await?;
    }
    Ok(())
}

async fn rename_file_key(old_key: &str, new_key: &str) -> Result<(), String> {
    let content = get_object(old_key).await?;
    put_object(new_key, &content).await?;
    delete_object(old_key).await
}

fn open_file(key: String, name: String) {
    let ctx = ctx();
    ctx.status.set(format!("载入 {name}…"));
    spawn_local(async move {
        match get_object(&key).await {
            Ok(text) => {
                ctx.suppress.set(js_sys::Date::now() + 500.0);
                set_editor_value(&text);
                ctx.current.set(Some(key));
                ctx.current_name.set(name.clone());
                ctx.dirty.set(false);
                if let Some(doc) = window().document() {
                    doc.set_title(&format!("{name} · 文件库"));
                }
                ctx.status.set(format!("已载入 {name}"));
            }
            Err(e) => ctx.status.set(e),
        }
    });
}

fn save_current() {
    let ctx = ctx();
    let Some(key) = ctx.current.get() else { return };
    let content = editor_value();
    ctx.status.set("保存中…".to_string());
    spawn_local(async move {
        match put_object(&key, &content).await {
            Ok(()) => {
                ctx.dirty.set(false);
                ctx.status.set(format!("已保存 {}", current_time()));
                refresh_tree();
            }
            Err(e) => ctx.status.set(e),
        }
    });
}

/// Rebase the open-file state if `old` was renamed to `new_prefix`,
/// or clear it when the file was deleted.
fn fix_current_after_move(old: &str, new_prefix: Option<&str>) {
    let ctx = ctx();
    let Some(cur) = ctx.current.get() else { return };
    let affected = if old.ends_with('/') {
        cur.starts_with(old)
    } else {
        cur == old
    };
    if !affected {
        return;
    }
    match new_prefix {
        Some(np) => {
            let rest = cur.strip_prefix(old).unwrap_or("");
            let new_key = format!("{np}{rest}");
            let new_name = new_key.rsplit('/').next().unwrap_or(&new_key).to_string();
            ctx.current.set(Some(new_key));
            ctx.current_name.set(new_name);
        }
        None => {
            ctx.current.set(None);
            ctx.current_name.set(String::new());
            ctx.dirty.set(false);
            set_editor_value("");
            if let Some(doc) = window().document() {
                doc.set_title("文件库");
            }
        }
    }
}

fn create_in(dir_prefix: String, is_dir: bool) {
    let ctx = ctx();
    let what = if is_dir { "文件夹" } else { "文件" };
    let Some(name) = window()
        .prompt_with_message(&format!("在 {dir_prefix} 下新建{what}："))
        .ok()
        .flatten()
        .and_then(|raw| sanitize_name(&raw))
    else {
        return;
    };
    let (key, content, final_name) = if is_dir {
        (format!("{dir_prefix}{name}/"), String::new(), name)
    } else {
        let final_name = if name.contains('.') { name.clone() } else { format!("{name}.md") };
        let key = format!("{dir_prefix}{final_name}");
        (key, format!("# {final_name}\n"), final_name)
    };
    ctx.status.set(format!("创建 {final_name}…"));
    spawn_local(async move {
        if !is_dir && object_exists(&key).await
            && !window()
                .confirm_with_message(&format!("{final_name} 已存在，覆盖它？"))
                .unwrap_or(false)
        {
            ctx.status.set("已取消".to_string());
            return;
        }
        match put_object(&key, &content).await {
            Ok(()) => {
                ctx.status.set(format!("已创建 {final_name}"));
                refresh_tree();
                if !is_dir {
                    open_file(key, final_name);
                }
            }
            Err(e) => ctx.status.set(e),
        }
    });
}

fn rename_entry(path: String, is_dir: bool) {
    let ctx = ctx();
    let old_name = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string();
    let Some(name) = window()
        .prompt_with_message(&format!("将 {old_name} 重命名为："))
        .ok()
        .flatten()
        .and_then(|raw| sanitize_name(&raw))
    else {
        return;
    };
    let parent = path
        .trim_end_matches('/')
        .rsplit_once('/')
        .map(|(p, _)| format!("{p}/"))
        .unwrap_or_default();
    let (new_path, confirm_msg) = if is_dir {
        (
            format!("{parent}{name}/"),
            format!("文件夹内的所有文件将移动到 {parent}{name}/，继续？"),
        )
    } else {
        let final_name = if name.contains('.') { name.clone() } else { format!("{name}.md") };
        (
            format!("{parent}{final_name}"),
            format!("确定重命名为 {final_name}？"),
        )
    };
    if new_path == path {
        return;
    }
    if !window()
        .confirm_with_message(&confirm_msg)
        .unwrap_or(false)
    {
        return;
    }
    ctx.status.set(format!("重命名 {old_name}…"));
    spawn_local(async move {
        let r = if is_dir {
            copy_prefix(&path, &new_path).await
        } else {
            rename_file_key(&path, &new_path).await
        };
        match r {
            Ok(()) => {
                fix_current_after_move(&path, Some(&new_path));
                ctx.status.set(format!("已重命名为 {name}"));
                refresh_tree();
            }
            Err(e) => ctx.status.set(e),
        }
    });
}

fn delete_entry(path: String, is_dir: bool) {
    let ctx = ctx();
    let name = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string();
    let hint = if is_dir { "（仅限空文件夹）" } else { "" };
    if !window()
        .confirm_with_message(&format!("删除 {name}{hint}？此操作不可恢复。"))
        .unwrap_or(false)
    {
        return;
    }
    ctx.status.set(format!("删除 {name}…"));
    spawn_local(async move {
        let r = if is_dir {
            // only empty folders: anything besides the marker blocks it
            match list_all_entries_under(&path).await {
                Ok(entries) if entries.iter().any(|e| e.key != path) => {
                    Err("文件夹非空，请先删除其中的文件".to_string())
                }
                Ok(_) => delete_object(&path).await,
                Err(e) => Err(e),
            }
        } else {
            delete_object(&path).await
        };
        match r {
            Ok(()) => {
                fix_current_after_move(&path, None);
                ctx.status.set(format!("已删除 {name}"));
                refresh_tree();
            }
            Err(e) => ctx.status.set(e),
        }
    });
}

#[component]
fn TreeDir(prefix: String) -> AnyView {
    let ctx = ctx();
    let p_memo = prefix.clone();
    let node = Memo::new(move |_| ctx.tree.with(|t| t.find(&p_memo).cloned()));
    let p_newfile = prefix.clone();
    let p_newdir = prefix.clone();
    let p_ren = prefix.clone();
    let p_del = prefix.clone();
    view! {
        <div class="oss-tdir">
            <div
                class="oss-trow"
                on:click=move |_| ctx.tree.update(|t| {
                    if let Some(n) = t.find_mut(&prefix.clone()) {
                        n.open = !n.open;
                    }
                })
            >
                <span
                    class="oss-chev"
                    class:oss-chev-open=move || node.with(|n| n.as_ref().map(|n| n.open).unwrap_or(false))
                >"▸"</span>
                <span class="oss-ticon">{move || {
                    node.with(|n| if n.as_ref().map(|n| n.open).unwrap_or(false) { "📂" } else { "📁" })
                }}</span>
                <span class="oss-tname">{move || node.with(|n| n.as_ref().map(|n| n.name.clone()).unwrap_or_default())}</span>
                <span class="oss-tact">
                    <button type="button" title="在此新建文件" on:click=move |ev| {
                        ev.stop_propagation();
                        create_in(p_newfile.clone(), false);
                    }>"＋📄"</button>
                    <button type="button" title="在此新建文件夹" on:click=move |ev| {
                        ev.stop_propagation();
                        create_in(p_newdir.clone(), true);
                    }>"＋📁"</button>
                    <button type="button" title="重命名文件夹" on:click=move |ev| {
                        ev.stop_propagation();
                        rename_entry(p_ren.clone(), true);
                    }>"✏️"</button>
                    <button type="button" title="删除文件夹（仅限空文件夹）" on:click=move |ev| {
                        ev.stop_propagation();
                        delete_entry(p_del.clone(), true);
                    }>"🗑"</button>
                </span>
            </div>
            <Show when=move || node.with(|n| n.as_ref().map(|n| n.open).unwrap_or(false))>
                <div class="oss-tkids">
                    <For
                        each=move || node.with(|n| n.as_ref().map(|n| n.dirs.clone()).unwrap_or_default())
                        key=|d| d.prefix.clone()
                        children=move |d| view! { <TreeDir prefix=d.prefix.clone()/> }
                    />
                    <For
                        each=move || node.with(|n| n.as_ref().map(|n| n.files.clone()).unwrap_or_default())
                        key=|f| f.key.clone()
                        children=move |f| {
                            let key_open = f.key.clone();
                            let name_open = f.name.clone();
                            let key_ren = f.key.clone();
                            let key_del = f.key.clone();
                            let key_active = f.key.clone();
                            let label = f.name.clone();
                            let ctx2 = ctx;
                            view! {
                                <div
                                    class="oss-trow oss-tfile"
                                    class:oss-active=move || ctx2.current.get().as_deref() == Some(key_active.as_str())
                                >
                                    <button type="button" class="oss-open"
                                        on:click=move |_| open_file(key_open.clone(), name_open.clone())
                                    >
                                        <span class="oss-ticon">"📄"</span>
                                        <span class="oss-tname">{label}</span>
                                    </button>
                                    <span class="oss-tact">
                                        <button type="button" title="重命名文件" on:click=move |_| rename_entry(key_ren.clone(), false)>"✏️"</button>
                                        <button type="button" title="删除文件" on:click=move |_| delete_entry(key_del.clone(), false)>"🗑"</button>
                                    </span>
                                </div>
                            }
                        }
                    />
                </div>
            </Show>
        </div>
    }
    .into_any()
}

#[component]
pub fn OssView() -> impl IntoView {
    if !OSS_ENABLED {
        return view! {
            <div class="oss-page">
                <div class="oss-top">
                    <a class="oss-home" href=format!("{BASE_PATH}/")>"📝 便签"</a>
                    <span class="oss-brand">"🗂 文件库"</span>
                </div>
                <div class="oss-empty">"对象存储未配置：需要 OSS_AK / OSS_BUCKET 构建参数"</div>
            </div>
        }
        .into_any();
    }

    let ctx = OssCtx {
        tree: RwSignal::new(TreeNode::default()),
        current: RwSignal::new(None),
        current_name: RwSignal::new(String::new()),
        dirty: RwSignal::new(false),
        suppress: RwSignal::new(0.0f64),
        status: RwSignal::new(String::new()),
    };
    CTX.with(|c| *c.borrow_mut() = Some(ctx));

    let editor_ref = NodeRef::<Div>::new();

    // editor: init once when the mount node appears; input marks dirty
    Effect::new(move |_| {
        let _ = editor_ref.get();
        let already = OSS_EDITOR.with(|c| c.borrow().is_some());
        if already {
            return;
        }
        if let Some(el) = editor_ref.get() {
            let d = ctx.dirty;
            let sup = ctx.suppress;
            let cb = Closure::<dyn FnMut()>::new(move || {
                // Vditor fires input asynchronously right after set_value —
                // that echo is not a user edit
                if js_sys::Date::now() < sup.get() {
                    return;
                }
                d.set(true);
            });
            if let Err(e) = init_editor(el.unchecked_ref::<web_sys::Element>(), cb) {
                ctx.status.set(e);
            }
        }
    });

    // first tree load
    Effect::new(move |_| {
        refresh_tree();
    });

    view! {
        <div class="oss-page">
            <div class="oss-top">
                <a class="oss-home" href=format!("{BASE_PATH}/") title="返回便签页面">"📝 便签"</a>
                <span class="oss-brand">"🗂 文件库"</span>
                <span class="oss-spacer"></span>
                <button type="button" class="tool" on:click=move |_| refresh_tree() title="重新加载目录">
                    "🔄"
                </button>
                <button
                    type="button"
                    class="tool oss-save"
                    prop:disabled=move || !(ctx.dirty.get() && ctx.current.get().is_some())
                    on:click=move |_| save_current()
                    title="保存回对象存储"
                >
                    "💾 保存"
                </button>
                <span class="oss-status">{move || ctx.status.get()}</span>
            </div>
            <div class="oss-body">
                <aside class="oss-side">
                    <div class="oss-tools">
                        <button type="button" class="tool" on:click=move |_| create_in(String::new(), false) title="在根目录新建 Markdown 文件">
                            "＋ 文件"
                        </button>
                        <button type="button" class="tool" on:click=move |_| create_in(String::new(), true) title="在根目录新建文件夹">
                            "＋ 文件夹"
                        </button>
                    </div>
                    <div class="oss-tree">
                        {move || {
                            if ctx.tree.with(|t| t.dirs.is_empty() && t.files.is_empty()) {
                                view! { <div class="oss-hint">"（空桶，点上方按钮新建）"</div> }.into_any()
                            } else {
                                view! { <span style="display:none"></span> }.into_any()
                            }
                        }}
                        <For
                            each=move || ctx.tree.with(|t| t.dirs.clone())
                            key=|d| d.prefix.clone()
                            children=move |d| view! { <TreeDir prefix=d.prefix.clone()/> }
                        />
                        <For
                            each=move || ctx.tree.with(|t| t.files.clone())
                            key=|f| f.key.clone()
                            children=move |f| {
                                let key_open = f.key.clone();
                                let name_open = f.name.clone();
                                let key_ren = f.key.clone();
                                let key_del = f.key.clone();
                                let key_active = f.key.clone();
                                let label = f.name.clone();
                                view! {
                                    <div
                                        class="oss-trow oss-tfile"
                                        class:oss-active=move || ctx.current.get().as_deref() == Some(key_active.as_str())
                                    >
                                        <button type="button" class="oss-open"
                                            on:click=move |_| open_file(key_open.clone(), name_open.clone())
                                        >
                                            <span class="oss-ticon">"📄"</span>
                                            <span class="oss-tname">{label}</span>
                                        </button>
                                        <span class="oss-tact">
                                            <button type="button" title="重命名文件" on:click=move |_| rename_entry(key_ren.clone(), false)>"✏️"</button>
                                            <button type="button" title="删除文件" on:click=move |_| delete_entry(key_del.clone(), false)>"🗑"</button>
                                        </span>
                                    </div>
                                }
                            }
                        />
                    </div>
                </aside>
                <main class="oss-main">
                    <div class="oss-filebar">
                        {move || ctx.current_name.get()}
                        {move || {
                            (ctx.dirty.get() && ctx.current.get().is_some())
                                .then(|| view! { <span class="oss-dirty">"● 未保存"</span> })
                        }}
                    </div>
                    <div class="oss-editor" node_ref=editor_ref></div>
                </main>
            </div>
        </div>
    }
    .into_any()
}

// ---- signer cross-check ----
//
// Fixed-input test vector generated with the independent Python
// implementation (/tmp oss probe, hashlib+hmac stdlib) — the two signers
// must agree byte-for-byte.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signer_matches_python_reference() {
        let creds = OssCreds {
            ak: "mpzfivcp",
            sk: "96p89bwhdqpmhd6z",
            region: "us-east-1",
            endpoint: "objectstorageapi.bja.sealos.run",
        };
        let sig = sign_request_with(
            &creds,
            "GET",
            "/mpzfivcp-watchwonderful",
            &[],
            b"",
            1767323045000.0,
        );
        assert_eq!(sig.x_amz_date, "20260102T030405Z");
        assert_eq!(
            sig.x_amz_content_sha256,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sig.authorization,
            "AWS4-HMAC-SHA256 Credential=mpzfivcp/20260102/us-east-1/s3/aws4_request, \
             SignedHeaders=host;x-amz-content-sha256;x-amz-date, \
             Signature=35716eb22305276af82b26f0e4c0b32efc35715796da8ff4f7d79d41c6d060bd"
        );
    }

    #[test]
    fn uri_encode_keeps_unreserved_and_slash() {
        assert_eq!(aws_uri_encode("go语言视频/x y.md", false), "go%E8%AF%AD%E8%A8%80%E8%A7%86%E9%A2%91/x%20y.md");
        assert_eq!(aws_uri_encode("a/b", true), "a%2Fb");
        assert_eq!(aws_uri_encode("a-b_c.d~e", true), "a-b_c.d~e");
    }

    #[test]
    fn full_list_parses_and_paginates() {
        let xml = r#"<?xml version="1.0"?><ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
        <IsTruncated>true</IsTruncated><NextContinuationToken>tok123</NextContinuationToken>
        <Contents><Key>a/b.md</Key><LastModified>2026-10-28T23:27:57.331Z</LastModified><Size>10</Size></Contents>
        </ListBucketResult>"#;
        let (entries, truncated, token) = parse_full_list(xml).unwrap();
        assert!(truncated);
        assert_eq!(token.as_deref(), Some("tok123"));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "a/b.md");
    }

    #[test]
    fn tree_builds_nested_dirs_and_skips_markers() {
        let entries = vec![
            OssEntry { name: "a/b.md".into(), key: "a/b.md".into(), size: 1, modified: String::new() },
            OssEntry { name: "a/c/d.md".into(), key: "a/c/d.md".into(), size: 1, modified: String::new() },
            OssEntry { name: "a/".into(), key: "a/".into(), size: 0, modified: String::new() },
            OssEntry { name: "root.md".into(), key: "root.md".into(), size: 1, modified: String::new() },
        ];
        let tree = build_tree(&entries, None);
        assert_eq!(tree.files.len(), 1);
        assert_eq!(tree.dirs.len(), 1);
        let a = &tree.dirs[0];
        assert_eq!(a.prefix, "a/");
        assert_eq!(a.files.len(), 1);
        assert_eq!(a.dirs.len(), 1);
        assert_eq!(a.dirs[0].prefix, "a/c/");
        assert_eq!(a.dirs[0].files[0].name, "d.md");
    }
}


