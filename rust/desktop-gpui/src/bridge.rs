//! Vditor 外部编辑器 loopback 桥(与 egui 版同一实现):
//! 127.0.0.1 随机端口,令牌随 URL 传给系统浏览器;/editor/doc GET 快照、
//! POST 写回(经 mpsc 通知主状态);/editor/upload 转发后端 /api/upload;
//! /vditor/* 静态资源。图片上传用主程序的 cookie jar。

use std::io::Read as _;
use std::sync::{Arc, Mutex};

use crate::state::{new_note_id, Msg};

#[derive(Clone)]
pub struct EditorDoc {
    pub id: String,
    pub title: String,
    pub content: String,
}

/// 启动桥(返回端口/令牌);正文回写经 tx 发 Msg::EditorPosted。
pub fn start_bridge(
    tx: std::sync::mpsc::Sender<Msg>,
    client: reqwest::blocking::Client,
    doc: Arc<Mutex<EditorDoc>>,
    api_base: Arc<Mutex<String>>,
) -> Option<(u16, String)> {
    let server = match tiny_http::Server::http("127.0.0.1:0") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("editor bridge failed: {e}");
            return None;
        }
    };
    let port = server.server_addr().to_ip().map(|a| a.port()).unwrap_or(0);
    let token = new_note_id() + &new_note_id();
    let tok = token.clone();
    std::thread::spawn(move || {
        for mut req in server.incoming_requests() {
            let resp = handle(&mut req, &tok, &doc, &tx, &client, &api_base);
            let _ = req.respond(resp);
        }
    });
    Some((port, token))
}

/// 资源目录:exe 旁 > .app 内 Resources > 源码目录(开发期)。
pub fn assets_dir() -> std::path::PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    let dir = exe.parent().unwrap_or(std::path::Path::new("."));
    for cand in [
        dir.join("assets"),
        dir.join("../Resources/assets"),
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets"),
        // .app 里是 Contents/MacOS → ../Resources/assets
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../Resources/assets"),
        // 开发期:复用 egui 版 desktop/assets(vditor + editor.html)
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../desktop/assets"),
    ] {
        if cand.exists() {
            return cand;
        }
    }
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets")
}

type Resp = tiny_http::Response<std::io::Cursor<Vec<u8>>>;

