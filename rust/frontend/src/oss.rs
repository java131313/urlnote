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

#[derive(Clone)]
pub struct OssEntry {
    /// file name relative to the listed prefix
    pub name: String,
    pub key: String,
    pub size: u64,
    pub modified: String,
}

/// List one directory level: common prefixes (dirs) and files under `prefix`.
pub async fn list_objects(prefix: &str) -> Result<(Vec<String>, Vec<OssEntry>), String> {
    let query = [
        ("delimiter", "/"),
        ("list-type", "2"),
        ("max-keys", "1000"),
        ("prefix", prefix),
    ];
    let path = format!("/{OSS_BUCKET}");
    let sig = sign_request("GET", &path, &query, b"", now_ms());
    let qs = canonical_query(&query);
    let url = format!("https://{OSS_ENDPOINT}{path}?{qs}");
    let resp = apply_sig(gloo_net::http::Request::get(&url), &sig)
        .send()
        .await
        .map_err(|e| format!("网络错误: {e}"))?;
    if !resp.ok() {
        return Err(format!("列目录失败 (HTTP {})", resp.status()));
    }
    let text = resp
        .text()
        .await
        .map_err(|e| format!("响应读取失败: {e}"))?;
    parse_list(&text, prefix)
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

/// Parse a ListObjectsV2 response into (dir prefixes, files under `prefix`).
fn parse_list(xml: &str, prefix: &str) -> Result<(Vec<String>, Vec<OssEntry>), String> {
    use quick_xml::events::Event;
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut stack: Vec<String> = Vec::new();
    let mut cur: Option<OssEntry> = None;
    loop {
        let ev = reader
            .read_event()
            .map_err(|e| format!("目录解析失败: {e}"))?;
        match ev {
            Event::Start(e) => {
                stack.push(String::from_utf8_lossy(e.name().as_ref()).into_owned());
            }
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
                    ("CommonPrefixes", "Prefix") => dirs.push(s.into_owned()),
                    ("Contents", "Key") => {
                        let key = s.into_owned();
                        let name = key
                            .strip_prefix(prefix)
                            .map(str::to_string)
                            .unwrap_or_else(|| key.clone());
                        cur = Some(OssEntry {
                            name,
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
                    _ => {}
                }
            }
            Event::End(e) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if name == "Contents" {
                    if let Some(c) = cur.take() {
                        files.push(c);
                    }
                }
                stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    dirs.sort();
    files.sort_by(|a, b| a.name.cmp(&b.name));
    Ok((dirs, files))
}

fn format_size(n: u64) -> String {
    if n >= 1 << 20 {
        format!("{:.1} MB", n as f64 / (1 << 20) as f64)
    } else if n >= 1 << 10 {
        format!("{:.1} KB", n as f64 / (1 << 10) as f64)
    } else {
        format!("{n} B")
    }
}

// ---- standalone editor ----

struct OssEditor {
    editor: VditorEditor,
    // keep the input callback alive
    #[allow(dead_code)]
    cbs: Vec<JsValue>,
}

thread_local! {
    static OSS_EDITOR: RefCell<Option<OssEditor>> = const { RefCell::new(None) };
}

fn init_editor(mount: &web_sys::Element, on_input: &Closure<dyn FnMut()>) -> Result<(), String> {
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
    OSS_EDITOR.with(|c| {
        *c.borrow_mut() = Some(OssEditor {
            editor,
            cbs: vec![on_input.as_ref().clone()],
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

    let prefix = RwSignal::new(String::new());
    let dirs = RwSignal::new(Vec::<String>::new());
    let files = RwSignal::new(Vec::<OssEntry>::new());
    let loading = RwSignal::new(true);
    let current = RwSignal::new(Option::<String>::None);
    let current_name = RwSignal::new(String::new());
    let dirty = RwSignal::new(false);
    let status = RwSignal::new(String::new());
    let editor_ref = NodeRef::<Div>::new();

    // editor: init once when the mount node appears; input marks dirty
    Effect::new(move |_| {
        let _ = editor_ref.get();
        let already = OSS_EDITOR.with(|c| c.borrow().is_some());
        if already {
            return;
        }
        if let Some(el) = editor_ref.get() {
            let d = dirty;
            let cb = Closure::<dyn FnMut()>::new(move || d.set(true));
            if let Err(e) = init_editor(el.unchecked_ref::<web_sys::Element>(), &cb) {
                status.set(e);
            }
        }
    });

    // listing: reload whenever the prefix changes (also on first mount)
    Effect::new(move |_| {
        let p = prefix.get();
        loading.set(true);
        spawn_local(async move {
            match list_objects(&p).await {
                Ok((d, f)) => {
                    dirs.set(d);
                    files.set(f);
                    loading.set(false);
                }
                Err(e) => {
                    status.set(e);
                    loading.set(false);
                }
            }
        });
    });

    let open_file = move |key: String, name: String| {
        status.set(format!("载入 {name}…"));
        spawn_local(async move {
            match get_object(&key).await {
                Ok(text) => {
                    set_editor_value(&text);
                    current.set(Some(key));
                    current_name.set(name.clone());
                    dirty.set(false);
                    if let Some(doc) = window().document() {
                        doc.set_title(&format!("{name} · 文件库"));
                    }
                    status.set(format!("已载入 {name}"));
                }
                Err(e) => status.set(e),
            }
        });
    };

    let save = move |_| {
        let Some(key) = current.get() else { return };
        let content = editor_value();
        status.set("保存中…".to_string());
        spawn_local(async move {
            match put_object(&key, &content).await {
                Ok(()) => {
                    dirty.set(false);
                    status.set(format!("已保存 {}", current_time()));
                }
                Err(e) => status.set(e),
            }
        });
    };

    let crumbs = Memo::new(move |_| {
        let p = prefix.get();
        let mut items = vec![("根目录".to_string(), String::new())];
        let mut acc = String::new();
        for seg in p.trim_end_matches('/').split('/') {
            if seg.is_empty() {
                continue;
            }
            acc.push_str(seg);
            acc.push('/');
            items.push((seg.to_string(), acc.clone()));
        }
        items
    });

    view! {
        <div class="oss-page">
            <div class="oss-top">
                <a class="oss-home" href=format!("{BASE_PATH}/") title="返回便签页面">"📝 便签"</a>
                <span class="oss-brand">"🗂 文件库"</span>
                <span class="oss-spacer"></span>
                <button
                    type="button"
                    class="tool oss-save"
                    prop:disabled=move || !(dirty.get() && current.get().is_some())
                    on:click=save
                    title="保存回对象存储"
                >
                    "💾 保存"
                </button>
                <span class="oss-status">{move || status.get()}</span>
            </div>
            <div class="oss-body">
                <aside class="oss-side">
                    <nav class="oss-crumb">
                        <For
                            each=move || crumbs.get()
                            key=|(_, p)| p.clone()
                            children=move |(name, p)| {
                                view! {
                                    <button
                                        type="button"
                                        class="oss-crumb-item"
                                        on:click=move |_| prefix.set(p.clone())
                                    >
                                        {name}
                                    </button>
                                }
                            }
                        />
                    </nav>
                    <div class="oss-list">
                        {move || {
                            if loading.get() {
                                view! { <div class="oss-hint">"加载中…"</div> }.into_any()
                            } else if dirs.get().is_empty() && files.get().is_empty() {
                                view! { <div class="oss-hint">"（空目录）"</div> }.into_any()
                            } else {
                                view! { <span style="display:none"></span> }.into_any()
                            }
                        }}
                        <For
                            each=move || dirs.get()
                            key=|d| d.clone()
                            children=move |d| {
                                let name = d
                                    .trim_end_matches('/')
                                    .rsplit('/')
                                    .next()
                                    .unwrap_or("")
                                    .to_string();
                                let target = d.clone();
                                view! {
                                    <button
                                        type="button"
                                        class="oss-row oss-dir"
                                        on:click=move |_| prefix.set(target.clone())
                                    >
                                        "📁 " {name}
                                    </button>
                                }
                            }
                        />
                        <For
                            each=move || files.get()
                            key=|f| f.key.clone()
                            children=move |f| {
                                let name = f.name.clone();
                                let label = name.clone();
                                let key = f.key.clone();
                                let meta = format!("{} · {}", format_size(f.size), f.modified);
                                let open = open_file;
                                view! {
                                    <button
                                        type="button"
                                        class="oss-row oss-file"
                                        on:click=move |_| open(key.clone(), name.clone())
                                    >
                                        "📄 " {label}
                                        <span class="oss-meta">{meta}</span>
                                    </button>
                                }
                            }
                        />
                    </div>
                </aside>
                <main class="oss-main">
                    <div class="oss-filebar">
                        {move || current_name.get()}
                        {move || {
                            (dirty.get() && current.get().is_some())
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
    fn list_xml_parses_dirs_and_files() {
        let xml = r#"<?xml version="1.0"?><ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
        <Name>b</Name><Prefix>docs/</Prefix><KeyCount>3</KeyCount>
        <CommonPrefixes><Prefix>docs/sub/</Prefix></CommonPrefixes>
        <Contents><Key>docs/a.md</Key><LastModified>2026-10-28T23:27:57.331Z</LastModified><Size>21497</Size></Contents>
        <Contents><Key>docs/b.md</Key><LastModified>2026-10-29T01:00:00.000Z</LastModified><Size>100</Size></Contents>
        </ListBucketResult>"#;
        let (dirs, files) = parse_list(xml, "docs/").unwrap();
        assert_eq!(dirs, vec!["docs/sub/".to_string()]);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].name, "a.md");
        assert_eq!(files[0].size, 21497);
        assert_eq!(files[0].modified, "2026-10-28");
        assert_eq!(files[1].name, "b.md");
    }
}


