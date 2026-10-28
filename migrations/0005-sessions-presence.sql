-- 会话表（替代 KV：免费版 KV 每天仅 1000 次写入，登录+心跳就会刷爆）
CREATE TABLE IF NOT EXISTS sessions (
	token      TEXT PRIMARY KEY,
	username   TEXT NOT NULL,
	expires_at TEXT NOT NULL
);

-- 在线状态（心跳 upsert；过期行在心跳时顺手清理）
CREATE TABLE IF NOT EXISTS presence (
	note_id    TEXT NOT NULL,
	username   TEXT NOT NULL,
	updated_at TEXT NOT NULL DEFAULT (datetime('now')),
	PRIMARY KEY (note_id, username)
);
