import { useCallback, useEffect, useState } from 'react';
import { api, newNoteId } from './api';
import { Editor } from './Editor';
import { Login } from './Login';
import { Picker } from './Picker';
import { OssRoute } from './oss/OssPage';

function noteIdFromHash(): string {
  const h = location.hash.replace(/^#\/?/, '');
  return /^[a-zA-Z0-9_-]{1,64}$/.test(h) ? h : newNoteId();
}

/// 保留首段:对象存储文件库页(#/oss;与 Rust 前端一致,oss 页在登录门之外)
function isOssHash(): boolean {
  return location.hash.replace(/^#\/?/, '') === 'oss';
}

export default function App() {
  const [user, setUser] = useState<string | null | undefined>(undefined);
  const [noteId, setNoteId] = useState(noteIdFromHash);
  const [dirty, setDirty] = useState(false);
  const [oss, setOss] = useState(isOssHash);

  useEffect(() => {
    api
      .whoami()
      .then((d) => setUser(d.user.username))
      .catch(() => setUser(null));
  }, []);

  useEffect(() => {
    const onHash = () => {
      setOss(isOssHash());
      setNoteId(noteIdFromHash());
    };
    window.addEventListener('hashchange', onHash);
    return () => window.removeEventListener('hashchange', onHash);
  }, []);

  const openNote = useCallback((id: string) => {
    location.hash = '/' + id;
    setNoteId(id);
  }, []);

  // oss 页有自己的顶栏与编辑器;S3 凭证编译进包,不做登录要求(同 Rust)
  if (oss) return <OssRoute />;

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
        <a className="tool oss-link" href="#/oss" title="对象存储文件库">
          🗂 文件库
        </a>
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
