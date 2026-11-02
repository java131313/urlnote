//! wasm-bindgen bindings for the Vditor markdown editor (loaded from
//! `/vditor/dist/index.min.js` as a classic script, exposing `window.Vditor`).
//! Only the surface the app uses is bound.

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

/// 选区文字着色/去色的 JS 实现(eval 一次装入 window)。
/// 颜色以 class fc-<rrggbb> 写在 <span> 上(Vditor 的 DOM→md 往返丢弃
/// style 但保留 class,与 POS 标注 span.posw 同理),CSS 规则动态注入;
/// MutationObserver 负责回显/预览时补规则。代码块不着色。
const COLORIZE_JS: &str = r#"
// class 承载颜色:Vditor 的 DOM→md 往返丢弃 style 属性但保留 class
// (与 POS 标注 span.posw 同理),颜色以 fc-<rrggbb> 类名存进 markdown,
// 展示时由下方动态注入的 CSS 规则着色。
window.__urlnoteFcSet = {};
window.__urlnoteEnsureFc = function (hex) {
  hex = hex.toLowerCase();
  if (window.__urlnoteFcSet[hex]) return;
  window.__urlnoteFcSet[hex] = 1;
  let st = document.getElementById('urlnote-fc-styles');
  if (!st) {
    st = document.createElement('style');
    st.id = 'urlnote-fc-styles';
    document.head.appendChild(st);
  }
  st.textContent += '.fc-' + hex + '{color:#' + hex + '}';
};
// 加载/重新渲染(回显、切文件、预览)时扫描新出现的 fc- span 补规则
window.__urlnoteFcObserver = new MutationObserver(function (muts) {
  for (const m of muts) {
    for (const n of m.addedNodes) {
      if (n.nodeType !== 1) continue;
      const hit = [];
      if (n.matches && n.matches('span[class^="fc-"]')) hit.push(n);
      if (n.querySelectorAll) {
        const q = n.querySelectorAll('span[class^="fc-"]');
        for (let i = 0; i < q.length; i++) hit.push(q[i]);
      }
      for (const el of hit) {
        const cls = (el.className || '').match(/(?:^|\s)fc-([0-9a-fA-F]{6})(?:\s|$)/);
        if (cls) window.__urlnoteEnsureFc(cls[1]);
      }
    }
  }
});
window.__urlnoteFcObserver.observe(document.body, { childList: true, subtree: true });

