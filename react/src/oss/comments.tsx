//! 批注(锚定文字)—— rust/frontend/src/comment.rs 的移植。
//! 选中文字 → 添加批注 → 侧栏列表,点击跳转到原文。
//! doc 为 "note/<id>" 或 "oss/<key>",数据在 MongoDB(走 rust server API)。
//! 锚定 = 引用文本 + 字符偏移提示;文档编辑后偏移会漂移,跳转时按引用
//! 文本重新定位(取离原偏移最近的出现处)。
import { useCallback, useEffect, useRef, useState } from 'react';
import { api, CommentItem } from '../api';
import { newCommentId, stashSelection, wrapAnchor } from './editorTools';

// ---- 文档级委托:点击正文 <a class="cmt"> → 打开侧栏并定位批注 ----
// 每页最多一个激活控制器,卸载时注销(与 Rust 的 register_ctl 同角色)

interface ActiveCtl {
  focusItem(cid: string): void;
}

let activeCtl: ActiveCtl | null = null;
let delegationInstalled = false;

function installDelegation(): void {
  if (delegationInstalled) return;
  delegationInstalled = true;
  document.addEventListener('click', (e) => {
    const el = (e.target as Element | null)
      ?.closest?.('a.cmt');
    if (!el) return;
    const id = el.getAttribute('id');
    if (!id || !id.startsWith('cm-')) return;
    activeCtl?.focusItem(id.slice(3));
  });
}

export function registerCommentCtl(ctl: ActiveCtl | null): void {
  activeCtl = ctl;
  installDelegation();
}

// ---- 状态提示走页面的 status(回调注入) ----

// ---- 选区 / 定位(字符偏移) ----

interface NodeInfo {
  node: Text;
  text: string;
  base: number; // 全文起始字符偏移
}

function collectNodes(el: Element): { nodes: NodeInfo[]; full: string } {
  const nodes: NodeInfo[] = [];
  let acc = 0;
  const inner = (e: Element) => {
    for (const node of Array.from(e.childNodes)) {
      if (node.nodeType === Node.TEXT_NODE) {
        const t = node as Text;
        const text = t.textContent || '';
        if (text.trim() !== '') nodes.push({ node: t, text, base: acc });
        acc += [...text].length;
      } else if (node.nodeType === Node.ELEMENT_NODE) {
        const ce = node as Element;
        if (!ce.closest('code, style, script')) {
          inner(ce);
        } else {
          acc += [...(ce.textContent || '')].length;
        }
      }
    }
  };
  inner(el);
  return { nodes, full: nodes.map((n) => n.text).join('') };
}

/// UTF-16 DOM 偏移 → 字符偏移
function utf16ToChars(text: string, utf16Off: number): number {
  let count = 0;
  let units = 0;
  for (const ch of text) {
    if (units >= utf16Off) break;
    units += ch.length > 0xffff ? 2 : 1;
    count++;
  }
  return count;
}

/// 字符偏移 → UTF-16 DOM 偏移
function charsToUtf16(text: string, charOff: number): number {
  let units = 0;
  let count = 0;
  for (const ch of text) {
    if (count >= charOff) break;
    units += ch.length > 0xffff ? 2 : 1;
    count++;
  }
  return units;
}

/// 当前选区 → (引用文本, 全文起始字符, 全文结束字符);选区必须在 root 内
function selectionSpan(root: Element): { quote: string; start: number; end: number } | null {
  const { nodes, full } = collectNodes(root);
  const sel = window.getSelection();
  if (!sel || sel.isCollapsed || !sel.anchorNode || !sel.focusNode) return null;
  const posOf = (node: Node, off: number): number | null => {
    const hit = nodes.find((n) => n.node === node);
    return hit ? hit.base + utf16ToChars(hit.text, off) : null;
  };
  const a = posOf(sel.anchorNode, sel.anchorOffset);
  const f = posOf(sel.focusNode, sel.focusOffset);
  if (a === null || f === null) return null;
  const [start, end] = a <= f ? [a, f] : [f, a];
  const chars = [...full];
  if (start >= chars.length) return null;
  const quote = chars.slice(start, Math.min(end, chars.length)).join('').slice(0, 200);
  return { quote, start, end };
}

