//! 选中文字着色/批注锚点的 DOM 操作 + 工具栏自定义按钮。
//! rust/frontend/src/vditor.rs COLORIZE_JS / add_font_color_tool / add_comment_tool 的 TS 移植
//! (eval 字符串改成真 TS 函数,逻辑逐行一致)。
//!
//! 颜色以 class fc-<rrggbb> 写在 <span> 上(Vditor 的 DOM→md 往返丢弃 style
//! 但保留 class,与 POS 标注 span.posw 同理),CSS 规则动态注入;
//! MutationObserver 负责回显/预览时补规则。代码块不着色。

let savedRange: Range | null = null;
const fcSet = new Set<string>();
let observerInstalled = false;

export function ensureFc(hex: string): void {
  const h = hex.toLowerCase();
  if (fcSet.has(h)) return;
  fcSet.add(h);
  let st = document.getElementById('urlnote-fc-styles');
  if (!st) {
    st = document.createElement('style');
    st.id = 'urlnote-fc-styles';
    document.head.appendChild(st);
  }
  st.textContent += `.fc-${h}{color:#${h}}`;
}

/// 加载/重新渲染(回显、切文件、预览)时扫描新出现的 fc- span 补规则
export function installFcObserver(): void {
  if (observerInstalled) return;
  observerInstalled = true;
  const observer = new MutationObserver((muts) => {
    for (const m of muts) {
      for (const n of Array.from(m.addedNodes)) {
        if (n.nodeType !== Node.ELEMENT_NODE) continue;
        const el = n as Element;
        const hit: Element[] = [];
        if (el.matches('span[class^="fc-"]')) hit.push(el);
        hit.push(...Array.from(el.querySelectorAll('span[class^="fc-"]')));
        for (const sp of hit) {
          const cls = (sp.getAttribute('class') || '').match(/(?:^|\s)fc-([0-9a-fA-F]{6})(?:\s|$)/);
          if (cls) ensureFc(cls[1]);
        }
      }
    }
  });
  observer.observe(document.body, { childList: true, subtree: true });
}

export function stashSelection(): void {
  installFcObserver();
  const sel = window.getSelection();
  savedRange = sel && sel.rangeCount > 0 && !sel.isCollapsed ? sel.getRangeAt(0).cloneRange() : null;
}

export function getSavedRange(): Range | null {
  return savedRange;
}

function forEachTargetText(
  range: Range,
  accept: (t: Text) => boolean,
  fn: (t: Text, s0: number, e0: number) => void,
): number {
  const anchor = range.commonAncestorContainer;
  const startEl = anchor.nodeType === Node.TEXT_NODE ? anchor.parentElement : (anchor as Element);
  const wysiwyg = startEl && startEl.closest ? startEl.closest('.vditor-wysiwyg') : null;
  if (!wysiwyg) return 0;
  const targets: Text[] = [];
  const walker = document.createTreeWalker(wysiwyg, NodeFilter.SHOW_TEXT, {
    acceptNode: (n) => {
      // SHOW_TEXT 遍历时只产出 Text 节点
      const t = n as Text;
      if (!t.data.trim()) return NodeFilter.FILTER_REJECT;
      if (!range.intersectsNode(t)) return NodeFilter.FILTER_REJECT;
      const p = t.parentElement;
      if (!p || p.closest('code')) return NodeFilter.FILTER_REJECT;
      return accept(t) ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_REJECT;
    },
  });
  while (walker.nextNode()) targets.push(walker.currentNode as Text);
  let applied = 0;
  for (const t of targets) {
    let s0 = 0;
    let e0 = t.data.length;
    if (t === range.startContainer) s0 = range.startOffset;
    if (t === range.endContainer) e0 = range.endOffset;
    if (s0 >= e0) continue;
    fn(t, s0, e0);
    applied++;
  }
  return applied;
}

function dispatchInput(wysiwyg: Element | null): void {
  wysiwyg?.dispatchEvent(new InputEvent('input', { bubbles: true }));
}

/// 把当前选区(或暂存选区)包成 <a id="cm-<cid>" class="cmt">,写进 markdown。
/// 已在批注锚点内的文字不重复包。
export function wrapAnchor(cid: string): boolean {
  installFcObserver();
  const sel = window.getSelection();
  const range = sel && sel.rangeCount > 0 && !sel.isCollapsed ? sel.getRangeAt(0) : savedRange;
  if (!range) return false;
  const anchor = range.commonAncestorContainer;
  const startEl = anchor.nodeType === Node.TEXT_NODE ? anchor.parentElement : (anchor as Element);
  const wysiwyg = startEl && startEl.closest ? startEl.closest('.vditor-wysiwyg') : null;
  if (!wysiwyg) return false;
  const wrapped = forEachTargetText(
    range,
    (t) => !(t.parentElement && t.parentElement.closest('a.cmt')),
    (t, s0, e0) => {
      const r = document.createRange();
      r.setStart(t, s0);
      r.setEnd(t, e0);
      const a = document.createElement('a');
      a.setAttribute('id', 'cm-' + cid);
      a.setAttribute('class', 'cmt');
      a.setAttribute('data-cmt', '1');
      try {
        r.surroundContents(a);
      } catch {
        const f = r.extractContents();
        a.appendChild(f);
        r.insertNode(a);
      }
    },
  ) > 0;
  if (wrapped) {
    if (sel) sel.removeAllRanges();
    savedRange = null;
    dispatchInput(wysiwyg);
  }
  return wrapped;
}

