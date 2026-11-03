// 词典提示(与 Rust 版同一契约):静态词表略去,只查服务端词典;
// lang 为空 = 自动。选中即把锚点范围内的文字替换为所选词。
import { useCallback, useEffect, useRef, useState } from 'react';
import { api, DictWord, DictLang } from './api';

export interface SuggestState {
  items: { word: string; lang: string; badge: string; hint: string; key: string }[];
  active: number;
  pos: { left: number; top: number };
  anchor: { node: Node; start: number; end: number } | null;
}

const isCjk = (c: string) => /[㐀-鿿぀-ヿ豈-﫿]/.test(c);
const isWordChar = (c: string) => /[\p{L}\p{N}_]/u.test(c);

function fragmentAtCursor(): { node: Text; start: number; end: number; text: string } | null {
  const sel = window.getSelection();
  // 与 Rust 版一致:只认光标(折叠选区);拖选展开时不提示
  if (!sel || !sel.isCollapsed || !sel.anchorNode || sel.anchorNode.nodeType !== Node.TEXT_NODE) {
    return null;
  }
  const node = sel.anchorNode as Text;
  const el = node.parentElement?.closest('.vditor-content');
  if (!el) return null;
  const text = node.textContent || '';
  const offset = sel.anchorOffset;
  let start = offset;
  while (start > 0 && isWordChar(text[start - 1])) start -= 1;
  return { node, start, end: offset, text: text.slice(start, offset) };
}

function wordsToItems(fragment: string, words: DictWord[]): SuggestState['items'] {
  const seen = new Set<string>();
  const out: SuggestState['items'] = [];
  const badge = (code: string) => ({ zh_cn: '简', zh_tw: '繁', ja: '日', en: '英' }[code] || '词');
  for (const w of words) {
    const hint = Object.values(w.trans).join(' · ');
    const key = `${w.lang}:${w.word}`;
    if (!seen.has(key)) {
      seen.add(key);
      out.push({ word: w.word, lang: w.lang, badge: badge(w.lang), hint, key });
    }
    if (w.word === fragment) {
      for (const [lang, tw] of Object.entries(w.trans)) {
        if (tw === fragment) continue;
        const k = `${lang}:${tw}`;
        if (seen.has(k)) continue;
        seen.add(k);
        const rest = Object.entries(w.trans)
          .filter(([, v]) => v !== tw && v !== fragment)
          .map(([, v]) => v)
          .join(' · ');
        out.push({ word: tw, lang, badge: badge(lang), hint: rest, key: k });
      }
    }
    if (out.length >= 10) break;
  }
  return out;
}

export function SuggestPanel(props: {
  state: SuggestState | null;
  onPick: (word: string) => void;
  onHover: (i: number) => void;
}) {
  const { state } = props;
  if (!state || state.items.length === 0) return null;
  return (
    <div className="suggest-panel" style={{ left: state.pos.left, top: state.pos.top }}>
      {state.items.map((it, i) => (
        <div
          key={it.key}
          className={'suggest-item' + (i === state.active ? ' active' : '')}
          onMouseEnter={() => props.onHover(i)}
          onMouseDown={(e) => {
            e.preventDefault();
            props.onPick(it.word);
          }}
        >
          <span className="suggest-word">{it.word}</span>
          <span className="suggest-hint">{it.hint}</span>
          <span className={'suggest-lang lang-' + it.lang}>{it.badge}</span>
        </div>
      ))}
    </div>
  );
}

export function useSuggest(
  editable: boolean,
  lang: string,
  onAccepted: () => void,
) {
  const [state, setState] = useState<SuggestState | null>(null);
  const [langs, setLangs] = useState<DictLang[]>([]);
  const seq = useRef(0);
  const accepted = useRef('');

  useEffect(() => {
    api.dictLangs().then((d) => setLangs(d.langs)).catch(() => {});
  }, []);

  const close = useCallback(() => setState(null), []);

  const accept = useCallback(
    (word: string) => {
      setState((st) => {
        if (st?.anchor) {
          const { node, start, end } = st.anchor;
          const sel = window.getSelection();
          const range = document.createRange();
          range.setStart(node, start);
          range.setEnd(node, end);
          sel?.removeAllRanges();
          sel?.addRange(range);
          // Vditor  wysiwyg:选中替换走 execCommand,再派发 input 使其重新取值
          document.execCommand('insertText', false, word);
          node.parentElement
            ?.closest('.vditor-wysiwyg')
            ?.dispatchEvent(new InputEvent('input', { bubbles: true }));
          accepted.current = word;
          onAccepted();
        }
        return null;
      });
    },
    [onAccepted],
  );

  // 键盘导航(document 捕获,先于 Vditor)
  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      if (!state || state.items.length === 0) return;
      if (!['ArrowDown', 'ArrowUp', 'Enter', 'Tab', 'Escape'].includes(e.key)) return;
      e.preventDefault();
      e.stopPropagation();
      if (e.key === 'ArrowDown') setState((s) => s && { ...s, active: (s.active + 1) % s.items.length });
      else if (e.key === 'ArrowUp') setState((s) => s && { ...s, active: (s.active + s.items.length - 1) % s.items.length });
      else if (e.key === 'Escape') setState(null);
      else accept(state.items[state.active].word);
    };
    document.addEventListener('keydown', h, true);
    return () => document.removeEventListener('keydown', h, true);
  }, [state, accept]);

  // 输入/选区变化 → 计算片段 → 查词典
  useEffect(() => {
    if (!editable) return;
    let timer: number | undefined;
    const update = () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(async () => {
        const frag = fragmentAtCursor();
        if (!frag) {
          setState(null);
          return;
        }
        const { text } = frag;
        if (accepted.current && accepted.current === text) {
          setState(null);
          return;
        }
        accepted.current = '';
        const n = [...text].length;
        const hasCjk = [...text].some(isCjk);
        const ok = hasCjk ? n >= 1 && n <= 8 : n >= 2 && n <= 16 && /^[a-zA-Z0-9]+$/.test(text);
        if (!ok) {
          setState(null);
          return;
        }
        const mySeq = ++seq.current;
        const pos = (() => {
          const sel = window.getSelection();
          if (!sel || sel.rangeCount === 0) return { left: 40, top: 40 };
          const r = sel.getRangeAt(0).getBoundingClientRect();
          return { left: r.left, top: r.bottom + 4 };
        })();
        try {
          const data = await api.dictLookup(text, lang);
          if (seq.current !== mySeq) return;
          const items = wordsToItems(text, data.words);
          if (items.length === 0) {
            setState(null);
            return;
          }
          setState({ items, active: 0, pos, anchor: { node: frag.node, start: frag.start, end: frag.end } });
        } catch {
          /* 网络/401 静默 */
        }
      }, 200);
    };
    document.addEventListener('input', update);
    document.addEventListener('selectionchange', update);
    return () => {
      window.clearTimeout(timer);
      document.removeEventListener('input', update);
      document.removeEventListener('selectionchange', update);
    };
  }, [editable, lang]);

  return { state, langs, accept, close, setActive: (i: number) => setState((s) => s && { ...s, active: i }) };
}