/// 按引用文本重定位(取离 hint 最近的出现处),返回 DOM Range
function placeRange(nodes: NodeInfo[], fullChars: string[], quote: string, hint: number): Range | null {
  const needle = [...quote];
  if (needle.length === 0 || needle.length > fullChars.length) return null;
  const occ: number[] = [];
  for (let i = 0; i <= fullChars.length - needle.length; i++) {
    if (fullChars.slice(i, i + needle.length).join('') === quote) occ.push(i);
    if (occ.length >= 200) break;
  }
  if (occ.length === 0) return null;
  const start = occ.reduce((best, i) => (Math.abs(i - hint) < Math.abs(best - hint) ? i : best));
  const end = start + needle.length;
  let startLoc: { node: Text; off: number } | null = null;
  let endLoc: { node: Text; off: number } | null = null;
  for (const n of nodes) {
    const len = [...n.text].length;
    if (!startLoc && start >= n.base && (start < n.base + len || (start === n.base && len === 0))) {
      startLoc = { node: n.node, off: charsToUtf16(n.text, start - n.base) };
    }
    if (!endLoc && end > n.base && end <= n.base + len) {
      endLoc = { node: n.node, off: charsToUtf16(n.text, end - n.base) };
    }
  }
  if (!startLoc || !endLoc) return null;
  const range = document.createRange();
  try {
    range.setStart(startLoc.node, startLoc.off);
    range.setEnd(endLoc.node, endLoc.off);
    return range;
  } catch {
    return null;
  }
}

/// 按引用文本在编辑器里重定位并选中
function selectQuote(root: Element, quote: string, hint: number, onHint: (m: string) => void): void {
  if (!quote) return;
  const { nodes, full } = collectNodes(root);
  const range = placeRange(nodes, [...full], quote, hint);
  if (!range) {
    onHint('原文已变化,找不到锚定文字');
    return;
  }
  const sel = window.getSelection();
  sel?.removeAllRanges();
  sel?.addRange(range);
  const rect = range.getBoundingClientRect();
  // 编辑器内容区滚动到选中处(编辑器自身是滚动容器)
  const content = root.closest('.vditor-content');
  if (content) {
    const shift = rect.top - 120;
    const next = Math.max(0, content.scrollTop + shift);
    content.scrollTop = next;
  }
  flashRect(rect);
}

/// 跳转后的黄色闪烁标记,1.3s 后自删
export function flashRect(rect: DOMRect): void {
  const m = document.createElement('div');
  m.className = 'comment-flash';
  m.setAttribute(
    'style',
    `left:${Math.max(0, rect.left)}px;top:${Math.max(0, rect.top)}px;width:${Math.max(30, rect.width)}px;height:${Math.max(20, rect.height)}px;`,
  );
  document.body.appendChild(m);
  window.setTimeout(() => m.remove(), 1300);
}

function selectAndFlash(el: Element): void {
  const sel = window.getSelection();
  const range = document.createRange();
  range.selectNodeContents(el);
  sel?.removeAllRanges();
  sel?.addRange(range);
  const rect = el.getBoundingClientRect();
  const content = el.closest('.vditor-content');
  if (content) {
    const shift = rect.top - 120;
    content.scrollTop = Math.max(0, content.scrollTop + shift);
  }
  flashRect(range.getBoundingClientRect());
}

// ---- 控制器 hook ----

export interface CommentsCtl {
  items: CommentItem[];
  open: boolean;
  busy: boolean;
  setContext(doc: string): void;
  ensureOpen(doc: string): void;
  close(): void;
  addFromSelection(): void;
  remove(id: string): void;
  jump(item: CommentItem): void;
}

