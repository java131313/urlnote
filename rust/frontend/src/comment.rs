//! 批注(锚定文字):选中文字 → 添加批注 → 侧栏列表,点击跳转到原文。
//! 便签与文档库共用;doc 为 "note/<id>" 或 "oss/<key>",数据在 MongoDB。
//! 锚定 = 引用文本 + 字符偏移提示;文档编辑后偏移会漂移,跳转时按引用
//! 文本重新定位(取离原偏移最近的出现处)。

use gloo_net::http::Request;
use leptos::prelude::*;
use leptos::task::spawn_local;
use shared::{CommentItem, CommentList, CommentUpsert};
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use web_sys::RequestCredentials;

use crate::{api_url, window};

#[derive(Clone, Copy)]
pub struct CommentCtl {
    doc: RwSignal<String>,
    items: RwSignal<Vec<CommentItem>>,
    open: RwSignal<bool>,
    busy: RwSignal<bool>,
    /// 编辑器内容根选择器(绘制锚点/定位用)
    root: RwSignal<String>,
    /// 当前上下文文档(未开面板时锚点也跟随它)
    context_doc: RwSignal<String>,
}

thread_local! {
    static DRAW_TIMER: std::cell::RefCell<Option<gloo_timers::callback::Timeout>> =
        const { std::cell::RefCell::new(None) };
}

impl CommentCtl {
    pub fn new() -> CommentCtl {
        CommentCtl {
            doc: RwSignal::new(String::new()),
            items: RwSignal::new(Vec::new()),
            open: RwSignal::new(false),
            busy: RwSignal::new(false),
            root: RwSignal::new(String::new()),
            context_doc: RwSignal::new(String::new()),
        }
    }

    pub fn set_root(&self, root: &str) {
        self.root.set(root.to_string());
    }

    /// 页面切换文档(打开便签/文件)时同步:加载批注并重绘锚点
    pub fn set_context(&self, doc: &str) {
        if self.context_doc.get() == doc {
            return;
        }
        self.context_doc.set(doc.to_string());
        if !self.open.get() {
            self.doc.set(doc.to_string());
            self.refresh();
        }
    }

    fn active_doc(&self) -> String {
        if self.open.get() {
            self.doc.get()
        } else {
            self.context_doc.get()
        }
    }


    pub fn is_open(&self) -> bool {
        self.open.get()
    }

    /// 打开并指向某文档(已开且同文档则不动)
    pub fn ensure_open(&self, doc: &str) {
        if self.doc.get() != doc || !self.open.get() {
            self.doc.set(doc.to_string());
            self.open.set(true);
            self.refresh();
        }
    }

    /// 切换侧栏并加载批注(内容根选择器用于"添加批注"时的选区定位)
    #[allow(dead_code)]
    pub fn toggle(&self, doc: &str) {
        if self.open.get() && self.doc.get() == doc {
            self.open.set(false);
            return;
        }
        self.doc.set(doc.to_string());
        self.open.set(true);
        self.refresh();
    }

    /// 便签的批注存 notes_mark(comments 数组);其余(文档库)走 comments 集合
    fn is_note_doc(doc: &str) -> bool {
        doc.starts_with("note/")
    }

    pub fn refresh(&self) {
        let doc = self.doc.get();
        if doc.is_empty() {
            return;
        }
        let c = *self;
        spawn_local(async move {
            let url = if Self::is_note_doc(&doc) {
                let enc: String = js_sys::encode_uri_component(doc.strip_prefix("note/").unwrap_or("")).into();
                api_url(&format!("/api/note/{enc}/comments"))
            } else {
                let enc: String = js_sys::encode_uri_component(&doc).into();
                api_url(&format!("/api/comments?doc={enc}"))
            };
            let Ok(resp) = Request::get(&url)
                .credentials(RequestCredentials::Include)
                .send()
                .await
            else {
                return;
            };
            if resp.ok() {
                if let Ok(list) = resp.json::<CommentList>().await {
                    c.items.set(list.comments);
                }
            }
        });
    }

