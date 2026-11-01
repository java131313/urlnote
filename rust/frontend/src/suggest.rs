//! IME-style word suggestions shared by the note editor (main) and the oss
//! document library's markdown editor (oss).
//!
//! Two sources feed the panel under the cursor:
//!  - static word lists (suggest_zh.txt / suggest_ja.txt), answered locally;
//!  - the four-language dictionary in D1 (table `dict`), queried on the
//!    server with a 200 ms debounce and merged in when it answers.
//!
//! A dictionary row maps same-meaning words 1:1 across zh-CN / zh-TW / ja /
//! en, so typing a word that exactly matches any column offers the *other*
//! languages' words (each row's hint also lists the remaining translations);
//! a partial word offers completions from every column.

use crate::{api_url, window, BASE_PATH};
use gloo_timers::callback::Timeout;
use leptos::prelude::*;
use leptos::task::spawn_local;
use send_wrapper::SendWrapper;
use shared::{DictEntry, DictLookupResponse};
use std::cell::RefCell;
use web_sys::RequestCredentials;

#[derive(Clone)]
pub struct SuggestItem {
    /// text inserted when the row is accepted
    pub word: String,
    /// language badge text: "简" | "繁" | "日" | "英" (static lists: "简"/"日")
    pub lang: &'static str,
    /// extra css class for the badge: "" / "ja" / "tw" / "en"
    pub lang_class: &'static str,
    /// the entry's other translations, shown beside the word
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
}

/// Suggestion controller for ONE editor. Holds only Copy handles, so it can
/// live inside a Ctx struct alongside the other UI signals.
#[derive(Clone, Copy)]
pub struct Suggest {
    items: RwSignal<Vec<SuggestItem>>,
    active: RwSignal<usize>,
    pos: RwSignal<(i32, i32)>,
    state: StoredValue<SendWrapper<State>>,
}

impl Suggest {
    pub fn new() -> Suggest {
        Suggest {
            items: RwSignal::new(Vec::new()),
            active: RwSignal::new(0),
            pos: RwSignal::new((0, 0)),
            state: StoredValue::new(SendWrapper::new(State {
                anchor_node: None,
                anchor_start: 0,
                anchor_end: 0,
                accepted_word: String::new(),
                seq: 0,
                debounce: None,
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
        // local word-list completions, shown immediately
        let items = lookup_local(&fragment);
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
        self.schedule_lookup(&fragment, seq);
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
    fn schedule_lookup(&self, fragment: &str, seq: usize) {
        let c = *self;
        let fragment = fragment.to_string();
        self.state.update_value(|st| {
            if let Some(t) = st.debounce.take() {
                t.cancel();
            }
            st.debounce = Some(Timeout::new(200, move || {
                spawn_local(dict_lookup_request(c, fragment, seq));
            }));
        });
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
                        let lang = it.lang;
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

/// Kick off the fetches once; lookups are no-ops until a list arrives.
/// Shared by all editors — call from page setup.
pub fn preload() {
    load_list(&format!("{BASE_PATH}/dict/suggest_zh.txt"), &SUGGEST_ZH);
    load_list(&format!("{BASE_PATH}/dict/suggest_ja.txt"), &SUGGEST_JA);
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
                lang,
                lang_class,
                hint: String::new(),
                key: format!("{lang}:{w}"),
            });
        }
    });
}

/// Up to 8 completions for the fragment (4 Chinese + 4 Japanese) from the
/// static word lists; the four-language dictionary is merged in separately
/// when the server answers (see dict_lookup_request).
fn lookup_local(fragment: &str) -> Vec<SuggestItem> {
    let mut out = Vec::new();
    prefix_lookup(&SUGGEST_ZH, fragment, 4, &mut out, "简", "");
    prefix_lookup(&SUGGEST_JA, fragment, 4, &mut out, "日", "ja");
    out
}

// ---- four-language dictionary (D1 table `dict`) ----

fn dict_zh(e: &DictEntry) -> &str {
    &e.zh_cn
}
fn dict_tw(e: &DictEntry) -> &str {
    &e.zh_tw
}
fn dict_ja_field(e: &DictEntry) -> &str {
    &e.ja
}
fn dict_en(e: &DictEntry) -> &str {
    &e.en
}

/// (badge text, badge css class, field getter) per dictionary language.
type DictFieldGetter = fn(&DictEntry) -> &str;
const DICT_LANGS: [(&str, &str, DictFieldGetter); 4] = [
    ("简", "", dict_zh),
    ("繁", "tw", dict_tw),
    ("日", "ja", dict_ja_field),
    ("英", "en", dict_en),
];

/// Dictionary rows → suggestion items, at most 6. An exact hit (a field
/// equals the typed fragment) yields one item per *other* language — the
/// typed word itself is already there; a prefix-only hit yields every
/// non-empty field as a completion. Every item's hint lists the row's
/// remaining translations, so the other languages are visible at a glance.
fn dict_suggest_items(fragment: &str, entries: &[DictEntry]) -> Vec<SuggestItem> {
    let mut out: Vec<SuggestItem> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    'rows: for e in entries {
        let exact = DICT_LANGS.iter().any(|(_, _, get)| get(e) == fragment);
        for (lang, lang_class, get) in DICT_LANGS {
            let w = get(e);
            if w.is_empty() || (exact && w == fragment) {
                continue;
            }
            let key = format!("{lang}:{w}");
            if !seen.insert(key.clone()) {
                continue;
            }
            let hint = DICT_LANGS
                .iter()
                .map(|(_, _, g)| g(e))
                .filter(|v| !v.is_empty() && *v != w && *v != fragment)
                .collect::<Vec<_>>()
                .join(" · ");
            out.push(SuggestItem {
                word: w.to_string(),
                lang,
                lang_class,
                hint,
                key,
            });
            if out.len() >= 6 {
                break 'rows;
            }
        }
    }
    out
}

/// Dictionary items first, then whatever local completions were already
/// showing, deduped by key, capped at 8 rows.
fn merge_items(dict_items: Vec<SuggestItem>, local: Vec<SuggestItem>) -> Vec<SuggestItem> {
    let mut out = dict_items;
    let mut seen: std::collections::HashSet<String> = out.iter().map(|i| i.key.clone()).collect();
    for it in local {
        if out.len() >= 8 {
            break;
        }
        if seen.insert(it.key.clone()) {
            out.push(it);
        }
    }
    out
}

/// Debounced lookup against GET /api/dict/lookup. A failure (older backend,
/// network error) silently keeps the local-only suggestions.
async fn dict_lookup_request(c: Suggest, fragment: String, seq: usize) {
    let enc: String = js_sys::encode_uri_component(&fragment).into();
    let Ok(resp) = gloo_net::http::Request::get(&api_url(&format!("/api/dict/lookup?q={enc}")))
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
    if data.entries.is_empty() {
        return;
    }
    // the cursor moved or the panel closed while the request was in flight
    if c.state.with_value(|st| st.seq != seq) {
        return;
    }
    let dict_items = dict_suggest_items(&fragment, &data.entries);
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
