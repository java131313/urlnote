//! 全局状态(AppState)+ 消息队列。
//!
//! 与 egui 版同一套契约(shared 类型 + /api/*)。后台线程跑 reqwest blocking,
//! 结果经 mpsc 发回;前台 pump 协程(main.rs)消费消息、跑防抖 tick 并同步输入框。

use gpui::{Entity, WeakEntity};
use shared::{
    DictLang, DictLangList, DictLookupResponse, DictWordItem, LoginRequest, Note, NoteList,
    NoteMeta, OkResponse, SaveRequest, MAX_FONT_SIZE, MIN_FONT_SIZE,
};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::bridge::EditorDoc;
use crate::draw;
use crate::s3;

pub const DEFAULT_API: &str = "https://yyuqmndqpnjr.sealosbja.site";
pub const AUTOSAVE_DELAY: Duration = Duration::from_millis(800);
pub const SEARCH_DELAY: Duration = Duration::from_millis(300);
pub const DICT_DELAY: Duration = Duration::from_millis(200);

pub fn normalize_base(s: &str) -> String {
    s.trim().trim_end_matches('/').to_string()
}

pub fn load_api_base() -> String {
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

pub fn save_api_base(base: &str) {
    let dir = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
        .join(".urlnote");
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = std::fs::write(dir.join("server.conf"), base);
    }
}