    /// 用当前选区创建批注(选区必须在 content_root 内)。
    /// 成功后选中文字被包成 <a id="cm-<id>" class="cmt"> 写进 markdown。
    pub fn add_from_selection(&self, content_root: &str) {
        // 先暂存选区:后续 prompt / POST 期间选区可能丢失
        crate::vditor::wrap_prep_stash();
        let Some((quote, start, end)) = selection_span(content_root) else {
            set_status_hint("请先在编辑器里选中要批注的文字");
            return;
        };
        if quote.trim().is_empty() {
            set_status_hint("选区里没有文字");
            return;
        }
        let Some(text) = window()
            .prompt_with_message("批注内容:")
            .ok()
            .flatten()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
        else {
            return;
        };
        let doc = self.active_doc();
        let cid = crate::vditor::new_comment_id();
        let c = *self;
        c.busy.set(true);
        spawn_local(async move {
            let body = CommentUpsert {
                id: cid.clone(),
                doc: doc.clone(),
                quote,
                text,
                start,
                end,
            };
            let url = if Self::is_note_doc(&doc) {
                let enc: String =
                    js_sys::encode_uri_component(doc.strip_prefix("note/").unwrap_or("")).into();
                api_url(&format!("/api/note/{enc}/comments"))
            } else {
                api_url("/api/comments")
            };
            let r = Request::post(&url)
                .credentials(RequestCredentials::Include)
                .json(&body);
            let Ok(resp) = r.unwrap().send().await else { return };
            if resp.ok() {
                // 批注入库后再包锚点(用暂存的选区),文档随之变脏走自动保存
                crate::vditor::wrap_selection_as_anchor(&cid);
                c.refresh();
            } else {
                set_status_hint("批注保存失败");
            }
            c.busy.set(false);
        });
    }

    pub fn remove(&self, id: String) {
        let doc = self.doc.get();
        let c = *self;
        spawn_local(async move {
            let url = if Self::is_note_doc(&doc) {
                let enc: String =
                    js_sys::encode_uri_component(doc.strip_prefix("note/").unwrap_or("")).into();
                api_url(&format!("/api/note/{enc}/comments/{id}"))
            } else {
                api_url(&format!("/api/comments/{id}"))
            };
            let Ok(resp) = Request::delete(&url)
                .credentials(RequestCredentials::Include)
                .send()
                .await
            else {
                return;
            };
            if resp.ok() {
                c.refresh();
            } else {
                set_status_hint("删除失败(仅作者或管理员)");
            }
        });
    }

    /// 跳转到批注锚点:优先 <a id="cm-.."> 锚点元素,回退按引用文本重定位
    pub fn jump(&self, item: &CommentItem, content_root: &str) {
        let el_id = format!("cm-{}", item.id);
        if let Some(el) = document().get_element_by_id(&el_id) {
            // 选中锚点内容,滚动到可视区,闪烁
            let Some(sel) = window().get_selection().ok().flatten() else { return };
            let Ok(range) = web_sys::Range::new() else { return };
            if range.select_node_contents(&el).is_ok() {
                sel.remove_all_ranges().ok();
                sel.add_range(&range).ok();
            }
            let rect = el.get_bounding_client_rect();
            if let Ok(Some(content)) = el.closest(".vditor-content") {
                let shift = rect.top() - 120.0;
                let next = (content.scroll_top() as f64 + shift).max(0.0);
                content.set_scroll_top(next as i32);
            }
            let rect2 = range.get_bounding_client_rect();
            flash_rect(&rect2);
            return;
        }
        select_quote(content_root, &item.quote, item.start);
    }
}

// 状态提示走各自页面的 status 信号 —— 这里用一个线程内回调槽
thread_local! {
    static HINT: std::cell::RefCell<Option<Box<dyn Fn(&str)>>> = const { std::cell::RefCell::new(None) };
}

pub fn set_hint_handler(f: impl Fn(&str) + 'static) {
    HINT.with(|h| *h.borrow_mut() = Some(Box::new(f)));
}

fn set_status_hint(msg: &str) {
    HINT.with(|h| {
        if let Some(f) = h.borrow().as_ref() {
            f(msg);
        }
    });
}

// ---- 选区 / 定位(字符偏移) ----

type NodeInfo = (web_sys::Node, String, usize); // (node, text, char_start)

