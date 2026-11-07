//! urlnote macOS 原生桌面版 —— eframe/egui UI + reqwest 直连 urlnote-server。
//!
//! 与 Web 版(react)同一份后端契约(shared 类型),无 webview、无 CORS、
//! cookie 由 reqwest  cookie jar 管理。第一阶段覆盖:登录、便签列表/搜索、
//! markdown 源码编辑(预览/文件库/批注/在线 Office 为后续阶段)。

use eframe::egui::{self, FontId, Key, RichText, TextEdit};
use shared::{
    DictLang, DictLangList, DictLookupResponse, DictWordItem, LoginRequest, Note, NoteList,
    NoteMeta, OkResponse, SaveRequest, WhoamiResponse, MAX_FONT_SIZE, MIN_FONT_SIZE,
};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod draw;
mod s3;

/// 默认后端(与 Web 前端 VITE_API_BASE 一致);登录页可改,
/// 持久化到 ~/.urlnote/server.conf,便于指向本地 cargo run 起的 server。
const DEFAULT_API: &str = "https://yyuqmndqpnjr.sealosbja.site";

fn normalize_base(s: &str) -> String {
    s.trim().trim_end_matches('/').to_string()
}

fn load_api_base() -> String {
    let conf = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
        .join(".urlnote/server.conf");
    std::fs::read_to_string(&conf)
        .ok()
        .map(|s| normalize_base(&s))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_API.to_string())
}

fn save_api_base(base: &str) {
    let dir = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
        .join(".urlnote");
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = std::fs::write(dir.join("server.conf"), base);
    }
}

const AUTOSAVE_DELAY: Duration = Duration::from_millis(800);
const SEARCH_DELAY: Duration = Duration::from_millis(300);
const DICT_DELAY: Duration = Duration::from_millis(200);

enum Msg {
    Whoami(Result<String, String>),
    Login(Result<(), String>),
    Notes(Result<Vec<NoteMeta>, String>),
    NoteLoaded(Result<Note, String>),
    Saved { id: String, res: Result<(), String> },
    Search(Result<Vec<NoteMeta>, String>),
    Dict(Result<DictLookupResponse, String>),
    Langs(Vec<DictLang>),
    Logout,
    // ---- 文件库(S3) ----
    OssCreds(Result<s3::OssCreds, String>),
    OssList(Result<(String, s3::ListResult), String>),
    OssGot(Result<(String, String, String), String>), // key, 正文, settings.json
    OssPut(Result<String, String>),                   // key
    OssDelete(Result<(), String>),
    // ---- Vditor 外部编辑器(loopback 桥) ----
    EditorPosted(String),
    // ---- 画图 ----
    DrawList(Result<Vec<draw::DrawMeta>, String>),
    DrawGot(Result<draw::DrawDoc, String>),
    DrawSaved { id: String, res: Result<(), String> },
    DrawDeleted(Result<String, String>),
}

/// 后台线程执行请求,完成后经 channel 回 UI 线程并触发重绘。
fn go(
    client: &reqwest::blocking::Client,
    ctx: &egui::Context,
    tx: &Sender<Msg>,
    f: impl FnOnce(&reqwest::blocking::Client) -> Msg + Send + 'static,
) {
    let client = client.clone();
    let ctx = ctx.clone();
    let tx = tx.clone();
    std::thread::spawn(move || {
        let m = f(&client);
        let _ = tx.send(m);
        ctx.request_repaint();
    });
}

fn err(e: reqwest::Error) -> String {
    format!("{}: {}", e.status().map(|s| s.to_string()).unwrap_or_default(), e)
}

fn new_note_id() -> String {
    use rand::RngCore;
    let alphabet = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut buf = [0u8; 5];
    rand::thread_rng().fill_bytes(&mut buf);
    buf.iter().map(|b| alphabet[(*b as usize) % 62] as char).collect()
}

struct EdNote {
    note: Note,
    dirty: bool,
    last_edit: Instant,
    saving: bool,
}

impl EdNote {
    fn new(id: String) -> Self {
        Self {
            note: Note::empty(&id),
            dirty: false,
            last_edit: Instant::now(),
            saving: false,
        }
    }
    fn loaded(note: Note) -> Self {
        Self { note, dirty: false, last_edit: Instant::now(), saving: false }
    }
    fn mark_edit(&mut self) {
        self.dirty = true;
        self.last_edit = Instant::now();
    }
    fn save_req(&self) -> SaveRequest {
        SaveRequest {
            content: self.note.content.clone(),
            font_size: Some(self.note.font_size as f64),
            color: self.note.color.clone(),
            title: self.note.title.clone(),
            tags: self.note.tags.clone(),
        }
    }
}

struct DictState {
    langs: Vec<DictLang>,
    lang: String,
    words: Vec<DictWordItem>,
    /// 当前词典面板对应的锚词(光标前单词),避免重复请求。
    shown_for: String,
    loading: bool,
    sel: usize,
    last_req: Instant,
}

impl DictState {
    fn new() -> Self {
        Self {
            langs: Vec::new(),
            lang: String::new(),
            words: Vec::new(),
            shown_for: String::new(),
            loading: false,
            sel: 0,
            last_req: Instant::now(),
        }
    }
    fn visible(&self) -> bool {
        self.loading || !self.words.is_empty()
    }
    fn close(&mut self) {
        self.words.clear();
        self.shown_for.clear();
    }
}

enum Screen {
    Login,
    Main,
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    Notes,
    Oss,
    Draw,
}

struct OssFileEd {
    key: String,
    content: String,
    font_size: i64,
    color: String,
    dirty: bool,
    last_edit: Instant,
    saving: bool,
}

impl OssFileEd {
    fn mark_edit(&mut self) {
        self.dirty = true;
        self.last_edit = Instant::now();
    }
}

struct OssState {
    creds: Option<s3::OssCreds>,
    creds_err: String,
    /// 当前目录 prefix("" = 根)。
    prefix: String,
    dirs: Vec<String>,
    files: Vec<s3::ListedFile>,
    loading: bool,
    current: Option<OssFileEd>,
    new_file: bool,
    new_folder: bool,
    input: String,
    confirm_delete: Option<(String, bool)>, // (name, 是否目录)
    status: String,
}

impl OssState {
    fn new() -> Self {
        Self {
            creds: None,
            creds_err: String::new(),
            prefix: String::new(),
            dirs: Vec::new(),
            files: Vec::new(),
            loading: false,
            current: None,
            new_file: false,
            new_folder: false,
            input: String::new(),
            confirm_delete: None,
            status: String::new(),
        }
    }
    fn close_dialogs(&mut self) {
        self.new_file = false;
        self.new_folder = false;
        self.confirm_delete = None;
    }
}

