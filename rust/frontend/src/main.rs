//! urlnote frontend — a Rust/Leptos port of the original Vue app
//! (frontend/src/App.vue). The behavior (autosave, polling, presence,
//! collaboration banners, editor settings) is kept identical; the Vditor
//! markdown editor itself is the same JS library, driven through
//! wasm-bindgen (see vditor.rs).

mod vditor;

use gloo_events::EventListener;
use send_wrapper::SendWrapper;
use gloo_timers::callback::{Interval, Timeout};
use leptos::html::Input;
use leptos::prelude::*;
use leptos::task::spawn_local;
use shared::*;
use std::cell::RefCell;
use vditor::VditorEditor;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Blob, Element, Event as WebEvent, HtmlAnchorElement, HtmlElement, HtmlInputElement, MediaQueryListEvent, RequestCredentials};

const FONT_STEP: i64 = 2;

// ---- JS helpers ----

fn window() -> web_sys::Window {
    web_sys::window().expect("window")
}

fn document() -> web_sys::Document {
    window().document().expect("document")
}

fn evt_input_value(e: &web_sys::Event) -> String {
    e.target()
        .and_then(|t| t.dyn_into::<HtmlInputElement>().ok())
        .map(|el| el.value())
        .unwrap_or_default()
}

fn new_id() -> String {
    const ALPHABET: &[u8; 62] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut v = [0u8; 5];
    window()
        .crypto()
        .expect("crypto")
        .get_random_values_with_u8_array(&mut v)
        .expect("getRandomValues");
    v.iter()
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect()
}

fn current_time() -> String {
    let s = js_sys::Date::new_0().to_time_string().as_string().unwrap_or_default();
    s.get(..8).unwrap_or(&s).to_string()
}

// ---- deployment configuration (compile-time) ----

/// Origin of the API for deployments where the frontend and the worker live
/// on different hosts (frontend on GitHub Pages, API on Cloudflare). Set the
/// API_BASE env var at build time; unset/empty keeps the original
/// same-origin behavior (the Cloudflare deployment).
const API_BASE: &str = match option_env!("API_BASE") {
    Some(v) => v,
    None => "",
};

/// URL prefix the frontend is served under ("/urlnote" for a project page).
/// Empty for root deployments (Cloudflare, a *.github.io user site).
const BASE_PATH: &str = match option_env!("BASE_PATH") {
    Some(v) => v,
    None => "",
};

fn api_url(path: &str) -> String {
    if API_BASE.is_empty() {
        path.to_string()
    } else {
        format!("{API_BASE}{path}")
    }
}

/// Uploaded-image links are stored relative (`/i/...`) so notes survive
/// re-hosting; a cross-origin frontend rewrites them to absolute for display.
fn display_md(md: &str) -> String {
    if API_BASE.is_empty() {
        return md.to_string();
    }
    md.replace("](/i/", &format!("]({API_BASE}/i/"))
}

/// Inverse of display_md — applied before persisting or sending content
/// anywhere, so the database keeps the portable relative form.
fn storage_md(md: &str) -> String {
    if API_BASE.is_empty() {
        return md.to_string();
    }
    md.replace(&format!("]({API_BASE}/i/"), "](/i/")
}

// ---- app state (non-reactive parts) ----

struct VditorHandle {
    editor: VditorEditor,
    // keep the JS callbacks alive for the editor's lifetime
    _cbs: Vec<JsValue>,
}

struct AppState {
    id: String,
    editor: Option<VditorHandle>,
    intervals: Vec<Interval>,
    poll_interval: Option<Interval>,
    retry: Option<Timeout>,
    search_debounce: Option<Timeout>,
    listeners: Vec<EventListener>,
    dirty: bool,
    loaded: bool,
    applying_remote: bool,
    poll_updated_at: String,
    // suggestion state: DOM anchor of the typed fragment (char offsets into
    // the text node) so accepting can re-select and replace it
    suggest_anchor_node: Option<SendWrapper<web_sys::Node>>,
    suggest_anchor_start: usize,
    suggest_anchor_end: usize,
    suggest_accepted_word: String,
    // POS annotation
    pos_debounce: Option<Timeout>,
    pos_seq: u64,
    pos_scroll_listener: Option<SendWrapper<EventListener>>,
    // in-editor find
    find_debounce: Option<Timeout>,
    /// last POS tokens / find ranges — reused to re-render marks when the
    /// editor zoom changes (no server round-trip)
    pos_tokens_cache: Vec<PosToken>,
    find_ranges_cache: Vec<(usize, usize)>,
    /// Vditor fires input asynchronously after set_value; ignore those so
    /// remote applies / mode switches don't leave phantom dirty state
    suppress_input_until_ms: f64,
    /// content last pushed into the editor programmatically — an input event
    /// whose value equals this is an echo, not a user edit
    last_applied_content: String,
    /// signal values captured at load/apply time; the dirty-watching Effect
    /// only fires when the user actually changes one of them
    baseline_font: i64,
    baseline_color: String,
    baseline_title: String,
    baseline_tags: String,
}

/// Everything the callbacks need: the editor/timers state (via a StoredValue
/// handle, so Ctx stays Copy + Send for Leptos rendering) plus the UI
/// signals.
#[derive(Clone, Copy)]
struct Ctx {
    state: StoredValue<SendWrapper<AppState>>,
    font_size: RwSignal<i64>,
    color: RwSignal<String>,
    title: RwSignal<String>,
    tags_text: RwSignal<String>,
    parsed_tags: Memo<Vec<String>>,
    status_text: RwSignal<String>,
    status_class: RwSignal<String>,
    author: RwSignal<String>,
    remote_changed: RwSignal<bool>,
    presence_others: RwSignal<Vec<String>>,
    show_login: RwSignal<bool>,
    picker_open: RwSignal<bool>,
    picker_query: RwSignal<String>,
    all_notes: RwSignal<Vec<NoteMeta>>,
    // edit lock state (mirrors GET /api/edit-status)
    is_admin: RwSignal<bool>,
    edit_locked: RwSignal<bool>,
    can_edit: RwSignal<bool>,
    /// "none" | "pending" | "approved" — my own request
    edit_request: RwSignal<String>,
    edit_requests: RwSignal<Vec<EditRequestItem>>,
    req_panel_open: RwSignal<bool>,
    req_message: RwSignal<String>,
    // dictionary word suggestions (IME-style completion)
    suggest_items: RwSignal<Vec<SuggestItem>>,
    suggest_active: RwSignal<usize>,
    suggest_pos: RwSignal<(i32, i32)>,
    // grammar analysis results
    analyze_open: RwSignal<bool>,
    analyze_busy: RwSignal<bool>,
    analyze_issues: RwSignal<Vec<Issue>>,
    analyze_applied: RwSignal<Vec<usize>>,
    // POS annotation overlay
    pos_on: RwSignal<bool>,
    pos_tagged_chars: RwSignal<usize>,
    /// this note has a marked copy in notes_mark
    mark_exists: RwSignal<bool>,
    // in-editor find
    find_open: RwSignal<bool>,
    find_query: RwSignal<String>,
    find_count: RwSignal<usize>,
    find_all: RwSignal<usize>,
    /// editing the notes_mark copy; saves go to notes_mark, never notes
    mark_mode: RwSignal<bool>,
    // edit-lock notice: dismissible banner + apply panel
    lock_dismissed: RwSignal<bool>,
    lock_apply_open: RwSignal<bool>,
}

#[derive(Clone)]
struct SuggestItem {
    word: String,
    is_ja: bool,
}

impl Ctx {
    fn id(&self) -> String {
        self.state.with_value(|st| st.id.clone())
    }

    fn set_status(&self, text: &str, cls: &str) {
        self.status_text.set(text.to_string());
        self.status_class.set(cls.to_string());
    }

    fn payload_with(&self, content: &str) -> String {
        serde_json::to_string(&SaveRequest {
            content: content.to_string(),
            font_size: Some(self.font_size.get() as f64),
            color: self.color.get(),
            title: self.title.get(),
            tags: self.parsed_tags.get(),
        })
        .unwrap()
    }

    fn note_payload(&self) -> String {
        let content = self
            .state
            .with_value(|st| st.editor.as_ref().map(|e| e.editor.get_value()))
            .unwrap_or_default();
        // persist the storage form — /i/ links stay relative in the database
        self.payload_with(&storage_md(&content))
    }

    fn mark_dirty(&self) {
        self.state.update_value(|st| st.dirty = true);
        self.set_status("有未保存的更改", "saving");
    }

    // ---- edit lock ----

    /// Sync all lock signals with the server; called on load, on a timer,
    /// and after every lock-related action.
    fn refresh_edit_status(&self) {
        let c = *self;
        spawn_local(async move {
            let Ok(resp) = gloo_net::http::Request::get(&api_url("/api/edit-status"))
                .credentials(RequestCredentials::Include)
                .send()
                .await
            else {
                return;
            };
            if !resp.ok() {
                return;
            }
            let Ok(d) = resp.json::<EditStatusResponse>().await else {
                return;
            };
            c.is_admin.set(d.is_admin);
            c.edit_locked.set(d.locked);
            c.can_edit.set(d.can_edit);
            c.edit_request.set(d.request);
        });
    }

    fn toggle_edit_lock(&self) {
        let locked = !self.edit_locked.get();
        let c = *self;
        spawn_local(async move {
            let req = gloo_net::http::Request::post(&api_url("/api/edit-lock"))
                .credentials(RequestCredentials::Include)
                .json(&EditLockRequest {
                locked,
            });
            let Ok(req) = req else { return };
            let Ok(resp) = req.send().await else { return };
            if resp.ok() {
                c.refresh_edit_status();
                c.set_status(
                    if locked { "已锁定编辑" } else { "已解锁编辑" },
                    "saved",
                );
            } else {
                c.set_status("锁定操作失败", "error");
            }
        });
    }

    fn submit_edit_request(&self) {
        let message = self.req_message.get();
        let c = *self;
        spawn_local(async move {
            let req = gloo_net::http::Request::post(&api_url("/api/edit-request"))
                .credentials(RequestCredentials::Include)
                .json(
                &EditRequestSubmit { message },
            );
            let Ok(req) = req else { return };
            let Ok(resp) = req.send().await else { return };
            if resp.ok() {
                c.req_message.set(String::new());
                c.refresh_edit_status();
                c.set_status("已提交编辑申请，等待管理员审批", "saved");
            } else {
                c.set_status("申请提交失败", "error");
            }
        });
    }

    /// Picker search box: an empty query restores the full list; a non-empty
    /// one hits the server-side full-text search (jieba + ipadic tokens),
    /// debounced so we don't fetch on every keystroke.
    fn on_picker_query(&self, q: String) {
        self.picker_query.set(q.clone());
        let trimmed = q.trim().to_string();
        let c = *self;
        self.state.update_value(|st| {
            if let Some(t) = st.search_debounce.take() {
                t.cancel();
            }
            if trimmed.is_empty() {
                spawn_local(async move {
                    if let Ok(resp) = gloo_net::http::Request::get(&api_url("/api/notes"))
                        .credentials(RequestCredentials::Include)
                        .send()
                        .await
                    {
                        if let Ok(d) = resp.json::<NoteList>().await {
                            c.all_notes.set(d.notes);
                        }
                    }
                });
            } else {
                st.search_debounce = Some(Timeout::new(300, move || {
                    let c = c;
                    spawn_local(async move {
                        let encoded: String = js_sys::encode_uri_component(&trimmed).into();
                        match gloo_net::http::Request::get(&api_url(&format!(
                            "/api/search?q={encoded}"
                        )))
                        .credentials(RequestCredentials::Include)
                        .send()
                        .await
                        {
                            Ok(resp) => {
                                if let Ok(d) = resp.json::<NoteList>().await {
                                    c.all_notes.set(d.notes);
                                }
                            }
                            Err(_) => c.set_status("搜索失败", "error"),
                        }
                    });
                }));
            }
        });
    }

    fn load_edit_requests(&self) {
        let c = *self;
        spawn_local(async move {
            let Ok(resp) = gloo_net::http::Request::get(&api_url("/api/edit-requests"))
                .credentials(RequestCredentials::Include)
                .send()
                .await
            else {
                return;
            };
            if let Ok(d) = resp.json::<EditRequestList>().await {
                c.edit_requests.set(d.requests);
            }
        });
    }

    fn handle_edit_request(&self, username: &str, action: &str) {
        let username = username.to_string();
        let action = action.to_string();
        let c = *self;
        spawn_local(async move {
            let req = gloo_net::http::Request::post(&api_url("/api/edit-requests/handle"))
                .credentials(RequestCredentials::Include)
                .json(
                &EditHandleRequest {
                    username: username.clone(),
                    action: action.clone(),
                },
            );
            let Ok(req) = req else { return };
            let Ok(resp) = req.send().await else { return };
            if resp.ok() {
                c.load_edit_requests();
                c.refresh_edit_status();
                c.set_status(
                    match action.as_str() {
                        "approve" => format!("已批准 {username} 的编辑权限"),
                        "reject" => format!("已拒绝 {username} 的申请"),
                        _ => format!("已撤销 {username} 的编辑权限"),
                    }
                    .as_str(),
                    "saved",
                );
            } else {
                c.set_status("操作失败", "error");
            }
        });
    }

    // ---- save ----

    fn save(&self) {
        if self.mark_mode.get() {
            self.save_mark_only();
            return;
        }
        let c = *self;
        spawn_local(async move {
            let payload = c.note_payload();
            let id = c.id();
            let result = match gloo_net::http::Request::put(&api_url(&format!("/api/note/{id}")))
                .credentials(RequestCredentials::Include)
                .header("Content-Type", "application/json")
                .body(payload)
            {
                Ok(req) => req.send().await,
                Err(e) => Err(e),
            };
            match result {
                Ok(resp) if resp.status() == 401 => {
                    // session expired — back to the login page
                    let _ = window().location().reload();
                }
                Ok(resp) if resp.status() == 403 => {
                    // edit was locked while we were editing — refresh the
                    // lock state so the overlay/status update, drop the edit
                    c.refresh_edit_status();
                    c.state.update_value(|st| st.dirty = false);
                    c.set_status("编辑已被锁定，本次修改未保存", "error");
                }
                Ok(resp) if resp.ok() => {
                    if let Ok(d) = resp.json::<Note>().await {
                        let author = d.author.clone();
                        c.state.update_value(|st| {
                            st.poll_updated_at = d.updated_at.clone();
                            st.dirty = false;
                            st.suppress_input_until_ms = js_sys::Date::now() + 300.0;
                        });
                        if !author.is_empty() {
                            c.author.set(author);
                        }
                        c.remote_changed.set(false);
                        c.set_status(&format!("已保存 {}", current_time()), "saved");
                    }
                }
                _ => {
                    c.set_status("保存失败，将重试", "error");
                    c.state.update_value(|st| {
                        st.retry = Some(Timeout::new(2000, move || c.save()));
                    });
                }
            }
        });
    }

