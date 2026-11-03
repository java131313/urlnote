import { FormEvent, useState } from 'react';
import { api } from './api';

export function Login(props: { onOk: () => void }) {
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError('');
    try {
      await api.login(username.trim(), password);
      props.onOk();
    } catch (err: any) {
      setError(err?.status === 401 ? '用户名或密码错误' : '登录失败,请检查后端连通性');
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="login-wrap">
      <form className="login-card" onSubmit={submit}>
        <strong>urlnote</strong>
        <p>登录后开始编辑便签</p>
        <input
          autoFocus
          placeholder="用户名"
          autoComplete="username"
          value={username}
          onChange={(e) => setUsername(e.target.value)}
          required
        />
        <input
          type="password"
          placeholder="密码"
          autoComplete="current-password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          required
        />
        <button type="submit" disabled={busy}>
          {busy ? '登录中…' : '登 录'}
        </button>
        {error && <div className="login-error">{error}</div>}
      </form>
    </div>
  );
}
