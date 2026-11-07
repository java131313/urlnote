-- 画图(drawings):与 urlnote-server(MongoDB)同构,前端(react/Leptos/桌面)
-- 共用同一契约:元素数组为 JSON 文本,按 id 整文档 upsert。
CREATE TABLE IF NOT EXISTS drawings (
	id         TEXT PRIMARY KEY,
	name       TEXT NOT NULL DEFAULT '',
	width      INTEGER NOT NULL DEFAULT 960,
	height     INTEGER NOT NULL DEFAULT 600,
	elements   TEXT NOT NULL DEFAULT '[]',
	author     TEXT NOT NULL DEFAULT '',
	created_at TEXT NOT NULL DEFAULT (datetime('now')),
	updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_drawings_updated ON drawings(updated_at DESC);