/// Vditor 编辑器的桥状态:GET 快照(浏览器承载页面,无子进程)。
struct EditorCtl {
    /// loopback 服务端口与令牌(随页面 URL 传给浏览器)。
    port: u16,
    token: String,
    /// 当前编辑器承载的文档快照(/editor/doc 由此应答;POST 写回)。
    doc: Arc<Mutex<EditorDoc>>,
    open_id: String,
    /// 后端地址(登录改服务器时同步;桥的图片上传用)。
    api_base: Arc<Mutex<String>>,
}

#[derive(Clone)]
struct EditorDoc {
    id: String,
    title: String,
    content: String,
}

impl EditorCtl {
    fn empty() -> Self {
        Self {
            port: 0,
            token: String::new(),
            doc: Arc::new(Mutex::new(EditorDoc { id: String::new(), title: String::new(), content: String::new() })),
            open_id: String::new(),
            api_base: Arc::new(Mutex::new(load_api_base())),
        }
    }
}

impl EditorCtl {
    /// 启动 loopback HTTP 桥(127.0.0.1 随机端口),供编辑器页面与
    /// 主程序交换正文/资源/图片上传。令牌随 URL 传递,拒绝越权 id。
    fn start_bridge(&mut self, tx: Sender<Msg>, ctx: egui::Context, client: reqwest::blocking::Client) {
        let server = match tiny_http::Server::http("127.0.0.1:0") {
            Ok(s) => s,
            Err(e) => {
                eprintln!("editor bridge failed: {e}");
                return;
            }
        };
        self.port = server.server_addr().to_ip().map(|a| a.port()).unwrap_or(0);
        self.token = new_note_id() + &new_note_id();

        let token = self.token.clone();
        let doc = self.doc.clone();
        let api_base = self.api_base.clone();
        std::thread::spawn(move || {
            for mut req in server.incoming_requests() {
                let resp = bridge_handle(&mut req, &token, &doc, &tx, &client, &api_base);
                let _ = req.respond(resp);
                ctx.request_repaint();
            }
        });
    }
}

