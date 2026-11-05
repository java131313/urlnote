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

// ---- 批注(便签 note/<id> 与文档库 oss/<key> 共用;doc 前缀决定端点) ----

export interface CommentItem {
  id: string;
  doc: string;
  quote: string;
  text: string;
  author: string;
  created_at: string;
  start: number;
  end: number;
}

export interface CommentList {
  comments: CommentItem[];
}

export interface CommentUpsert {
  id: string;
  doc: string;
  quote: string;
  text: string;
  start: number;
  end: number;
}

export interface PosDocResponse {
  markdown?: string;
}

// 与 Rust 版同契约:note 文档走 /api/note/<id>/comments,其余走全局 /api/comments
function commentUrls(doc: string): { list: string; post: string; del: (id: string) => string } {
  if (doc.startsWith('note/')) {
    const enc = encodeURIComponent(doc.slice(5));
    return {
      list: `/api/note/${enc}/comments`,
      post: `/api/note/${enc}/comments`,
      del: (id) => `/api/note/${enc}/comments/${id}`,
    };
  }
  const enc = encodeURIComponent(doc);
  return { list: `/api/comments?doc=${enc}`, post: `/api/comments`, del: (id) => `/api/comments/${id}` };
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

  comments: (doc: string) => req<CommentList>(commentUrls(doc).list),
  upsertComment: (body: CommentUpsert) =>
    req<{ ok: boolean }>(commentUrls(body.doc).post, { method: 'POST', body: JSON.stringify(body) }),
  deleteComment: (doc: string, id: string) =>
    req<{ ok: boolean }>(commentUrls(doc).del(id), { method: 'DELETE' }),

  // PDF → markdown(multipart 字段 file;不走 req,因为不能设 JSON Content-Type)
  pdf2md: async (form: FormData): Promise<PosDocResponse> => {
    const r = await fetch(API_BASE + '/api/pdf2md', { method: 'POST', body: form, credentials: 'include' });
    if (!r.ok) throw new ApiError(r.status, await r.text().catch(() => ''));
    return r.json() as Promise<PosDocResponse>;
  },

  // Collabora 在线编辑:换 iframe URL(token 有过期,用完即弃)
  officeConfig: (key: string) => req<{ url: string }>(`/api/office/config?key=${encodeURIComponent(key)}`),
};

export function newNoteId(): string {
  const alphabet = 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789';
  const buf = new Uint8Array(5);
  crypto.getRandomValues(buf);
  return [...buf].map((b) => alphabet[b % 62]).join('');
}