fn collect_nodes(root: &web_sys::Element) -> Option<(Vec<NodeInfo>, String)> {
    let mut out: Vec<NodeInfo> = Vec::new();
    let mut acc = 0usize;
    collect_nodes_inner(root, &mut out, &mut acc);
    let full: String = out.iter().map(|(_, t, _)| t.as_str()).collect();
    Some((out, full))
}

fn collect_nodes_inner(el: &web_sys::Element, out: &mut Vec<NodeInfo>, acc: &mut usize) {
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
                if let Ok(el) = node.dyn_into::<web_sys::Element>() {
                    if el.closest("code, style, script").ok().flatten().is_none() {
                        collect_nodes_inner(&el, out, acc);
                    } else {
                        let text = el.text_content().unwrap_or_default();
                        *acc += text.chars().count();
                    }
                }
            }
            _ => {}
        }
    }
}

/// UTF-16 DOM 偏移 → 字符偏移
fn utf16_to_chars(text: &str, utf16_off: u32) -> usize {
    let mut count = 0usize;
    let mut units = 0u32;
    for ch in text.chars() {
        if units >= utf16_off {
            break;
        }
        units += ch.len_utf16() as u32;
        count += 1;
    }
    count
}

/// 当前选区 → (引用文本, 全文起始字符, 全文结束字符)
fn selection_span(content_root: &str) -> Option<(String, usize, usize)> {
    let root = document()
        .query_selector(content_root)
        .ok()
        .flatten()?;
    let (nodes, full) = collect_nodes(&root)?;
    let sel = window().get_selection().ok()??;
    if sel.is_collapsed() {
        return None;
    }
    let pos_of = |node: &web_sys::Node, off: u32| -> Option<usize> {
        let (_n, text, base) = nodes.iter().find(|(n, _, _)| n == node)?;
        Some(base + utf16_to_chars(text, off))
    };
    let start = pos_of(&sel.anchor_node()?, sel.anchor_offset())?;
    let end = pos_of(&sel.focus_node()?, sel.focus_offset())?;
    let (start, end) = if start <= end { (start, end) } else { (end, start) };
    let chars: Vec<char> = full.chars().collect();
    if start >= chars.len() {
        return None;
    }
    let end = end.min(chars.len());
    let quote: String = chars[start..end].iter().collect::<String>().chars().take(200).collect();
    Some((quote, start, end))
}

/// 按引用文本在编辑器里定位:取离 hint 最近的出现处,返回 DOM Range
/// (起点/终点分别落在各自所在文本节点,支持跨格式引文)
fn place_range(
    root: &web_sys::Element,
    nodes: &[(web_sys::Node, String, usize)],
    full_chars: &[char],
    quote: &str,
    hint: usize,
) -> Option<web_sys::Range> {
    let needle: Vec<char> = quote.chars().collect();
    if needle.is_empty() || needle.len() > full_chars.len() {
        return None;
    }
    let mut occ = Vec::new();
    for i in 0..=(full_chars.len() - needle.len()) {
        if full_chars[i..i + needle.len()] == needle[..] {
            occ.push(i);
        }
        if occ.len() >= 200 {
            break;
        }
    }
    let start = *occ.iter().min_by_key(|&&i| i.abs_diff(hint))?;
    let end = start + needle.len();
    let mut start_loc = None;
    let mut end_loc = None;
    for (node, text, base) in nodes {
        let n = text.chars().count();
        if start_loc.is_none() && start >= *base && (start < *base + n || (start == *base && n == 0)) {
            start_loc = Some((node, start - *base, text));
        }
        if end_loc.is_none() && end > *base && end <= *base + n {
            end_loc = Some((node, end - *base, text));
        }
    }
    let (sn, so, st) = start_loc?;
    let (en, eo, et) = end_loc?;
    let u16s: u32 = st.chars().take(so).map(|c| c.len_utf16() as u32).sum();
    let u16e: u32 = et.chars().take(eo).map(|c| c.len_utf16() as u32).sum();
    let range = web_sys::Range::new().ok()?;
    if range.set_start(sn, u16s).is_ok() && range.set_end(en, u16e).is_ok() {
        Some(range)
    } else {
        None
    }
}