    fn save_now(&self) {
        if !self.can_edit.get() {
            self.set_status("编辑已锁定，无法保存", "error");
            return;
        }
        let dirty = self.state.with_value(|st| st.dirty);
        if !dirty {
            self.set_status("没有更改需要保存", "");
            return;
        }
        self.state.update_value(|st| {
            if let Some(t) = st.retry.take() {
                t.cancel();
            }
        });
        self.save();
    }

    // ---- remote sync ----

    fn apply_remote(&self, d: &Note) {
        self.state.update_value(|st| st.applying_remote = true);
        let content = display_md(&d.content);
        self.state.with_value(|st| {
            if let Some(ed) = st.editor.as_ref() {
                ed.editor.set_value(&content);
            }
        });
        self.font_size.set(resolve_font_size(d.font_size as f64));
        self.color.set(if valid_color(&d.color) {
            d.color.clone()
        } else {
            String::new()
        });
        self.title.set(d.title.clone());
        self.tags_text.set(d.tags.join(", "));
        self.author.set(d.author.clone());
        self.state.update_value(|st| {
            st.applying_remote = false;
            st.poll_updated_at = d.updated_at.clone();
            st.dirty = false;
            st.loaded = true;
            // Vditor emits input shortly after set_value — ignore it
            st.suppress_input_until_ms = js_sys::Date::now() + 400.0;
            st.last_applied_content = d.content.clone();
            st.baseline_font = resolve_font_size(d.font_size as f64);
            st.baseline_color = if valid_color(&d.color) { d.color.clone() } else { String::new() };
            st.baseline_title = d.title.clone();
            st.baseline_tags = d.tags.join(", ");
        });
        self.remote_changed.set(false);
    }

    fn poll_note(&self) {
        let c = *self;
        spawn_local(async move {
            if c.state.with_value(|st| st.applying_remote) || c.show_login.get() || c.mark_mode.get() {
                return;
            }
            let Ok(resp) = gloo_net::http::Request::get(&api_url(&format!("/api/note/{}", c.id())))
                .credentials(RequestCredentials::Include)
                .send()
                .await
            else {
                return;
            };
            match resp.status() {
                401 => {
                    // stop polling on session expiry (Vue clears pollTimer)
                    c.state.update_value(|st| st.poll_interval = None);
                }
                s if (200..300).contains(&s) => {
                    let Ok(d) = resp.json::<Note>().await else { return };
                    let remote = c.state.with_value(|st| st.poll_updated_at.clone());
                    if remote.is_empty() || d.updated_at == remote {
                        return;
                    }
                    let dirty = c.state.with_value(|st| st.dirty);
                    if dirty {
                        // local unsaved edits — don't clobber them, offer to reload
                        c.remote_changed.set(true);
                    } else {
                        let who = if d.author.is_empty() {
                            "他人".to_string()
                        } else {
                            d.author.clone()
                        };
                        c.apply_remote(&d);
                        c.set_status(&format!("已同步 {who} 的更新"), "saved");
                    }
                }
                _ => {}
            }
        });
    }

    fn load_latest(&self) {
        let c = *self;
        spawn_local(async move {
            match gloo_net::http::Request::get(&api_url(&format!("/api/note/{}", c.id())))
                .credentials(RequestCredentials::Include)
                .send()
                .await
            {
                Ok(resp) if resp.ok() => {
                    if let Ok(d) = resp.json::<Note>().await {
                        c.apply_remote(&d);
                        c.set_status("已载入最新内容", "saved");
                    }
                }
                _ => c.set_status("加载失败", "error"),
            }
        });
    }

    // ---- presence ----

    fn user(&self) -> Option<String> {
        USER.with(|u| u.get())
    }

    fn beat_presence(&self) {
        let c = *self;
        spawn_local(async move {
            let Ok(req) = gloo_net::http::Request::post(&api_url("/api/presence"))
                .credentials(RequestCredentials::Include)
                .json(&PresenceRequest { note_id: c.id() })
            else {
                return;
            };
            if let Ok(resp) = req.send().await {
                if resp.ok() {
                    if let Ok(d) = resp.json::<PresenceResponse>().await {
                        let me = c.user();
                        c.presence_others.set(
                            d.users.into_iter().filter(|u| Some(u) != me.as_ref()).collect(),
                        );
                    }
                }
            }
        });
    }

    // ---- load & editor ----

    fn load_note(&self) {
        if self.state.with_value(|st| st.editor.is_some()) {
            return;
        }
        let c = *self;
        spawn_local(async move {
            let resp = match gloo_net::http::Request::get(&api_url(&format!("/api/note/{}", c.id())))
                .credentials(RequestCredentials::Include)
                .send()
                .await
            {
                Ok(r) => r,
                Err(_) => {
                    c.set_status("加载失败", "error");
                    return;
                }
            };
            if resp.status() == 401 {
                c.show_login.set(true);
                return;
            }
            let Ok(d) = resp.json::<Note>().await else {
                c.set_status("加载失败", "error");
                return;
            };
            c.font_size.set(resolve_font_size(d.font_size as f64));
            c.color.set(if valid_color(&d.color) {
                d.color.clone()
            } else {
                String::new()
            });
            c.title.set(d.title.clone());
            c.tags_text.set(d.tags.join(", "));
            c.author.set(d.author.clone());
            c.state
                .update_value(|st| st.poll_updated_at = d.updated_at.clone());
            c.set_status(if d.content.is_empty() { "" } else { "已加载" }, "");
            c.state.update_value(|st| {
                st.loaded = true;
                st.suppress_input_until_ms = js_sys::Date::now() + 600.0;
                st.last_applied_content = d.content.clone();
                st.baseline_font = resolve_font_size(d.font_size as f64);
                st.baseline_color = if valid_color(&d.color) { d.color.clone() } else { String::new() };
                st.baseline_title = d.title.clone();
                st.baseline_tags = d.tags.join(", ");
            });
            let dark = window()
                .match_media("(prefers-color-scheme: dark)")
                .ok()
                .flatten()
                .map(|m| m.matches())
                .unwrap_or(false);
            c.init_editor(&d.content, dark);
            c.start_collaboration();
        });
    }

    fn init_editor(&self, markdown: &str, dark: bool) {
        let el: Element = document()
            .query_selector(".editor")
            .ok()
            .flatten()
            .expect("editor element");

        let options = js_sys::Object::new();
        let set = |k: &str, v: &JsValue| {
            js_sys::Reflect::set(&options, &JsValue::from_str(k), v).unwrap();
        };
        set("value", &markdown.into());
        set("mode", &"wysiwyg".into());
        set("lang", &"zh_CN".into());
        // serve Vditor's runtime assets from our own static assets
        set("cdn", &format!("{BASE_PATH}/vditor").into());
        set("theme", &if dark { "dark" } else { "classic" }.into());
        let preview = js_sys::Object::new();
        js_sys::Reflect::set(
            &preview,
            &"theme".into(),
            &if dark { "dark" } else { "light" }.into(),
        )
        .unwrap();
        set("preview", &preview);
        set(
            "placeholder",
            &"在这里输入 Markdown，内容会自动保存到这个网址…".into(),
        );
        // note content lives in D1 — no localStorage draft cache
        let cache = js_sys::Object::new();
        js_sys::Reflect::set(&cache, &"enable".into(), &false.into()).unwrap();
        set("cache", &cache);
        // toolbar image button / paste / drag-drop upload to the worker.
        // A custom handler instead of Vditor's built-in `url` XHR upload:
        // the XHR is sent without credentials, which breaks cross-origin
        // deployments (session cookie would not be attached). gloo-net fetch
        // sends the cookie and Vditor still gets its protocol response.
        let upload = js_sys::Object::new();
        js_sys::Reflect::set(&upload, &"max".into(), &(8 * 1024 * 1024).into()).unwrap();
        js_sys::Reflect::set(&upload, &"fieldName".into(), &"file[]".into()).unwrap();
        let mut cbs: Vec<JsValue> = Vec::new();
        let c = *self;
        let upload_cb = Closure::<dyn FnMut(JsValue)>::new(move |files: JsValue| {
            let c = c;
            spawn_local(async move {
                let Ok(form) = web_sys::FormData::new() else { return };
                let files = js_sys::Array::from(&files);
                for f in files.iter() {
                    let f = f.unchecked_into::<web_sys::File>();
                    if form.append_with_blob("file[]", &f).is_err() {
                        return;
                    }
                }
                let Ok(req) = gloo_net::http::Request::post(&api_url("/api/upload"))
                    .credentials(RequestCredentials::Include)
                    .body(form)
                else {
                    return;
                };
                match req.send().await {
                    Ok(resp) if resp.ok() => match resp.json::<UploadResponse>().await {
                        Ok(ur) => {
                            c.state.with_value(|st| {
                                if let Some(ed) = st.editor.as_ref() {
                                    for (name, path) in &ur.data.succ_map {
                                        // display absolute on cross-origin
                                        // deployments; storage_md normalizes
                                        // it back to /i/... when saving
                                        let src = api_url(path);
                                        ed.editor
                                            .insert_value(&format!("![{name}]({src})\n"), true);
                                    }
                                }
                            });
                            if !ur.data.err_files.is_empty() {
                                c.set_status("部分图片上传失败", "error");
                            }
                        }
                        Err(_) => c.set_status("图片上传失败", "error"),
                    },
                    _ => c.set_status("图片上传失败", "error"),
                }
            });
        });
        js_sys::Reflect::set(&upload, &"handler".into(), upload_cb.as_ref()).unwrap();
        set("upload", &upload);
        cbs.push(upload_cb.into_js_value());
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
            "upload",
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
        set("toolbar", &toolbar);

        let c = *self;
        let input_cb = Closure::<dyn FnMut()>::new(move || {
            if c.state.with_value(|st| st.applying_remote)
                || js_sys::Date::now() < c.state.with_value(|st| st.suppress_input_until_ms)
            {
                return;
            }
            let echo = c.state.with_value(|st| {
                st.editor
                    .as_ref()
                    .map(|e| {
                        // compare in storage form: the editor may hold
                        // absolute /i/ URLs on cross-origin deployments
                        storage_md(&e.editor.get_value()).trim_end()
                            == st.last_applied_content.trim_end()
                    })
                    .unwrap_or(false)
            });
            if echo {
                return;
            }
            c.mark_dirty();
            c.update_suggestions();
            c.schedule_pos_refresh(false);
            if c.find_open.get() && !c.find_query.with(|q| q.trim().is_empty()) {
                let c2 = c;
                c.state.update_value(|st| {
                    st.find_debounce.take().map(|t| t.cancel());
                    st.find_debounce = Some(Timeout::new(400, move || c2.run_find()));
                });
            }
        });
        set("input", input_cb.as_ref().unchecked_ref());

        let c = *self;
        let blur_cb = Closure::<dyn FnMut()>::new(move || {
            // defer so a click on a suggestion row still lands first
            let c = c;
            let _ = Timeout::new(150, move || c.close_suggestions());
        });
        set("blur", blur_cb.as_ref().unchecked_ref());

        // `after` fires when Vditor finishes its async init, by which time the
        // handle is stored — safe to focus through state.
        let c = *self;
        let after_cb = Closure::<dyn FnMut()>::new(move || {
            c.state.with_value(|st| {
                if let Some(ed) = st.editor.as_ref() {
                    ed.editor.focus();
                }
            });
        });
        set("after", after_cb.as_ref().unchecked_ref());

        cbs.push(input_cb.into_js_value());
        cbs.push(after_cb.into_js_value());
        cbs.push(blur_cb.into_js_value());
        let editor = VditorEditor::new(&el, options.into());
        self.state.update_value(|st| {
            st.editor = Some(VditorHandle { editor, _cbs: cbs });
        });
    }

    fn start_collaboration(&self) {
        self.beat_presence();
        let c = *self;
        self.state
            .update_value(|st| st.intervals.push(Interval::new(15_000, move || c.beat_presence())));
        let c = *self;
        let poll = Interval::new(10_000, move || c.poll_note());
        self.state.update_value(|st| st.poll_interval = Some(poll));
        // keep lock/request state fresh (admin sees new applications, a
        // pending user learns when they are approved)
        let c = *self;
        self.state
            .update_value(|st| st.intervals.push(Interval::new(15_000, move || c.refresh_edit_status())));
        let c = *self;
        // autosave flushes pending edits every 3 minutes
        self.state.update_value(|st| {
            st.intervals.push(Interval::new(
                180_000,
                move || {
                    let do_save =
                        c.state.with_value(|st| st.dirty && st.editor.is_some());
                    if do_save {
                        c.save();
                    }
                },
            ));
        });
    }

