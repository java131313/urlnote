//! 画图 —— 画廊(#/draw)与画布(#/draw/{id}),仿 Windows 画图的基础功能:
//! 画笔/直线/矩形/椭圆/橡皮/文字,颜色与线宽,撤销、清空、自动保存。
//! 数据在 MongoDB draw 集合(整文档),元素数组回放渲染。

import { useCallback, useEffect, useRef, useState } from 'react';
import { api, DrawingDoc, DrawingMeta, newNoteId } from './api';

const CW = 960;
const CH = 600;

type Pt = [number, number];
type DrawElement =
  | { kind: 'pen' | 'eraser'; color: string; width: number; points: Pt[] }
  | { kind: 'line' | 'rect' | 'ellipse'; color: string; width: number; x0: number; y0: number; x1: number; y1: number }
  | { kind: 'text'; color: string; x: number; y: number; fontSize: number; text: string };

type Tool = 'pen' | 'line' | 'rect' | 'ellipse' | 'eraser' | 'text';

const TOOLS: { id: Tool; label: string; tip: string }[] = [
  { id: 'pen', label: '✏️', tip: '画笔' },
  { id: 'line', label: '📏', tip: '直线' },
  { id: 'rect', label: '▭', tip: '矩形' },
  { id: 'ellipse', label: '◯', tip: '椭圆' },
  { id: 'eraser', label: '🧽', tip: '橡皮' },
  { id: 'text', label: '🔤', tip: '文字(点击画布输入)' },
];

const COLORS = ['#000000', '#ffffff', '#e74c3c', '#f59e0b', '#2ecc71', '#3498db', '#8e44ad', '#7f8c8d'];
const WIDTHS = [1, 3, 5, 8];

