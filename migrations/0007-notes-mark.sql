-- 标记文档表:便签的"含词性标注"副本单独存放,notes 永远保持干净原文。
-- 一致性由后台 Cron 定期校验:notes 编辑后标记自动重新生成;
-- notes 删除后孤儿标记自动清理。
CREATE TABLE IF NOT EXISTS notes_mark (
	id                TEXT PRIMARY KEY,
	content           TEXT NOT NULL,
	source_updated_at TEXT NOT NULL,
	updated_at        TEXT NOT NULL DEFAULT (datetime('now'))
);
