CREATE TABLE IF NOT EXISTS notes (
	id         TEXT PRIMARY KEY,
	content    TEXT NOT NULL DEFAULT '',
	title      TEXT NOT NULL DEFAULT '',
	tags       TEXT NOT NULL DEFAULT '[]',
	author     TEXT NOT NULL DEFAULT '',
	font_size  INTEGER NOT NULL DEFAULT 16,
	color      TEXT NOT NULL DEFAULT '',
	updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- 内置账号（无注册）：admin/root@123, guest/guest, amigo/amigo
-- 密码哈希公式：sha256("用户名:密码")
CREATE TABLE IF NOT EXISTS users (
	username      TEXT PRIMARY KEY,
	display_name  TEXT NOT NULL DEFAULT '',
	password_hash TEXT NOT NULL,
	created_at    TEXT NOT NULL DEFAULT (datetime('now'))
);

INSERT OR IGNORE INTO users (username, display_name, password_hash) VALUES
	('admin', '管理员', 'efece4e87c16d55378975e996e5ac989ae74ef1995abba6e0493ea080f828068'),
	('guest', '访客',   'c669ef97e3555f4811d659da5036b28ab30ee8daec80333cb15a09e8ceee4690'),
	('amigo', 'Amigo',  '1cc481da1004eb37fe31618cbb5718c261b13c876884c51476c812ec665bc105');

-- 全局编辑锁（id 恒为 1）：locked=1 时仅 admin 和 edit_requests 中已批准用户可编辑
CREATE TABLE IF NOT EXISTS edit_lock (
	id         INTEGER PRIMARY KEY CHECK (id = 1),
	locked     INTEGER NOT NULL DEFAULT 0,
	updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- 编辑权限申请：status 为 pending / approved；拒绝或撤销直接删行，用户可重新申请
CREATE TABLE IF NOT EXISTS edit_requests (
	username   TEXT PRIMARY KEY,
	status     TEXT NOT NULL DEFAULT 'pending',
	message    TEXT NOT NULL DEFAULT '',
	created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

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

-- 全文搜索:worker 内的 jieba-rs(中文)+ vibrato/ipadic(日语)分词后,
-- 以空格连接的词元存入 FTS5。D1 不能加载外部分词器扩展,
-- 故用 tokenize='ascii'(仅按空白切分,词元不会被二次拆分);
-- 大小写已在写入和查询两端统一折叠。
CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
	note_id UNINDEXED,
	tokens,
	tokenize = 'ascii'
);

-- 标记文档表:便签的"含词性标注"副本单独存放,notes 永远保持干净原文。
-- 一致性由后台 Cron 定期校验:notes 编辑后标记自动重新生成;
-- notes 删除后孤儿标记自动清理。
CREATE TABLE IF NOT EXISTS notes_mark (
	id                TEXT PRIMARY KEY,
	content           TEXT NOT NULL,
	source_updated_at TEXT NOT NULL,
	updated_at        TEXT NOT NULL DEFAULT (datetime('now'))
);