/// 路由入口:App 在 hash 以 draw 开头时渲染本组件。
export function DrawRoute() {
  const id = location.hash.replace(/^#\/?draw\/?/, '').trim();
  return id ? <DrawEditor id={id} /> : <DrawGallery />;
}

function drawElement(ctx: CanvasRenderingContext2D, el: DrawElement): void {
  ctx.lineCap = 'round';
  ctx.lineJoin = 'round';
  if ('points' in el) {
    if (el.points.length < 2) {
      const [x, y] = el.points[0] ?? [0, 0];
      ctx.beginPath();
      ctx.arc(x, y, el.width / 2, 0, Math.PI * 2);
      ctx.fillStyle = el.color;
      ctx.fill();
      return;
    }
    ctx.save();
    if (el.kind === 'eraser') ctx.globalCompositeOperation = 'destination-out';
    ctx.strokeStyle = el.color;
    ctx.lineWidth = el.width * 3; // 橡皮粗一点
    ctx.beginPath();
    ctx.moveTo(el.points[0][0], el.points[0][1]);
    for (const [x, y] of el.points.slice(1)) ctx.lineTo(x, y);
    ctx.stroke();
    ctx.restore();
    return;
  }
  if ('text' in el) {
    ctx.fillStyle = el.color;
    ctx.font = `${el.fontSize}px sans-serif`;
    ctx.fillText(el.text, el.x, el.y);
    return;
  }
  ctx.strokeStyle = el.color;
  ctx.lineWidth = el.width;
  ctx.beginPath();
  if (el.kind === 'line') {
    ctx.moveTo(el.x0, el.y0);
    ctx.lineTo(el.x1, el.y1);
  } else if (el.kind === 'rect') {
    ctx.rect(Math.min(el.x0, el.x1), Math.min(el.y0, el.y1), Math.abs(el.x1 - el.x0), Math.abs(el.y1 - el.y0));
  } else {
    ctx.ellipse(
      (el.x0 + el.x1) / 2,
      (el.y0 + el.y1) / 2,
      Math.abs(el.x1 - el.x0) / 2,
      Math.abs(el.y1 - el.y0) / 2,
      0,
      0,
      Math.PI * 2,
    );
  }
  ctx.stroke();
}

// ---------------- 画廊 ----------------

function DrawGallery() {
  const [items, setItems] = useState<DrawingMeta[]>([]);
  const [loading, setLoading] = useState(true);
  const [err, setErr] = useState('');

  const refresh = useCallback(() => {
    setLoading(true);
    api
      .drawings()
      .then((d) => setItems(d.drawings))
      .catch((e) => setErr(String(e)))
      .finally(() => setLoading(false));
  }, []);
  useEffect(refresh, [refresh]);

  const create = () => {
    const name = window.prompt('新画板名称:', `未命名 ${new Date().toLocaleString()}`);
    if (name === null) return;
    location.hash = `/draw/${newNoteId()}`;
  };

  return (
    <div className="app">
      <div className="topbar">
        <a className="tool" href="#/" title="返回便签">
          📝 便签
        </a>
        <a className="tool" href="#/oss" title="对象存储文件库">
          🗂 文件库
        </a>
        <span className="brand">🎨 画图</span>
        <button type="button" className="tool" onClick={create}>
          ＋ 新建画板
        </button>
        <span className="spacer" />
        <button type="button" className="tool" onClick={refresh}>
          🔄 刷新
        </button>
      </div>
      <div className="draw-gallery">
        {loading && <div className="boot">加载中…</div>}
        {err && <div className="oss-empty">{err}</div>}
        {!loading && !items.length && !err && <div className="oss-empty">还没有画板,点「＋ 新建画板」开始</div>}
        {items.map((m) => (
          <div key={m.id} className="draw-card">
            <a className="draw-card-name" href={`#/draw/${m.id}`}>
              {m.name || '(未命名)'}
            </a>
            <span className="draw-card-time">{m.updated_at}</span>
            <button
              type="button"
              className="tool"
              onClick={() => {
                if (window.confirm(`删除画板「${m.name || m.id}」?`)) {
                  api.deleteDrawing(m.id).finally(refresh);
                }
              }}
            >
              🗑
            </button>
          </div>
        ))}
      </div>
    </div>
  );
}

// ---------------- 画布编辑器 ----------------

function DrawEditor({ id }: { id: string }) {
  const [doc, setDoc] = useState<DrawingDoc | null>(null);
  const [err, setErr] = useState('');
  const [tool, setTool] = useState<Tool>('pen');
  const [color, setColor] = useState('#000000');
  const [width, setWidth] = useState(3);
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);

  const canvasRef = useRef<HTMLCanvasElement>(null);
  const stateRef = useRef({
    elements: [] as DrawElement[],
    inProgress: null as DrawElement | null,
    drawing: false,
    start: [0, 0] as Pt,
    dirty: false,
    saveTimer: 0,
  });
  const st = stateRef.current;

  // 初始载入
  useEffect(() => {
    api
      .drawing(id)
      .then((d) => {
        st.elements = (d.elements || []) as DrawElement[];
        setDoc(d);
        setDirty(false);
      })
      .catch((e) => setErr(String(e)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id]);

  const render = useCallback(() => {
    const cv = canvasRef.current;
    if (!cv) return;
    const ctx = cv.getContext('2d');
    if (!ctx) return;
    ctx.fillStyle = '#ffffff';
    ctx.fillRect(0, 0, CW, CH);
    for (const el of st.elements) drawElement(ctx, el);
    if (st.inProgress) drawElement(ctx, st.inProgress);
  }, [st]);

  useEffect(() => {
    render();
  }, [render, doc]);

  const pos = (e: React.MouseEvent): Pt => {
    const r = canvasRef.current!.getBoundingClientRect();
    return [((e.clientX - r.left) / r.width) * CW, ((e.clientY - r.top) / r.height) * CH];
  };

  const scheduleSave = useCallback(() => {
    window.clearTimeout(st.saveTimer);
    st.saveTimer = window.setTimeout(() => {
      setSaving(true);
      api
        .saveDrawing(id, {
          name: doc?.name || '',
          width: CW,
          height: CH,
          elements: st.elements as unknown[],
        })
        .then((d) => {
          setDoc(d);
          st.dirty = false;
          setDirty(false);
        })
        .catch(() => setErr('保存失败'))
        .finally(() => setSaving(false));
    }, 800);
  }, [doc?.name, id, st]);

  const markDirty = useCallback(() => {
    st.dirty = true;
    setDirty(true);
    scheduleSave();
  }, [scheduleSave, st]);

  const onDown = (e: React.MouseEvent) => {
    if (!doc) return;
    const [x, y] = pos(e);
    if (tool === 'text') {
      const text = window.prompt('文字内容:')?.trim();
      if (text) {
        st.elements.push({ kind: 'text', color, x, y, fontSize: 24, text: text.slice(0, 200) });
        markDirty();
        render();
      }
      return;
    }
    st.drawing = true;
    st.start = [x, y];
    if (tool === 'pen' || tool === 'eraser') {
      st.inProgress = { kind: tool, color, width, points: [[x, y]] };
    } else {
      st.inProgress = { kind: tool, color, width, x0: x, y0: y, x1: x, y1: y };
    }
    render();
  };

  const onMove = (e: React.MouseEvent) => {
    if (!st.drawing || !st.inProgress) return;
    const [x, y] = pos(e);
    const ip = st.inProgress;
    if ('points' in ip) {
      ip.points.push([x, y]);
    } else if (!('text' in ip)) {
      ip.x1 = x;
      ip.y1 = y;
    }
    render();
  };

  const onUp = () => {
    if (!st.drawing || !st.inProgress) return;
    st.drawing = false;
    st.elements.push(st.inProgress);
    st.inProgress = null;
    markDirty();
    render();
  };

  const undo = () => {
    st.elements.pop();
    markDirty();
    render();
  };

  if (err && !doc) {
    return (
      <div className="app">
        <div className="topbar">
          <a className="tool" href="#/draw">🎨 画廊</a>
          <span className="brand">🎨 画图</span>
        </div>
        <div className="oss-empty">{err}</div>
      </div>
    );
  }
  if (!doc) return <div className="boot">加载中…</div>;

  return (
    <div className="app">
      <div className="topbar">
        <a className="tool" href="#/" title="返回便签">📝 便签</a>
        <a className="tool" href="#/oss" title="对象存储文件库">🗂 文件库</a>
        <a className="tool" href="#/draw" title="返回画廊">🎨 画廊</a>
        <span className="brand">🎨 {doc.name || '(未命名)'}</span>
        <input
          className="draw-name"
          value={doc.name}
          placeholder="画板名称"
          onChange={(e) => setDoc({ ...doc, name: e.target.value })}
          onBlur={() => markDirty()}
        />
        <button type="button" className="tool" onClick={undo} disabled={!st.elements.length}>
          ↩ 撤销
        </button>
        <button
          type="button"
          className="tool"
          onClick={() => {
            if (window.confirm('清空画布?')) {
              st.elements = [];
              markDirty();
              render();
            }
          }}
        >
          🗑 清空
        </button>
        <span className="spacer" />
        {err && <span className="dirty-dot">{err}</span>}
        {saving ? '保存中…' : dirty ? '● 未保存' : '已保存'}
      </div>
      <div className="draw-tools">
        {TOOLS.map((t) => (
          <button
            key={t.id}
            type="button"
            className={`tool ${tool === t.id ? 'active' : ''}`}
            title={t.tip}
            onClick={() => setTool(t.id)}
          >
            {t.label}
          </button>
        ))}
        <span className="draw-sep" />
        {COLORS.map((c) => (
          <button
            key={c}
            type="button"
            className={`draw-color ${color === c ? 'active' : ''}`}
            style={{ background: c }}
            title={c}
            onClick={() => setColor(c)}
          />
        ))}
        <input
          type="color"
          value={color}
          title="自定义颜色"
          onChange={(e) => setColor(e.target.value)}
        />
        <span className="draw-sep" />
        {WIDTHS.map((w) => (
          <button
            key={w}
            type="button"
            className={`tool ${width === w ? 'active' : ''}`}
            title={`线宽 ${w}`}
            onClick={() => setWidth(w)}
          >
            <span style={{ fontSize: 8 + w * 2 }}>●</span>
          </button>
        ))}
      </div>
      <div className="draw-stage">
        <canvas
          ref={canvasRef}
          width={CW}
          height={CH}
          className="draw-canvas"
          onMouseDown={onDown}
          onMouseMove={onMove}
          onMouseUp={onUp}
          onMouseLeave={onUp}
        />
      </div>
    </div>
  );
}