window.__urlnoteWrapAnchor = function (cid, saved) {
  const sel = window.getSelection();
  let range =
    sel && sel.rangeCount && !sel.isCollapsed ? sel.getRangeAt(0) : saved || null;
  if (!range) return false;
  const anchor = range.commonAncestorContainer;
  const startEl = anchor.nodeType === 3 ? anchor.parentElement : anchor;
  const wysiwyg = startEl && startEl.closest ? startEl.closest('.vditor-wysiwyg') : null;
  if (!wysiwyg) return false;
  const targets = [];
  const walker = document.createTreeWalker(wysiwyg, NodeFilter.SHOW_TEXT, {
    acceptNode: function (t) {
      if (!t.data.trim()) return NodeFilter.FILTER_REJECT;
      if (!range.intersectsNode(t)) return NodeFilter.FILTER_REJECT;
      const p = t.parentElement;
      if (!p || p.closest('code')) return NodeFilter.FILTER_REJECT;
      // 已在批注锚点内则不重复包
      if (p.closest('a.cmt')) return NodeFilter.FILTER_REJECT;
      return NodeFilter.FILTER_ACCEPT;
    },
  });
  while (walker.nextNode()) targets.push(walker.currentNode);
  let changed = false;
  for (const t of targets) {
    let s0 = 0, e0 = t.data.length;
    if (t === range.startContainer) s0 = range.startOffset;
    if (t === range.endContainer) e0 = range.endOffset;
    if (s0 >= e0) continue;
    const r = document.createRange();
    r.setStart(t, s0);
    r.setEnd(t, e0);
    const a = document.createElement('a');
    a.setAttribute('id', 'cm-' + cid);
    a.setAttribute('class', 'cmt');
    a.setAttribute('data-cmt', '1');
    try { r.surroundContents(a); }
    catch (e) { const f = r.extractContents(); a.appendChild(f); r.insertNode(a); }
    changed = true;
  }
  if (changed) {
    if (sel) sel.removeAllRanges();
    window.__urlnoteSavedRange = null;
    wysiwyg.dispatchEvent(new InputEvent('input', { bubbles: true }));
  }
  return changed;
};
window.__urlnoteStash = function () {
  const sel = window.getSelection();
  window.__urlnoteSavedRange =
    sel && sel.rangeCount && !sel.isCollapsed ? sel.getRangeAt(0).cloneRange() : null;
};
window.__urlnoteColorize = function (color, saved) {
  const sel = window.getSelection();
  let range =
    sel && sel.rangeCount && !sel.isCollapsed ? sel.getRangeAt(0) : saved || null;
  if (!range) return false;
  const hex = String(color).replace('#', '').slice(0, 6).toLowerCase();
  if (!/^[0-9a-f]{6}$/.test(hex)) return false;
  window.__urlnoteEnsureFc(hex);
  const anchor = range.commonAncestorContainer;
  const startEl = anchor.nodeType === 3 ? anchor.parentElement : anchor;
  const wysiwyg = startEl && startEl.closest ? startEl.closest('.vditor-wysiwyg') : null;
  if (!wysiwyg) return false;
  const targets = [];
  const walker = document.createTreeWalker(wysiwyg, NodeFilter.SHOW_TEXT, {
    acceptNode: function (t) {
      if (!t.data.trim()) return NodeFilter.FILTER_REJECT;
      if (!range.intersectsNode(t)) return NodeFilter.FILTER_REJECT;
      const p = t.parentElement;
      if (!p || p.closest('code')) return NodeFilter.FILTER_REJECT;
      return NodeFilter.FILTER_ACCEPT;
    },
  });
  while (walker.nextNode()) targets.push(walker.currentNode);
  let changed = false;
  for (const t of targets) {
    let s0 = 0, e0 = t.data.length;
    if (t === range.startContainer) s0 = range.startOffset;
    if (t === range.endContainer) e0 = range.endOffset;
    if (s0 >= e0) continue;
    const r = document.createRange();
    r.setStart(t, s0);
    r.setEnd(t, e0);
    const span = document.createElement('span');
    span.setAttribute('class', 'fc-' + hex);
    try { r.surroundContents(span); }
    catch (e) { const f = r.extractContents(); span.appendChild(f); r.insertNode(span); }
    changed = true;
  }
  if (changed) {
    if (sel) sel.removeAllRanges();
    window.__urlnoteSavedRange = null;
    wysiwyg.dispatchEvent(new InputEvent('input', { bubbles: true }));
  }
  return changed;
};
window.__urlnoteUncolor = function (saved) {
  const sel = window.getSelection();
  let range =
    sel && sel.rangeCount && !sel.isCollapsed ? sel.getRangeAt(0) : saved || null;
  if (!range) return false;
  const anchor = range.commonAncestorContainer;
  const startEl = anchor.nodeType === 3 ? anchor.parentElement : anchor;
  const wysiwyg = startEl && startEl.closest ? startEl.closest('.vditor-wysiwyg') : null;
  if (!wysiwyg) return false;
  const spans = [];
  const walker = document.createTreeWalker(wysiwyg, NodeFilter.SHOW_ELEMENT, {
    acceptNode: function (el) {
      if (!range.intersectsNode(el)) return NodeFilter.FILTER_REJECT;
      if (el.tagName !== 'SPAN') return NodeFilter.FILTER_REJECT;
      if (/(?:^|\s)fc-[0-9a-fA-F]{6}(?:\s|$)/.test(el.className || ''))
        return NodeFilter.FILTER_ACCEPT;
      if (/(?:^|;)\s*color\s*:/.test(el.getAttribute('style') || ''))
        return NodeFilter.FILTER_ACCEPT;
      return NodeFilter.FILTER_REJECT;
    },
  });
  while (walker.nextNode()) spans.push(walker.currentNode);
  if (!spans.length) return false;
  for (const sp of spans) {
    const parent = sp.parentNode;
    while (sp.firstChild) parent.insertBefore(sp.firstChild, sp);
    parent.removeChild(sp);
  }
  if (sel) sel.removeAllRanges();
  window.__urlnoteSavedRange = null;
  wysiwyg.dispatchEvent(new InputEvent('input', { bubbles: true }));
  return true;
};
"#;

