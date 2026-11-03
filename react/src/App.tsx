import { useCallback, useEffect, useState } from 'react';
import { api, newNoteId } from './api';
import { Editor } from './Editor';
import { Login } from './Login';
import { Picker } from './Picker';

function noteIdFromHash(): string {
  const h = location.hash.replace(/^#\/?/, '');
  return /^[a-zA-Z0-9_-]{1,64}$/.test(h) ? h : newNoteId();
}

export default function App() {
  const [user, setUser] = useState<string | null | undefined>(undefined);
  const [noteId, setNoteId] = useState(noteIdFromHash);
  const [dirty, setDirty] = useState(false);

  useEffect(() => {
    api
      .whoami()
      .then((d) => setUser(d.user.username))
      .catch(() => setUser(null));
  }, []);

  useEffect(() => {
    const onHash = () => setNoteId(noteIdFromHash());
    window.addEventListener('hashchange', onHash);
    return () => window.removeEventListener('hashchange', onHash);
  }, []);

  const openNote = useCallback((id: string) => {
    location.hash = '/' + id;
    setNoteId(id);
  }, []);

  if (user === undefined) return <div className="boot">加载中…</div>;
  if (user === null) return <Login onOk={() => setUser('')} />;

  return (
    <div className="app">
      <div className="topbar">
        <span className="brand">📝 urlnote</span>
        <Picker current={noteId} onPick={openNote} />
        <button type="button" className="tool" onClick={() => openNote(newNoteId())}>
          ＋ 新建
        </button>
        <span className="spacer" />
        {dirty && <span className="dirty-dot">● 未保存</span>}
        <span className="whoami">{user}</span>
        <button
          type="button"
          className="tool"
          onClick={() => {
            api.logout().finally(() => setUser(null));
          }}
        >
          退出
        </button>
      </div>
      {noteId && <Editor key={noteId} noteId={noteId} canEdit onSaved={() => {}} onDirty={setDirty} />}
    </div>
  );
}