/// 选中文字着色(取色器完成时调用);saved 为暂存选区
export function colorize(color: string, saved: Range | null): boolean {
  const sel = window.getSelection();
  const range = sel && sel.rangeCount > 0 && !sel.isCollapsed ? sel.getRangeAt(0) : saved;
  if (!range) return false;
  const hex = String(color).replace('#', '').slice(0, 6).toLowerCase();
  if (!/^[0-9a-f]{6}$/.test(hex)) return false;
  ensureFc(hex);
  const anchor = range.commonAncestorContainer;
  const startEl = anchor.nodeType === Node.TEXT_NODE ? anchor.parentElement : (anchor as Element);
  const wysiwyg = startEl && startEl.closest ? startEl.closest('.vditor-wysiwyg') : null;
  if (!wysiwyg) return false;
  const changed =
    forEachTargetText(
      range,
      () => true,
      (t, s0, e0) => {
        const r = document.createRange();
        r.setStart(t, s0);
        r.setEnd(t, e0);
        const span = document.createElement('span');
        span.setAttribute('class', 'fc-' + hex);
        try {
          r.surroundContents(span);
        } catch {
          const f = r.extractContents();
          span.appendChild(f);
          r.insertNode(span);
        }
      },
    ) > 0;
  if (changed) {
    if (sel) sel.removeAllRanges();
    savedRange = null;
    dispatchInput(wysiwyg);
  }
  return changed;
}

/// 清除选区颜色(Shift+点击工具栏)
export function uncolor(saved: Range | null): boolean {
  const sel = window.getSelection();
  const range = sel && sel.rangeCount > 0 && !sel.isCollapsed ? sel.getRangeAt(0) : saved;
  if (!range) return false;
  const anchor = range.commonAncestorContainer;
  const startEl = anchor.nodeType === Node.TEXT_NODE ? anchor.parentElement : (anchor as Element);
  const wysiwyg = startEl && startEl.closest ? startEl.closest('.vditor-wysiwyg') : null;
  if (!wysiwyg) return false;
  const spans: Element[] = [];
  const walker = document.createTreeWalker(wysiwyg, NodeFilter.SHOW_ELEMENT, {
    acceptNode: (el) => {
      const e = el as Element;
      if (!range.intersectsNode(e)) return NodeFilter.FILTER_REJECT;
      if (e.tagName !== 'SPAN') return NodeFilter.FILTER_REJECT;
      if (/(?:^|\s)fc-[0-9a-fA-F]{6}(?:\s|$)/.test(e.getAttribute('class') || '')) return NodeFilter.FILTER_ACCEPT;
      if (/(?:^|;)\s*color\s*:/.test(e.getAttribute('style') || '')) return NodeFilter.FILTER_ACCEPT;
      return NodeFilter.FILTER_REJECT;
    },
  });
  while (walker.nextNode()) spans.push(walker.currentNode as Element);
  if (spans.length === 0) return false;
  for (const sp of spans) {
    const parent = sp.parentNode!;
    while (sp.firstChild) parent.insertBefore(sp.firstChild, sp);
    parent.removeChild(sp);
  }
  if (sel) sel.removeAllRanges();
  savedRange = null;
  dispatchInput(wysiwyg);
  return true;
}

// ---- 工具栏按钮 ----

/// "文字颜色"按钮(选中文字 → 取色 → 包 fc- span;Shift+点击清除)。
/// 取色器全局只建一个,便签页/文档库页反复进出也不会泄漏。
export function createFontColorTool(): IMenuItem {
  installFcObserver();
  let picker = document.getElementById('urlnote-fc-picker') as HTMLInputElement | null;
  if (!picker) {
    picker = document.createElement('input');
    picker.id = 'urlnote-fc-picker';
    picker.type = 'color';
    picker.style.position = 'fixed';
    picker.style.top = '-120px';
    picker.style.left = '-120px';
    document.body.appendChild(picker);
    picker.addEventListener('input', () => {
      colorize(picker!.value, getSavedRange());
    });
  }

  return {
    name: 'fontColor',
    icon: "<svg viewBox='0 0 24 24' width='14' height='14' fill='currentColor'><text x='2' y='15' font-size='14'>A</text><rect x='2' y='18' width='15' height='3.2' rx='1' fill='#e0245e'/></svg>",
    tip: '文字颜色(Shift+点击清除)',
    click: (event: Event) => {
      // 先暂存选区(防按钮夺焦丢失);Shift=清除,否则打开取色器
      stashSelection();
      const shift = (event as MouseEvent).shiftKey;
      if (shift) {
        uncolor(getSavedRange());
        return;
      }
      picker!.click();
    },
  };
}

/// "批注"按钮(选中文字 → 添加批注;具体动作由页面注入)
export function createCommentTool(onClick: () => void): IMenuItem {
  return {
    name: 'comment',
    icon: "<svg viewBox='0 0 24 24' width='14' height='14' fill='currentColor'><path d='M4 3h16a1 1 0 0 1 1 1v11a1 1 0 0 1-1 1H9l-5 4V4a1 1 0 0 1 1-1z'/></svg>",
    tip: '对选中文字添加批注',
    click: () => onClick(),
  };
}

/// 生成 24 位 hex(与 Mongo ObjectId 同形),批注锚点 id
export function newCommentId(): string {
  const v = new Uint8Array(12);
  crypto.getRandomValues(v);
  return Array.from(v).map((b) => b.toString(16).padStart(2, '0')).join('');
}
