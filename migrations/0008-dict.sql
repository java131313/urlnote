-- 四语对照词典:同一行的 zh_cn / zh_tw / ja / en 是相同意思的词,一一对应。
-- 编辑器输入时匹配任意一个字段(精确或前缀),即提示该行另外几种语言的词。
-- 某个语言暂缺译词时该字段留空,匹配与提示自动跳过空字段。
CREATE TABLE IF NOT EXISTS dict (
	id         INTEGER PRIMARY KEY AUTOINCREMENT,
	zh_cn      TEXT NOT NULL DEFAULT '',
	zh_tw      TEXT NOT NULL DEFAULT '',
	ja         TEXT NOT NULL DEFAULT '',
	en         TEXT NOT NULL DEFAULT '',
	updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- 输入查询按各语言列做前缀匹配(LIKE '词%'),每列一个索引
CREATE INDEX IF NOT EXISTS idx_dict_zh_cn ON dict(zh_cn);
CREATE INDEX IF NOT EXISTS idx_dict_zh_tw ON dict(zh_tw);
CREATE INDEX IF NOT EXISTS idx_dict_ja ON dict(ja);
CREATE INDEX IF NOT EXISTS idx_dict_en ON dict(en);