    fn bump_font(&self, delta: i64) {
        let next = self.font_size.get() + delta;
        self.font_size.set(next.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE));
    }

    // ---- POS annotation overlay ----

    fn toggle_pos(&self) {
        let on = !self.pos_on.get();
        self.pos_on.set(on);
        if on {
            self.schedule_pos_refresh(true);
            self.refresh_mark_status();
        } else {
            self.state.update_value(|st| {
                st.pos_debounce.take().map(|t| t.cancel());
                st.pos_scroll_listener.take();
            });
            if let Some(ov) = document()
                .query_selector(".pos-overlay")
                .ok()
                .flatten()
            {
                ov.set_inner_html("");
            }
        }
    }

    fn schedule_pos_refresh(&self, immediate: bool) {
        if !self.pos_on.get() {
            return;
        }
        let c = *self;
        self.state.update_value(|st| {
            st.pos_debounce.take().map(|t| t.cancel());
            let delay = if immediate { 0 } else { 800 };
            st.pos_debounce = Some(Timeout::new(delay, move || c.do_pos_refresh()));
        });
    }

    fn do_pos_refresh(&self) {
        if !self.pos_on.get() {
            return;
        }
        // POS offsets must live in the same coordinate system as the DOM we
        // annotate: wysiwyg renders markdown to rich text (placeholders,
        // formatting), so we tag the visible text nodes, not the markdown.
        let Some((nodes, full_text)) = collect_editor_text() else { return };
        let c = *self;
        let id = self.id();
        // a FRESH notes_mark copy (generated at index time) is the preferred
        // annotation source — no tokenizer round-trip; stale or hand-edited
        // marks fall through to the live /api/pos pass
        let current_updated = self.state.with_value(|st| st.poll_updated_at.clone());
        spawn_local(async move {
            let seq = {
                let mut n = 0u64;
                c.state.update_value(|st| {
                    st.pos_seq += 1;
                    n = st.pos_seq;
                });
                n
            };
            let mut tagged_chars = 0usize;
            let tokens = if let Ok(resp) =
                gloo_net::http::Request::get(&api_url(&format!("/api/mark/{id}")))
                    .credentials(RequestCredentials::Include)
                    .send()
                    .await
            {
                if resp.ok() {
                    if let Ok(info) = resp.json::<MarkInfo>().await {
                        if !info.source_updated_at.is_empty()
                            && info.source_updated_at == current_updated
                        {
                            c.mark_exists.set(true);
                            tagged_chars = info.content.chars().count();
                            let (mut toks, clean) = parse_marked_tokens(&info.content);
                            // token offsets are relative to the clean note
                            // text; the editor walk includes UI chrome
                            // before the content — shift to match
                            if let Some(byte) = full_text.find(&clean[..clean.len().min(40)]) {
                                let shift = full_text[..byte].chars().count();
                                for t in &mut toks {
                                    t.s += shift;
                                    t.e += shift;
                                }
                            }
                            toks
                        } else {
                            Vec::new()
                        }
                    } else {
                        Vec::new()
                    }
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };
            let (tokens, tagged_chars) = if tokens.is_empty() {
                let Ok(req) = gloo_net::http::Request::post(&api_url("/api/pos"))
                    .credentials(RequestCredentials::Include)
                    .json(&AnalyzeRequest { content: full_text.clone() })
                else {
                    return;
                };
                let Ok(resp) = req.send().await else { return };
                let Ok(d) = resp.json::<PosResponse>().await else { return };
                tagged_chars = d.tagged_chars;
                (d.tokens, tagged_chars)
            } else {
                (tokens, tagged_chars)
            };
            if !c.pos_on.get() || c.state.with_value(|st| st.pos_seq) != seq {
                return; // toggled off or a newer refresh superseded us
            }
            c.pos_tagged_chars.set(tagged_chars);
            c.state.update_value(|st| st.pos_tokens_cache = tokens.clone());
            let drawn = render_pos_marks(&c, nodes, &full_text, &tokens);
            if !drawn && !tokens.is_empty() {
                // transient editor state (mid-render) can yield zero rects —
                // retry once shortly
                let c = c;
                Timeout::new(600, move || {
                    if c.pos_on.get() {
                        c.schedule_pos_refresh(true);
                    }
                })
                .forget();
            }
        });
    }

    // ---- in-editor find ----

    fn toggle_find(&self) {
        let on = !self.find_open.get();
        self.find_open.set(on);
        if on {
            let _ = Timeout::new(60, move || {
                if let Some(input) = document().query_selector(".find-input").ok().flatten() {
                    let _ = input.dyn_into::<HtmlElement>().map(|e| e.focus());
                }
            });
        } else {
            self.find_query.set(String::new());
            self.find_count.set(0);
            self.find_all.set(0);
            self.state.update_value(|st| {
                st.find_debounce.take().map(|t| t.cancel());
            });
            if let Some(l) = document().query_selector(".find-layer").ok().flatten() {
                l.set_inner_html("");
            }
        }
    }

    fn on_find_query(&self, q: String) {
        self.find_query.set(q);
        let c = *self;
        self.state.update_value(|st| {
            st.find_debounce.take().map(|t| t.cancel());
            st.find_debounce = Some(Timeout::new(300, move || c.run_find()));
        });
    }

    /// Highlight every occurrence of the query in the editor (same
    /// Range-rect positioning as POS marks), scroll to the first, and ask
    /// the search API how many notes match overall.
    fn run_find(&self) {
        let q = self.find_query.with(|s| s.trim().to_string());
        if q.is_empty() || !self.find_open.get() {
            if let Some(l) = document().query_selector(".find-layer").ok().flatten() {
                l.set_inner_html("");
            }
            self.find_count.set(0);
            return;
        }
        let Some((_nodes, full)) = collect_editor_text() else { return };
        let occ = find_occurrences(&full, &q);
        self.find_count.set(occ.len());

        self.state.update_value(|st| st.find_ranges_cache = occ.clone());
        let drawn = self.draw_find_marks(&occ);

        // cross-note info from the search API
        let c = *self;
        let enc: String = js_sys::encode_uri_component(&q).into();
        spawn_local(async move {
            if let Ok(resp) = gloo_net::http::Request::get(&api_url(&format!(
                "/api/search?q={enc}"
            )))
            .credentials(RequestCredentials::Include)
            .send()
            .await
            {
                if resp.ok() {
                    if let Ok(d) = resp.json::<NoteList>().await {
                        if c.find_open.get() {
                            c.find_all.set(d.notes.len());
                        }
                    }
                }
            }
        });
        let _ = drawn;
    }

    // ---- mark editing mode: the editor shows notes_mark, saves go there ----

    /// Load the marked copy into the editor. The clean original stays in the
    /// database and is restored on exit.
    fn enter_mark_mode(&self) {
        let c = *self;
        if self.state.with_value(|st| st.dirty) {
            // persist pending original edits first, then enter
            c.save();
            let cc = c;
            Timeout::new(700, move || cc.do_enter_mark_mode()).forget();
            return;
        }
        self.do_enter_mark_mode();
    }

    fn do_enter_mark_mode(&self) {
        let c = *self;
        let id = self.id();
        spawn_local(async move {
            let Ok(resp) = gloo_net::http::Request::get(&api_url(&format!("/api/mark/{id}")))
                .credentials(RequestCredentials::Include)
                .send()
                .await
            else {
                return;
            };
            if !resp.ok() {
                c.set_status("没有标记文档,请先生成标记", "error");
                return;
            }
            let Ok(info) = resp.json::<MarkInfo>().await else { return };
            let content = display_md(&info.content);
            c.state.with_value(|st| {
                if let Some(ed) = st.editor.as_ref() {
                    ed.editor.set_value(&content);
                }
            });
            c.state.update_value(|st| {
                st.dirty = false;
                st.poll_updated_at = String::new();
                st.last_applied_content = info.content.clone();
            });
            c.mark_mode.set(true);
            c.set_status("正在编辑标记文档(保存只写入 notes_mark)", "saved");
        });
    }

    /// Save any pending mark edits, then reload the clean original.
    fn exit_mark_mode(&self) {
        let dirty = self.state.with_value(|st| st.dirty);
        let c = *self;
        if dirty {
            spawn_local(async move {
                c.save_mark_only();
                // give the save a moment, then restore the clean note
                let cc = c;
                Timeout::new(600, move || cc.load_clean_note()).forget();
            });
        } else {
            self.load_clean_note();
        }
    }

    fn load_clean_note(&self) {
        let c = *self;
        let id = self.id();
        spawn_local(async move {
            if let Ok(resp) = gloo_net::http::Request::get(&api_url(&format!("/api/note/{id}")))
                .credentials(RequestCredentials::Include)
                .send()
                .await
            {
                if let Ok(d) = resp.json::<Note>().await {
                    c.apply_remote(&d);
                    c.mark_mode.set(false);
                    c.set_status("已返回原文", "saved");
                }
            }
        });
    }

    /// Persist editor content into notes_mark only.
    fn save_mark_only(&self) {
        let content = self
            .state
            .with_value(|st| st.editor.as_ref().map(|e| e.editor.get_value()))
            .unwrap_or_default();
        let content = storage_md(&content);
        let c = *self;
        let id = self.id();
        spawn_local(async move {
            let result = match gloo_net::http::Request::put(&api_url(&format!("/api/mark/{id}")))
                .credentials(RequestCredentials::Include)
                .json(&AnalyzeRequest { content })
            {
                Ok(req) => req.send().await,
                Err(e) => Err(e),
            };
            match result {
                Ok(resp) if resp.status() == 401 => {
                    let _ = window().location().reload();
                }
                Ok(resp) if resp.ok() => {
                    c.state.update_value(|st| {
                        st.dirty = false;
                        st.suppress_input_until_ms = js_sys::Date::now() + 300.0;
                    });
                    c.set_status(&format!("已保存标记 {}", current_time()), "saved");
                }
                Ok(resp) if resp.status() == 403 => c.set_status("编辑已锁定", "error"),
                _ => c.set_status("保存标记失败,将重试", "error"),
            }
        });
    }

    /// Highlight the given occurrences (shared by run_find and zoom refresh).
    fn draw_find_marks(&self, occ: &[(usize, usize)]) -> bool {
        if occ.is_empty() {
            if let Some(l) = document().query_selector(".find-layer").ok().flatten() {
                l.set_inner_html("");
            }
            return false;
        }
        let Some(content_el) = document()
            .query_selector(".editor .vditor-content")
            .ok()
            .flatten()
        else {
            return false;
        };
        let Some(overlay) = document().query_selector(".pos-overlay").ok().flatten() else {
            return false;
        };
        let Some(space) = MarkSpace::build(self, &content_el, &overlay) else {
            return false;
        };
        let Some((nodes, _)) = collect_editor_text() else { return false };
        let prep = prep_nodes(&nodes);
        let ranges: Vec<(usize, usize, String)> = occ
            .iter()
            .map(|&(a, b)| (a, b, "find-mark".to_string()))
            .collect();
        let marks = rects_for_ranges(&prep, &ranges, &space, 500);
        let ok = draw_marks(self, "find-layer", &marks);
        // 定位:scroll the first match into view. Mark overlay y maps back to
        // content scroll via the zoom factor.
        if let Some((_, y, _, _, _)) = marks.first() {
            let target = ((*y - space.oy - 120.0) / space.zoom).max(0.0) as i32;
            scroll_host(&content_el).set_scroll_top(target);
        }
        ok
    }

    /// Re-render both mark layers from cached data — called when the editor
    /// zoom (font size) changes, so marks track the resized text.
    fn refresh_overlay_marks(&self) {
        if self.pos_on.get() {
            let tokens = self.state.with_value(|st| st.pos_tokens_cache.clone());
            if !tokens.is_empty() {
                if let Some((nodes, _)) = collect_editor_text() {
                    render_pos_marks(self, nodes, "", &tokens);
                }
            }
        }
        if self.find_open.get() {
            let occ = self.state.with_value(|st| st.find_ranges_cache.clone());
            if !occ.is_empty() {
                self.draw_find_marks(&occ);
            }
        }
    }

    /// Probe notes_mark for the current note (called when the POS panel opens).
    fn refresh_mark_status(&self) {
        let c = *self;
        let id = self.id();
        spawn_local(async move {
            let Ok(resp) = gloo_net::http::Request::get(&api_url(&format!("/api/mark/{id}")))
                .credentials(RequestCredentials::Include)
                .send()
                .await
            else {
                return;
            };
            c.mark_exists.set(resp.ok());
        });
    }

    /// Create/update the marked copy in notes_mark (notes itself stays clean).
    fn generate_mark(&self) {
        let c = *self;
        let id = self.id();
        spawn_local(async move {
            match gloo_net::http::Request::post(&api_url(&format!("/api/mark/{id}")))
                .credentials(RequestCredentials::Include)
                .send()
                .await
            {
                Ok(resp) if resp.ok() => {
                    c.mark_exists.set(true);
                    c.set_status("标记文档已生成(notes_mark)", "saved");
                }
                Ok(resp) if resp.status() == 403 => c.set_status("编辑已锁定", "error"),
                _ => c.set_status("生成标记失败", "error"),
            }
        });
    }

    /// Drop the marked copy; the original note is untouched.
    fn remove_mark(&self) {
        if !window()
            .confirm_with_message("取消标记将删除标记文档(原便签不受影响),确定?")
            .unwrap_or(false)
        {
            return;
        }
        let c = *self;
        let id = self.id();
        spawn_local(async move {
            match gloo_net::http::Request::delete(&api_url(&format!("/api/mark/{id}")))
                .credentials(RequestCredentials::Include)
                .send()
                .await
            {
                Ok(resp) if resp.ok() => {
                    c.mark_exists.set(false);
                    c.set_status("已取消标记", "saved");
                }
                _ => c.set_status("取消标记失败", "error"),
            }
        });
    }

    /// Download the marked copy as a .md file.
    fn export_mark(&self) {
        let c = *self;
        let id = self.id();
        spawn_local(async move {
            let Ok(resp) = gloo_net::http::Request::get(&api_url(&format!("/api/mark/{id}")))
                .credentials(RequestCredentials::Include)
                .send()
                .await
            else {
                return;
            };
            if !resp.ok() {
                c.set_status("没有标记文档", "error");
                return;
            }
            let Ok(info) = resp.json::<MarkInfo>().await else {
                return;
            };
            let arr = js_sys::Array::new();
            arr.push(&JsValue::from_str(&info.content));
            let Ok(blob) = Blob::new_with_str_sequence(&arr) else {
                return;
            };
            let Ok(url) = web_sys::Url::create_object_url_with_blob(&blob) else {
                return;
            };
            if let Some(a) = document()
                .create_element("a")
                .ok()
                .and_then(|e| e.dyn_into::<HtmlAnchorElement>().ok())
            {
                a.set_href(&url);
                a.set_download(&format!("{id}-marked.md"));
                a.click();
                c.set_status("已导出标记文档", "saved");
            }
            let _ = web_sys::Url::revoke_object_url(&url);
        });
    }

    // ---- grammar analysis ----

    fn run_analysis(&self) {
        if self.analyze_busy.get() {
            return;
        }
        self.analyze_busy.set(true);
        self.analyze_open.set(true);
        let c = *self;
        spawn_local(async move {
            let content = c
                .state
                .with_value(|st| st.editor.as_ref().map(|e| e.editor.get_value()))
                .unwrap_or_default();
            let result = match gloo_net::http::Request::post(&api_url("/api/analyze"))
                .credentials(RequestCredentials::Include)
                .json(&AnalyzeRequest { content })
            {
                Ok(req) => req.send().await,
                Err(_) => {
                    c.analyze_busy.set(false);
                    c.set_status("分析请求失败", "error");
                    return;
                }
            };
            c.analyze_busy.set(false);
            match result {
                Ok(resp) if resp.ok() => {
                    if let Ok(d) = resp.json::<AnalyzeResponse>().await {
                        let empty = d.issues.is_empty();
                        c.analyze_issues.set(d.issues);
                        c.analyze_applied.set(Vec::new());
                        c.set_status(
                            if empty { "分析完成:未发现明显问题" } else { "分析完成" },
                            "saved",
                        );
                    } else {
                        c.set_status("分析结果解析失败", "error");
                    }
                }
                _ => c.set_status("分析失败", "error"),
            }
        });
    }

    /// Apply an issue's suggestion by char-offset replacement.
    fn apply_issue(&self, idx: usize) {
        let Some(issue) = self.analyze_issues.with(|v| v.get(idx).cloned()) else {
            return;
        };
        if issue.start == issue.end {
            return; // whole-text warning, nothing to replace
        }
        let current = self
            .state
            .with_value(|st| st.editor.as_ref().map(|e| e.editor.get_value()))
            .unwrap_or_default();
        let chars: Vec<char> = current.chars().collect();
        if issue.end > chars.len() {
            self.set_status("内容已变化,请重新分析", "error");
            return;
        }
        let mut next: String = chars[..issue.start].iter().collect();
        next.push_str(&issue.suggestion);
        next.extend(chars[issue.end..].iter());
        self.state.with_value(|st| {
            if let Some(ed) = st.editor.as_ref() {
                ed.editor.set_value(&next);
            }
        });
        self.analyze_applied.update(|v| v.push(idx));
        self.mark_dirty();
        self.save();
        // later issue offsets are now stale — shift them by the delta
        let delta = issue.suggestion.chars().count() as isize
            - (issue.end - issue.start) as isize;
        self.analyze_issues.update(|v| {
            for (i, it) in v.iter_mut().enumerate() {
                if i != idx && it.start >= issue.end {
                    it.start = (it.start as isize + delta).max(0) as usize;
                    it.end = (it.end as isize + delta).max(0) as usize;
                }
            }
        });
    }

    // ---- dictionary word suggestions (IME-style) ----

    fn close_suggestions(&self) {
        self.suggest_items.update(|v| {
            if !v.is_empty() {
                v.clear();
            }
        });
        self.state.update_value(|st| st.suggest_anchor_node = None);
    }

    /// Read the word fragment typed before the cursor and refresh the
    /// suggestion panel (called from editor input/selection events).
    fn update_suggestions(&self) {
        if self.state.with_value(|st| st.applying_remote) || !self.can_edit.get() {
            self.close_suggestions();
            return;
        }
        let Some(sel) = window().get_selection().ok().flatten() else {
            self.close_suggestions();
            return;
        };
        if !sel.is_collapsed() {
            self.close_suggestions();
            return;
        }
        let Some(node) = sel.anchor_node() else {
            self.close_suggestions();
            return;
        };
        if node.node_type() != web_sys::Node::TEXT_NODE {
            self.close_suggestions();
            return;
        }
        let offset = sel.anchor_offset();
        let text = node.text_content().unwrap_or_default();
        let chars: Vec<char> = text.chars().collect();
        let offset = offset as usize;
        if offset > chars.len() {
            self.close_suggestions();
            return;
        }
        // walk backwards over word characters (CJK or alnum)
        let mut start = offset;
        while start > 0 && is_word_char(chars[start - 1]) {
            start -= 1;
        }
        let fragment: String = chars[start..offset].iter().collect();
        // stay hidden right after accepting: the accepted word itself would
        // re-trigger the panel at the same cursor position
        if self
            .state
            .with_value(|st| !st.suggest_accepted_word.is_empty())
        {
            if self
                .state
                .with_value(|st| st.suggest_accepted_word == fragment)
            {
                self.close_suggestions();
                return;
            }
            self.state.update_value(|st| st.suggest_accepted_word.clear());
        }
        let has_cjk = fragment.chars().any(is_cjk_char);
        if !has_cjk || fragment.is_empty() || fragment.chars().count() > 8 {
            self.close_suggestions();
            return;
        }
        let items = lookup_suggestions(&fragment);
        if items.is_empty() {
            self.close_suggestions();
            return;
        }
        // position the panel at the cursor (viewport coords + position:fixed)
        let pos = sel
            .get_range_at(0)
            .ok()
            .map(|r| r.get_bounding_client_rect())
            .map(|r| (r.left() as i32, (r.bottom() + 4.0) as i32))
            .unwrap_or((40, 40));
        self.state.update_value(|st| {
            st.suggest_anchor_node = Some(SendWrapper::new(node));
            st.suggest_anchor_start = start;
            st.suggest_anchor_end = offset;
        });
        self.suggest_active.set(0);
        self.suggest_pos.set(pos);
        self.suggest_items.set(items);
    }

    fn on_suggest_keydown(&self, e: &web_sys::KeyboardEvent) {
        if self.suggest_items.with(|v| v.is_empty()) {
            return;
        }
        let len = self.suggest_items.with(|v| v.len());
        match e.key().as_str() {
            "ArrowDown" => {
                e.prevent_default();
                self.suggest_active.update(|i| *i = (*i + 1) % len);
            }
            "ArrowUp" => {
                e.prevent_default();
                self.suggest_active.update(|i| *i = (*i + len - 1) % len);
            }
            "Enter" | "Tab" => {
                e.prevent_default();
                let i = self.suggest_active.get();
                self.accept_suggestion(i);
            }
            "Escape" => {
                self.close_suggestions();
            }
            _ => {}
        }
    }

    /// Replace the typed fragment with the chosen word.
    fn accept_suggestion(&self, idx: usize) {
        let Some(item) = self.suggest_items.with(|v| v.get(idx).cloned()) else {
            return;
        };
        let anchor = self.state.with_value(|st| {
            st.suggest_anchor_node
                .as_ref()
                .map(|n| (n.clone(), st.suggest_anchor_start, st.suggest_anchor_end))
        });
        self.close_suggestions();
        let Some((node, start, end)) = anchor else { return };
        if let (Ok(Some(sel)), Ok(range)) =
            (window().get_selection(), web_sys::Range::new())
        {
            if range.set_start(&node, start as u32).is_ok()
                && range.set_end(&node, end as u32).is_ok()
            {
                sel.remove_all_ranges().ok();
                sel.add_range(&range).ok();
                self.state.with_value(|st| {
                    if let Some(ed) = st.editor.as_ref() {
                        // insertValue collapses the selection and inserts at
                        // its start — delete the selected fragment first
                        ed.editor.delete_value();
                        ed.editor.insert_value(&item.word, true);
                    }
                });
                self.state
                    .update_value(|st| st.suggest_accepted_word = item.word.clone());
                self.mark_dirty();
            }
        }
    }

    // ---- export & import ----

    /// Download the current editor content as a .md file named after the
    /// note title (falling back to the note id).
    fn export_md(&self) {
        let content = self
            .state
            .with_value(|st| st.editor.as_ref().map(|e| e.editor.get_value()))
            .unwrap_or_default();
        if content.trim().is_empty() {
            self.set_status("没有内容可导出", "error");
            return;
        }
        let name = sanitize_filename(&self.title.get());
        let name = if name.is_empty() { self.id() } else { name };

        let arr = js_sys::Array::new();
        arr.push(&JsValue::from_str(&content));
        let Ok(blob) = Blob::new_with_str_sequence(&arr) else {
            self.set_status("导出失败", "error");
            return;
        };
        let Ok(url) = web_sys::Url::create_object_url_with_blob(&blob) else {
            self.set_status("导出失败", "error");
            return;
        };
        let clicked = document()
            .create_element("a")
            .ok()
            .and_then(|e| e.dyn_into::<HtmlAnchorElement>().ok())
            .map(|a| {
                a.set_href(&url);
                a.set_download(&format!("{name}.md"));
                a.click();
            })
            .is_some();
        let _ = web_sys::Url::revoke_object_url(&url);
        let msg = if clicked {
            format!("已导出 {name}.md")
        } else {
            "导出失败".to_string()
        };
        self.set_status(&msg, if clicked { "saved" } else { "error" });
    }
}

