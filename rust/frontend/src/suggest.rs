//! IME-style word suggestions shared by the note editor (main) and the oss
//! document library's markdown editor (oss).
//!
//! Data lives in D1: a language table (dict_lang), meaning groups
//! (dict_group) and words (dict_word) — same-group words in different
//! languages are mutual translations. The panel header carries a language
//! selector (persisted per viewer, "auto" follows any language); typing
//! looks words up in the selected language and shows the group's other
//! languages as hints / extra rows.
//!
//! Local static word lists (suggest_zh.txt / suggest_ja.txt) still answer
//! immediately and cover the pinned zh_cn / ja languages while the server
//! round-trip is in flight.

use crate::{api_url, window, BASE_PATH};
use gloo_timers::callback::Timeout;
use leptos::prelude::*;
use leptos::task::spawn_local;
use send_wrapper::SendWrapper;
use shared::{valid_lang_code, DictLang, DictLookupResponse, DictWordItem};
use std::cell::RefCell;
use web_sys::RequestCredentials;

#[derive(Clone)]
pub struct SuggestItem {
    /// text inserted when the row is accepted
    pub word: String,
    /// language badge text (from dict_lang.badge)
    pub lang: String,
    /// extra css class for the badge: "" / "ja" / "tw" / "en"
    pub lang_class: &'static str,
    /// the word's translations in other languages, shown beside the word
    pub hint: String,
    /// unique identity for <For> rendering and dedup
    pub key: String,
}

struct State {
    /// DOM anchor of the typed fragment (char offsets into the text node) so
    /// accepting can re-select and replace it
    anchor_node: Option<SendWrapper<web_sys::Node>>,
    anchor_start: usize,
    anchor_end: usize,
    /// accepted word, so the panel stays hidden right after an accept (the
    /// inserted text itself would re-trigger at the same cursor position)
    accepted_word: String,
    /// bumps on every fragment change / close so a late dictionary response
    /// for a stale fragment is dropped
    seq: usize,
    debounce: Option<Timeout>,
    /// gate captured at the last update, so switching language can refresh
    last_enabled: bool,
}

/// Suggestion controller for ONE editor. Holds only Copy handles, so it can
/// live inside a Ctx struct alongside the other UI signals.
#[derive(Clone, Copy)]
pub struct Suggest {
    items: RwSignal<Vec<SuggestItem>>,
    active: RwSignal<usize>,
    pos: RwSignal<(i32, i32)>,
    langs: RwSignal<Vec<DictLang>>,
    state: StoredValue<SendWrapper<State>>,
}

impl Suggest {
    pub fn new() -> Suggest {
        Suggest {
            items: RwSignal::new(Vec::new()),
            active: RwSignal::new(0),
            pos: RwSignal::new((0, 0)),
            langs: RwSignal::new(Vec::new()),
            state: StoredValue::new(SendWrapper::new(State {
                anchor_node: None,
                anchor_start: 0,
                anchor_end: 0,
                accepted_word: String::new(),
                seq: 0,
                debounce: None,
                last_enabled: false,
            })),
        }
    }

    pub fn is_open(&self) -> bool {
        self.items.with(|v| !v.is_empty())
    }

    pub fn close(&self) {
        self.items.update(|v| {
            if !v.is_empty() {
                v.clear();
            }
        });
        self.state.update_value(|st| {
            st.anchor_node = None;
            // drop a pending dictionary response together with the panel
            st.seq = st.seq.wrapping_add(1);
            if let Some(t) = st.debounce.take() {
                t.cancel();
            }
        });
    }

