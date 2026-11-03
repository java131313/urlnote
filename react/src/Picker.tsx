import { useEffect, useState } from 'react';
import { api, NoteMeta } from './api';

// 便签列表 + 全文搜索(跨便签)
export function Picker(props: { current: string | null; onPick: (id: string) => void }) {
  const [items, setItems] = useState<NoteMeta[]>([]);
  const [q, setQ] = useState('');
  const [open, setOpen] = useState(false);

  useEffect(() => {
    if (!open) return;
    const t = window.setTimeout(() => {
      (q.trim() ? api.search(q.trim()) : api.notes())
        .then((d) => setItems(d.notes))
        .catch(() => {});
    }, 200);
    return () => window.clearTimeout(t);
  }, [q, open]);

  return (
    <div className="picker-wrap">
      <button type="button" className="tool" onClick={() => setOpen(!open)}>
        {open ? '收起' : '便签'}
      </button>
      {open && (
        <div className="picker" onMouseDown={(e) => e.preventDefault()}>
          <input
            autoFocus
            className="picker-search"
            placeholder="搜索(全文,词级匹配)…"
            value={q}
            onChange={(e) => setQ(e.target.value)}
          />
          <div className="picker-list">
            {items.map((n) => (
              <div
                key={n.id}
                className={'picker-item' + (n.id === props.current ? ' active' : '')}
                onClick={() => {
                  props.onPick(n.id);
                  setOpen(false);
                }}
              >
                <div className="picker-title">{n.title || n.id}</div>
                <div className="picker-excerpt">{n.excerpt}</div>
                <div className="picker-meta">
                  {n.author} · {n.updated_at}
                </div>
              </div>
            ))}
            {items.length === 0 && <div className="picker-empty">无匹配便签</div>}
          </div>
        </div>
      )}
    </div>
  );
}