pub fn urlencode(s: &str) -> String {
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

pub fn new_note_id() -> String {
    use rand::RngCore;
    let alphabet = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut buf = [0u8; 5];
    rand::thread_rng().fill_bytes(&mut buf);
    buf.iter().map(|b| alphabet[(*b as usize) % 62] as char).collect()
}

pub fn err(e: reqwest::Error) -> String {
    format!("{}: {}", e.status().map(|s| s.to_string()).unwrap_or_default(), e)
}

/// 光标前最近一段"词"(CJK 或字母数字)的起始字节位置。
pub fn word_start_before(content: &str, cursor: usize) -> usize {
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

pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || (0x4E00..=0x9FFF).contains(&(c as u32))
}

pub enum Msg {
    Whoami(Result<String, String>),
    Login(Result<(), String>),
    Notes(Result<Vec<NoteMeta>, String>),
    NoteLoaded(Result<Note, String>),
    Saved { id: String, res: Result<(), String> },
    Search(Result<Vec<NoteMeta>, String>),
    Dict(Result<DictLookupResponse, String>),
    Langs(Vec<DictLang>),
    Logout,
    // 文件库
    OssCreds(Result<s3::OssCreds, String>),
    OssList(Result<(String, s3::ListResult), String>),
    OssGot(Result<(String, String, String), String>),
    OssPut(Result<String, String>),
    OssDelete(Result<(), String>),
    // Vditor 回写
    EditorPosted(String),
    // 画图
    DrawList(Result<Vec<draw::DrawMeta>, String>),
    DrawGot(Result<draw::DrawDoc, String>),
    DrawSaved { id: String, res: Result<(), String> },
    DrawDeleted(Result<String, String>),
}

pub struct EdNote {
    pub note: Note,
    pub dirty: bool,
    pub last_edit: Instant,
    pub saving: bool,
}

impl EdNote {
    pub fn new(id: String) -> Self {
        Self { note: Note::empty(&id), dirty: false, last_edit: Instant::now(), saving: false }
    }
    pub fn loaded(note: Note) -> Self {
        Self { note, dirty: false, last_edit: Instant::now(), saving: false }
    }
    pub fn mark_edit(&mut self) {
        self.dirty = true;
        self.last_edit = Instant::now();
    }
    pub fn save_req(&self) -> SaveRequest {
        SaveRequest {
            content: self.note.content.clone(),
            font_size: Some(self.note.font_size as f64),
            color: self.note.color.clone(),
            title: self.note.title.clone(),
            tags: self.note.tags.clone(),
        }
    }
}

pub struct DictState {
    pub langs: Vec<DictLang>,
    pub lang: String,
    pub words: Vec<DictWordItem>,
    pub shown_for: String,
    pub loading: bool,
    pub sel: usize,
    pub last_req: Instant,
}

impl DictState {
    pub fn new() -> Self {
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
    pub fn visible(&self) -> bool {
        self.loading || !self.words.is_empty()
    }
    pub fn close(&mut self) {
        self.words.clear();
        self.shown_for.clear();
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Screen {
    Login,
    Main,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum View {
    Notes,
    Oss,
    Draw,
}

pub struct OssFileEd {
    pub key: String,
    pub content: String,
    pub font_size: i64,
    pub color: String,
    pub dirty: bool,
    pub last_edit: Instant,
    pub saving: bool,
}

impl OssFileEd {
    pub fn mark_edit(&mut self) {
        self.dirty = true;
        self.last_edit = Instant::now();
    }
}

pub struct OssState {
    pub creds: Option<s3::OssCreds>,
    pub creds_err: String,
    pub prefix: String,
    pub dirs: Vec<String>,
    pub files: Vec<s3::ListedFile>,
    pub loading: bool,
    pub current: Option<OssFileEd>,
    pub dialog: Option<OssDialog>,
    pub status: String,
}

pub enum OssDialog {
    NewFile(String),
    NewFolder(String),
    ConfirmDelete(String, bool),
}

impl OssState {
    pub fn new() -> Self {
        Self {
            creds: None,
            creds_err: String::new(),
            prefix: String::new(),
            dirs: Vec::new(),
            files: Vec::new(),
            loading: false,
            current: None,
            dialog: None,
            status: String::new(),
        }
    }
    pub fn close_dialogs(&mut self) {
        self.dialog = None;
    }
}

/// Vditor 编辑器桥状态。
pub struct EditorCtl {
    pub port: u16,
    pub token: String,
    pub doc: Arc<std::sync::Mutex<EditorDoc>>,
    pub open_id: String,
    pub api_base: Arc<std::sync::Mutex<String>>,
}

impl EditorCtl {
    pub fn empty() -> Self {
        Self {
            port: 0,
            token: String::new(),
            doc: Arc::new(std::sync::Mutex::new(EditorDoc {
                id: String::new(),
                title: String::new(),
                content: String::new(),
            })),
            open_id: String::new(),
            api_base: Arc::new(std::sync::Mutex::new(load_api_base())),
        }
    }
}

pub struct AppState {
    pub me: WeakEntity<AppState>,
    pub client: reqwest::blocking::Client,
    pub tx: Sender<Msg>,
    pub rx: Receiver<Msg>,
    pub screen: Screen,
    pub view: View,
    // 登录(用户名/密码由登录页的输入框实体持有)
    pub api_base: String,
    pub login_err: String,
    pub busy: bool,
    // 便签
    pub notes: Vec<NoteMeta>,
    pub notes_loading: bool,
    pub search_dirty_at: Option<Instant>,
    pub searching: bool,
    pub current: Option<EdNote>,
    pub status: String,
    pub dict: DictState,
    // 文件库 / 编辑器桥 / 画图
    pub oss: OssState,
    pub editor: EditorCtl,
    pub draw: draw::DrawState,
    /// NoteLoaded/OssGot/EditorPosted 后待推入输入框的内容(tick 线程消费)。
    pub pending_note_sync: Option<(String, String, String, String, i64)>, // id,title,tags(逗号串),content,font_size
    pub pending_note_content: Option<String>,                            // Vditor 回写
    pub pending_oss_sync: Option<(String, String, i64)>,                 // key,content,font_size
}

impl AppState {
    pub fn new(cx: &mut gpui::Context<AppState>) -> Self {
        let client = reqwest::blocking::Client::builder()
            .cookie_store(true)
            .user_agent("urlnote-desktop-gpui/0.1")
            .build()
            .expect("http client");
        let (tx, rx) = channel();
        let mut st = Self {
            me: WeakEntity::new_invalid(),
            client,
            tx,
            rx,
            screen: Screen::Login,
            view: View::Notes,
            api_base: load_api_base(),
            login_err: String::new(),
            busy: false,
            notes: Vec::new(),
            notes_loading: false,
            search_dirty_at: None,
            searching: false,
            current: None,
            status: String::new(),
            dict: DictState::new(),
            oss: OssState::new(),
            editor: EditorCtl::empty(),
            draw: draw::DrawState::new(),
            pending_note_sync: None,
            pending_note_content: None,
            pending_oss_sync: None,
        };
        st.me = cx.entity().downgrade();
        st
    }

    /// 后台线程执行请求,结果经 mpsc 发回,由前台 pump 协程消费(见 main.rs)。
    pub fn go(&self, f: impl FnOnce(&reqwest::blocking::Client) -> Msg + Send + 'static) {
        let tx = self.tx.clone();
        let client = self.client.clone();
        std::thread::spawn(move || {
            let _ = tx.send(f(&client));
        });
    }

    /// 消费消息队列(主线程,render 前)。
    pub fn drain(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            self.apply(m);
        }
    }

    pub fn apply(&mut self, m: Msg) {
        match m {
            Msg::Whoami(res) => {
                self.busy = false;
                if res.is_ok() {
                    self.enter_main();
                } else if std::env::var("GPUI_AUTO_OSS").is_ok() {
                    // 本地自检:自动登录本地 server 并进入文件库
                    eprintln!("[auto] whoami failed, auto-login local server");
                    self.api_base = "http://localhost:8080".into();
                    self.do_login("admin".into(), "root@123".into());
                }
            }
            Msg::Login(res) => {
                self.busy = false;
                match res {
                    Ok(()) => {
                        save_api_base(&self.api_base);
                        self.enter_main();
                        if std::env::var("GPUI_AUTO_OSS").is_ok() {
                            eprintln!("[auto] login ok, enter oss");
                            self.enter_oss();
                        }
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
                        let id = n.id.clone();
                        let title = n.title.clone();
                        let tags = n.tags.join(", ");
                        let content = n.content.clone();
                        let fs = n.font_size;
                        self.current = Some(EdNote::loaded(n));
                        self.pending_note_sync = Some((id, title, tags, content, fs));
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
            }
            Msg::Dict(res) => {
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
                            // 本地有改动时保留本地;服务器名非空才覆盖,避免新建画板名被空值清掉。
                            let local = ed.dirty || !ed.elements.is_empty() || !ed.name.is_empty();
                            if !local {
                                ed.name = d.name;
                                ed.elements = d.elements;
                                ed.dirty = false;
                            } else {
                                if !d.name.is_empty() {
                                    ed.name = d.name;
                                }
                                if ed.elements.is_empty() && !ed.dirty {
                                    ed.elements = d.elements;
                                }
                            }
                        }
                    }
                }
                Err(e) => self.draw.status = e,
            },
            Msg::DrawSaved { id, res } => {
                if let Some(ed) = &mut self.draw.ed {
                    if ed.id == id {
                        ed.saving = false;
                        if let Err(e) = res {
                            ed.dirty = true;
                            self.draw.status = e;
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
                                f.content = content.clone();
                                f.mark_edit();
                            }
                        }
                    }
                    self.pending_note_content = Some(content);
                }
            }
            Msg::OssCreds(res) => match res {
                Ok(c) => {
                    eprintln!("[oss] creds ok: endpoint={} bucket={}", c.endpoint, c.bucket);
                    self.oss.creds = Some(c);
                    self.oss_list(String::new());
                }
                Err(e) => {
                    eprintln!("[oss] creds err: {e}");
                    self.oss.creds_err = e;
                }
            },
            Msg::OssList(res) => {
                self.oss.loading = false;
                match res {
                    Ok((prefix, r)) => {
                        eprintln!("[oss] list ok: prefix={prefix} dirs={} files={}", r.dirs.len(), r.files.len());
                        self.oss.prefix = prefix;
                        self.oss.dirs = r.dirs;
                        self.oss.files = r.files;
                    }
                    Err(e) => {
                        eprintln!("[oss] list err: {e}");
                        self.oss.status = e;
                    }
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
                            key: key.clone(),
                            content: content.clone(),
                            font_size,
                            color,
                            dirty: false,
                            last_edit: Instant::now(),
                            saving: false,
                        });
                        self.pending_oss_sync = Some((key, content, font_size));
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
                self.oss.dialog = None;
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

    // ---- 行为 ----

    pub fn enter_main(&mut self) {
        self.screen = Screen::Main;
        self.refresh_notes();
        let base = self.api_base.clone();
        self.go(move |c| {
            let r = c.get(format!("{base}/api/dict/langs")).send();
            Msg::Langs(match r {
                Ok(resp) if resp.status().is_success() => {
                    resp.json::<DictLangList>().map(|l| l.langs).unwrap_or_default()
                }
                _ => Vec::new(),
            })
        });
    }

    pub fn refresh_notes(&mut self) {
        self.notes_loading = true;
        let base = self.api_base.clone();
        self.go(move |c| {
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

    pub fn load_note(&mut self, id: &str) {
        self.save_now_if_dirty();
        let id = id.to_string();
        self.status = format!("打开 {id}…");
        let base = self.api_base.clone();
        self.go(move |c| {
            let r = c.get(format!("{base}/api/note/{id}")).send();
            Msg::NoteLoaded(match r {
                Ok(resp) if resp.status().is_success() => resp.json::<Note>().map_err(err),
                Ok(resp) => Err(format!("HTTP {}", resp.status())),
                Err(e) => Err(err(e)),
            })
        });
    }

    pub fn save_now_if_dirty(&mut self) {
        if self.current.as_ref().map(|ed| ed.dirty).unwrap_or(false) {
            self.send_save();
        }
    }

    pub fn send_save(&mut self) {
        let Some(ed) = &mut self.current else { return };
        if ed.saving {
            return;
        }
        ed.saving = true;
        ed.dirty = false;
        let id = ed.note.id.clone();
        let body = ed.save_req();
        let base = self.api_base.clone();
        self.go(move |c| {
            let r = c.put(format!("{base}/api/note/{id}")).json(&body).send();
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

    pub fn new_note(&mut self) {
        self.save_now_if_dirty();
        let id = new_note_id();
        self.current = Some(EdNote::new(id));
        self.dict.close();
        self.status = "新便签(输入即自动保存)".into();
    }

    pub fn do_login(&mut self, username: String, password: String) {
        if self.busy {
            return;
        }
        self.api_base = normalize_base(&self.api_base);
        if self.api_base.is_empty() {
            self.api_base = DEFAULT_API.to_string();
        }
        if let Ok(mut b) = self.editor.api_base.lock() {
            *b = self.api_base.clone();
        }
        self.busy = true;
        self.login_err.clear();
        let body = LoginRequest { username: username.trim().to_string(), password };
        let base = self.api_base.clone();
        self.go(move |c| {
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

    pub fn do_search(&mut self, q: String) {
        let q = q.trim().to_string();
        self.searching = !q.is_empty();
        let base = self.api_base.clone();
        self.go(move |c| {
            let r = c.get(format!("{base}/api/search?q={}", urlencode(&q))).send();
            Msg::Search(match r {
                Ok(resp) if resp.status().is_success() => {
                    resp.json::<NoteList>().map(|l| l.notes).map_err(err)
                }
                Ok(resp) => Err(format!("HTTP {}", resp.status())),
                Err(e) => Err(err(e)),
            })
        });
    }

    pub fn do_dict(&mut self, word: String) {
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
        let base = self.api_base.clone();
        self.go(move |c| {
            let r = c
                .get(format!(
                    "{base}/api/dict/lookup?q={}&lang={}",
                    urlencode(&word),
                    urlencode(&lang)
                ))
                .send();
            Msg::Dict(match r {
                Ok(resp) if resp.status().is_success() => {
                    resp.json::<DictLookupResponse>().map_err(err)
                }
                Ok(resp) => Err(format!("HTTP {}", resp.status())),
                Err(e) => Err(err(e)),
            })
        });
    }

    pub fn do_logout(&mut self) {
        let base = self.api_base.clone();
        self.go(move |c| {
            let _ = c.post(format!("{base}/api/logout")).send();
            Msg::Logout
        });
    }

    // ---- 文件库 ----

    pub fn enter_oss(&mut self) {
        self.view = View::Oss;
        if self.oss.creds.is_some() {
            if self.oss.files.is_empty() && self.oss.dirs.is_empty() {
                self.oss_list(String::new());
            }
            return;
        }
        if !self.oss.creds_err.is_empty() {
            return;
        }
        self.go(|_| Msg::OssCreds(s3::load_creds()));
    }

    pub fn oss_list(&mut self, prefix: String) {
        self.oss.loading = true;
        let Some(creds) = self.oss.creds.clone() else { return };
        self.go(move |c| {
            let res = s3::list_dir(c, &creds, &prefix).map(|r| (prefix.clone(), r));
            Msg::OssList(res)
        });
    }

    pub fn oss_open(&mut self, key: String) {
        self.oss_save_now();
        let Some(creds) = self.oss.creds.clone() else { return };
        self.oss.status = format!("打开 {}…", key.rsplit('/').next().unwrap_or(&key));
        self.go(move |c| {
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

    pub fn oss_save_now(&mut self) {
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
            let settings =
                format!("{{\"font_size\":{},\"color\":\"{}\"}}", f.font_size, f.color.replace('"', ""));
            self.go(move |c| {
                let res = s3::put_object(c, &creds, &key, content.as_bytes())
                    .and_then(|_| {
                        s3::put_object(c, &creds, &format!("{key}.settings.json"), settings.as_bytes())
                    })
                    .map(|_| key.clone());
                Msg::OssPut(res)
            });
        }
    }

    pub fn oss_create(&mut self, folder: bool, input: String) {
        let name = input.trim().trim_matches('/').to_string();
        if name.is_empty() {
            return;
        }
        self.oss.dialog = None;
        let key = if folder {
            format!("{}{name}/.keep", self.oss.prefix)
        } else {
            format!("{}{name}", self.oss.prefix)
        };
        let Some(creds) = self.oss.creds.clone() else { return };
        let prefix = self.oss.prefix.clone();
        self.go(move |c| {
            let res = s3::put_object(c, &creds, &key, b"").map(|_| prefix.clone());
            Msg::OssPut(res)
        });
    }

    pub fn oss_delete(&mut self, name: String, is_dir: bool) {
        let Some(creds) = self.oss.creds.clone() else { return };
        let key = format!("{}{name}", self.oss.prefix);
        self.go(move |c| {
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

    // ---- Vditor ----

    pub fn open_vditor(&mut self, id: String, title: String, content: String) {
        {
            let mut d = self.editor.doc.lock().unwrap();
            *d = EditorDoc { id: id.clone(), title: title.clone(), content };
        }
        self.close_editor();
        let url = format!(
            "http://127.0.0.1:{}/editor/page?id={}&token={}",
            self.editor.port, id, self.editor.token
        );
        match std::process::Command::new("open").arg(&url).spawn() {
            Ok(_) => {
                self.editor.open_id = id;
                self.status = "已在浏览器中打开 Vditor 编辑器".into();
            }
            Err(e) => self.status = format!("打开浏览器失败:{e}"),
        }
    }

    pub fn close_editor(&mut self) {
        self.editor.open_id.clear();
    }

    /// 周期任务(pump 循环):防抖搜索、自动保存、词典查询。
    /// 返回本轮是否有可见状态变化(决定是否 notify 重绘)。
    pub fn tick(&mut self, search_text: String, content_text: String, cursor: usize) -> bool {
        if self.screen != Screen::Main {
            return false;
        }
        let mut changed = false;
        match self.view {
            View::Notes => {
                // 便签自动保存
                let save_due = self
                    .current
                    .as_ref()
                    .map(|ed| ed.dirty && !ed.saving && ed.last_edit.elapsed() > AUTOSAVE_DELAY)
                    .unwrap_or(false);
                if save_due {
                    self.send_save();
                    changed = true;
                }
                // 搜索防抖
                if let Some(t) = self.search_dirty_at {
                    if t.elapsed() > SEARCH_DELAY {
                        self.search_dirty_at = None;
                        self.do_search(search_text);
                        changed = true;
                    }
                }
                // 词典:光标前单词防抖查询
                let (word, has_current) = {
                    if let Some(ed) = &self.current {
                        let idx = cursor.min(content_text.len());
                        let start = word_start_before(&content_text, idx);
                        (content_text[start..idx].to_string(), true)
                    } else {
                        (String::new(), false)
                    }
                };
                if !has_current || word.is_empty() {
                    if self.dict.visible() {
                        self.dict.close();
                        changed = true;
                    }
                } else if word != self.dict.shown_for
                    && !self.dict.loading
                    && self.dict.last_req.elapsed() >= DICT_DELAY
                {
                    self.do_dict(word);
                    changed = true;
                }
            }
            View::Oss => {
                let due = self
                    .oss
                    .current
                    .as_ref()
                    .map(|f| f.dirty && !f.saving && f.last_edit.elapsed() > AUTOSAVE_DELAY)
                    .unwrap_or(false);
                if due {
                    self.oss_save_now();
                    changed = true;
                }
            }
            View::Draw => {
                let due = self
                    .draw
                    .ed
                    .as_ref()
                    .map(|ed| ed.dirty && !ed.saving && ed.last_edit.elapsed() > AUTOSAVE_DELAY)
                    .unwrap_or(false);
                if due {
                    self.draw_save_now();
                    changed = true;
                }
            }
        }
        changed
    }

    // ---- 画图 ----

    pub fn enter_draw(&mut self) {
        self.view = View::Draw;
        self.draw_load_list();
    }

    pub fn draw_load_list(&mut self) {
        self.draw.loading = true;
        let base = self.api_base.clone();
        self.go(move |c| {
            let r = c.get(format!("{base}/api/drawings")).send();
            Msg::DrawList(match r {
                Ok(resp) if resp.status().is_success() => resp
                    .json::<serde_json::Value>()
                    .map(|v| {
                        serde_json::from_value::<Vec<draw::DrawMeta>>(v["drawings"].clone())
                            .unwrap_or_default()
                    })
                    .map_err(|e| e.to_string()),
                Ok(resp) => Err(format!("HTTP {}", resp.status())),
                Err(e) => Err(format!("网络错误: {e}")),
            })
        });
    }

    pub fn draw_open(&mut self, id: String) {
        self.draw.ed = Some(draw::DrawEd::new(id.clone()));
        let base = self.api_base.clone();
        self.go(move |c| {
            let r = c.get(format!("{base}/api/drawing/{id}")).send();
            Msg::DrawGot(match r {
                Ok(resp) if resp.status().is_success() => {
                    resp.json::<draw::DrawDoc>().map_err(|e| e.to_string())
                }
                Ok(resp) => Err(format!("HTTP {}", resp.status())),
                Err(e) => Err(format!("网络错误: {e}")),
            })
        });
    }

    pub fn draw_save_now(&mut self) {
        let Some(ed) = &mut self.draw.ed else { return };
        if !ed.dirty || ed.saving {
            return;
        }
        ed.saving = true;
        ed.dirty = false;
        let id = ed.id.clone();
        let name = ed.name.clone();
        let elements = ed.elements.clone();
        let base = self.api_base.clone();
        self.go(move |c| {
            let body = serde_json::json!({ "name": name, "width": 960, "height": 600, "elements": elements });
            let r = c
                .put(format!("{base}/api/drawing/{id}"))
                .header("Content-Type", "application/json")
                .body(body.to_string())
                .send();
            Msg::DrawSaved {
                id: id.clone(),
                res: match r {
                    Ok(resp) if resp.status().is_success() => Ok(()),
                    Ok(resp) => Err(format!("HTTP {}", resp.status())),
                    Err(e) => Err(format!("网络错误: {e}")),
                },
            }
        });
    }

    pub fn draw_delete(&mut self, id: String) {
        let base = self.api_base.clone();
        self.go(move |c| {
            let r = c.post(format!("{base}/api/drawing/{id}/delete")).send();
            Msg::DrawDeleted(match r {
                Ok(resp) if resp.status().is_success() => Ok(id.clone()),
                Ok(resp) => Err(format!("HTTP {}", resp.status())),
                Err(e) => Err(format!("网络错误: {e}")),
            })
        });
    }
}