    /// Read the word fragment typed before the cursor and refresh the panel.
    /// `enabled` folds the caller's gates (edit permission, remote applies):
    /// when false the panel is simply closed.
    pub fn update(&self, enabled: bool) {
        self.state.update_value(|st| st.last_enabled = enabled);
        if !enabled {
            self.close();
            return;
        }
        let Some(sel) = window().get_selection().ok().flatten() else {
            self.close();
            return;
        };
        if !sel.is_collapsed() {
            self.close();
            return;
        }
        let Some(node) = sel.anchor_node() else {
            self.close();
            return;
        };
        if node.node_type() != web_sys::Node::TEXT_NODE {
            self.close();
            return;
        }
        // 光标必须在编辑器内容区内才提示:点文件树/工具栏/标题输入框等
        // 处也会触发 selectionchange,否则浮层面板会盖在无关文字上方
        let in_editor = node
            .parent_element()
            .and_then(|el| el.closest(".vditor-content").ok().flatten())
            .is_some();
        if !in_editor {
            self.close();
            return;
        }
        let offset = sel.anchor_offset();
        let text = node.text_content().unwrap_or_default();
        let chars: Vec<char> = text.chars().collect();
        let offset = offset as usize;
        if offset > chars.len() {
            self.close();
            return;
        }
        // walk backwards over word characters (CJK or alnum)
        let mut start = offset;
        while start > 0 && is_word_char(chars[start - 1]) {
            start -= 1;
        }
        let fragment: String = chars[start..offset].iter().collect();
        if !self
            .state
            .with_value(|st| st.accepted_word.is_empty() || st.accepted_word != fragment)
        {
            self.close();
            return;
        }
        if self
            .state
            .with_value(|st| !st.accepted_word.is_empty() && st.accepted_word != fragment)
        {
            self.state.update_value(|st| st.accepted_word.clear());
        }
        let n_chars = fragment.chars().count();
        let has_cjk = fragment.chars().any(is_cjk_char);
        // CJK runs complete up to 8 chars; latin runs (English lookups) need
        // at least 2 chars and no underscores (is_word_char allows them)
        let ok = if has_cjk {
            (1..=8).contains(&n_chars)
        } else {
            (2..=16).contains(&n_chars) && fragment.chars().all(|c| c.is_ascii_alphanumeric())
        };
        if !ok {
            self.close();
            return;
        }
        // local word-list completions, shown immediately (only the lists
        // whose language matches the selector)
        let selected = selected_lang();
        let items = lookup_local(&fragment, &selected);
        // panel position + anchor + request sequence for the dictionary lookup
        let pos = sel
            .get_range_at(0)
            .ok()
            .map(|r| r.get_bounding_client_rect())
            .map(|r| (r.left() as i32, (r.bottom() + 4.0) as i32))
            .unwrap_or((40, 40));
        self.state.update_value(|st| {
            st.seq = st.seq.wrapping_add(1);
            st.anchor_node = Some(SendWrapper::new(node));
            st.anchor_start = start;
            st.anchor_end = offset;
        });
        let seq = self.state.with_value(|st| st.seq);
        self.pos.set(pos);
        let has_items = !items.is_empty();
        self.items.set(items);
        if has_items {
            self.active.set(0);
        }
        self.schedule_lookup(&fragment, &selected, seq);
    }

    /// Suggestion panel keyboard navigation. Caller is expected to swallow
    /// (prevent_default/stop_propagation) the corresponding key events while
    /// the panel is open — see `is_open`.
    pub fn on_keydown(&self, e: &web_sys::KeyboardEvent, insert: &Callback<String>) {
        if self.items.with(|v| v.is_empty()) {
            return;
        }
        let len = self.items.with(|v| v.len());
        match e.key().as_str() {
            "ArrowDown" => {
                e.prevent_default();
                self.active.update(|i| *i = (*i + 1) % len);
            }
            "ArrowUp" => {
                e.prevent_default();
                self.active.update(|i| *i = (*i + len - 1) % len);
            }
            "Enter" | "Tab" => {
                e.prevent_default();
                let i = self.active.get();
                self.accept(i, insert);
            }
            "Escape" => {
                self.close();
            }
            _ => {}
        }
    }

    /// Replace the typed fragment with the chosen word. `insert` is the
    /// editor-specific "replace current selection with this text" hook.
    pub fn accept(&self, idx: usize, insert: &Callback<String>) {
        let Some(item) = self.items.with(|v| v.get(idx).cloned()) else {
            return;
        };
        let anchor = self.state.with_value(|st| {
            st.anchor_node
                .as_ref()
                .map(|n| (n.clone(), st.anchor_start, st.anchor_end))
        });
        self.close();
        let Some((node, start, end)) = anchor else { return };
        if let (Ok(Some(sel)), Ok(range)) =
            (window().get_selection(), web_sys::Range::new())
        {
            if range.set_start(&node, start as u32).is_ok()
                && range.set_end(&node, end as u32).is_ok()
            {
                sel.remove_all_ranges().ok();
                sel.add_range(&range).ok();
                insert.run(item.word.clone());
                self.state
                    .update_value(|st| st.accepted_word = item.word.clone());
            }
        }
    }

