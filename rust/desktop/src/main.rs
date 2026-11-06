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
use std::time::{Duration, Instant};

/// 后端地址(与 Web 前端 VITE_API_BASE 一致)。
const API: &str = "https://yyuqmndqpnjr.sealosbja.site";

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
}

impl UrlnoteApp {
    fn new(ctx: &egui::Context) -> Self {
        let client = reqwest::blocking::Client::builder()
            .cookie_store(true)
            .user_agent("urlnote-desktop/0.1")
            .build()
            .expect("http client");
        let (tx, rx) = channel();
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
        };
        // 启动时探测既有会话(cookie jar 为空,必然未登录,仅校验连通性)。
        let tx0 = app.rx_tx();
        go(&app.client, ctx, &tx0, |c| {
            let r = c.get(format!("{API}/api/whoami")).send();
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
        go(&self.client, &self.ctx, &tx, |c| {
            let r = c.get(format!("{API}/api/dict/langs")).send();
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
        go(&self.client, &self.ctx, &tx, |c| {
            let r = c.get(format!("{API}/api/notes")).send();
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
        go(&self.client, &self.ctx, &tx, move |c| {
            let r = c.get(format!("{API}/api/note/{id}")).send();
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
        go(&self.client, &self.ctx, &tx, move |c| {
            let r = c
                .put(format!("{API}/api/note/{id}"))
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
        self.busy = true;
        self.login_err.clear();
        let body = LoginRequest {
            username: self.username.trim().to_string(),
            password: self.password.clone(),
        };
        let tx = self.rx_tx();
        go(&self.client, &self.ctx, &tx, move |c| {
            let r = c.post(format!("{API}/api/login")).json(&body).send();
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
        go(&self.client, &self.ctx, &tx, move |c| {
            let r = c
                .get(format!("{API}/api/search?q={}", urlencode(&q)))
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
        go(&self.client, &self.ctx, &tx, move |c| {
            let r = c
                .get(format!(
                    "{API}/api/dict/lookup?q={}&lang={}",
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
                    self.screen = Screen::Login;
                    self.current = None;
                    self.notes.clear();
                    self.dict.close();
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
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("退出登录").clicked() {
                        let tx = self.rx_tx();
                        go(&self.client, &self.ctx, &tx, |c| {
                            let _ = c.post(format!("{API}/api/logout")).send();
                            Msg::Logout
                        });
                    }
                    if !self.status.is_empty() {
                        ui.label(RichText::new(&self.status).weak().size(12.0));
                    }
                });
            });
        });

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
            // 正文(markdown 源码编辑;预览在后续阶段)
            let size = ed.note.font_size.max(MIN_FONT_SIZE) as f32;
            let mut content = std::mem::take(&mut ed.note.content);
            let output = TextEdit::multiline(&mut content)
                .font(FontId::monospace(size))
                .desired_width(f32::INFINITY)
                .desired_rows(30)
                .lock_focus(true)
                .show(ui);
            ed.note.content = content;
            if output.response.changed() {
                ed.mark_edit();
            }
            if let Some(range) = output.cursor_range {
                let idx = range.primary.ccursor.index.min(ed.note.content.len());
                if idx != self.cursor_index {
                    self.cursor_index = idx;
                }
            }
        });
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
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        match self.screen {
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
                // 有计时器在跑时维持帧节奏(否则 egui 空闲不重绘)
                if self.current.as_ref().map(|e| e.dirty).unwrap_or(false)
                    || self.search_dirty_at.is_some()
                {
                    ctx.request_repaint_after(Duration::from_millis(150));
                }
            }
        }
    }
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
            let ctx = cc.egui_ctx.clone();
            Ok(Box::new(UrlnoteApp::new(&ctx)))
        }),
    )
}
