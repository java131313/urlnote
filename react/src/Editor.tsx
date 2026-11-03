// 便签编辑器:Vditor(wysiwyg)+ 自动保存 + 字号/颜色 + 词典提示。
// 行为对齐 Rust 前端:内容脏检测防抖保存,提示面板锚点替换。
import { useCallback, useEffect, useRef, useState } from 'react';
import Vditor from 'vditor';
import 'vditor/dist/index.css';
import { api, Note } from './api';
import { SuggestPanel, useSuggest } from './suggest';

const DEFAULT_FONT = 16;
const FONT_STEP = 2;

export function Editor(props: {
  noteId: string;
  canEdit: boolean;
  onSaved: (note: Note) => void;
  onDirty: (dirty: boolean) => void;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  const vditorRef = useRef<Vditor | null>(null);
  const loadedFor = useRef<string>('');
  const dirty = useRef(false);
  const saveTimer = useRef<number>();
  const [note, setNote] = useState<Note | null>(null);
  const [fontSize, setFontSize] = useState(DEFAULT_FONT);
  const [color, setColor] = useState('');
  const [status, setStatus] = useState('');
  const [lang, setLang] = useState(() => localStorage.getItem('urlnote-suggest-lang') || (import.meta as any).env.VITE_SUGGEST_LANG || '');

  const markSaved = useCallback(
    (n: Note) => {
      dirty.current = false;
      props.onDirty(false);
      setStatus(`已保存 ${new Date().toLocaleTimeString('zh-CN', { hour12: false })}`);
      props.onSaved(n);
    },
    [props],
  );

  const doSave = useCallback(async () => {
    const vd = vditorRef.current;
    if (!vd || !dirty.current) return;
    setStatus('保存中…');
    try {
      const n = await api.saveNote(props.noteId, {
        content: vd.getValue(),
        title: note?.title || '',
        tags: note?.tags || [],
        font_size: fontSize,
        color,
      });
      markSaved(n);
    } catch (e) {
      setStatus('保存失败,将随下次编辑重试');
    }
  }, [props.noteId, note?.title, note?.tags, fontSize, color, markSaved]);

  const scheduleSave = useCallback(() => {
    window.clearTimeout(saveTimer.current);
    saveTimer.current = window.setTimeout(doSave, 800);
  }, [doSave]);

  const { state: suggest, langs, accept, setActive } = useSuggest(
    props.canEdit,
    lang,
    useCallback(() => {
      dirty.current = true;
      props.onDirty(true);
      scheduleSave();
    }, [props, scheduleSave]),
  );

  // 语言持久化
  useEffect(() => {
    if (lang) localStorage.setItem('urlnote-suggest-lang', lang);
    else localStorage.removeItem('urlnote-suggest-lang');
  }, [lang]);

  // 初始化 Vditor(每个便签 id 复用实例,内容用 setValue 切换)
  useEffect(() => {
    if (!hostRef.current || vditorRef.current) return;
    const vd = new Vditor(hostRef.current, {
      mode: 'wysiwyg',
      cdn: '/vditor',
      lang: 'zh_CN',
      placeholder: '打开即写,自动保存…',
      cache: { enable: false },
      preview: { theme: (window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light') as any },
      toolbar: [
        'headings', 'bold', 'italic', 'strike', '|', 'list', 'ordered-list', 'check', 'quote', 'line',
        'code', 'inline-code', 'link', 'table', '|', 'undo', 'redo', '|', 'edit-mode', 'preview',
      ],
      upload: {
        url: '/api/upload',
        fieldName: 'file[]',
        max: 8 * 1024 * 1024,
      },
      input: () => {
        dirty.current = true;
        props.onDirty(true);
        setStatus('有未保存的更改');
        scheduleSave();
      },
      after: () => {
        if (props.noteId && loadedFor.current !== props.noteId) {
          loadedFor.current = props.noteId;
          api
            .note(props.noteId)
            .then((n) => {
              setNote(n);
              setFontSize(n.font_size || DEFAULT_FONT);
              setColor(n.color || '');
              vd.setValue(n.content || '');
              dirty.current = false;
              props.onDirty(false);
              document.title = `${n.title || props.noteId} · urlnote`;
            })
            .catch((e) => setStatus(String(e)));
        }
      },
    });
    vditorRef.current = vd;
    return () => {
      vditorRef.current?.destroy();
      vditorRef.current = null;
      loadedFor.current = '';
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 切换便签:复用实例,重新装载
  useEffect(() => {
    const vd = vditorRef.current;
    if (!vd || !props.noteId || loadedFor.current === props.noteId) return;
    loadedFor.current = props.noteId;
    setNote(null);
    api
      .note(props.noteId)
      .then((n) => {
        setNote(n);
        setFontSize(n.font_size || DEFAULT_FONT);
        setColor(n.color || '');
        vd.setValue(n.content || '');
        dirty.current = false;
        props.onDirty(false);
        document.title = `${n.title || props.noteId} · urlnote`;
      })
      .catch((e) => setStatus(String(e)));
  }, [props.noteId, props]);

  // 卸载前落盘
  useEffect(
    () => () => {
      window.clearTimeout(saveTimer.current);
    },
    [],
  );

  const zoom = fontSize / DEFAULT_FONT;
  const style = { '--note-zoom': zoom, ...(color ? { '--note-color': color } : {}) } as React.CSSProperties;

  return (
    <div className="editor-page" style={style}>
      <div className="settings-row">
        <input
          className="title-input"
          placeholder="标题"
          value={note?.title || ''}
          onChange={(e) => {
            setNote((n) => (n ? { ...n, title: e.target.value } : n));
            dirty.current = true;
            props.onDirty(true);
            scheduleSave();
          }}
        />
        <input
          className="tags-input"
          placeholder="标签(逗号分隔)"
          value={(note?.tags || []).join(', ')}
          onChange={(e) => {
            const tags = e.target.value
              .split(/[,，]/)
              .map((t) => t.trim())
              .filter(Boolean)
              .slice(0, 10);
            setNote((n) => (n ? { ...n, tags } : n));
            dirty.current = true;
            props.onDirty(true);
            scheduleSave();
          }}
        />
        <button type="button" className="tool" title="缩小" onClick={() => setFontSize((f) => Math.max(10, f - FONT_STEP))}>
          A−
        </button>
        <button type="button" className="tool" title="放大" onClick={() => setFontSize((f) => Math.min(40, f + FONT_STEP))}>
          A+
        </button>
        {fontSize !== DEFAULT_FONT && (
          <button type="button" className="tool" onClick={() => setFontSize(DEFAULT_FONT)}>
            A
          </button>
        )}
        <input
          type="color"
          className="tool color"
          title="文字颜色"
          value={color || '#000000'}
          onChange={(e) => setColor(e.target.value)}
        />
        {color && (
          <button type="button" className="tool" onClick={() => setColor('')}>
            默认
          </button>
        )}
        <select className="tool lang-select" title="提示语言" value={lang} onChange={(e) => setLang(e.target.value)}>
          <option value="">自动</option>
          {langs.map((l) => (
            <option key={l.code} value={l.code}>
              {l.name}
            </option>
          ))}
        </select>
        <span className="status">{status}</span>
      </div>
      <div className="editor-host" ref={hostRef} />
      <SuggestPanel state={suggest} onPick={accept} onHover={setActive} />
    </div>
  );
}