    /// Debounce the server lookup: every keystroke resets the 200 ms timer.
    fn schedule_lookup(&self, fragment: &str, lang: &str, seq: usize) {
        let c = *self;
        let fragment = fragment.to_string();
        let lang = lang.to_string();
        self.state.update_value(|st| {
            if let Some(t) = st.debounce.take() {
                t.cancel();
            }
            st.debounce = Some(Timeout::new(200, move || {
                spawn_local(dict_lookup_request(c, fragment, lang, seq));
            }));
        });
    }

    /// Language selector value ("" = auto / any language).
    pub fn selected_lang(&self) -> String {
        selected_lang()
    }

    /// Switch the selector language and immediately refresh open panel.
    pub fn set_lang(&self, code: &str) {
        set_selected_lang(code);
        let enabled = self.state.with_value(|st| st.last_enabled);
        self.update(enabled);
    }
}

/// The floating suggestion panel; render it next to the editor container
/// (position: fixed, so placement in the tree only affects stacking).
#[component]
pub fn SuggestPanel(sug: Suggest, insert: Callback<String>) -> impl IntoView {
    view! {
        <Show when=move || !sug.items.get().is_empty()>
            <div
                class="suggest-panel"
                style=move || {
                    let (l, t) = sug.pos.get();
                    format!("left:{l}px; top:{t}px;")
                }
                on:mousedown=|e: web_sys::MouseEvent| e.prevent_default()
            >
                <LangChips sug/>
                <For
                    each=move || sug.items.get()
                    key=|it| it.key.clone()
                    children=move |it: SuggestItem| {
                        let idx = sug
                            .items
                            .get()
                            .iter()
                            .position(|x| x.key == it.key)
                            .unwrap_or(0);
                        let word = it.word.clone();
                        let hint = it.hint.clone();
                        let lang = it.lang.clone();
                        let lang_class = it.lang_class;
                        view! {
                            <div
                                class=move || {
                                    if sug.active.get() == idx { "suggest-item active" } else { "suggest-item" }
                                }
                                on:mouseenter=move |_| sug.active.set(idx)
                                on:click=move |_| sug.accept(idx, &insert)
                            >
                                <span class="suggest-word">{word.clone()}</span>
                                <span class="suggest-hint">{hint}</span>
                                <span class=move || format!("suggest-lang {lang_class}")>{lang}</span>
                            </div>
                        }
                    }
                />
            </div>
        </Show>
    }
}

/// Language selector row at the top of the panel: 自动 + one chip per
/// language from /api/dict/langs. Clicking switches (persisted per viewer).
#[component]
fn LangChips(sug: Suggest) -> impl IntoView {
    view! {
        <div class="suggest-langs" on:mousedown=|e: web_sys::MouseEvent| e.stop_propagation()>
            <button
                type="button"
                class=move || if sug.selected_lang().is_empty() { "suggest-chip active" } else { "suggest-chip" }
                on:click=move |_| sug.set_lang("")
            >
                "自动"
            </button>
            <For
                each=move || sug.langs.get()
                key=|l| l.code.clone()
                children=move |l: DictLang| {
                    let code = l.code.clone();
                    let code_for_class = code.clone();
                    let label = if l.badge.is_empty() { l.name.clone() } else { l.badge.clone() };
                    let title = l.name.clone();
                    view! {
                        <button
                            type="button"
                            class=move || {
                                if sug.selected_lang() == code_for_class { "suggest-chip active" } else { "suggest-chip" }
                            }
                            title=title
                            on:click=move |_| sug.set_lang(&code)
                        >
                            {label}
                        </button>
                    }
                }
            />
        </div>
    }
}

// ---- language selection (persisted per viewer) ----

thread_local! {
    static LANGS: RefCell<Option<std::rc::Rc<Vec<DictLang>>>> = const { RefCell::new(None) };
    static SELECTED: RefCell<String> = const { RefCell::new(String::new()) };
}

const LANG_STORAGE_KEY: &str = "urlnote-suggest-lang";

/// 构建期注入的默认语言(如 zh_cn);空 = 自动。用户在选择器里改过后
/// 以 localStorage 为准(见 init_selected_lang)
const DEFAULT_SELECTED_LANG: &str = match option_env!("SUGGEST_LANG") {
    Some(v) => v,
    None => "",
};