// ---- POS annotation rendering ----

/// Collect editable text nodes (skipping code blocks etc.) with their global
/// char offsets, as (node, text, char_start).
fn collect_text_nodes(el: &Element, out: &mut Vec<(web_sys::Node, String, usize)>, acc: &mut usize) {
    let children = el.child_nodes();
    for i in 0..children.length() {
        let Some(node) = children.item(i) else { continue };
        match node.node_type() {
            web_sys::Node::TEXT_NODE => {
                let text = node.text_content().unwrap_or_default();
                let len = text.chars().count();
                if !text.trim().is_empty() {
                    out.push((node.clone(), text, *acc));
                }
                *acc += len;
            }
            web_sys::Node::ELEMENT_NODE => {
                if let Ok(el) = node.dyn_into::<Element>() {
                    // Vditor wraps every wysiwyg block in <pre class="vditor-reset">
                    // — only <code> (real code blocks), style and script
                    // containers are skipped
                    if el.closest("code, style, script").ok().flatten().is_none() {
                        collect_text_nodes(&el, out, acc);
                    } else {
                        // still advance the offset so positions stay aligned
                        let text = el.text_content().unwrap_or_default();
                        *acc += text.chars().count();
                    }
                }
            }
            _ => {}
        }
    }
}

/// Walk the editor's rendered text, returning (node, text, char_start) plus
/// the concatenated visible text.
fn collect_editor_text() -> Option<(Vec<(web_sys::Node, String, usize)>, String)> {
    let content_el = document()
        .query_selector(".editor .vditor-content")
        .ok()
        .flatten()?;
    let mut nodes: Vec<(web_sys::Node, String, usize)> = Vec::new();
    let mut acc = 0usize;
    collect_text_nodes(&content_el, &mut nodes, &mut acc);
    let full: String = nodes.iter().map(|(_, t, _)| t.as_str()).collect();
    Some((nodes, full))
}

/// Per node: char length + UTF-16 prefix sums (DOM offsets are UTF-16
/// units; `prefix[k]` = UTF-16 length of the first `k` chars).
type PrepNode = (web_sys::Node, usize, usize, Vec<u32>);

fn prep_nodes(nodes: &[(web_sys::Node, String, usize)]) -> Vec<PrepNode> {
    nodes
        .iter()
        .map(|(n, t, s)| {
            let mut prefix = Vec::with_capacity(t.chars().count() + 1);
            prefix.push(0);
            for ch in t.chars() {
                prefix.push(prefix.last().unwrap() + ch.len_utf16() as u32);
            }
            (n.clone(), *s, prefix.len() - 1, prefix)
        })
        .collect()
}

/// Coordinate mapping for mark boxes. The overlay sits in .editor-wrap,
/// which starts above .vditor-content (toolbar strip) and is NOT affected by
/// the editor's CSS zoom — so rect deltas must be re-based onto the overlay
/// origin (ox/oy) and de-zoomed.
struct MarkSpace {
    zoom: f64,
    cx: f64,
    cy: f64,
    ox: f64,
    oy: f64,
    scroll_left: f64,
    scroll_top: f64,
}

impl MarkSpace {
    fn build(c: &Ctx, content_el: &Element, overlay: &Element) -> Option<Self> {
        let cr = content_el.get_bounding_client_rect();
        let or = overlay.get_bounding_client_rect();
        Some(Self {
            zoom: c.font_size.get() as f64 / DEFAULT_FONT_SIZE as f64,
            cx: cr.left(),
            cy: cr.top(),
            ox: cr.left() - or.left(),
            oy: cr.top() - or.top(),
            scroll_left: 0.0,
            scroll_top: 0.0,
        })
        .map(|mut sp| {
            let host = scroll_host(content_el);
            sp.scroll_left = host.scroll_left() as f64;
            sp.scroll_top = host.scroll_top() as f64;
            sp
        })
    }
    /// Marks live in the overlay's viewport-pixel space, addressed in
    /// content-document units × zoom: positions bake the CURRENT scroll so
    /// the layer transform only has to track scroll *changes*. (The overlay
    /// is outside the editor's CSS zoom, so scroll compensation is scaled
    /// by zoom in the scroll listener.)
    fn xy(&self, left: f64, top: f64) -> (f64, f64) {
        (
            (left - self.cx) + self.ox + self.scroll_left * self.zoom,
            (top - self.cy) + self.oy + self.scroll_top * self.zoom,
        )
    }
}

/// Vditor 4 keeps every block in one <pre class="vditor-reset"> that carries
/// its own overflow — THAT element scrolls, not .vditor-content (which is a
/// flex shell). Fall back to .vditor-content if the pre doesn't overflow.
fn scroll_host(content_el: &Element) -> Element {
    if let Some(pre) = content_el
        .query_selector("pre.vditor-reset")
        .ok()
        .flatten()
    {
        if pre.scroll_height() > pre.client_height() || pre.scroll_width() > pre.client_width() {
            return pre;
        }
    }
    content_el.clone()
}

/// Current scroll compensation for the overlay layers (content units × zoom).
fn overlay_scroll_offset(c: &Ctx, content_el: &Element) -> (f64, f64) {
    let zoom = c.font_size.get() as f64 / DEFAULT_FONT_SIZE as f64;
    let host = scroll_host(content_el);
    (
        host.scroll_left() as f64 * zoom,
        host.scroll_top() as f64 * zoom,
    )
}

/// Compute mark boxes for char ranges. Reads layout only —
/// the caller batches all rects before writing any DOM (interleaved
/// read/write would force a reflow per range and freeze on large notes).
fn rects_for_ranges(
    prep: &[PrepNode],
    ranges: &[(usize, usize, String)],
    space: &MarkSpace,
    max: usize,
) -> Vec<(f64, f64, f64, f64, String)> {
    let mut out = Vec::new();
    let mut node_idx = 0usize;
    'outer: for (s, e, class) in ranges {
        while node_idx < prep.len() && *s >= prep[node_idx].1 + prep[node_idx].2 {
            node_idx += 1;
        }
        let Some((node, start, len, prefix)) = prep.get(node_idx) else { break };
        if *e > *start + *len {
            continue; // spans nodes — rare, skip
        }
        let su = prefix[(*s - *start).min(*len)];
        let eu = prefix[(*e - *start).min(*len)];
        let Ok(range) = web_sys::Range::new() else { continue };
        if range.set_start(node, su).is_err() || range.set_end(node, eu).is_err() {
            continue;
        }
        let Some(rects) = range.get_client_rects() else { continue };
        for i in 0..rects.length() {
            let Some(rect) = rects.item(i) else { continue };
            if rect.width() <= 0.0 || rect.height() <= 0.0 {
                continue;
            }
            let (x, y) = space.xy(rect.left(), rect.top());
            out.push((x, y, rect.width(), rect.height(), class.clone()));
            if out.len() >= max {
                break 'outer;
            }
        }
    }
    out
}

