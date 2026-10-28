-- 全局编辑锁：单行表（id 恒为 1）。locked=1 时仅 admin 和已批准用户可编辑。
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