/// 桥请求处理(运行在桥线程):页面/正文/资源/上传。
fn bridge_handle(
    req: &mut tiny_http::Request,
    token: &str,
    doc: &Arc<Mutex<EditorDoc>>,
    tx: &Sender<Msg>,
    client: &reqwest::blocking::Client,
    api_base: &Arc<Mutex<String>>,
) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    use tiny_http::{Header, Response};
    let ok = |body: String, ctype: &str| {
        Response::from_string(body).with_header(
            Header::from_bytes(&b"Content-Type"[..], ctype.as_bytes()).unwrap(),
        )
    };
    let cors = |r: Response<std::io::Cursor<Vec<u8>>>| {
        r.with_header(Header::from_bytes(&b"Access-Control-Allow-Origin"[..], &b"*"[..]).unwrap())
            .with_header(Header::from_bytes(&b"Access-Control-Allow-Headers"[..], &b"*"[..]).unwrap())
            .with_header(Header::from_bytes(&b"Access-Control-Allow-Methods"[..], &b"*"[..]).unwrap())
    };
    let url = req.url().to_string();
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
                return cors(ok("{\"error\":\"forbidden\"}".into(), "application/json").with_status_code(403));
            }
            let id = q("id");
            let d = doc.lock().unwrap();
            if d.id != id {
                return cors(ok("{\"error\":\"doc mismatch\"}".into(), "application/json").with_status_code(409));
            }
            cors(ok(
                serde_json::json!({"content": d.content, "title": d.title}).to_string(),
                "application/json",
            ))
        }
        (tiny_http::Method::Post, "/editor/doc") => {
            if q("token") != token {
                return cors(ok("{\"error\":\"forbidden\"}".into(), "application/json").with_status_code(403));
            }
            let id = q("id");
            let mut body = String::new();
            if req.as_reader().read_to_string(&mut body).is_err() {
                return cors(ok("{\"error\":\"bad body\"}".into(), "application/json").with_status_code(400));
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
            // 简易 multipart 解析(vditor 单文件字段 file[]),转发后端 /api/upload。
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

/// 极简 multipart:取第一个 file[] 文件(filename + 内容字节)。
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

/// 转发后端 /api/upload(带主程序的会话 cookie),返回图片 URL。
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

/// 资源目录:exe 旁 > .app 内 Resources > 源码目录(开发期)。
fn assets_dir() -> std::path::PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    let dir = exe.parent().unwrap_or(std::path::Path::new("."));
    for cand in [
        dir.join("assets"),
        dir.join("../Resources/assets"),
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets"),
    ] {
        if cand.exists() {
            return cand;
        }
    }
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets")
}

struct UrlnoteApp {
    ctx: egui::Context,
    client: reqwest::blocking::Client,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    screen: Screen,
    // 登录
    username: String,
    password: String,
    login_err: String,
    busy: bool,
    // 列表
    notes: Vec<NoteMeta>,
    notes_loading: bool,
    search: String,
    search_dirty_at: Option<Instant>,
    searching: bool,
    // 编辑
    current: Option<EdNote>,
    status: String,
    dict: DictState,
    /// 编辑器光标当前字节位置(由 TextEditOutput 回填)。
    cursor_index: usize,
    view: View,
    oss: OssState,
    editor: EditorCtl,
    draw: draw::DrawState,
    /// 当前后端地址(登录页可改,成功登录后持久化)。
    api_base: String,
    /// 登录页的服务器地址输入框。
    server: String,
}

impl UrlnoteApp {
    fn new(ctx: &egui::Context) -> Self {
        let client = reqwest::blocking::Client::builder()
            .cookie_store(true)
            .user_agent("urlnote-desktop/0.1")
            .build()
            .expect("http client");
        let (tx, rx) = channel();
        let mut editor = EditorCtl::empty();
        editor.start_bridge(tx.clone(), ctx.clone(), client.clone());
        let mut app = Self {
            ctx: ctx.clone(),
            client,
            tx,
            rx,
            screen: Screen::Login,
            username: String::new(),
            password: String::new(),
            login_err: String::new(),
            busy: false,
            notes: Vec::new(),
            notes_loading: false,
            search: String::new(),
            search_dirty_at: None,
            searching: false,
            current: None,
            status: String::new(),
            dict: DictState::new(),
            cursor_index: 0,
            view: View::Notes,
            oss: OssState::new(),
            editor,
            draw: draw::DrawState::new(),
            api_base: load_api_base(),
            server: String::new(),
        };
        app.server = app.api_base.clone();
        // 启动时探测既有会话(cookie jar 为空,必然未登录,仅校验连通性)。
        let tx0 = app.rx_tx();
        let base = app.api_base.clone();
        go(&app.client, ctx, &tx0, move |c| {
            let r = c.get(format!("{base}/api/whoami")).send();
            Msg::Whoami(match r {
                Ok(resp) => {
                    if resp.status().is_success() {
                        match resp.json::<WhoamiResponse>() {
                            Ok(w) => Ok(w.user.username),
                            Err(e) => Err(err(e)),
                        }
                    } else {
                        Err("unauthorized".into())
                    }
                }
                Err(e) => Err(err(e)),
            })
        });
        app.busy = true;
        app
    }

    fn enter_main(&mut self) {
        self.screen = Screen::Main;
        self.refresh_notes();
        let tx = self.rx_tx();
        let base = self.api_base.clone();
        go(&self.client, &self.ctx, &tx, move |c| {
            let r = c.get(format!("{base}/api/dict/langs")).send();
            Msg::Langs(match r {
                Ok(resp) if resp.status().is_success() => {
                    resp.json::<DictLangList>().map(|l| l.langs).unwrap_or_default()
                }
                _ => Vec::new(),
            })
        });
    }

    /// Sender 副本(Receiver 不 Clone,请求派发都要用)。
    fn rx_tx(&self) -> Sender<Msg> {
        self.tx.clone()
    }

    fn refresh_notes(&mut self) {
        self.notes_loading = true;
        let tx = self.rx_tx();
        let base = self.api_base.clone();
        go(&self.client, &self.ctx, &tx, move |c| {
            let r = c.get(format!("{base}/api/notes")).send();
            Msg::Notes(match r {
                Ok(resp) if resp.status().is_success() => {
                    resp.json::<NoteList>().map(|l| l.notes).map_err(err)
                }
                Ok(resp) => Err(format!("HTTP {}", resp.status())),
                Err(e) => Err(err(e)),
            })
        });
    }

    fn load_note(&mut self, id: &str) {
        self.save_now_if_dirty();
        let id = id.to_string();
        self.status = format!("打开 {id}…");
        let tx = self.rx_tx();
        let base = self.api_base.clone();
        go(&self.client, &self.ctx, &tx, move |c| {
            let r = c.get(format!("{base}/api/note/{id}")).send();
            Msg::NoteLoaded(match r {
                Ok(resp) if resp.status().is_success() => resp.json::<Note>().map_err(err),
                Ok(resp) => Err(format!("HTTP {}", resp.status())),
                Err(e) => Err(err(e)),
            })
        });
    }

    fn save_now_if_dirty(&mut self) {
        if let Some(ed) = &mut self.current {
            if ed.dirty {
                self.send_save();
            }
        }
    }

    fn send_save(&mut self) {
        let Some(ed) = &mut self.current else { return };
        if ed.saving {
            return;
        }
        ed.saving = true;
        ed.dirty = false;
        let id = ed.note.id.clone();
        let body = ed.save_req();
        let tx = self.rx_tx();
        let base = self.api_base.clone();
        go(&self.client, &self.ctx, &tx, move |c| {
            let r = c
                .put(format!("{base}/api/note/{id}"))
                .json(&body)
                .send();
            Msg::Saved {
                id: id.clone(),
                res: match r {
                    Ok(resp) if resp.status().is_success() => Ok(()),
                    Ok(resp) => Err(format!("HTTP {}", resp.status())),
                    Err(e) => Err(err(e)),
                },
            }
        });
    }

    fn new_note(&mut self) {
        self.save_now_if_dirty();
        let id = new_note_id();
        self.current = Some(EdNote::new(id));
        self.dict.close();
        self.status = "新便签(输入即自动保存)".into();
    }

    fn do_login(&mut self) {
        if self.busy {
            return;
        }
        // 登录页可改后端地址(本地 cargo run 的 server 用 http://localhost:8080)
        self.api_base = normalize_base(&self.server);
        if self.api_base.is_empty() {
            self.api_base = DEFAULT_API.to_string();
            self.server = self.api_base.clone();
        }
        if let Ok(mut b) = self.editor.api_base.lock() {
            *b = self.api_base.clone();
        }
        self.busy = true;
        self.login_err.clear();
        let body = LoginRequest {
            username: self.username.trim().to_string(),
            password: self.password.clone(),
        };
        let tx = self.rx_tx();
        let base = self.api_base.clone();
        go(&self.client, &self.ctx, &tx, move |c| {
            let r = c.post(format!("{base}/api/login")).json(&body).send();
            Msg::Login(match r {
                Ok(resp) if resp.status().is_success() => {
                    let _ = resp.json::<OkResponse>();
                    Ok(())
                }
                Ok(resp) if resp.status() == 401 => Err("用户名或密码错误".into()),
                Ok(resp) => Err(format!("HTTP {}", resp.status())),
                Err(e) => Err(err(e)),
            })
        });
    }

    fn do_search(&mut self) {
        let q = self.search.trim().to_string();
        self.searching = !q.is_empty();
        let tx = self.rx_tx();
        let base = self.api_base.clone();
        go(&self.client, &self.ctx, &tx, move |c| {
            let r = c
                .get(format!("{base}/api/search?q={}", urlencode(&q)))
                .send();
            Msg::Search(match r {
                Ok(resp) if resp.status().is_success() => {
                    resp.json::<NoteList>().map(|l| l.notes).map_err(err)
                }
                Ok(resp) => Err(format!("HTTP {}", resp.status())),
                Err(e) => Err(err(e)),
            })
        });
    }

    fn do_dict(&mut self, word: String) {
        if word == self.dict.shown_for && !self.dict.words.is_empty() {
            return;
        }
        if self.dict.loading && self.dict.shown_for == word {
            return;
        }
        self.dict.loading = true;
        self.dict.shown_for = word.clone();
        self.dict.last_req = Instant::now();
        let lang = self.dict.lang.clone();
        let tx = self.rx_tx();
        let base = self.api_base.clone();
        go(&self.client, &self.ctx, &tx, move |c| {
            let r = c
                .get(format!(
                    "{base}/api/dict/lookup?q={}&lang={}",
                    urlencode(&word),
                    urlencode(&lang)
                ))
                .send();
            Msg::Dict(match r {
                Ok(resp) if resp.status().is_success() => resp.json::<DictLookupResponse>().map_err(err),
                Ok(resp) => Err(format!("HTTP {}", resp.status())),
                Err(e) => Err(err(e)),
            })
        });
    }

    fn accept_dict(&mut self, idx: usize) {
        let Some(word) = self.dict.words.get(idx).map(|w| w.word.clone()) else { return };
        self.insert_word_at_cursor(&word);
        self.dict.close();
    }

    // ---- 文件库(S3) ----

    fn enter_oss(&mut self) {
        self.view = View::Oss;
        if self.oss.creds.is_some() {
            if self.oss.files.is_empty() && self.oss.dirs.is_empty() {
                self.oss_list(String::new());
            }
            return;
        }
        if !self.oss.creds_err.is_empty() {
            return; // 已尝试过且失败(错误显示在面板上)
        }
        let tx = self.rx_tx();
        go(&self.client, &self.ctx, &tx, |_| Msg::OssCreds(s3::load_creds()));
    }

    fn oss_list(&mut self, prefix: String) {
        self.oss.loading = true;
        let Some(creds) = self.oss.creds.clone() else { return };
        let tx = self.rx_tx();
        go(&self.client, &self.ctx, &tx, move |c| {
            let res = s3::list_dir(c, &creds, &prefix).map(|r| (prefix.clone(), r));
            Msg::OssList(res)
        });
    }

    fn oss_open(&mut self, key: String) {
        self.oss_save_now();
        let Some(creds) = self.oss.creds.clone() else { return };
        self.oss.status = format!("打开 {}…", key.rsplit('/').next().unwrap_or(&key));
        let tx = self.rx_tx();
        go(&self.client, &self.ctx, &tx, move |c| {
            let content = s3::get_object(c, &creds, &key).map(|o| o.unwrap_or_default());
            let settings = s3::get_object(c, &creds, &format!("{key}.settings.json"))
                .map(|o| o.unwrap_or_default());
            let res = match (content, settings) {
                (Ok(content), Ok(settings)) => Ok((key.clone(), content, settings)),
                (Err(e), _) => Err(e),
                (_, Err(e)) => Err(e),
            };
            Msg::OssGot(res)
        });
    }

    fn oss_save_now(&mut self) {
        let Some(f) = &mut self.oss.current else { return };
        if f.dirty && !f.saving {
            f.saving = true;
            f.dirty = false;
            let creds = match &self.oss.creds {
                Some(c) => c.clone(),
                None => return,
            };
            let key = f.key.clone();
            let content = f.content.clone();
            let settings = format!(
                "{{\"font_size\":{},\"color\":\"{}\"}}",
                f.font_size,
                f.color.replace('"', "")
            );
            let tx = self.rx_tx();
            go(&self.client, &self.ctx, &tx, move |c| {
                let res = s3::put_object(c, &creds, &key, content.as_bytes())
                    .and_then(|_| s3::put_object(c, &creds, &format!("{key}.settings.json"), settings.as_bytes()))
                    .map(|_| key.clone());
                Msg::OssPut(res)
            });
        }
    }

    fn oss_create(&mut self, folder: bool) {
        let name = self.oss.input.trim().trim_matches('/').to_string();
        if name.is_empty() {
            return;
        }
        self.oss.input.clear();
        self.oss.new_file = false;
        self.oss.new_folder = false;
        let key = if folder {
            format!("{}{name}/.keep", self.oss.prefix)
        } else {
            format!("{}{name}", self.oss.prefix)
        };
        let Some(creds) = self.oss.creds.clone() else { return };
        let prefix = self.oss.prefix.clone();
        let tx = self.rx_tx();
        go(&self.client, &self.ctx, &tx, move |c| {
            let res = s3::put_object(c, &creds, &key, b"").map(|_| prefix.clone());
            Msg::OssPut(res)
        });
    }

    fn oss_delete(&mut self, name: String, is_dir: bool) {
        let Some(creds) = self.oss.creds.clone() else { return };
        let key = format!("{}{name}", self.oss.prefix);
        let tx = self.rx_tx();
        go(&self.client, &self.ctx, &tx, move |c| {
            let res = if is_dir {
                match s3::list_all_keys(c, &creds, &key) {
                    Ok(keys) => {
                        let mut last = Ok(());
                        for k in keys {
                            if let Err(e) = s3::delete_object(c, &creds, &k) {
                                last = Err(e);
                                break;
                            }
                        }
                        last
                    }
                    Err(e) => Err(e),
                }
            } else {
                s3::delete_object(c, &creds, &key)
            };
            Msg::OssDelete(res)
        });
    }

    // ---- Vditor 外部编辑器 ----

    fn open_vditor(&mut self) {
        let Some(ed) = &self.current else { return };
        let id = ed.note.id.clone();
        let title = if ed.note.title.trim().is_empty() {
            id.clone()
        } else {
            ed.note.title.clone()
        };
        {
            let mut d = self.editor.doc.lock().unwrap();
            *d = EditorDoc { id: id.clone(), title: title.clone(), content: ed.note.content.clone() };
        }
        self.spawn_editor(&id, &title);
    }

    fn spawn_editor(&mut self, id: &str, _title: &str) {
        // 浏览器承载编辑器:页面与桥同源于 127.0.0.1,无跨域/ATS/cookie 问题。
        // (wry 内嵌窗口在 macOS 上对 http://127.0.0.1 白板,疑被 ATS 拦截,弃用。)
        self.close_editor();
        let url = format!(
            "http://127.0.0.1:{}/editor/page?id={}&token={}",
            self.editor.port, id, self.editor.token
        );
        match std::process::Command::new("open").arg(&url).spawn() {
            Ok(_) => {
                self.editor.open_id = id.to_string();
                eprintln!("vditor editor: {url}");
                let _ = std::fs::write("/tmp/urlnote_editor_url", &url);
                self.status = "已在浏览器中打开 Vditor 编辑器".into();
            }
            Err(e) => self.status = format!("打开浏览器失败:{e}"),
        }
    }

    fn close_editor(&mut self) {
        self.editor.open_id.clear();
    }

    fn oss_open_vditor(&mut self) {
        let Some(f) = &self.oss.current else { return };
        let key = f.key.clone();
        let title = key.rsplit('/').next().unwrap_or(&key).to_string();
        {
            let mut d = self.editor.doc.lock().unwrap();
            *d = EditorDoc { id: key.clone(), title: title.clone(), content: f.content.clone() };
        }
        self.spawn_editor(&key, &title);
    }

    /// 用选中的词典词替换光标前的锚词。
    fn insert_word_at_cursor(&mut self, word: &str) {
        let Some(ed) = &mut self.current else { return };
        let idx = self.cursor_index.min(ed.note.content.len());
        let start = word_start_before(&ed.note.content, idx);
        ed.note.content.replace_range(start..idx, word);
        self.cursor_index = start + word.len();
        ed.mark_edit();
    }

    fn poll(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Whoami(res) => {
                    self.busy = false;
                    match res {
                        Ok(_) => self.enter_main(),
                        Err(_) => {}
                    }
                }
                Msg::Login(res) => {
                    self.busy = false;
                    match res {
                        Ok(()) => {
                            save_api_base(&self.api_base);
                            self.password.clear();
                            self.enter_main();
                        }
                        Err(e) => self.login_err = e,
                    }
                }
                Msg::Notes(res) => {
                    self.notes_loading = false;
                    match res {
                        Ok(n) => self.notes = n,
                        Err(e) => self.status = e,
                    }
                }
                Msg::NoteLoaded(res) => {
                    self.status.clear();
                    match res {
                        Ok(n) => {
                            self.dict.close();
                            self.current = Some(EdNote::loaded(n));
                        }
                        Err(e) => self.status = e,
                    }
                }
                Msg::Saved { id, res } => {
                    if let Some(ed) = &mut self.current {
                        if ed.note.id == id {
                            ed.saving = false;
                            match res {
                                Ok(()) => self.status = "已保存".into(),
                                Err(e) => {
                                    ed.dirty = true;
                                    self.status = format!("保存失败:{e}");
                                }
                            }
                        }
                    }
                }
            Msg::Search(res) => {
                    self.searching = false;
                    if let Ok(n) = res {
                        self.notes = n;
                    }
                }                Msg::Dict(res) => {
                    self.dict.loading = false;
                    match res {
                        Ok(r) => {
                            self.dict.sel = 0;
                            self.dict.words = r.words;
                        }
                        Err(_) => self.dict.words.clear(),
                    }
                }
                Msg::Langs(langs) => {
                    if let Some(first) = langs.first() {
                        if self.dict.lang.is_empty() {
                            self.dict.lang = first.code.clone();
                        }
                    }
                    self.dict.langs = langs;
                }
                Msg::Logout => {
                    self.close_editor();
                    self.screen = Screen::Login;
                    self.current = None;
                    self.notes.clear();
                    self.dict.close();
                }
                Msg::DrawList(res) => {
                    self.draw.loading = false;
                    match res {
                        Ok(l) => self.draw.list = l,
                        Err(e) => self.draw.status = e,
                    }
                }
                Msg::DrawGot(res) => match res {
                    Ok(d) => {
                        if let Some(ed) = &mut self.draw.ed {
                            if ed.id == d.id {
                                ed.name = d.name;
                                ed.elements = d.elements;
                                ed.dirty = false;
                            }
                        }
                    }
                    Err(e) => self.draw.status = e,
                },
                Msg::DrawSaved { id, res } => {
                    if let Some(ed) = &mut self.draw.ed {
                        if ed.id == id {
                            ed.saving = false;
                            match res {
                                Ok(()) => {}
                                Err(e) => {
                                    ed.dirty = true;
                                    self.draw.status = e;
                                }
                            }
                        }
                    }
                }
                Msg::DrawDeleted(res) => {
                    self.draw.confirm_delete = None;
                    match res {
                        Ok(id) => {
                            self.draw.list.retain(|m| m.id != id);
                            if self.draw.ed.as_ref().map(|e| e.id.clone()) == Some(id) {
                                self.draw.ed = None;
                            }
                        }
                        Err(e) => self.draw.status = e,
                    }
                }
                Msg::EditorPosted(content) => {
                    if !self.editor.open_id.is_empty() {
                        let oid = self.editor.open_id.clone();
                        let mut applied = false;
                        if let Some(ed) = &mut self.current {
                            if ed.note.id == oid {
                                ed.note.content = content.clone();
                                ed.mark_edit();
                                applied = true;
                            }
                        }
                        if !applied {
                            if let Some(f) = &mut self.oss.current {
                                if f.key == oid {
                                    f.content = content;
                                    f.mark_edit();
                                }
                            }
                        }
                    }
                }
                Msg::OssCreds(res) => match res {
                    Ok(c) => {
                        self.oss.creds = Some(c);
                        self.oss_list(String::new());
                    }
                    Err(e) => self.oss.creds_err = e,
                },
                Msg::OssList(res) => {
                    self.oss.loading = false;
                    match res {
                        Ok((prefix, r)) => {
                            self.oss.prefix = prefix;
                            self.oss.dirs = r.dirs;
                            self.oss.files = r.files;
                        }
                        Err(e) => self.oss.status = e,
                    }
                }
                Msg::OssGot(res) => {
                    self.oss.status.clear();
                    match res {
                        Ok((key, content, settings)) => {
                            let v: serde_json::Value =
                                serde_json::from_str(&settings).unwrap_or_default();
                            let font_size = v["font_size"]
                                .as_i64()
                                .map(|n| n.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE))
                                .unwrap_or(16);
                            let color = v["color"].as_str().unwrap_or_default().to_string();
                            self.oss.current = Some(OssFileEd {
                                key,
                                content,
                                font_size,
                                color,
                                dirty: false,
                                last_edit: Instant::now(),
                                saving: false,
                            });
                        }
                        Err(e) => self.oss.status = e,
                    }
                }
                Msg::OssPut(res) => match res {
                    Ok(key) => {
                        if let Some(f) = &mut self.oss.current {
                            if f.key == key {
                                f.saving = false;
                            }
                        }
                        self.oss.status = "已保存".into();
                        self.oss_list(self.oss.prefix.clone());
                    }
                    Err(e) => {
                        if let Some(f) = &mut self.oss.current {
                            f.saving = false;
                            f.dirty = true;
                        }
                        self.oss.status = format!("保存失败:{e}");
                    }
                },
                Msg::OssDelete(res) => {
                    self.oss.confirm_delete = None;
                    match res {
                        Ok(()) => {
                            self.oss.status = "已删除".into();
                            self.oss_list(self.oss.prefix.clone());
                        }
                        Err(e) => self.oss.status = format!("删除失败:{e}"),
                    }
                }
            }
        }
    }

    fn ui_login(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            let max_w = 320.0;
            ui.vertical_centered(|ui| {
                ui.add_space(120.0);
                ui.heading(RichText::new("📝 urlnote").size(28.0));
                ui.add_space(16.0);
                ui.set_max_width(max_w);
                ui.label("服务器地址");
                ui.add(TextEdit::singleline(&mut self.server).desired_width(max_w))
                    .on_hover_text("默认线上后端;本地调试填 http://localhost:8080");
                ui.add_space(6.0);
                ui.label("用户名");
                ui.add(TextEdit::singleline(&mut self.username).desired_width(max_w));
                ui.add_space(6.0);
                ui.label("密码");
                let pw = TextEdit::singleline(&mut self.password)
                    .password(true)
                    .desired_width(max_w);
                let pr = ui.add(pw);
                if pr.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    self.do_login();
                }
                ui.add_space(12.0);
                if self.busy {
                    ui.label("连接中…");
                } else if ui.button(RichText::new("登 录").size(16.0)).clicked() {
                    self.do_login();
                }
                if !self.login_err.is_empty() {
                    ui.add_space(8.0);
                    ui.colored_label(egui::Color32::from_rgb(220, 80, 80), &self.login_err);
                }
            });
        });
    }

    fn ui_main(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("📝 urlnote");
                if ui.selectable_label(self.view == View::Notes, "便签").clicked() {
                    self.view = View::Notes;
                }
                if ui.selectable_label(self.view == View::Oss, "文件库").clicked() {
                    self.enter_oss();
                }
                if ui.selectable_label(self.view == View::Draw, "画图").clicked() {
                    self.enter_draw();
                }
                if self.view == View::Notes {
                    if ui.button("＋ 新建").clicked() {
                        self.new_note();
                    }
                    ui.add_space(8.0);
                    let se = TextEdit::singleline(&mut self.search)
                        .hint_text("全文搜索…")
                        .desired_width(180.0);
                    if ui.add(se).changed() {
                        self.search_dirty_at = Some(Instant::now());
                    }
                    if self.searching || self.notes_loading {
                        ui.spinner();
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("退出登录").clicked() {
                        let tx = self.rx_tx();
                        let base = self.api_base.clone();
                        go(&self.client, &self.ctx, &tx, move |c| {
                            let _ = c.post(format!("{base}/api/logout")).send();
                            Msg::Logout
                        });
                    }
                    if self.view == View::Notes && !self.status.is_empty() {
                        ui.label(RichText::new(&self.status).weak().size(12.0));
                    }
                });
            });
        });

        if self.view == View::Oss {
            self.ui_oss(ctx);
            return;
        }
        if self.view == View::Draw {
            self.ui_draw(ctx);
            return;
        }

        egui::SidePanel::left("notes").min_width(220.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let current_id = self.current.as_ref().map(|c| c.note.id.clone());
                let notes = self.notes.clone();
                for meta in &notes {
                    let selected = current_id.as_deref() == Some(meta.id.as_str());
                    let title = if meta.title.trim().is_empty() {
                        meta.excerpt.chars().take(24).collect::<String>()
                    } else {
                        meta.title.clone()
                    };
                    let resp = ui.selectable_label(
                        selected,
                        RichText::new(format!(
                            "{}\n{}",
                            title,
                            meta.updated_at.trim_end_matches('Z').trim_end_matches(":00")
                        ))
                        .size(13.0),
                    );
                    if resp.clicked() {
                        self.load_note(&meta.id);
                    }
                }
                if notes.is_empty() && !self.notes_loading {
                    ui.weak("暂无便签,点「＋ 新建」");
                }
            });
        });

        let dict_visible = self.dict.visible();
        if dict_visible {
            egui::TopBottomPanel::bottom("dict").resizable(false).show(ctx, |ui| {
                self.ui_dict(ui);
            });
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            // 编辑器状态行(先渲染,避免与下方 ed 借用冲突)
            let editor_open = !self.editor.open_id.is_empty();
            let editor_for_this = editor_open
                && self
                    .current
                    .as_ref()
                    .map(|e| e.note.id == self.editor.open_id)
                    .unwrap_or(false);
            let editor_other = editor_open && !editor_for_this;
            ui.horizontal(|ui| {
                if editor_for_this {
                    ui.colored_label(
                        egui::Color32::from_rgb(60, 160, 90),
                        "✅ Vditor 编辑中(浏览器窗口,自动回写)",
                    );
                    if ui.button("✕ 断开编辑器").on_hover_text("浏览器标签页请手动关闭").clicked() {
                        self.close_editor();
                    }
                } else {
                    if ui.button("📝 用 Vditor 编辑正文").clicked() {
                        self.open_vditor();
                    }
                    if editor_other {
                        ui.colored_label(
                            egui::Color32::from_rgb(230, 160, 40),
                            "⚠️ 编辑器开着别的便签(互不干扰,切回那篇或先关闭)",
                        );
                    }
                }
            });
            ui.separator();
            let Some(ed) = &mut self.current else {
                ui.weak("左侧选择或新建便签");
                return;
            };
            // 标题
            let title = TextEdit::singleline(&mut ed.note.title)
                .hint_text("标题")
                .font(FontId::proportional(18.0))
                .desired_width(f32::INFINITY);
            if ui.add(title).changed() {
                ed.mark_edit();
            }
            ui.horizontal(|ui| {
                ui.label(RichText::new("标签").size(12.0).weak());
                let mut tags = ed.note.tags.join(", ");
                let te = TextEdit::singleline(&mut tags)
                    .hint_text("逗号分隔")
                    .desired_width(280.0);
                if ui.add(te).changed() {
                    ed.note.tags = tags
                        .split([',', '，'])
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    ed.mark_edit();
                }
                ui.separator();
                if ui
                    .button(RichText::new("A−").size(13.0))
                    .on_hover_text("减小字号")
                    .clicked()
                {
                    ed.note.font_size = (ed.note.font_size - 1).max(MIN_FONT_SIZE);
                    ed.mark_edit();
                }
                ui.label(RichText::new(format!("{}", ed.note.font_size)).size(12.0).weak());
                if ui
                    .button(RichText::new("A+").size(15.0))
                    .on_hover_text("增大字号")
                    .clicked()
                {
                    ed.note.font_size = (ed.note.font_size + 1).min(MAX_FONT_SIZE);
                    ed.mark_edit();
                }
                if ed.dirty {
                    ui.colored_label(egui::Color32::from_rgb(230, 160, 40), "● 未保存");
                } else if ed.saving {
                    ui.weak("保存中…");
                }
            });
            ui.separator();
            if !editor_for_this {
                // markdown 简易编辑(Vditor 不可用时的退路)
                // 必须包 ScrollArea:TextEdit 自身不限制高度,
                // 不包的话内容超出面板后被裁掉,表现就是"无法向下滚动"。
                let size = ed.note.font_size.max(MIN_FONT_SIZE) as f32;
                let mut content = std::mem::take(&mut ed.note.content);
                let mut output_holder: Option<egui::text_edit::TextEditOutput> = None;
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let output = TextEdit::multiline(&mut content)
                            .font(FontId::monospace(size))
                            .desired_width(f32::INFINITY)
                            .desired_rows(24)
                            .lock_focus(true)
                            .show(ui);
                        output_holder = Some(output);
                    });
                ed.note.content = content;
                if let Some(output) = output_holder {
                    if output.response.changed() {
                        ed.mark_edit();
                    }
                    if let Some(range) = output.cursor_range {
                        let idx = range.primary.ccursor.index.min(ed.note.content.len());
                        if idx != self.cursor_index {
                            self.cursor_index = idx;
                        }
                    }
                }
            }
        });
    }

    fn ui_oss(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("oss-bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                // 面包屑
                if ui.button("📁 根目录").clicked() {
                    self.oss.close_dialogs();
                    self.oss_list(String::new());
                }
                let prefix = self.oss.prefix.clone();
                let parts: Vec<String> = prefix
                    .trim_end_matches('/')
                    .split('/')
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .collect();
                let mut acc = String::new();
                for p in &parts {
                    acc.push_str(p);
                    acc.push('/');
                    let target = acc.clone();
                    ui.label("/");
                    if ui.button(p).clicked() {
                        self.oss.close_dialogs();
                        self.oss_list(target);
                    }
                }
                ui.separator();
                if ui.button("＋ 文件").clicked() {
                    self.oss.new_file = true;
                    self.oss.new_folder = false;
                    self.oss.input.clear();
                }
                if ui.button("＋ 文件夹").clicked() {
                    self.oss.new_folder = true;
                    self.oss.new_file = false;
                    self.oss.input.clear();
                }
                if ui.button("刷新").clicked() {
                    self.oss_list(self.oss.prefix.clone());
                }
                if self.oss.loading {
                    ui.spinner();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(f) = &self.oss.current {
                        if f.dirty {
                            ui.colored_label(egui::Color32::from_rgb(230, 160, 40), "● 未保存");
                        } else if f.saving {
                            ui.weak("保存中…");
                        }
                        ui.label(RichText::new(f.key.rsplit('/').next().unwrap_or(&f.key)).size(13.0));
                        if ui.button("✕ 关闭文件").clicked() {
                            self.oss_save_now();
                            self.oss.current = None;
                        }
                    }
                    if !self.oss.status.is_empty() {
                        ui.label(RichText::new(&self.oss.status).weak().size(12.0));
                    }
                });
            });
        });

        egui::SidePanel::left("oss-list").min_width(240.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                if self.oss.creds.is_none() {
                    if self.oss.creds_err.is_empty() {
                        ui.weak("加载对象存储配置…");
                    } else {
                        ui.colored_label(
                            egui::Color32::from_rgb(220, 80, 80),
                            format!("对象存储未配置:{}", self.oss.creds_err),
                        );
                        ui.weak("请在环境变量或 ~/.urlnote/oss.conf 中设置:");
                        ui.monospace("OSS_ENDPOINT=objectstorageapi.bja.sealos.run\nOSS_AK=…\nOSS_SK=…\nOSS_BUCKET=…");
                    }
                    return;
                }
                let dirs = self.oss.dirs.clone();
                for d in &dirs {
                    let name = d.trim_start_matches(&self.oss.prefix).to_string();
                    ui.horizontal(|ui| {
                        if ui.button(format!("📁 {name}")).clicked() {
                            self.oss.close_dialogs();
                            self.oss_list(d.clone());
                        }
                        if ui.small_button("✕").on_hover_text("删除文件夹(含全部内容)").clicked() {
                            self.oss.confirm_delete = Some((name.clone(), true));
                        }
                    });
                }
                let files = self.oss.files.clone();
                for f in &files {
                    if f.name.ends_with(".settings.json") {
                        continue;
                    }
                    ui.horizontal(|ui| {
                        let label = if f.name.ends_with(".md") {
                            format!("📄 {}", f.name)
                        } else {
                            format!("📎 {}", f.name)
                        };
                        if ui.button(label).on_hover_text(format!("{} 字节 · {}", f.size, f.modified)).clicked() {
                            if f.name.ends_with(".md") || f.name.ends_with(".txt") || f.name.ends_with(".markdown") {
                                self.oss.close_dialogs();
                                self.oss_open(f.key.clone());
                            } else {
                                self.oss.status = "该类型暂不支持在线编辑(图片/Office 等后续版本)".into();
                            }
                        }
                        if ui.small_button("✕").on_hover_text("删除").clicked() {
                            self.oss.confirm_delete = Some((f.name.clone(), false));
                        }
                    });
                }
                if dirs.is_empty() && files.is_empty() && !self.oss.loading {
                    ui.weak("空目录,点上方「＋ 文件 / ＋ 文件夹」新建");
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            // Vditor 状态行(先渲染,避免与下方 f 借用冲突)
            let cur_key = self.oss.current.as_ref().map(|f| f.key.clone());
            let editor_open = !self.editor.open_id.is_empty();
            let editor_for_this =
                editor_open && cur_key.as_deref() == Some(self.editor.open_id.as_str());
            let editor_other = editor_open && !editor_for_this;
            ui.horizontal(|ui| {
                if editor_for_this {
                    ui.colored_label(
                        egui::Color32::from_rgb(60, 160, 90),
                        "✅ Vditor 编辑中(浏览器窗口,自动保存回桶)",
                    );
                    if ui.button("✕ 断开编辑器").on_hover_text("浏览器标签页请手动关闭").clicked() {
                        self.close_editor();
                    }
                } else {
                    if ui.button("📝 用 Vditor 编辑").clicked() {
                        self.oss_open_vditor();
                    }
                    if editor_other {
                        ui.colored_label(
                            egui::Color32::from_rgb(230, 160, 40),
                            "⚠️ 编辑器开着别的文档(互不干扰)",
                        );
                    }
                }
            });
            ui.separator();
            let Some(f) = &mut self.oss.current else {
                ui.weak("左侧选择 md 文件,或新建");
                return;
            };
            if !editor_for_this {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("字号").size(12.0).weak());
                    if ui.button("A−").clicked() {
                        f.font_size = (f.font_size - 1).max(MIN_FONT_SIZE);
                        f.mark_edit();
                    }
                    if ui.button("A+").clicked() {
                        f.font_size = (f.font_size + 1).min(MAX_FONT_SIZE);
                        f.mark_edit();
                    }
                    ui.weak(format!("({} 键入自动保存)", f.font_size));
                });
                ui.separator();
                let size = f.font_size as f32;
                let mut content = std::mem::take(&mut f.content);
                let mut changed = false;
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let resp = TextEdit::multiline(&mut content)
                            .font(FontId::monospace(size))
                            .desired_width(f32::INFINITY)
                            .desired_rows(24)
                            .show(ui);
                        changed = resp.response.changed();
                    });
                f.content = content;
                if changed {
                    f.mark_edit();
                }
            } else {
                ui.weak("正文正在 Vditor 浏览器窗口中编辑(此处置灰)");
            }
        });

        // 对话框:新建文件 / 文件夹
        if self.oss.new_file || self.oss.new_folder {
            let is_file = self.oss.new_file;
            let mut open = true;
            egui::Window::new(if is_file { "新建文件" } else { "新建文件夹" })
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("名称");
                        let te = TextEdit::singleline(&mut self.oss.input)
                            .desired_width(240.0)
                            .hint_text(if is_file { "如 笔记.md" } else { "如 资料" });
                        let r = ui.add(te);
                        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            self.oss_create(!is_file);
                        }
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("确定").clicked() {
                            self.oss_create(!is_file);
                        }
                        if ui.button("取消").clicked() {
                            self.oss.new_file = false;
                            self.oss.new_folder = false;
                        }
                    });
                });
            if !open {
                self.oss.new_file = false;
                self.oss.new_folder = false;
            }
        }

        // 对话框:删除确认
        if let Some((name, is_dir)) = self.oss.confirm_delete.clone() {
            let mut open = true;
            egui::Window::new("确认删除")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label(if is_dir {
                        format!("删除文件夹 {name}/ 及其全部内容?")
                    } else {
                        format!("删除 {name}?")
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("删除").clicked() {
                            self.oss_delete(name.clone(), is_dir);
                        }
                        if ui.button("取消").clicked() {
                            self.oss.confirm_delete = None;
                        }
                    });
                });
            if !open {
                self.oss.confirm_delete = None;
            }
        }
    }

    fn ui_dict(&mut self, ui: &mut egui::Ui) {
        let mut close = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("词典").size(12.0).weak());
            if !self.dict.langs.is_empty() {
                let mut cur = self.dict.lang.clone();
                egui::ComboBox::from_id_salt("dict-lang")
                    .selected_text(self.dict.lang.clone())
                    .show_ui(ui, |ui| {
                        for l in &self.dict.langs {
                            ui.selectable_value(&mut cur, l.code.clone(), format!("{} {}", l.code, l.name));
                        }
                    });
                if cur != self.dict.lang {
                    self.dict.lang = cur;
                    self.dict.close();
                }
            }
            if self.dict.loading {
                ui.spinner();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("✕").clicked() {
                    close = true;
                }
            });
        });
        if close {
            self.dict.close();
            return;
        }
        // 键盘导航:↑↓ 选择,Enter/Tab 接受,Esc 关闭。
        let (down, up, enter, tab, esc) = ui.ctx().input(|i| {
            (
                i.key_pressed(Key::ArrowDown),
                i.key_pressed(Key::ArrowUp),
                i.key_pressed(Key::Enter),
                i.key_pressed(Key::Tab),
                i.key_pressed(Key::Escape),
            )
        });
        if !self.dict.words.is_empty() {
            if down {
                self.dict.sel = (self.dict.sel + 1).min(self.dict.words.len() - 1);
            }
            if up {
                self.dict.sel = self.dict.sel.saturating_sub(1);
            }
            if enter || tab {
                self.accept_dict(self.dict.sel);
                return;
            }
            if esc {
                self.dict.close();
                return;
            }
        }
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.horizontal(|ui| {
                let words = self.dict.words.clone();
                for (i, w) in words.iter().enumerate() {
                    let trans: Vec<String> = w
                        .trans
                        .iter()
                        .map(|(k, v)| format!("{k}:{v}"))
                        .collect();
                    let label = if trans.is_empty() {
                        w.word.clone()
                    } else {
                        format!("{}  ({})", w.word, trans.join("  "))
                    };
                    let btn = ui.selectable_label(self.dict.sel == i, RichText::new(label).size(13.0));
                    if btn.clicked() {
                        self.accept_dict(i);
                        return;
                    }
                    if btn.hovered() {
                        self.dict.sel = i;
                    }
                }
            });
        });
    }
}