/// Draw mark boxes into a dedicated layer inside .pos-overlay (find and POS
/// layers coexist). Also installs the shared scroll translator once.
fn draw_marks(c: &Ctx, layer_class: &str, marks: &[(f64, f64, f64, f64, String)]) -> bool {
    let Some(overlay) = document().query_selector(".pos-overlay").ok().flatten() else {
        return false;
    };
    let Some(content_el) = document()
        .query_selector(".editor .vditor-content")
        .ok()
        .flatten()
    else {
        return false;
    };
    let zoom = c.font_size.get() as f64 / DEFAULT_FONT_SIZE as f64;
    let _ = overlay.set_attribute("style", &format!("--mark-zoom: {zoom};"));
    let layer = match overlay
        .query_selector(&format!(".{layer_class}"))
        .ok()
        .flatten()
    {
        Some(l) => {
            l.set_inner_html("");
            l
        }
        None => {
            let Ok(l) = document().create_element("div") else { return false };
            l.set_class_name(layer_class);
            let _ = overlay.append_child(&l);
            l
        }
    };
    let mut html = String::with_capacity(marks.len() * 90);
    for (x, y, w, h, cls) in marks {
        html.push_str(&format!(
            "<div class=\"{cls}\" style=\"left:{x:.1}px;top:{y:.1}px;width:{w:.1}px;height:{h:.1}px\"></div>"
        ));
    }
    layer.set_inner_html(&html);
    let (sx, sy) = overlay_scroll_offset(c, &content_el);
    let _ = layer.set_attribute(
        "style",
        &format!("transform: translate({}px, {}px);", -sx, -sy),
    );
    ensure_scroll_listener(c, &content_el, &overlay);
    !marks.is_empty()
}

/// One scroll listener that translates every overlay layer (POS marks, find
/// highlights) so they track the scrolling content.
fn ensure_scroll_listener(c: &Ctx, content_el: &Element, overlay: &Element) {
    let exists = c
        .state
        .with_value(|st| st.pos_scroll_listener.is_some());
    if exists {
        return;
    }
    let listener_target = content_el.clone();
    let content_for_cb = content_el.clone();
    let overlay = overlay.clone();
    let c = *c;
    let cb = move |_: &WebEvent| {
        // the real scroller (inner pre) may be re-created by Vditor — look
        // it up live; offsets are in document units, scaled by zoom
        let zoom = c.font_size.get() as f64 / DEFAULT_FONT_SIZE as f64;
        let host = scroll_host(&content_for_cb);
        let sx = host.scroll_left() as f64 * zoom;
        let sy = host.scroll_top() as f64 * zoom;
        let children = overlay.children();
        for i in 0..children.length() {
            if let Some(el) = children
                .item(i)
                .and_then(|n| n.dyn_into::<Element>().ok())
            {
                let _ = el.set_attribute(
                    "style",
                    &format!("transform: translate({}px, {}px);", -sx, -sy),
                );
            }
        }
    };
    // capture phase: scroll events don't bubble, but they pass through
    // .vditor-content on the way down to the inner pre
    let listener = gloo_events::EventListener::new_with_options(
        &listener_target,
        "scroll",
        gloo_events::EventListenerOptions::run_in_capture_phase(),
        cb,
    );
    c.state
        .update_value(|st| st.pos_scroll_listener = Some(SendWrapper::new(listener)));
}

fn render_pos_marks(
    c: &Ctx,
    nodes: Vec<(web_sys::Node, String, usize)>,
    content: &str,
    tokens: &[PosToken],
) -> bool {
    let (Some(content_el), Some(overlay)) = (
        document().query_selector(".editor .vditor-content").ok().flatten(),
        document().query_selector(".pos-overlay").ok().flatten(),
    ) else {
        return false;
    };
    let Some(space) = MarkSpace::build(c, &content_el, &overlay) else {
        return false;
    };
    let prep = prep_nodes(&nodes);
    let ranges: Vec<(usize, usize, String)> = tokens
        .iter()
        .map(|t| (t.s, t.e, format!("pos-mark cat-{}", t.cat)))
        .collect();
    let marks = rects_for_ranges(&prep, &ranges, &space, 5000);
    let drawn = draw_marks(c, "pos-layer", &marks);
    let _ = content;
    drawn || tokens.is_empty()
}

/// Parse `<span class="posw cat-X">word</span>` tags out of a marked
/// document, producing tokens with char offsets relative to the CLEAN text
/// (tags stripped) — the same shape /api/pos returns, so the overlay can
/// render straight from the persisted notes_mark copy.
/// Returns the tokens plus the clean text (all posw tags stripped) so the
/// caller can align token offsets with the editor's text-node walk, which
/// includes UI chrome ("大纲", preview-mode buttons) before the content.
fn parse_marked_tokens(marked: &str) -> (Vec<PosToken>, String) {
    const OPEN: &str = "<span class=\"posw cat-";
    let mut tokens = Vec::new();
    let mut clean = String::new();
    let mut rest = marked;
    let mut clean_base = 0usize; // cumulative clean-text offset
    while let Some(start) = rest.find(OPEN) {
        let Some(close_quote) = rest[start + OPEN.len()..].find('"') else { break };
        let cat: String = rest[start + OPEN.len()..start + OPEN.len() + close_quote].chars().collect();
        let Some(gt) = rest[start..].find('>') else { break };
        let body_start = start + gt + 1;
        let Some(body_end_rel) = rest[body_start..].find("</span>") else { break };
        let body_end = body_start + body_end_rel;
        let prefix = strip_posw(&rest[..start]);
        clean_base += prefix.chars().count();
        clean.push_str(&prefix);
        let word = &rest[body_start..body_end];
        tokens.push(PosToken {
            w: word.to_string(),
            cat,
            s: clean_base,
            e: clean_base + word.chars().count(),
        });
        clean_base += word.chars().count();
        clean.push_str(word);
        rest = &rest[body_end + "</span>".len()..];
    }
    clean.push_str(&strip_posw(rest));
    (tokens, clean)
}

/// Remove all posw span tags from a marked fragment (content kept).
fn strip_posw(s: &str) -> String {
    s.replace("<span class=\"posw cat-n\">", "")
        .replace("<span class=\"posw cat-v\">", "")
        .replace("<span class=\"posw cat-a\">", "")
        .replace("<span class=\"posw cat-d\">", "")
        .replace("<span class=\"posw cat-u\">", "")
        .replace("<span class=\"posw cat-r\">", "")
        .replace("<span class=\"posw cat-m\">", "")
        .replace("<span class=\"posw cat-q\">", "")
        .replace("<span class=\"posw cat-c\">", "")
        .replace("<span class=\"posw cat-p\">", "")
        .replace("<span class=\"posw cat-aux\">", "")
        .replace("<span class=\"posw cat-x\">", "")
        .replace("</span>", "")
}

/// Case-insensitive (ASCII) occurrence positions of `query` in `text`.
fn find_occurrences(text: &str, query: &str) -> Vec<(usize, usize)> {
    let needle: Vec<char> = query
        .chars()
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if needle.is_empty() || needle.len() > 64 {
        return Vec::new();
    }
    let hay: Vec<char> = text.chars().collect();
    if needle.len() > hay.len() {
        return Vec::new();
    }
    let lower: Vec<char> = hay.iter().map(|c| c.to_ascii_lowercase()).collect();
    let mut out = Vec::new();
    for i in 0..=(lower.len() - needle.len()) {
        if lower[i..i + needle.len()] == needle[..] {
            out.push((i, i + needle.len()));
            if out.len() >= 500 {
                break;
            }
        }
    }
    out
}

// ---- suggestion word lists (generated by scripts/gen_suggest_dicts.py) ----

thread_local! {
    static SUGGEST_ZH: RefCell<Option<std::rc::Rc<Vec<String>>>> = const { RefCell::new(None) };
    static SUGGEST_JA: RefCell<Option<std::rc::Rc<Vec<String>>>> = const { RefCell::new(None) };
}

// kick off the fetches once; lookups are no-ops until a list arrives
fn preload_suggest_dicts() {
    load_suggest_list(&format!("{BASE_PATH}/dict/suggest_zh.txt"), &SUGGEST_ZH);
    load_suggest_list(&format!("{BASE_PATH}/dict/suggest_ja.txt"), &SUGGEST_JA);
}

fn load_suggest_list(
    url: &str,
    slot: &'static std::thread::LocalKey<RefCell<Option<std::rc::Rc<Vec<String>>>>>,
) {
    let url = url.to_string();
    spawn_local(async move {
        let Ok(resp) = gloo_net::http::Request::get(&url).send().await else { return };
        let Ok(text) = resp.text().await else { return };
        let mut words: Vec<String> = text.lines().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        words.sort();
        words.dedup();
        slot.with(|c| *c.borrow_mut() = Some(std::rc::Rc::new(words)));
    });
}

fn prefix_lookup(
    slot: &'static std::thread::LocalKey<RefCell<Option<std::rc::Rc<Vec<String>>>>>,
    prefix: &str,
    limit: usize,
    out: &mut Vec<SuggestItem>,
    is_ja: bool,
) {
    slot.with(|c| {
        let c = c.borrow();
        let Some(words) = c.as_ref() else { return };
        let idx = words.partition_point(|w| w.as_str() < prefix);
        for w in words[idx..].iter().take(limit) {
            if !w.starts_with(prefix) {
                break;
            }
            out.push(SuggestItem { word: w.clone(), is_ja });
        }
    });
}

