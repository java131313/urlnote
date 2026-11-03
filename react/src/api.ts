// rust server(urlnote-server)API 客户端。全部走携带凭据的 fetch;
// 开发环境经 Vite 代理同源,生产用 VITE_API_BASE 跨域。

export const API_BASE: string = (import.meta as any).env.VITE_API_BASE || '';

async function req<T>(path: string, init?: RequestInit): Promise<T> {
  const r = await fetch(API_BASE + path, {
    credentials: 'include',
    headers: init?.body ? { 'Content-Type': 'application/json' } : undefined,
    ...init,
  });
  if (!r.ok) {
    const text = await r.text().catch(() => '');
    throw new ApiError(r.status, text);
  }
  return r.json() as Promise<T>;
}

export class ApiError extends Error {
  constructor(public status: number, body: string) {
    super(`HTTP ${status}: ${body.slice(0, 120)}`);
  }
}

export interface Note {
  id: string;
  content: string;
  font_size: number;
  color: string;
  title: string;
  tags: string[];
  author: string;
  updated_at: string;
}

export interface NoteMeta {
  id: string;
  title: string;
  tags: string[];
  updated_at: string;
  excerpt: string;
  author: string;
}

export interface DictLang {
  code: string;
  name: string;
  badge: string;
}

export interface DictWord {
  id: number;
  group_id: number;
  lang: string;
  word: string;
  trans: Record<string, string>;
}

export const api = {
  whoami: () => req<{ user: { username: string } }>('/api/whoami'),
  login: (username: string, password: string) =>
    req<{ ok: boolean }>('/api/login', { method: 'POST', body: JSON.stringify({ username, password }) }),
  logout: () => req<{ ok: boolean }>('/api/logout', { method: 'POST' }),

  note: (id: string) => req<Note>(`/api/note/${id}`),
  saveNote: (id: string, body: Partial<Note>) =>
    req<Note>(`/api/note/${id}`, { method: 'PUT', body: JSON.stringify(body) }),
  notes: () => req<{ notes: NoteMeta[] }>('/api/notes'),
  search: (q: string) => req<{ notes: NoteMeta[] }>(`/api/search?q=${encodeURIComponent(q)}`),

  dictLangs: () => req<{ langs: DictLang[] }>('/api/dict/langs'),
  dictLookup: (q: string, lang: string) =>
    req<{ words: DictWord[] }>(`/api/dict/lookup?q=${encodeURIComponent(q)}&lang=${encodeURIComponent(lang)}`),
};

export function newNoteId(): string {
  const alphabet = 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789';
  const buf = new Uint8Array(5);
  crypto.getRandomValues(buf);
  return [...buf].map((b) => alphabet[b % 62]).join('');
}