fn handle(
    req: &mut tiny_http::Request,
    token: &str,
    doc: &Arc<Mutex<EditorDoc>>,
    tx: &std::sync::mpsc::Sender<Msg>,
    client: &reqwest::blocking::Client,
    api_base: &Arc<Mutex<String>>,
) -> Resp {
    use tiny_http::{Header, Response};
    let ok = |body: String, ctype: &str| {
        Response::from_string(body).with_header(
            Header::from_bytes(&b"Content-Type"[..], ctype.as_bytes()).unwrap(),
        )
    };
    let cors = |r: Resp| {
        r.with_header(Header::from_bytes(&b"Access-Control-Allow-Origin"[..], &b"*"[..]).unwrap())
            .with_header(Header::from_bytes(&b"Access-Control-Allow-Headers"[..], &b"*"[..]).unwrap())
            .with_header(Header::from_bytes(&b"Access-Control-Allow-Methods"[..], &b"*"[..]).unwrap())
    };
    let url = req.url().to_string();
    if std::env::var("GPUI_BRIDGE_LOG").is_ok() {
        eprintln!("[bridge] {} {}", req.method(), url.split('?').next().unwrap_or(""));
    }
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let q = |k: &str| -> String {
        query
            .split('&')
            .filter_map(|p| p.split_once('='))
            .find(|(a, _)| *a == k)
            .map(|(_, v)| urldecode(v))
            .unwrap_or_default()
    };
    if *req.method() == tiny_http::Method::Options {
        return cors(ok(String::new(), "text/plain"));
    }

    match (req.method().clone(), path) {
        (tiny_http::Method::Get, "/editor/page") => {
            let html = std::fs::read_to_string(assets_dir().join("editor.html"))
                .unwrap_or_else(|e| format!("editor.html 缺失:{e}"));
            cors(ok(html, "text/html; charset=utf-8"))
        }
        (tiny_http::Method::Get, "/editor/doc") => {
            if q("token") != token {
                return cors(ok("{\"error\":\"forbidden\"}".into(), "application/json")
                    .with_status_code(403));
            }
            let id = q("id");
            let d = doc.lock().unwrap();
            if d.id != id {
                return cors(
                    ok("{\"error\":\"doc mismatch\"}".into(), "application/json").with_status_code(409)
                );
            }
            cors(ok(
                serde_json::json!({"content": d.content, "title": d.title}).to_string(),
                "application/json",
            ))
        }
        (tiny_http::Method::Post, "/editor/doc") => {
            if q("token") != token {
                return cors(ok("{\"error\":\"forbidden\"}".into(), "application/json")
                    .with_status_code(403));
            }
            let id = q("id");
            let mut body = String::new();
            if req.as_reader().read_to_string(&mut body).is_err() {
                return cors(
                    ok("{\"error\":\"bad body\"}".into(), "application/json").with_status_code(400)
                );
            }
            let content = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v["content"].as_str().map(|s| s.to_string()))
                .unwrap_or_default();
            {
                let mut d = doc.lock().unwrap();
                if d.id == id {
                    d.content = content.clone();
                }
            }
            let _ = tx.send(Msg::EditorPosted(content));
            cors(ok("{\"ok\":true}".into(), "application/json"))
        }
        (tiny_http::Method::Post, "/editor/upload") => {
            if q("token") != token {
                return cors(ok("{\"succMap\":{}}".into(), "application/json"));
            }
            let mut body = Vec::new();
            if req.as_reader().read_to_end(&mut body).is_err() {
                return cors(ok("{\"succMap\":{}}".into(), "application/json"));
            }
            let ct = req
                .headers()
                .iter()
                .find(|h| h.field.equiv("Content-Type"))
                .and_then(|h| h.value.as_str().parse::<String>().ok())
                .unwrap_or_default();
            let base = api_base.lock().map(|b| b.clone()).unwrap_or_default();
            let result = parse_multipart(&ct, &body)
                .and_then(|(fname, bytes)| upload_to_backend(client, &base, &fname, &bytes));
            let succ = match result {
                Ok(url) => serde_json::json!({ fname_of(&url): url }),
                Err(_) => serde_json::json!({}),
            };
            cors(ok(serde_json::json!({"succMap": succ}).to_string(), "application/json"))
        }
        (tiny_http::Method::Get, p) if p.starts_with("/vditor/") => {
            let rel = p.trim_start_matches("/vditor/");
            if rel.contains("..") {
                return cors(ok("not found".into(), "text/plain").with_status_code(404));
            }
            let file = assets_dir().join("vditor").join(rel);
            match std::fs::read(&file) {
                Ok(bytes) => {
                    let ctype = match file.extension().and_then(|e| e.to_str()) {
                        Some("js") => "text/javascript",
                        Some("css") => "text/css",
                        Some("png") => "image/png",
                        Some("svg") => "image/svg+xml",
                        Some("woff") => "font/woff",
                        Some("woff2") => "font/woff2",
                        Some("ttf") => "font/ttf",
                        _ => "application/octet-stream",
                    };
                    cors(ok(String::from_utf8_lossy(&bytes).into_owned(), ctype))
                }
                Err(_) => cors(ok("not found".into(), "text/plain").with_status_code(404)),
            }
        }
        _ => cors(ok("not found".into(), "text/plain").with_status_code(404)),
    }
}

fn fname_of(url: &str) -> String {
    url.rsplit('/').next().unwrap_or(url).to_string()
}

fn parse_multipart(content_type: &str, body: &[u8]) -> Result<(String, Vec<u8>), String> {
    let boundary = content_type
        .split("boundary=")
        .nth(1)
        .ok_or("no boundary")?
        .trim_matches('"')
        .to_string();
    let marker = format!("--{boundary}");
    let text = String::from_utf8_lossy(body);
    for part in text.split(&marker) {
        if part.contains("name=\"file[]\"") && part.contains("filename=\"") {
            let fname = part
                .split("filename=\"")
                .nth(1)
                .and_then(|s| s.split('"').next())
                .unwrap_or("upload.bin")
                .to_string();
            let content = part
                .split("\r\n\r\n")
                .nth(1)
                .unwrap_or("")
                .trim_end_matches("\r\n-")
                .trim_end_matches("\r\n")
                .as_bytes()
                .to_vec();
            if !content.is_empty() {
                return Ok((fname, content));
            }
        }
    }
    Err("no file part".into())
}

fn upload_to_backend(
    client: &reqwest::blocking::Client,
    base: &str,
    fname: &str,
    bytes: &[u8],
) -> Result<String, String> {
    let part = reqwest::blocking::multipart::Part::bytes(bytes.to_vec())
        .file_name(fname.to_string())
        .mime_str("application/octet-stream")
        .map_err(|e| e.to_string())?;
    let form = reqwest::blocking::multipart::Form::new().part("file[]", part);
    let resp = client
        .post(format!("{base}/api/upload"))
        .multipart(form)
        .send()
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let v: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
    v["data"][0]["url"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "no url".into())
}

fn urldecode(s: &str) -> String {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(h) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(h);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