fn is_cjk_char(c: char) -> bool {
    matches!(c,
        '\u{4e00}'..='\u{9fff}'
        | '\u{3040}'..='\u{30ff}'
        | '\u{f900}'..='\u{faff}')
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Up to 8 completions for the fragment (4 Chinese + 4 Japanese).
fn lookup_suggestions(fragment: &str) -> Vec<SuggestItem> {
    let mut out = Vec::new();
    prefix_lookup(&SUGGEST_ZH, fragment, 4, &mut out, false);
    prefix_lookup(&SUGGEST_JA, fragment, 4, &mut out, true);
    out
}

/// Strip characters that are illegal in file names (Windows/macOS/Linux).
fn sanitize_filename(t: &str) -> String {    t.chars()
        .filter(|c| !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\n' | '\r'))
        .take(60)
        .collect::<String>()
        .trim()
        .to_string()
}

// ---- import conversion helpers ----
//
// Word/PDF → markdown conversion runs entirely in the browser: mammoth
// (docx→HTML), Turndown (HTML→markdown), pdf.js (PDF text). The libraries
// are served from /mammoth /turndown /pdfjs (staged like /vditor).

fn js_fn(obj: &JsValue, name: &str) -> Result<js_sys::Function, String> {
    js_sys::Reflect::get(obj, &JsValue::from_str(name))
        .map_err(|_| format!("JS 函数缺失: {name}"))?
        .dyn_into::<js_sys::Function>()
        .map_err(|_| format!("JS 函数缺失: {name}"))
}

/// Call an object's method that returns a JS Promise and await it.
async fn js_call_promise(this: &JsValue, name: &str, arg: Option<&JsValue>) -> Result<JsValue, String> {
    let f = js_fn(this, name)?;
    let p = match arg {
        Some(a) => f.call1(this, a),
        None => f.call0(this),
    }
    .map_err(|_| format!("{name} 调用失败"))?;
    JsFuture::from(
        p.dyn_into::<js_sys::Promise>()
            .map_err(|_| format!("{name} 未返回 Promise"))?,
    )
    .await
    .map_err(|_| format!("{name} 失败"))
}

/// .docx → markdown: mammoth converts to HTML, Turndown to markdown.
/// Embedded images come back as data URIs — upload them through the normal
/// /api/upload flow so the note keeps /i/ links like pasted images.
async fn docx_to_markdown(file: &web_sys::File) -> Result<String, String> {
    let buf = JsFuture::from(file.array_buffer())
        .await
        .map_err(|_| "读取文件失败".to_string())?;
    let global = js_sys::global();
    let mammoth = js_sys::Reflect::get(&global, &"mammoth".into())
        .map_err(|_| "docx 转换库未加载".to_string())?;
    let opts = js_sys::Object::new();
    js_sys::Reflect::set(&opts, &"arrayBuffer".into(), &buf).unwrap();
    let res = js_call_promise(&mammoth, "convertToHtml", Some(&opts)).await?;
    let html = js_sys::Reflect::get(&res, &"value".into())
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();

    let td = js_fn(&global, "TurndownService").map_err(|_| "Markdown 转换库未加载".to_string())?;
    let opts = js_sys::Object::new();
    for (k, v) in [
        ("headingStyle", "atx"),
        ("hr", "---"),
        ("bulletListMarker", "-"),
        ("codeBlockStyle", "fenced"),
        ("emDelimiter", "*"),
    ] {
        js_sys::Reflect::set(&opts, &k.into(), &JsValue::from_str(v)).unwrap();
    }
    let args = js_sys::Array::of1(&opts.into());
    let svc = js_sys::Reflect::construct(&td, &args)
        .map_err(|_| "TurndownService 初始化失败".to_string())?;
    let svc = JsValue::from(svc);
    let md = js_fn(&svc, "turndown")?
        .call1(&svc, &JsValue::from_str(&html))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();

    Ok(upload_data_uri_images(md).await)
}

/// Upload every `![](data:image/...)` in converted markdown via /api/upload
/// and replace it with an /i/ link (absolute for display; storage_md keeps
/// the relative form in the database). Data URIs whose upload fails are kept.
async fn upload_data_uri_images(md: String) -> String {
    let mut out = String::with_capacity(md.len());
    let mut rest = md.as_str();
    while let Some(start) = rest.find("](data:image/") {
        let uri_start = start + 2;
        let Some(uri_end_rel) = rest[uri_start..].find(')') else { break };
        let uri_end = uri_start + uri_end_rel;
        let uri = &rest[uri_start..uri_end];
        out.push_str(&rest[..uri_start]);
        match upload_data_uri(uri).await {
            Some(url) => out.push_str(&url),
            None => out.push_str(uri),
        }
        rest = &rest[uri_end..];
    }
    out.push_str(rest);
    out
}

async fn upload_data_uri(uri: &str) -> Option<String> {
    use base64::Engine;
    let (meta, b64) = uri.split_once(',')?;
    let mime = meta.strip_prefix("data:")?.split(';').next()?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .ok()?;
    let u8arr = js_sys::Uint8Array::from(&bytes[..]);
    let parts = js_sys::Array::of1(&u8arr.into());
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type(mime);
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts.into(), &opts).ok()?;
    let form = web_sys::FormData::new().ok()?;
    form.append_with_blob("file[]", &blob).ok()?;
    let req = gloo_net::http::Request::post(&api_url("/api/upload"))
        .credentials(RequestCredentials::Include)
        .body(form)
        .ok()?;
    let resp = req.send().await.ok()?;
    if !resp.ok() {
        return None;
    }
    let ur = resp.json::<UploadResponse>().await.ok()?;
    ur.data.succ_map.values().next().map(|p| api_url(p))
}

/// .pdf → text with pdf.js. PDFs carry no reliable markdown structure
/// (headings/tables are visual only), so the result is plain text with
/// source line breaks; complex layouts (multi-column, tables) may interleave.
async fn pdf_to_text(file: &web_sys::File) -> Result<String, String> {
    let buf = JsFuture::from(file.array_buffer())
        .await
        .map_err(|_| "读取文件失败".to_string())?;
    let global = js_sys::global();
    let pdfjs = js_sys::Reflect::get(&global, &"pdfjsLib".into())
        .map_err(|_| "PDF 转换库未加载".to_string())?;
    if let Ok(gw) = js_sys::Reflect::get(&pdfjs, &"GlobalWorkerOptions".into()) {
        let _ = js_sys::Reflect::set(
            &gw,
            &"workerSrc".into(),
            &JsValue::from_str(&format!("{BASE_PATH}/pdfjs/pdf.worker.min.js")),
        );
    }
    let opts = js_sys::Object::new();
    let data = js_sys::Uint8Array::new(&buf);
    js_sys::Reflect::set(&opts, &"data".into(), &data).unwrap();
    let task = js_fn(&pdfjs, "getDocument")?
        .call1(&pdfjs, &opts)
        .map_err(|_| "PDF 打开失败".to_string())?;
    let doc = JsFuture::from(
        task.dyn_into::<js_sys::Promise>()
            .map_err(|_| "PDF 打开失败".to_string())?,
    )
    .await
    .map_err(|_| "PDF 打开失败（可能已加密或损坏）".to_string())?;
    let num_pages = js_sys::Reflect::get(&doc, &"numPages".into())
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as i32;

    let mut out = String::new();
    for p in 1..=num_pages {
        let page = js_call_promise(&doc, "getPage", Some(&JsValue::from(p))).await?;
        let tc = js_call_promise(&page, "getTextContent", None).await?;
        let items = js_sys::Reflect::get(&tc, &"items".into())
            .unwrap_or_else(|_| js_sys::Array::new().into())
            .dyn_into::<js_sys::Array>()
            .unwrap_or_default();
        let mut line = String::new();
        for item in items.iter() {
            let s = js_sys::Reflect::get(&item, &"str".into())
                .ok()
                .and_then(|v| v.as_string())
                .unwrap_or_default();
            // CJK glyphs come as per-character items without spaces; western
            // words usually carry their own trailing space — concatenate.
            line.push_str(&s);
            let has_eol = js_sys::Reflect::get(&item, &"hasEOL".into())
                .ok()
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if has_eol {
                out.push_str(line.trim_end());
                out.push('\n');
                line.clear();
            }
        }
        out.push_str(line.trim_end());
        out.push_str("\n\n");
    }
    Ok(out.trim_end().to_string())
}

/// Replace (append=false) or append (append=true) the editor content with the
/// text of the selected files, then save. Drives the two hidden file inputs.
fn read_files_into_editor(ctx: Ctx, append: bool, input: HtmlInputElement) {
    if ctx.state.with_value(|st| st.editor.is_none()) {
        ctx.set_status("编辑器尚未加载完成", "error");
        return;
    }
    let Some(files) = input.files() else { return };
    if files.length() == 0 {
        return;
    }
    // replacing non-empty content is destructive — confirm first
    if !append {
        let current = ctx
            .state
            .with_value(|st| st.editor.as_ref().map(|e| e.editor.get_value()))
            .unwrap_or_default();
        if !current.trim().is_empty()
            && !window()
                .confirm_with_message("当前编辑已有内容，确定要替换为文件内容吗？")
                .unwrap_or(false)
        {
            input.set_value("");
            return;
        }
    }
    let count = files.length();
    // Snapshot the File handles before clearing the input: the FileList is a
    // live view of the input, so set_value("") below would empty it while the
    // async read below is still pending.
    let mut picked = Vec::new();
    for i in 0..count {
        if let Some(f) = files.get(i) {
            picked.push(f);
        }
    }
    input.set_value("");
    if picked.is_empty() {
        return;
    }
    let c = ctx;
    spawn_local(async move {
        let mut content = if append {
            c.state
                .with_value(|st| st.editor.as_ref().map(|e| e.editor.get_value()))
                .unwrap_or_default()
        } else {
            String::new()
        };
        let mut names = Vec::new();
        let mut converted = 0usize;
        for file in picked {
            let name = file.name();
            names.push(name.clone());
            let lower = name.to_lowercase();
            let text = if lower.ends_with(".docx") {
                match docx_to_markdown(&file).await {
                    Ok(t) => {
                        converted += 1;
                        t
                    }
                    Err(e) => {
                        c.set_status(&format!("{name}：{e}"), "error");
                        continue;
                    }
                }
            } else if lower.ends_with(".pdf") {
                match pdf_to_text(&file).await {
                    Ok(t) => {
                        converted += 1;
                        t
                    }
                    Err(e) => {
                        c.set_status(&format!("{name}：{e}"), "error");
                        continue;
                    }
                }
            } else {
                match JsFuture::from(file.text()).await {
                    Ok(t) => t.as_string().unwrap_or_default(),
                    Err(_) => {
                        c.set_status(&format!("{name} 读取失败"), "error");
                        continue;
                    }
                }
            };
            if append {
                if !content.is_empty() {
                    content.push_str("\n\n");
                }
                content.push_str(&text);
            } else {
                content = text;
            }
        }
        c.state.with_value(|st| {
            if let Some(ed) = st.editor.as_ref() {
                ed.editor.set_value(&content);
            }
        });
        c.mark_dirty();
        c.set_status(
            &if converted > 0 {
                format!("已载入 {} 个文件（转换 {} 个），正在保存…", names.len(), converted)
            } else {
                format!("已载入 {} 个文件，正在保存…", names.len())
            },
            "saving",
        );
        c.save();
    });
}

// current user lives in a thread-local so LoginView can set it too
thread_local! {
    static USER: RwSignal<Option<String>> = RwSignal::new(None);
}

// ---- app ----

fn main() {
    console_error_panic_hook::set_once();
    // Create the USER signal outside any component owner (it lives in the
    // global arena), so it survives LoginView being unmounted after login.
    USER.with(|_| ());
    leptos::mount::mount_to_body(App);
}

#[component]
fn App() -> impl IntoView {
    // /:id is the note id; "/" redirects to a fresh one
    let id = init_note_id();

    let font_size = RwSignal::new(DEFAULT_FONT_SIZE);
    let color = RwSignal::new(String::new());
    let title = RwSignal::new(String::new());
    let tags_text = RwSignal::new(String::new());
    let parsed_tags = Memo::new(move |_| {
        tags_text
            .get()
            .split([',', '，'])
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
    });
    let status_text = RwSignal::new(String::new());
    let status_class = RwSignal::new(String::new());
    let author = RwSignal::new(String::new());
    let remote_changed = RwSignal::new(false);
    let presence_others = RwSignal::new(Vec::<String>::new());
    let show_login = RwSignal::new(false);
    let picker_open = RwSignal::new(false);
    let picker_query = RwSignal::new(String::new());
    let all_notes = RwSignal::new(Vec::<NoteMeta>::new());
    let is_admin = RwSignal::new(false);
    // default unlocked so the UI doesn't flash the lock overlay before the
    // first /api/edit-status response (matches the server default)
    let edit_locked = RwSignal::new(false);
    let can_edit = RwSignal::new(true);
    let edit_request = RwSignal::new(String::new());
    let edit_requests = RwSignal::new(Vec::<EditRequestItem>::new());
    let req_panel_open = RwSignal::new(false);
    let req_message = RwSignal::new(String::new());
    let suggest_items = RwSignal::new(Vec::<SuggestItem>::new());
    let suggest_active = RwSignal::new(0usize);
    let suggest_pos = RwSignal::new((0i32, 0i32));
    let analyze_open = RwSignal::new(false);
    let analyze_busy = RwSignal::new(false);
    let analyze_issues = RwSignal::new(Vec::<Issue>::new());
    let analyze_applied = RwSignal::new(Vec::<usize>::new());
    let pos_on = RwSignal::new(false);
    let pos_tagged_chars = RwSignal::new(0usize);
    let mark_exists = RwSignal::new(false);
    let find_open = RwSignal::new(false);
    let find_query = RwSignal::new(String::new());
    let find_count = RwSignal::new(0usize);
    let find_all = RwSignal::new(0usize);
    let mark_mode = RwSignal::new(false);
    let lock_dismissed = RwSignal::new(false);
    let lock_apply_open = RwSignal::new(false);

    let ctx = Ctx {
        state: StoredValue::new(SendWrapper::new(AppState {
            id: id.clone(),
            editor: None,
            intervals: Vec::new(),
            poll_interval: None,
            retry: None,
            search_debounce: None,
            listeners: Vec::new(),
            dirty: false,
            loaded: false,
            applying_remote: false,
            poll_updated_at: String::new(),
            suggest_anchor_node: None,
            suggest_anchor_start: 0,
            suggest_anchor_end: 0,
            suggest_accepted_word: String::new(),
            pos_debounce: None,
            pos_seq: 0,
            pos_scroll_listener: None,
            find_debounce: None,
            pos_tokens_cache: Vec::new(),
            find_ranges_cache: Vec::new(),
            suppress_input_until_ms: 0.0,
            last_applied_content: String::new(),
            baseline_font: 0,
            baseline_color: String::new(),
            baseline_title: String::new(),
            baseline_tags: String::new(),
        })),
        font_size,
        color,
        title,
        tags_text,
        parsed_tags,
        status_text,
        status_class,
        author,
        remote_changed,
        presence_others,
        show_login,
        picker_open,
        picker_query,
        all_notes,
        is_admin,
        edit_locked,
        can_edit,
        edit_request,
        edit_requests,
        req_panel_open,
        req_message,
        suggest_items,
        suggest_active,
        suggest_pos,
        analyze_open,
        analyze_busy,
        analyze_issues,
        analyze_applied,
        pos_on,
        pos_tagged_chars,
        mark_exists,
        find_open,
        find_query,
        find_count,
        find_all,
        mark_mode,
        lock_dismissed,
        lock_apply_open,
    };

    // editor zoom changed → text re-layouts; re-render overlay marks so they
    // track the resized text (cached tokens, no server round-trip)
    {
        let ctx = ctx;
        Effect::new(move |_| {
            font_size.get();
            let c = ctx;
            Timeout::new(250, move || c.refresh_overlay_marks()).forget();
        });
    }

    // mark edits as pending; the 3-minute autosave timer (and the save button /
    // Ctrl+S) flush them — same watch as the Vue app. Locked-out users can't
    // edit anything, so their changes must never become dirty (that would
    // trigger futile saves / sendBeacon on unload).
    Effect::new(move |_| {
        // Leptos runs effects in a microtask AFTER the signal writes, so the
        // plain "loaded && !applying_remote" check cannot tell load-time
        // initialization from a real user edit — compare against the values
        // captured when the note was loaded/applied instead.
        let cur_font = font_size.get();
        let cur_color = color.get();
        let cur_title = title.get();
        let cur_tags = tags_text.get();
        if !ctx.can_edit.get() || ctx.mark_mode.get() {
            return;
        }
        let changed = ctx.state.with_value(|st| {
            if !st.loaded || st.applying_remote {
                return false;
            }
            cur_font != st.baseline_font
                || cur_color != st.baseline_color
                || cur_title != st.baseline_title
                || cur_tags != st.baseline_tags
        });
        if changed {
            ctx.state.update_value(|st| {
                st.baseline_font = cur_font;
                st.baseline_color = cur_color.clone();
                st.baseline_title = cur_title.clone();
                st.baseline_tags = cur_tags.clone();
            });
            ctx.mark_dirty();
        }
    });

    // Ctrl/Cmd +/- scales the editor, Ctrl/Cmd 0 resets it, Ctrl/Cmd S saves
    {
        let ctx = ctx;
        ctx.state.update_value(|st| {
            st.listeners.push(EventListener::new(
                &window(),
                "keydown",
                move |e| {
                    let Some(k) = e.dyn_ref::<web_sys::KeyboardEvent>() else {
                        return;
                    };
                    if !(k.ctrl_key() || k.meta_key()) {
                        return;
                    }
                    match k.key().as_str() {
                        "s" | "S" => {
                            e.prevent_default();
                            ctx.save_now();
                        }
                        "=" | "+" => {
                            e.prevent_default();
                            ctx.bump_font(FONT_STEP);
                        }
                        "-" => {
                            e.prevent_default();
                            ctx.bump_font(-FONT_STEP);
                        }
                        "0" => {
                            e.prevent_default();
                            ctx.font_size.set(DEFAULT_FONT_SIZE);
                        }
                        _ => {}
                    }
                },
            ));
        });
    }

    // close the notes picker on clicks outside it
    {
        let ctx = ctx;
        ctx.state.update_value(|st| {
            st.listeners.push(EventListener::new(
                &document(),
                "click",
                move |e| {
                    let target = e.target().and_then(|t| t.dyn_into::<Element>().ok());
                    let inside = target
                        .and_then(|el| el.closest(".picker, .req-panel").ok())
                        .flatten()
                        .is_some();
                    if !inside && ctx.picker_open.get() {
                        ctx.picker_open.set(false);
                    }
                    if !inside && ctx.req_panel_open.get() {
                        ctx.req_panel_open.set(false);
                    }
                },
            ));
        });
    }

    // keep the suggestion panel in sync with cursor movements (clicks, arrows)
    {
        let ctx = ctx;
        ctx.state.update_value(|st| {
            st.listeners.push(EventListener::new(
                &document(),
                "selectionchange",
                move |_| ctx.update_suggestions(),
            ));
        });
    }

    // suggestion panel keyboard navigation — captured at document level so it
    // runs before Vditor's own keydown handling (which ignores
    // defaultPrevented), letting us swallow Enter/Tab/Arrows while the panel
    // is open
    {
        let ctx = ctx;
        ctx.state.update_value(|st| {
            st.listeners.push(EventListener::new_with_options(
                &document(),
                "keydown",
                gloo_events::EventListenerOptions::run_in_capture_phase(),
                move |e| {
                    let Some(k) = e.dyn_ref::<web_sys::KeyboardEvent>() else {
                        return;
                    };
                    if !matches!(
                        k.key().as_str(),
                        "ArrowDown" | "ArrowUp" | "Enter" | "Tab" | "Escape"
                    ) {
                        return;
                    }
                    if ctx.suggest_items.with(|v| v.is_empty()) {
                        return;
                    }
                    k.prevent_default();
                    k.stop_propagation();
                    ctx.on_suggest_keydown(k);
                },
            ));
        });
    }

    // dictionary suggestion word lists: fetch in the background
    preload_suggest_dicts();

    // keep the editor theme in sync with the OS appearance
    {
        let ctx = ctx;
        if let Some(mql) = window()
            .match_media("(prefers-color-scheme: dark)")
            .ok()
            .flatten()
        {
            let cb = EventListener::new(&mql, "change", move |e| {
                let matches = e
                    .dyn_ref::<MediaQueryListEvent>()
                    .map(|m| m.matches())
                    .unwrap_or(false);
                ctx.state.with_value(|st| {
                    if let Some(ed) = st.editor.as_ref() {
                        ed.editor.set_theme(if matches { "dark" } else { "classic" });
                    }
                });
            });
            ctx.state.update_value(|st| st.listeners.push(cb));
        }
    }

    // whoami on mount → load the note, or show the login form
    {
        let ctx = ctx;
        spawn_local(async move {
            match gloo_net::http::Request::get(&api_url("/api/whoami"))
                .credentials(RequestCredentials::Include)
                .send()
                .await
            {
                Ok(resp) if resp.ok() => match resp.json::<WhoamiResponse>().await {
                    Ok(d) => {
                        USER.with(|u| u.set(Some(d.user.username)));
                        ctx.refresh_edit_status();
                        ctx.load_note();
                    }
                    Err(_) => ctx.show_login.set(true),
                },
                _ => ctx.show_login.set(true),
            }
        });
    }

    let editor_style = move || {
        let mut s = format!(
            "--note-zoom: {:.3};",
            font_size.get() as f64 / DEFAULT_FONT_SIZE as f64
        );
        let c = color.get();
        if !c.is_empty() {
            s.push_str(&format!("--note-color: {c};"));
        }
        s
    };

    view! {
        <div id="app">
            {move || {
                if show_login.get() {
                    view! { <LoginView ctx show_login/> }.into_any()
                } else {
                    view! { <MainView ctx editor_style/> }.into_any()
                }
            }}
        </div>
    }
}

fn init_note_id() -> String {
    let loc = window().location();
    let raw = loc.pathname().unwrap_or_default();
    // strip the base path ("/urlnote" for GitHub project pages, "" on the
    // Cloudflare deployment) before reading the note id from the first
    // remaining segment
    let path = match raw.strip_prefix(BASE_PATH) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => rest,
        _ => &raw,
    };
    let id = path
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or("")
        .to_string();
    let id = if valid_note_id(&id) {
        id
    } else {
        let id = new_id();
        let _ = window()
            .history()
            .expect("history")
            .replace_state_with_url(&JsValue::NULL, "", Some(&format!("{BASE_PATH}/{id}")));
        id
    };
    document().set_title(&format!("{id} · urlnote"));
    id
}