pub fn selected_lang() -> String {
    SELECTED.with(|c| c.borrow().clone())
}

pub fn set_selected_lang(code: &str) {
    SELECTED.with(|c| *c.borrow_mut() = code.to_string());
    // per-viewer convenience only — best effort
    if let Ok(Some(store)) = window().local_storage() {
        if code.is_empty() {
            let _ = store.remove_item(LANG_STORAGE_KEY);
        } else {
            let _ = store.set_item(LANG_STORAGE_KEY, code);
        }
    }
}

fn init_selected_lang() {
    // 初始值:构建期默认;localStorage 里存过用户选择则覆盖
    let mut initial = DEFAULT_SELECTED_LANG.trim().to_string();
    if let Ok(Some(store)) = window().local_storage() {
        if let Ok(Some(v)) = store.get_item(LANG_STORAGE_KEY) {
            if v.is_empty() || valid_lang_code(&v) {
                initial = v;
            }
        }
    }
    if initial.is_empty() || valid_lang_code(&initial) {
        SELECTED.with(|c| *c.borrow_mut() = initial);
    }
}

/// Badge text + css class for a language code ("" = neutral, for custom
/// languages loaded from the server the badge text is taken from langs).
fn lang_style(code: &str) -> (&'static str, &'static str) {
    match code {
        "zh_cn" => ("简", ""),
        "zh_tw" => ("繁", "tw"),
        "ja" => ("日", "ja"),
        "en" => ("英", "en"),
        _ => ("词", ""),
    }
}

/// Badge text for a code: built-in short names first, then the server's
/// badge, falling back to a neutral glyph.
fn badge_for(code: &str) -> String {
    let (built_in, _) = lang_style(code);
    if built_in != "词" {
        return built_in.to_string();
    }
    LANGS.with(|c| {
        c.borrow()
            .as_ref()
            .and_then(|ls| ls.iter().find(|l| l.code == code))
            .map(|l| if l.badge.is_empty() { "词".to_string() } else { l.badge.clone() })
            .unwrap_or_else(|| "词".to_string())
    })
}

// ---- word fragment classification ----