fn install_colorize() {
    thread_local! {
        static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    DONE.with(|d| {
        if !d.get() {
            let _ = js_sys::eval(COLORIZE_JS);
            d.set(true);
        }
    });
}

/// 生成 24 位 hex(与 Mongo ObjectId 同形),批注锚点 id
pub fn new_comment_id() -> String {
    let mut v = [0u8; 12];
    let _ = window()
        .crypto()
        .expect("crypto")
        .get_random_values_with_u8_array(&mut v);
    let mut s = String::with_capacity(24);
    for b in v {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 暂存当前选区(add 批注流程在 prompt/POST 前调用,防选区丢失)
pub fn wrap_prep_stash() {
    install_colorize();
    call_window0("__urlnoteStash");
}

/// 把当前选区(或暂存选区)包成 <a id="cm-<cid>" class="cmt">,写进 markdown
pub fn wrap_selection_as_anchor(cid: &str) {
    install_colorize();
    let g = js_sys::global();
    if let Ok(f) = js_sys::Reflect::get(&g, &JsValue::from_str("__urlnoteStash"))
        .and_then(|f| f.dyn_into::<js_sys::Function>())
    {
        let _ = f.call0(&g);
    }
    if let Ok(f) = js_sys::Reflect::get(&g, &JsValue::from_str("__urlnoteWrapAnchor"))
        .and_then(|f| f.dyn_into::<js_sys::Function>())
    {
        let _ = f.call1(&g, &JsValue::from_str(cid));
    }
}

fn window() -> web_sys::Window {
    web_sys::window().expect("window")
}

fn call_window0(name: &str) {
    let g = js_sys::global();
    if let Ok(f) = js_sys::Reflect::get(&g, &JsValue::from_str(name)) {
        if let Ok(f) = f.dyn_into::<js_sys::Function>() {
            let _ = f.call0(&g);
        }
    }
}

/// 在工具栏数组的 "strike" 之后插入"文字颜色"自定义按钮(选中文字 →
/// 取色 → 包 <span style="color:..">;Shift+点击清除选区颜色)。
/// 返回需要保活的 JsValue(closure/picker),调用方放进自己的 cbs。
pub fn add_font_color_tool(toolbar: &js_sys::Array) -> Vec<JsValue> {
    install_colorize();
    let doc = web_sys::window()
        .and_then(|w| w.document())
        .expect("document");
    let picker = doc
        .create_element("input")
        .expect("input")
        .dyn_into::<web_sys::HtmlInputElement>()
        .expect("HtmlInputElement");
    picker.set_type("color");
    let _ = picker.set_attribute("style", "position:fixed;top:-120px;left:-120px;");
    if let Some(body) = doc.body() {
        let _ = body.append_child(&picker);
    }

    // 取色完成 → 用保存的选区着色
    let picker_for_input = picker.clone();
    let oninput = Closure::<dyn FnMut()>::new(move || {
        let color = picker_for_input.value();
        let g = js_sys::global();
        let f = js_sys::Reflect::get(&g, &JsValue::from_str("__urlnoteColorize")).ok();
        let saved = js_sys::Reflect::get(&g, &JsValue::from_str("__urlnoteSavedRange")).unwrap_or(JsValue::NULL);
        if let Some(Ok(f)) = f.map(|f| f.dyn_into::<js_sys::Function>()) {
            let _ = f.call2(&g, &JsValue::from_str(&color), &saved);
        }
    });
    picker.set_oninput(Some(oninput.as_ref().unchecked_ref()));

    // 工具栏点击:先暂存选区(防按钮夺焦丢失),Shift=清除,否则打开取色器
    let picker_for_click = picker.clone();
    let click = Closure::<dyn FnMut(JsValue)>::new(move |ev: JsValue| {
        let shift = js_sys::Reflect::get(&ev, &JsValue::from_str("shiftKey"))
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        call_window0("__urlnoteStash");
        if shift {
            let g = js_sys::global();
            let saved = js_sys::Reflect::get(&g, &JsValue::from_str("__urlnoteSavedRange"))
                .unwrap_or(JsValue::NULL);
            if let Ok(f) = js_sys::Reflect::get(&g, &JsValue::from_str("__urlnoteUncolor"))
                .and_then(|f| f.dyn_into::<js_sys::Function>())
            {
                let _ = f.call1(&g, &saved);
            }
            return;
        }
        picker_for_click.click();
    });

    let tool = js_sys::Object::new();
    let set = |k: &str, v: &JsValue| {
        js_sys::Reflect::set(&tool, &JsValue::from_str(k), v).unwrap();
    };
    set("name", &JsValue::from_str("fontColor"));
    set(
        "icon",
        &JsValue::from_str(
            "<svg viewBox='0 0 24 24' width='14' height='14' fill='currentColor'><text x='2' y='15' font-size='14'>A</text><rect x='2' y='18' width='15' height='3.2' rx='1' fill='#e0245e'/></svg>",
        ),
    );
    set("tip", &JsValue::from_str("文字颜色(Shift+点击清除)"));
    set("click", click.as_ref().unchecked_ref());

    // 在 "strike" 之后插入;没有 strike 就追加
    let rebuilt = js_sys::Array::new();
    let mut inserted = false;
    for item in toolbar.iter() {
        if !inserted && item.as_string().as_deref() == Some("strike") {
            rebuilt.push(&tool);
            inserted = true;
        }
        rebuilt.push(&item);
    }
    if !inserted {
        rebuilt.push(&tool);
    }
    toolbar.set_length(0);
    for item in rebuilt.iter() {
        toolbar.push(&item);
    }

    vec![oninput.into_js_value(), click.into_js_value(), picker.into()]
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = Vditor)]
    pub type VditorEditor;

    #[wasm_bindgen(constructor, js_class = "Vditor")]
    pub fn new(el: &web_sys::Element, options: JsValue) -> VditorEditor;

    #[wasm_bindgen(method, js_name = getValue)]
    pub fn get_value(this: &VditorEditor) -> String;

    #[wasm_bindgen(method, js_name = setValue)]
    pub fn set_value(this: &VditorEditor, value: &str);

    #[wasm_bindgen(method, js_name = insertValue)]
    pub fn insert_value(this: &VditorEditor, value: &str, render: bool);

    #[wasm_bindgen(method, js_name = deleteValue)]
    pub fn delete_value(this: &VditorEditor);

    #[wasm_bindgen(method, js_name = setTheme)]
    pub fn set_theme(this: &VditorEditor, theme: &str);

    #[wasm_bindgen(method, js_name = focus)]
    pub fn focus(this: &VditorEditor);

    #[wasm_bindgen(method, js_name = destroy)]
    pub fn destroy(this: &VditorEditor);
}