#[component]
fn LoginView(ctx: Ctx, show_login: RwSignal<bool>) -> impl IntoView {
    let user_ref = NodeRef::<Input>::new();
    let pass_ref = NodeRef::<Input>::new();
    let login_error = RwSignal::new(String::new());

    let on_submit = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        let u = user_ref.get().map(|e| e.value()).unwrap_or_default();
        let p = pass_ref.get().map(|e| e.value()).unwrap_or_default();
        login_error.set(String::new());
        let ctx = ctx;
        spawn_local(async move {
            let req = gloo_net::http::Request::post(&api_url("/api/login"))
                .credentials(RequestCredentials::Include)
                .json(&LoginRequest {
                username: u,
                password: p,
            });
            match req {
                Ok(req) => match req.send().await {
                    Ok(resp) if resp.ok() => {
                        let name = resp
                            .json::<LoginResponse>()
                            .await
                            .map(|d| d.user.username)
                            .unwrap_or_default();
                        USER.with(|s| s.set(Some(name)));
                        show_login.set(false);
                        ctx.refresh_edit_status();
                        ctx.load_note();
                    }
                    Ok(resp) => {
                        let msg = resp
                            .json::<ErrorResponse>()
                            .await
                            .map(|d| d.error)
                            .unwrap_or_else(|_| "登录失败".to_string());
                        login_error.set(msg);
                    }
                    Err(_) => login_error.set("网络错误，请重试".to_string()),
                },
                Err(_) => login_error.set("网络错误，请重试".to_string()),
            }
        });
    };

    view! {
        <div class="login-wrap">
            <form class="login-card" on:submit=on_submit>
                <strong>"urlnote"</strong>
                <p class="login-hint">"登录后开始编辑便签"</p>
                <input
                    class="login-input"
                    placeholder="用户名"
                    autocomplete="username"
                    required
                    node_ref=user_ref
                />
                <input
                    class="login-input"
                    type="password"
                    placeholder="密码"
                    autocomplete="current-password"
                    required
                    node_ref=pass_ref
                />
                <button type="submit" class="login-btn">
                    "登 录"
                </button>
                <span class="login-error">{move || login_error.get()}</span>
            </form>
        </div>
    }
}