export function useComments(
  getRoot: () => Element | null,
  onHint: (m: string) => void,
): CommentsCtl {
  const [doc, setDoc] = useState('');
  const [contextDoc, setContextDoc] = useState('');
  const [items, setItems] = useState<CommentItem[]>([]);
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const stateRef = useRef({ doc: '', contextDoc: '', open: false, items: [] as CommentItem[] });
  stateRef.current = { doc, contextDoc, open, items };

  const refresh = useCallback((d: string) => {
    if (!d) return;
    api
      .comments(d)
      .then((l) => setItems(l.comments))
      .catch(() => {});
  }, []);

  const setContext = useCallback(
    (d: string) => {
      if (stateRef.current.contextDoc === d) return;
      setContextDoc(d);
      if (!stateRef.current.open) {
        setDoc(d);
        refresh(d);
      }
    },
    [refresh],
  );

  const ensureOpen = useCallback(
    (d: string) => {
      const s = stateRef.current;
      if (s.doc !== d || !s.open) {
        setDoc(d);
        setOpen(true);
        refresh(d);
      }
    },
    [refresh],
  );

  const close = useCallback(() => setOpen(false), []);

  const activeDoc = () => (stateRef.current.open ? stateRef.current.doc : stateRef.current.contextDoc);

  /// 用当前选区创建批注(选区必须在内容根内)。
  /// 成功后选中文字被包成 <a id="cm-<id>" class="cmt"> 写进 markdown。
  const addFromSelection = useCallback(() => {
    // 先暂存选区:后续 prompt / POST 期间选区可能丢失
    stashSelection();
    const root = getRoot();
    if (!root) {
      onHint('请先在编辑器里选中要批注的文字');
      return;
    }
    const span = selectionSpan(root);
    if (!span) {
      onHint('请先在编辑器里选中要批注的文字');
      return;
    }
    if (!span.quote.trim()) {
      onHint('选区里没有文字');
      return;
    }
    const text = window.prompt('批注内容:')?.trim() ?? '';
    if (!text) return;
    const d = activeDoc();
    const cid = newCommentId();
    setBusy(true);
    api
      .upsertComment({ id: cid, doc: d, quote: span.quote, text, start: span.start, end: span.end })
      .then(() => {
        // 批注入库后再包锚点(用暂存的选区),文档随之变脏走保存
        wrapAnchor(cid);
        refresh(d);
      })
      .catch(() => onHint('批注保存失败'))
      .finally(() => setBusy(false));
  }, [getRoot, onHint, refresh]);

  const remove = useCallback(
    (id: string) => {
      const d = stateRef.current.doc;
      api
        .deleteComment(d, id)
        .then(() => refresh(d))
        .catch(() => onHint('删除失败(仅作者或管理员)'));
    },
    [onHint, refresh],
  );

  /// 跳转到批注锚点:优先 <a id="cm-.."> 锚点元素,回退按引用文本重定位
  const jump = useCallback(
    (item: CommentItem) => {
      const el = document.getElementById(`cm-${item.id}`);
      if (el) {
        selectAndFlash(el);
        return;
      }
      const root = getRoot();
      if (root) selectQuote(root, item.quote, item.start, onHint);
    },
    [getRoot, onHint],
  );

  const flashItem = useCallback((cid: string) => {
    window.setTimeout(() => {
      const el = document.querySelector(`.comment-item[data-cid='${cid}']`);
      if (!el) return;
      el.setAttribute('data-flash', '1');
      (el as HTMLElement).scrollIntoView({ block: 'nearest' });
      window.setTimeout(() => el.removeAttribute('data-flash'), 1600);
    }, 0);
  }, []);

  /// 点击正文锚点:侧栏打开/滚动到对应条目并闪烁
  const focusItem = useCallback(
    (cid: string) => {
      const s = stateRef.current;
      if (!s.open) {
        const d = s.contextDoc;
        setDoc(d);
        setOpen(true);
        refresh(d);
      }
      if (!stateRef.current.items.some((i) => i.id === cid)) {
        // 面板数据可能还没加载完,稍后重试一次
        window.setTimeout(() => flashItem(cid), 400);
        return;
      }
      flashItem(cid);
    },
    [flashItem, refresh],
  );

  useEffect(() => {
    registerCommentCtl({ focusItem });
    return () => registerCommentCtl(null);
  }, [focusItem]);

  return { items, open, busy, setContext, ensureOpen, close, addFromSelection, remove, jump };
}

// ---- 侧栏组件 ----

export function CommentDrawer(props: { ctl: CommentsCtl }) {
  const { ctl } = props;
  if (!ctl.open) return null;
  return (
    <div className="comment-drawer" onMouseDown={(e) => e.stopPropagation()}>
      <div className="comment-head">
        <strong>批注 · {ctl.items.length}</strong>
        <button type="button" className="tool" onClick={ctl.close}>
          关闭
        </button>
      </div>
      <div className="comment-hint">选中正文文字后点工具栏 💬 按钮添加</div>
      {ctl.items.map((c) => (
        <div key={c.id} className="comment-item" data-cid={c.id}>
          <div className="comment-quote" title="点击定位到原文" onClick={() => ctl.jump(c)}>
            {c.quote}
          </div>
          <div className="comment-text">{c.text}</div>
          <div className="comment-meta">
            <span>
              {c.author} · {c.created_at}
            </span>
            <button type="button" className="tool" onClick={() => ctl.remove(c.id)}>
              删除
            </button>
          </div>
        </div>
      ))}
    </div>
  );
}