/// 按引用文本重定位并选中(取离 hint 最近的出现处)
fn select_quote(content_root: &str, quote: &str, hint: usize) {
    if quote.is_empty() {
        return;
    }
    let Some(root) = document()
        .query_selector(content_root)
        .ok()
        .flatten()
    else {
        return;
    };
    let Some((nodes, full)) = collect_nodes(&root) else { return };
    let chars: Vec<char> = full.chars().collect();
    let Some(range) = place_range(&root, &nodes, &chars, quote, hint) else {
        set_status_hint("原文已变化,找不到锚定文字");
        return;
    };
    if let Some(sel) = window().get_selection().ok().flatten() {
        sel.remove_all_ranges().ok();
        sel.add_range(&range).ok();
    }
    let rect = range.get_bounding_client_rect();
    // 编辑器内容区滚动到选中处(编辑器自身是滚动容器)
    if let Ok(Some(content)) = root.closest(".vditor-content") {
        let shift = rect.top() - 120.0;
        let next = (content.scroll_top() as f64 + shift).max(0.0);
        content.set_scroll_top(next as i32);
    }
    flash_rect(&rect);
}

// ---- 正文锚点(覆盖层,不进 markdown) ----

impl CommentCtl {
    /// 点击正文锚点:侧栏打开/滚动到对应条目并闪烁
    pub fn focus_item(&self, cid: &str) {
        let doc = self.active_doc();
        if !self.open.get() {
            self.doc.set(doc.clone());
            self.open.set(true);
            self.refresh();
        }
        let items = self.items.get();
        if !items.iter().any(|i| i.id == cid) {
            // 面板数据可能还没加载完,稍后重试一次
            let c = *self;
            let cid = cid.to_string();
            leptos::task::spawn_local(async move {
                gloo_timers::future::sleep(std::time::Duration::from_millis(400)).await;
                c.flash_item(&cid);
            });
            return;
        }
        self.flash_item(cid);
    }

    fn flash_item(&self, cid: &str) {
        let Some(doc) = window().document() else { return };
        let sel = format!(".comment-item[data-cid='{cid}']");
        let Some(el) = doc.query_selector(&sel).ok().flatten() else { return };
        let _ = el.set_attribute("data-flash", "1");
        let _ = el.scroll_into_view_with_bool(false);
        let cb = wasm_bindgen::closure::Closure::<dyn FnMut()>::new(move || {
            let _ = el.remove_attribute("data-flash");
        });
        let _ = window()
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                cb.as_ref().unchecked_ref(),
                1600,
            )
            .map(|_| cb.forget());
    }
}

thread_local! {
    static CTL: std::cell::RefCell<Option<CommentCtl>> = const { std::cell::RefCell::new(None) };
}