#[component]
fn MainView(ctx: Ctx, editor_style: impl Fn() -> String + Copy + Send + 'static) -> impl IntoView {
    let Ctx {
        font_size,
        color,
        title,
        tags_text,
        parsed_tags,
        status_text,
        status_class,
        author,
        remote_changed,
        presence_others,
        picker_open,
        picker_query,
        all_notes,
        is_admin,
        edit_locked,
        can_edit,
        edit_request,
        edit_requests,
        req_panel_open,
        req_message,
        suggest_items,
        suggest_active,
        suggest_pos,
        analyze_open,
        analyze_busy,
        analyze_issues,
        analyze_applied,
        pos_on,
        pos_tagged_chars,
        mark_exists,
        find_open,
        find_query,
        find_count,
        find_all,
        mark_mode,
        lock_dismissed,
        lock_apply_open,
        ..
    } = ctx;

    let pending_count = Memo::new(move |_| {
        edit_requests
            .get()
            .iter()
            .filter(|r| r.status == "pending")
            .count()
    });

    // hidden file inputs behind the 上传 / 追加 buttons
    let upload_ref = NodeRef::<Input>::new();
    let append_ref = NodeRef::<Input>::new();

    let toggle_req_panel = move |e: web_sys::MouseEvent| {
        e.stop_propagation();
        let open = !req_panel_open.get();
        req_panel_open.set(open);
        if open {
            ctx.load_edit_requests();
        }
    };

    let toggle_picker = move |e: web_sys::MouseEvent| {
        e.stop_propagation();
        let open = !picker_open.get();
        picker_open.set(open);
        if open {
            picker_query.set(String::new());
            let ctx = ctx;
            spawn_local(async move {
                match gloo_net::http::Request::get(&api_url("/api/notes"))
                    .credentials(RequestCredentials::Include)
                    .send()
                    .await
                {
                    Ok(resp) => {
                        if let Ok(d) = resp.json::<NoteList>().await {
                            ctx.all_notes.set(d.notes);
                        }
                    }
                    Err(_) => ctx.set_status("便签列表加载失败", "error"),
                }
            });
        }
    };

    let id = ctx.id();
    let do_logout = move |_| {
        spawn_local(async move {
            let _ = gloo_net::http::Request::post(&api_url("/api/logout"))
                .credentials(RequestCredentials::Include)
                .send()
                .await;
            let _ = window().location().reload();
        });
    };

    view! {
        <header>
            <strong>"urlnote"</strong>
            <a href=format!("{BASE_PATH}/{id}") title="当前便签地址">
                {format!("{BASE_PATH}/{id}")}
            </a>
            <a href=format!("{BASE_PATH}/{}", new_id()) title="新建一个便签">
                "＋ 新建"
            </a>
            <input
                class="title-input"
                placeholder="未命名便签"
                maxlength="200"
                title="便签标题"
                disabled=move || !can_edit.get() || mark_mode.get()
                prop:value=move || title.get()
                on:input=move |e| title.set(evt_input_value(&e))
            />
            <Show when=move || !author.get().is_empty()>
                <span class="author-badge" title="最后编辑者">
                    "✍️ "
                    {move || author.get()}
                </span>
            </Show>
            <Show when=move || !presence_others.get().is_empty()>
                <span
                    class="presence-badge"
                    title=move || format!("正在查看本文：{}", presence_others.get().join("、"))
                >
                    "👀 "
                    {move || presence_others.get().join("、")}
                    " 正在查看"
                </span>
            </Show>
            <span class="tags-wrap" title="标签（逗号分隔，用于分类）">
                {move || {
                    parsed_tags
                        .get()
                        .into_iter()
                        .map(|t| view! { <span class="chip">{t}</span> })
                        .collect_view()
                }}
                <input
                    class="tags-input"
                    placeholder="标签，逗号分隔"
                    disabled=move || !can_edit.get() || mark_mode.get()
                    prop:value=move || tags_text.get()
                    on:input=move |e| tags_text.set(evt_input_value(&e))
                />
            </span>
            <span class="picker">
                <button type="button" class="tool" title="全部便签" on:click=toggle_picker>
                    "☰ 便签"
                </button>
                <Show when=move || picker_open.get()>
                    <div
                        class="picker-panel"
                        on:click=|e: web_sys::MouseEvent| e.stop_propagation()
                    >
                        <input
                            class="picker-search"
                            placeholder="搜索标题、标签或 ID…"
                            prop:value=move || picker_query.get()
                            on:input=move |e| ctx.on_picker_query(evt_input_value(&e))
                        />
                        <ul class="picker-list">
                            <For
                                each=move || all_notes.get()
                                key=|n| n.id.clone()
                                children=move |n: NoteMeta| {
                                    let nid = n.id.clone();
                                    let has_excerpt = n.title.is_empty() && !n.excerpt.is_empty();
                                    let excerpt = n.excerpt.clone();
                                    let tags = n.tags.clone();
                                    let author_name = n.author.clone();
                                    let has_author = !author_name.is_empty();
                                    let updated = n.updated_at.clone();
                                    view! {
                                        <li on:click=move |_| {
                                            let _ = window()
                                                .location()
                                                .set_href(&format!("{BASE_PATH}/{nid}"));
                                        }>
                                            <span class="row-main">
                                                <span class="row-title">{n.title.clone()}</span>
                                                <span class="row-id">{format!("{BASE_PATH}/{}", n.id)}</span>
                                            </span>
                                            {has_excerpt.then(|| {
                                                view! { <span class="row-excerpt">{excerpt}</span> }
                                            })}
                                            <span class="row-meta">
                                                {tags
                                                    .into_iter()
                                                    .map(|t| view! { <span class="chip">{t}</span> })
                                                    .collect_view()}
                                                <Show when=move || has_author>
                                                    <span class="row-author">
                                                        "✍️ "
                                                        {author_name.clone()}
                                                    </span>
                                                </Show>
                                                <span class="row-date">
                                                    {updated.get(..10).unwrap_or("").to_string()}
                                                </span>
                                            </span>
                                        </li>
                                    }
                                }
                            />
                            <Show when=move || all_notes.get().is_empty()>
                                <li class="picker-empty">"没有匹配的便签"</li>
                            </Show>
                        </ul>
                    </div>
                </Show>
            </span>
            <Show when=move || is_admin.get()>
                <button
                    type="button"
                    class="tool"
                    title="锁定后仅你和已批准的用户可以编辑"
                    on:click=move |_| ctx.toggle_edit_lock()
                >
                    {move || if edit_locked.get() { "🔓 解锁编辑" } else { "🔒 锁定编辑" }}
                </button>
                <Show when=move || edit_locked.get()>
                    <span class="req-panel">
                        <button
                            type="button"
                            class="tool"
                            title="编辑权限申请"
                            on:click=toggle_req_panel
                        >
                            {move || format!("📝 申请 ({})", pending_count.get())}
                        </button>
                        <Show when=move || req_panel_open.get()>
                            <div
                                class="req-list-panel"
                                on:click=|e: web_sys::MouseEvent| e.stop_propagation()
                            >
                                <ul class="req-list">
                                    <For
                                        each=move || edit_requests.get()
                                        key=|r| r.username.clone()
                                        children=move |r: EditRequestItem| {
                                            let name = r.username.clone();
                                            let approved = r.status == "approved";
                                            let msg = r.message.clone();
                                            let has_msg = !msg.is_empty();
                                            let when = r.created_at.clone();
                                            // built inline (not inside <Show>) so the
                                            // move-capturing click handlers only consume
                                            // per-call locals — a Fn component-children
                                            // wrapper can't move its own captures
                                            let actions = if approved {
                                                let n = name.clone();
                                                view! {
                                                    <button
                                                        type="button"
                                                        class="tool"
                                                        on:click=move |_| {
                                                            ctx.handle_edit_request(&n.clone(), "revoke")
                                                        }
                                                    >
                                                        "撤销"
                                                    </button>
                                                }
                                                .into_any()
                                            } else {
                                                let na = name.clone();
                                                let nr = name.clone();
                                                view! {
                                                    <button
                                                        type="button"
                                                        class="tool"
                                                        on:click=move |_| {
                                                            ctx.handle_edit_request(&na.clone(), "approve")
                                                        }
                                                    >
                                                        "同意"
                                                    </button>
                                                    <button
                                                        type="button"
                                                        class="tool"
                                                        on:click=move |_| {
                                                            ctx.handle_edit_request(&nr.clone(), "reject")
                                                        }
                                                    >
                                                        "拒绝"
                                                    </button>
                                                }
                                                .into_any()
                                            };
                                            view! {
                                                <li>
                                                    <span class="req-row">
                                                        <strong>{name}</strong>
                                                        <span class=move || {
                                                            if approved { "req-status approved" } else { "req-status" }
                                                        }>
                                                            {if approved { "已批准" } else { "待审批" }}
                                                        </span>
                                                        <Show when=move || has_msg>
                                                            <span class="req-msg">{msg.clone()}</span>
                                                        </Show>
                                                        <span class="req-date">
                                                            {when.get(..16).unwrap_or("").to_string()}
                                                        </span>
                                                        <span class="req-actions">{actions}</span>
                                                    </span>
                                                </li>
                                            }
                                        }
                                    />
                                    <Show when=move || edit_requests.get().is_empty()>
                                        <li class="req-empty">"暂无申请"</li>
                                    </Show>
                                </ul>
                            </div>
                        </Show>
                    </span>
                </Show>
            </Show>
            <span class="tools">
                <button
                    type="button"
                    class="tool"
                    title="把编辑的内容导出为 Markdown 文件"
                    on:click=move |_| ctx.export_md()
                >
                    "⬇ 导出"
                </button>
                <button
                    type="button"
                    class="tool"
                    title="分析中文/日文的词法问题并给出更正建议"
                    on:click=move |_| ctx.run_analysis()
                >
                    "🔍 分析"
                </button>
                <button
                    type="button"
                    class=move || if pos_on.get() { "tool pos-on" } else { "tool" }
                    title="按词性给中文/日文词语着色标注(名词/动词/形容词/副词…)"
                    on:click=move |_| ctx.toggle_pos()
                >
                    "🎨 词性"
                </button>
                <button
                    type="button"
                    class=move || if find_open.get() { "tool pos-on" } else { "tool" }
                    title="在本文便签中查找并高亮(全站匹配数来自搜索接口)"
                    on:click=move |_| ctx.toggle_find()
                >
                    "🔎 查找"
                </button>
                <Show when=move || find_open.get()>
                    <input
                        class="find-input"
                        placeholder="查找…"
                        prop:value=move || find_query.get()
                        on:input=move |e| ctx.on_find_query(evt_input_value(&e))
                        on:keydown=move |e: web_sys::KeyboardEvent| {
                            if e.key() == "Escape" {
                                ctx.toggle_find();
                            }
                        }
                    />
                    <Show when=move || !find_query.get().trim().is_empty()>
                        <span class="find-meta">
                            {move || format!("{} 处", find_count.get())}
                        </span>
                        <Show when=move || find_all.get() != 0>
                            <span class="find-meta all">
                                {move || format!("全站 {} 条", find_all.get())}
                            </span>
                        </Show>
                    </Show>
                </Show>
                <Show when=move || can_edit.get()>
                    <button
                        type="button"
                        class="tool"
                        title="导入文件（Markdown / Word / PDF），替换当前全文"
                        on:click=move |_| {
                            let r = upload_ref;
                            if let Some(i) = r.get() {
                                i.unchecked_ref::<HtmlElement>().click();
                            }
                        }
                    >
                        "⬆ 上传"
                    </button>
                    <button
                        type="button"
                        class="tool"
                        title="选择一个或多个文件（Markdown / Word / PDF），依次追加到文末"
                        on:click=move |_| {
                            let r = append_ref;
                            if let Some(i) = r.get() {
                                i.unchecked_ref::<HtmlElement>().click();
                            }
                        }
                    >
                        "➕ 追加"
                    </button>
                </Show>
                <input
                    type="file"
                    class="file-hidden"
                    accept=".md,.markdown,.txt,.docx,.pdf,text/markdown,text/plain,application/vnd.openxmlformats-officedocument.wordprocessingml.document,application/pdf"
                    node_ref=upload_ref
                    on:change=move |_| {
                        let r = upload_ref;
                        if let Some(i) = r.get() {
                            read_files_into_editor(ctx, false, i);
                        }
                    }
                />
                <input
                    type="file"
                    class="file-hidden"
                    accept=".md,.markdown,.txt,.docx,.pdf,text/markdown,text/plain,application/vnd.openxmlformats-officedocument.wordprocessingml.document,application/pdf"
                    multiple=true
                    node_ref=append_ref
                    on:change=move |_| {
                        let r = append_ref;
                        if let Some(i) = r.get() {
                            read_files_into_editor(ctx, true, i);
                        }
                    }
                />
                <button
                    type="button"
                    class="tool"
                    title="缩小显示 (Ctrl+-)"
                    on:click=move |_| ctx.bump_font(-FONT_STEP)
                >
                    "A−"
                </button>
                <button
                    type="button"
                    class="tool"
                    title="放大显示 (Ctrl+=)"
                    on:click=move |_| ctx.bump_font(FONT_STEP)
                >
                    "A+"
                </button>
                <Show when=move || font_size.get() != DEFAULT_FONT_SIZE>
                    <button
                        type="button"
                        class="tool"
                        title="恢复默认大小 (Ctrl+0)"
                        on:click=move |_| font_size.set(DEFAULT_FONT_SIZE)
                    >
                        "A"
                    </button>
                </Show>
                <input
                    type="color"
                    class="color"
                    title="文字颜色"
                    prop:value=move || {
                        let c = color.get();
                        if c.is_empty() { "#000000".to_string() } else { c }
                    }
                    on:input=move |e| color.set(evt_input_value(&e))
                />
                <Show when=move || !color.get().is_empty()>
                    <button
                        type="button"
                        class="tool"
                        title="恢复默认颜色"
                        on:click=move |_| color.set(String::new())
                    >
                        "默认"
                    </button>
                </Show>
            </span>
            <span class="user-wrap" title="当前用户">
                "👤 "
                {move || USER.with(|u| u.get()).unwrap_or_default()}
                <button type="button" class="tool" on:click=do_logout>
                    "退出"
                </button>
            </span>
            <button
                type="button"
                class="tool save-btn"
                title="立即保存 (Ctrl+S)"
                on:click=move |_| ctx.save_now()
            >
                "💾 保存"
            </button>
            <span id="status" class=move || status_class.get()>
                {move || status_text.get()}
            </span>
        </header>
        <Show when=move || mark_mode.get()>
            <div class="mark-banner">
                "📝 正在编辑标记文档 — 保存只写入 notes_mark,原便签不受影响"
                <button type="button" on:click=move |_| ctx.exit_mark_mode()>
                    "返回原文"
                </button>
            </div>
        </Show>
        <Show when=move || remote_changed.get()>
            <div class="sync-banner">
                "⚠️ 此便签已被其他人更新"
                <button type="button" on:click=move |_| ctx.load_latest()>
                    "载入最新内容"
                </button>
            </div>
        </Show>
        <div class="editor-wrap">
            <div class="editor" style=move || editor_style()></div>
            <div class="pos-overlay">
                <div class="pos-layer"></div>
            </div>
            <Show when=move || pos_on.get()>
                <div class="pos-legend">
                    <Show
                        when=move || mark_exists.get()
                        fallback=move || {
                            view! {
                                <button
                                    type="button"
                                    class="tool pos-doc-btn"
                                    title="生成含词性标注的 Markdown 副本,存入 notes_mark(原便签不变)"
                                    on:click=move |_| ctx.generate_mark()
                                >
                                    "生成标记"
                                </button>
                            }
                        }
                    >
                        <Show when=move || !mark_mode.get()>
                            <button
                                type="button"
                                class="tool pos-doc-btn"
                                title="在编辑器中编辑标记文档(保存只写入 notes_mark,不影响原文)"
                                on:click=move |_| ctx.enter_mark_mode()
                            >
                                "编辑标记"
                            </button>
                        </Show>
                        <button
                            type="button"
                            class="tool pos-doc-btn"
                            title="下载 notes_mark 中的标记文档(.md)"
                            on:click=move |_| ctx.export_mark()
                        >
                            "导出标记"
                        </button>
                        <button
                            type="button"
                            class="tool pos-doc-btn"
                            title="删除标记文档,原便签保持不变"
                            on:click=move |_| ctx.remove_mark()
                        >
                            "取消标记"
                        </button>
                    </Show>
                    <span><i class="dot cat-n"></i>"名词"</span>
                    <span><i class="dot cat-v"></i>"动词"</span>
                    <span><i class="dot cat-a"></i>"形容词"</span>
                    <span><i class="dot cat-d"></i>"副词"</span>
                    <span><i class="dot cat-u"></i>"助词"</span>
                    <span><i class="dot cat-r"></i>"代词"</span>
                    <span><i class="dot cat-c"></i>"连词"</span>
                    <span><i class="dot cat-aux"></i>"助动词"</span>
                    <span><i class="dot cat-x"></i>"其他"</span>
                    {move || {
                        let total = ctx
                            .state
                            .with_value(|st| st.editor.as_ref().map(|e| e.editor.get_value()))
                            .unwrap_or_default()
                            .chars()
                            .count();
                        let tagged = pos_tagged_chars.get();
                        (tagged < total).then(|| {
                            view! { <em class="pos-truncated">{format!("仅标注前 {tagged} 字")}</em> }
                        })
                    }}
                </div>
            </Show>
            <Show when=move || !suggest_items.get().is_empty()>
                <div
                    class="suggest-panel"
                    style=move || {
                        let (l, t) = suggest_pos.get();
                        format!("left:{l}px; top:{t}px;")
                    }
                    on:mousedown=|e: web_sys::MouseEvent| e.prevent_default()
                >
                    <For
                        each=move || suggest_items.get()
                        key=|it| format!("{}{}", if it.is_ja { "ja:" } else { "zh:" }, it.word)
                        children=move |it: SuggestItem| {
                            let word = it.word.clone();
                            let idx = suggest_items
                                .get()
                                .iter()
                                .position(|x| x.word == word && x.is_ja == it.is_ja)
                                .unwrap_or(0);
                            view! {
                                <div
                                    class=move || {
                                        if suggest_active.get() == idx { "suggest-item active" } else { "suggest-item" }
                                    }
                                    on:mouseenter=move |_| suggest_active.set(idx)
                                    on:click=move |_| ctx.accept_suggestion(idx)
                                >
                                    <span class="suggest-word">{word.clone()}</span>
                                    <span class=move || {
                                        if it.is_ja { "suggest-lang ja" } else { "suggest-lang" }
                                    }>
                                        {if it.is_ja { "日" } else { "中" }}
                                    </span>
                                </div>
                            }
                        }
                    />
                </div>
            </Show>
            <Show when=move || analyze_open.get()>
                <div class="analyze-drawer" on:mousedown=|e: web_sys::MouseEvent| e.stop_propagation()>
                    <div class="analyze-head">
                        <strong>
                            {move || {
                                if analyze_busy.get() {
                                    "分析中…".to_string()
                                } else {
                                    let n = analyze_issues.get().len();
                                    if n == 0 {
                                        "词法分析".to_string()
                                    } else {
                                        format!("词法分析 · {n} 个问题")
                                    }
                                }
                            }}
                        </strong>
                        <button
                            type="button"
                            class="tool"
                            on:click=move |_| analyze_open.set(false)
                        >
                            "关闭"
                        </button>
                    </div>
                    <div class="analyze-body">
                        <Show
                            when=move || !analyze_issues.get().is_empty()
                            fallback=move || {
                                view! {
                                    <p class="analyze-empty">
                                        {if analyze_busy.get() { "正在分析…" } else { "未发现明显问题 ✅" }}
                                    </p>
                                }
                            }
                        >
                            <For
                                each=move || {
                                    analyze_issues
                                        .get()
                                        .iter()
                                        .cloned()
                                        .enumerate()
                                        .collect::<Vec<_>>()
                                }
                                key=|(i, it)| format!("{i}-{}-{}", it.start, it.end)
                                children=move |(idx, it): (usize, Issue)| {
                                    let quote = it.quote.clone();
                                    let problem = it.problem.clone();
                                    let suggestion = it.suggestion.clone();
                                    let fixable = it.start != it.end;
                                    view! {
                                        <div class="analyze-issue">
                                            <div class="issue-quote">
                                                {if quote.is_empty() { "—".to_string() } else { quote.clone() }}
                                            </div>
                                            <div class="issue-problem">{problem.clone()}</div>
                                            {(!suggestion.is_empty()).then(|| {
                                                let s2 = suggestion.clone();
                                                view! {
                                                    <div class="issue-fix">
                                                        "建议:" {s2}
                                                    </div>
                                                }
                                            })}
                                            <Show when=move || fixable>
                                                <button
                                                    type="button"
                                                    class="tool issue-apply"
                                                    disabled=move || analyze_applied.get().contains(&idx)
                                                    on:click=move |_| ctx.apply_issue(idx)
                                                >
                                                    {move || {
                                                        if analyze_applied.get().contains(&idx) {
                                                            "已应用"
                                                        } else {
                                                            "应用到编辑器"
                                                        }
                                                    }}
                                                </button>
                                            </Show>
                                        </div>
                                    }
                                }
                            />
                        </Show>
                    </div>
                </div>
            </Show>
            <Show
                when=move || {
                    edit_locked.get() && !can_edit.get() && !lock_dismissed.get()
                }
            >
                <div class="lock-banner">
                    <span class="lock-banner-text">
                        "🔒 编辑已锁定 — 你可以阅读本文，申请通过后即可编辑"
                    </span>
                    <Show when=move || edit_request.get() != "pending">
                        <button
                            type="button"
                            class="tool"
                            on:click=move |_| lock_apply_open.set(true)
                        >
                            "申请编辑权限"
                        </button>
                    </Show>
                    <Show when=move || edit_request.get() == "pending">
                        <span class="lock-pending-inline">"申请已提交，等待审批…"</span>
                    </Show>
                    <button
                        type="button"
                        class="tool lock-close"
                        title="关闭提示(不影响阅读)"
                        on:click=move |_| lock_dismissed.set(true)
                    >
                        "×"
                    </button>
                </div>
            </Show>
            <Show when=move || lock_apply_open.get() && edit_locked.get() && !can_edit.get()>
                <div class="lock-apply-panel">
                    <div class="lock-card">
                        <div class="lock-card-head">
                            <strong>"申请编辑权限"</strong>
                            <button
                                type="button"
                                class="tool lock-close"
                                on:click=move |_| lock_apply_open.set(false)
                            >
                                "×"
                            </button>
                        </div>
                        {move || {
                            if edit_request.get() == "pending" {
                                view! {
                                    <p class="lock-pending">"已提交申请，等待管理员审批…"</p>
                                }
                                    .into_any()
                            } else {
                                view! {
                                    <div class="lock-form">
                                        <input
                                            class="lock-input"
                                            placeholder="申请理由（可选）"
                                            maxlength="200"
                                            prop:value=move || req_message.get()
                                            on:input=move |e| {
                                                req_message.set(evt_input_value(&e))
                                            }
                                            on:keydown=move |e: web_sys::KeyboardEvent| {
                                                if e.key() == "Enter" {
                                                    ctx.submit_edit_request();
                                                }
                                            }
                                        />
                                        <button
                                            type="button"
                                            class="lock-btn"
                                            on:click=move |_| ctx.submit_edit_request()
                                        >
                                            "提交申请"
                                        </button>
                                    </div>
                                }
                                    .into_any()
                            }
                        }}
                    </div>
                </div>
            </Show>
        </div>
    }
}