fn is_cjk_char(c: char) -> bool {
    matches!(c,
        '\u{4e00}'..='\u{9fff}'
        | '\u{3040}'..='\u{30ff}'
        | '\u{f900}'..='\u{faff}')
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

// ---- static word lists (generated by scripts/gen_suggest_dicts.py) ----

thread_local! {
    static SUGGEST_ZH: RefCell<Option<std::rc::Rc<Vec<String>>>> = const { RefCell::new(None) };
    static SUGGEST_JA: RefCell<Option<std::rc::Rc<Vec<String>>>> = const { RefCell::new(None) };
}

/// Kick off the background fetches once per page (word lists + languages);
/// lookups are no-ops until data arrives. Shared by all editors.
pub fn preload(sug: Suggest) {
    load_list(&format!("{BASE_PATH}/dict/suggest_zh.txt"), &SUGGEST_ZH);
    load_list(&format!("{BASE_PATH}/dict/suggest_ja.txt"), &SUGGEST_JA);
    init_selected_lang();
    spawn_local(async move {
        let Ok(resp) =
            gloo_net::http::Request::get(&api_url("/api/dict/langs"))
                .credentials(RequestCredentials::Include)
                .send()
                .await
        else {
            return;
        };
        if !resp.ok() {
            return;
        }
        let Ok(list) = resp.json::<shared::DictLangList>().await else {
            return;
        };
        LANGS.with(|c| *c.borrow_mut() = Some(std::rc::Rc::new(list.langs.clone())));
        sug.langs.set(list.langs);
    });
}

fn load_list(
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
    lang: &'static str,
    lang_class: &'static str,
) {
    slot.with(|c| {
        let c = c.borrow();
        let Some(words) = c.as_ref() else { return };
        let idx = words.partition_point(|w| w.as_str() < prefix);
        for w in words[idx..].iter().take(limit) {
            if !w.starts_with(prefix) {
                break;
            }
            out.push(SuggestItem {
                word: w.clone(),
                lang: lang.to_string(),
                lang_class,
                hint: String::new(),
                key: format!("{lang}:{w}"),
            });
        }
    });
}

/// Static word-list completions. With the selector on a specific language
/// only that language's list applies (auto = both).
fn lookup_local(fragment: &str, selected: &str) -> Vec<SuggestItem> {
    let mut out = Vec::new();
    if selected.is_empty() || selected == "zh_cn" {
        prefix_lookup(&SUGGEST_ZH, fragment, 4, &mut out, "简", "");
    }
    if selected.is_empty() || selected == "ja" {
        prefix_lookup(&SUGGEST_JA, fragment, 4, &mut out, "日", "ja");
    }
    out
}

// ---- dictionary lookup (D1 tables dict_lang / dict_group / dict_word) ----

/// Dictionary words → suggestion items, at most 10. Each matched word
/// completes itself with its translations as the hint; an exact hit (word
/// equals the typed fragment) additionally offers every translation as its
/// own insertable row — the other languages are one Enter away.
fn dict_suggest_items(fragment: &str, words: &[DictWordItem]) -> Vec<SuggestItem> {
    let mut out: Vec<SuggestItem> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for w in words {
        let (badge, class) = lang_style(&w.lang);
        let badge = if badge == "词" { badge_for(&w.lang) } else { badge.to_string() };
        let hint = w
            .trans
            .values()
            .map(|word| word.as_str())
            .collect::<Vec<_>>()
            .join(" · ");
        let key = format!("{}:{}", w.lang, w.word);
        if seen.insert(key.clone()) {
            out.push(SuggestItem {
                word: w.word.clone(),
                lang: badge.clone(),
                lang_class: class,
                hint,
                key,
            });
        }
        // exact hit: the typed word is already there — offer its
        // translations as insertable rows too
        if w.word == fragment {
            for (code, tw) in &w.trans {
                if *tw == fragment {
                    continue;
                }
                let key = format!("{code}:{tw}");
                if !seen.insert(key.clone()) {
                    continue;
                }
                let (tb, tc) = lang_style(code);
                let tb = if tb == "词" { badge_for(code) } else { tb.to_string() };
                let hint = w
                    .trans
                    .iter()
                    .filter(|(c, t)| c.as_str() != code.as_str() && t.as_str() != fragment && t.as_str() != tw.as_str())
                    .map(|(_, t)| t.as_str())
                    .collect::<Vec<_>>()
                    .join(" · ");
                out.push(SuggestItem {
                    word: tw.clone(),
                    lang: tb,
                    lang_class: tc,
                    hint,
                    key,
                });
            }
        }
        if out.len() >= 10 {
            break;
        }
    }
    out
}

/// Dictionary items first, then whatever local completions were already
/// showing, deduped by key, capped at 10 rows.
fn merge_items(dict_items: Vec<SuggestItem>, local: Vec<SuggestItem>) -> Vec<SuggestItem> {
    let mut out = dict_items;
    let mut seen: std::collections::HashSet<String> = out.iter().map(|i| i.key.clone()).collect();
    for it in local {
        if out.len() >= 10 {
            break;
        }
        if seen.insert(it.key.clone()) {
            out.push(it);
        }
    }
    out
}

/// Debounced lookup against GET /api/dict/lookup (lang = the selector
/// value, empty = auto). A failure (older backend, network error) silently
/// keeps the local-only suggestions.
async fn dict_lookup_request(c: Suggest, fragment: String, lang: String, seq: usize) {
    let enc: String = js_sys::encode_uri_component(&fragment).into();
    let lang_enc: String = js_sys::encode_uri_component(&lang).into();
    let Ok(resp) = gloo_net::http::Request::get(&api_url(&format!(
        "/api/dict/lookup?q={enc}&lang={lang_enc}"
    )))
    .credentials(RequestCredentials::Include)
    .send()
    .await
    else {
        return;
    };
    if !resp.ok() {
        return;
    }
    let Ok(data) = resp.json::<DictLookupResponse>().await else {
        return;
    };
    if data.words.is_empty() {
        return;
    }
    // the cursor moved, the panel closed, or the selector changed while the
    // request was in flight
    if c.state.with_value(|st| st.seq != seq) {
        return;
    }
    let dict_items = dict_suggest_items(&fragment, &data.words);
    if dict_items.is_empty() {
        return;
    }
    let merged = merge_items(dict_items, c.items.get());
    if merged.is_empty() {
        return;
    }
    c.active.update(|i| {
        if *i >= merged.len() {
            *i = 0;
        }
    });
    c.items.set(merged);
}