/// 页面装配时注册本页的 ctl(锚点点击回跳用;每页唯一),
/// 并挂一次文档级委托:点击 <a class="cmt"> → 打开侧栏并定位批注
pub fn register_ctl(ctl: CommentCtl) {
    CTL.with(|c| *c.borrow_mut() = Some(ctl));
    thread_local! {
        static CLICK_WIRED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if CLICK_WIRED.with(|w| w.get()) {
        return;
    }
    CLICK_WIRED.with(|w| w.set(true));
    let listener = gloo_events::EventListener::new(&document(), "click", |e| {
        let el = e
            .target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
            .and_then(|el| el.closest("a.cmt").ok().flatten());
        let Some(el) = el else { return };
        let Some(id) = el.get_attribute("id") else { return };
        let Some(cid) = id.strip_prefix("cm-") else { return };
        CTL.with(|c| {
            if let Some(ctl) = c.borrow().as_ref() {
                ctl.focus_item(cid);
            }
        });
    });
    // 页面存活期有效;两个页面各自唯一,泄漏保活
    KEEP_LISTENERS.with(|l| l.borrow_mut().push(listener));
}

thread_local! {
    static KEEP_LISTENERS: std::cell::RefCell<Vec<gloo_events::EventListener>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// 跳转后的黄色闪烁标记,1.3s 后自删
fn flash_rect(rect: &web_sys::DomRect) {
    let Some(doc) = window().document() else { return };
    let Ok(m) = doc.create_element("div") else { return };
    m.set_class_name("comment-flash");
    let style = format!(
        "left:{}px;top:{}px;width:{}px;height:{}px;",
        rect.left().max(0.0),
        rect.top().max(0.0),
        rect.width().max(30.0),
        rect.height().max(20.0),
    );
    let _ = m.set_attribute("style", &style);
    let Some(body) = doc.body() else { return };
    let _ = body.append_child(&m);
    let cb = wasm_bindgen::closure::Closure::<dyn FnMut()>::new(move || {
        m.remove();
    });
    let _ = window()
        .set_timeout_with_callback_and_timeout_and_arguments_0(
            cb.as_ref().unchecked_ref(),
            1300,
        )
        .map(|_| cb.forget());
}

// ---- 侧栏组件 ----

/// 批注侧栏。props: 控制器、内容根选择器(添加/跳转用)、当前文档 ref。
#[component]
pub fn CommentPanel(ctl: CommentCtl, content_root: &'static str) -> impl IntoView {
    view! {
        <Show when=move || ctl.open.get()>
            <div class="comment-drawer" on:mousedown=|e: web_sys::MouseEvent| e.stop_propagation()>
                <div class="comment-head">
                    <strong>{move || format!("批注 · {}", ctl.items.with(|v| v.len()))}</strong>
                    <button
                        type="button"
                        class="tool"
                        on:click=move |_| ctl.open.set(false)
                    >
                        "关闭"
                    </button>
                </div>
                <div class="comment-hint">
                    "选中正文文字后点工具栏 💬 按钮添加"
                </div>
                <For
                    each=move || ctl.items.get()
                    key=|c| c.id.clone()
                    children=move |c: CommentItem| {
                        let jump = ctl;
                        let cr = content_root;
                        let del = ctl;
                        let del_id = c.id.clone();
                        let jump_item = c.clone();
                        let quote = c.quote.clone();
                        view! {
                            <div class="comment-item">
                                <div
                                    class="comment-quote"
                                    title="点击定位到原文"
                                    on:click=move |_| jump.jump(&jump_item, cr)
                                >
                                    {quote}
                                </div>
                                <div class="comment-text">{c.text.clone()}</div>
                                <div class="comment-meta">
                                    <span>{format!("{} · {}", c.author, c.created_at)}</span>
                                    <button
                                        type="button"
                                        class="tool"
                                        on:click=move |_| del.remove(del_id.clone())
                                    >
                                        "删除"
                                    </button>
                                </div>
                            </div>
                        }
                    }
                />
            </div>
        </Show>
    }
}

/// 编辑器工具栏"批注"按钮(JS 侧走 content_root 的选区)
pub fn add_comment_tool(
    toolbar: &js_sys::Array,
    on_click: impl Fn() + 'static,
) -> Vec<JsValue> {
    let cb = wasm_bindgen::closure::Closure::<dyn FnMut(JsValue)>::new(move |_| on_click());
    let tool = js_sys::Object::new();
    let set = |k: &str, v: &JsValue| {
        js_sys::Reflect::set(&tool, &JsValue::from_str(k), v).unwrap();
    };
    set("name", &JsValue::from_str("comment"));
    set(
        "icon",
        &JsValue::from_str(
            "<svg viewBox='0 0 24 24' width='14' height='14' fill='currentColor'><path d='M4 3h16a1 1 0 0 1 1 1v11a1 1 0 0 1-1 1H9l-5 4V4a1 1 0 0 1 1-1z'/></svg>",
        ),
    );
    set("tip", &JsValue::from_str("对选中文字添加批注"));
    set("click", cb.as_ref().unchecked_ref());
    // 在 fontColor 后插入(没有则追加)
    let rebuilt = js_sys::Array::new();
    let mut inserted = false;
    for item in toolbar.iter() {
        rebuilt.push(&item);
        if !inserted && item.as_string().as_deref() == Some("fontColor") {
            rebuilt.push(&tool);
            inserted = true;
        }
    }
    if !inserted {
        rebuilt.push(&tool);
    }
    toolbar.set_length(0);
    for item in rebuilt.iter() {
        toolbar.push(&item);
    }
    vec![cb.into_js_value()]
}

/// 兼容 main.rs 的 document 私有 fn
pub fn document() -> web_sys::Document {
    window().document().expect("document")
}
