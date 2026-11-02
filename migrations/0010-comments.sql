-- 批注(锚定文字):全局批注(文档库用)+ notes_mark 增加 comments 列(便签用,
-- JSON 数组,与 MongoDB 版结构一致;标记副本重新生成时 ON CONFLICT 只更新
-- content/source_updated_at,不触碰该列)
CREATE TABLE IF NOT EXISTS comments (
	id         TEXT PRIMARY KEY,
	doc        TEXT NOT NULL,
	quote      TEXT NOT NULL DEFAULT '',
	text       TEXT NOT NULL DEFAULT '',
	author     TEXT NOT NULL DEFAULT '',
	created_at TEXT NOT NULL DEFAULT (datetime('now')),
	start      INTEGER NOT NULL DEFAULT 0,
	"end"      INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_comments_doc ON comments(doc);

ALTER TABLE notes_mark ADD COLUMN comments TEXT NOT NULL DEFAULT '[]';