/// 光标前最近一段"词"(CJK 或字母数字)的起始字节位置。
fn word_start_before(content: &str, cursor: usize) -> usize {
    let mut it = cursor.min(content.len());
    while it > 0 {
        let prev = content[..it].chars().next_back().unwrap_or('\0');
        if !is_word_char(prev) {
            break;
        }
        it -= prev.len_utf8();
    }
    it
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || (0x4E00..=0x9FFF).contains(&(c as u32))
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

impl eframe::App for UrlnoteApp {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.close_editor();
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();        match self.screen {
            Screen::Login => self.ui_login(ctx),
            Screen::Main => {
                self.ui_main(ctx);
                // 自动保存(800ms 防抖)
                let save_due = self
                    .current
                    .as_ref()
                    .map(|ed| ed.dirty && !ed.saving && ed.last_edit.elapsed() > AUTOSAVE_DELAY)
                    .unwrap_or(false);
                if save_due {
                    self.send_save();
                }
                // 搜索防抖
                if let Some(t) = self.search_dirty_at {
                    if t.elapsed() > SEARCH_DELAY {
                        self.search_dirty_at = None;
                        self.do_search();
                    }
                }
                // 词典:光标前单词防抖查询
                if let Some(ed) = &self.current {
                    let idx = self.cursor_index.min(ed.note.content.len());
                    let start = word_start_before(&ed.note.content, idx);
                    let word = ed.note.content[start..idx].to_string();
                    if word.is_empty() {
                        if self.dict.visible() {
                            self.dict.close();
                        }
                    } else if word != self.dict.shown_for
                        && !self.dict.loading
                        && self.dict.last_req.elapsed() >= DICT_DELAY
                    {
                        self.do_dict(word);
                    }
                }
                // 文件库自动保存
                if self.view == View::Oss {
                    let oss_due = self
                        .oss
                        .current
                        .as_ref()
                        .map(|f| f.dirty && !f.saving && f.last_edit.elapsed() > AUTOSAVE_DELAY)
                        .unwrap_or(false);
                    if oss_due {
                        self.oss_save_now();
                    }
                }
                // 画图自动保存
                if self.view == View::Draw {
                    let due = self
                        .draw
                        .ed
                        .as_ref()
                        .map(|ed| ed.dirty && !ed.saving && ed.last_edit.elapsed() > AUTOSAVE_DELAY)
                        .unwrap_or(false);
                    if due {
                        self.draw_save_now();
                    }
                }
                // 有计时器在跑时维持帧节奏(否则 egui 空闲不重绘)
                if self.current.as_ref().map(|e| e.dirty).unwrap_or(false)
                    || self.oss.current.as_ref().map(|f| f.dirty).unwrap_or(false)
                    || self.draw.ed.as_ref().map(|d| d.dirty).unwrap_or(false)
                    || self.search_dirty_at.is_some()
                {
                    ctx.request_repaint_after(Duration::from_millis(150));
                }
            }
        }
    }
}

/// 加载中文字体:egui 默认字体不含 CJK,从 macOS 系统字体挑一个可用的
/// (按解析可靠性排序;.ttc 集合部分解析器不支持,放后面)。
fn setup_fonts(ctx: &egui::Context) {
    const CANDIDATES: [&str; 5] = [
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        "/System/Library/Fonts/Supplemental/Songti.ttc",
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Medium.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
    ];
    for path in CANDIDATES {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert("cjk".to_owned(), egui::FontData::from_owned(bytes).into());
            for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                // 追加为回退字体:拉丁字符仍用 egui 内置字体,CJK 落到 cjk
                fonts.families.entry(fam).or_default().push("cjk".to_owned());
            }
            ctx.set_fonts(fonts);
            eprintln!("CJK font loaded: {path}");
            return;
        }
    }
    eprintln!("warn: no CJK system font found, Chinese will render as boxes");
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("urlnote")
            .with_inner_size([1280.0, 860.0])
            .with_min_inner_size([960.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native(
        "urlnote",
        options,
        Box::new(|cc| {
            setup_fonts(&cc.egui_ctx);
            let ctx = cc.egui_ctx.clone();
            Ok(Box::new(UrlnoteApp::new(&ctx)))
        }),
    )
}
