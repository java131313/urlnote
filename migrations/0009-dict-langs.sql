-- 四语词典升级为「语言表 + 词义组 + 词表」结构：
--   dict_lang   语言类型表(code 唯一,含角标显示),可由 admin 继续扩充
--   dict_group  词义组(同一组内不同语言的词互为译词,一一对应)
--   dict_word   提示词,关联语言与词义组;同一语言下每个词只归属一个组
-- 旧版 dict 表(0008,简/繁/日/英四个固定列)如有数据则迁移后删除。
CREATE TABLE IF NOT EXISTS dict_lang (
	id    INTEGER PRIMARY KEY AUTOINCREMENT,
	code  TEXT NOT NULL UNIQUE,
	name  TEXT NOT NULL DEFAULT '',
	badge TEXT NOT NULL DEFAULT '',
	ord   INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS dict_group (
	id         INTEGER PRIMARY KEY AUTOINCREMENT,
	key        TEXT UNIQUE,               -- 可选稳定标识(种子/导入用),admin 手建可为空
	updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS dict_word (
	id         INTEGER PRIMARY KEY AUTOINCREMENT,
	group_id   INTEGER NOT NULL REFERENCES dict_group(id),
	lang_id    INTEGER NOT NULL REFERENCES dict_lang(id),
	word       TEXT NOT NULL,
	updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- 词的唯一性按 (语言, 词) 全局判定:一个词在一个语言下只归属一个词义组,
-- 保证 词→译词 的映射无歧义,同时让 INSERT OR IGNORE 幂等去重
CREATE UNIQUE INDEX IF NOT EXISTS uq_dict_word_lang_word ON dict_word(lang_id, word);
CREATE INDEX IF NOT EXISTS idx_dict_word_group ON dict_word(group_id);
CREATE INDEX IF NOT EXISTS idx_dict_word_prefix ON dict_word(word);

-- 内置四种语言(与旧表四列对应)
INSERT OR IGNORE INTO dict_lang (code, name, badge, ord) VALUES
	('zh_cn', '简体中文', '简', 1),
	('zh_tw', '繁體中文', '繁', 2),
	('ja',    '日本語',   '日', 3),
	('en',    'English',  '英', 4);

-- 迁移旧版 dict 表(0008)数据:每行变成一个词义组,四个列转为组内不同语言的词
INSERT OR IGNORE INTO dict_group (id, key)
	SELECT id, 'legacy:' || id FROM dict;

INSERT OR IGNORE INTO dict_word (group_id, lang_id, word)
	SELECT d.id, l.id, d.zh_cn
	FROM dict d JOIN dict_lang l ON l.code = 'zh_cn'
	WHERE d.zh_cn <> '';

INSERT OR IGNORE INTO dict_word (group_id, lang_id, word)
	SELECT d.id, l.id, d.zh_tw
	FROM dict d JOIN dict_lang l ON l.code = 'zh_tw'
	WHERE d.zh_tw <> '';

INSERT OR IGNORE INTO dict_word (group_id, lang_id, word)
	SELECT d.id, l.id, d.ja
	FROM dict d JOIN dict_lang l ON l.code = 'ja'
	WHERE d.ja <> '';

INSERT OR IGNORE INTO dict_word (group_id, lang_id, word)
	SELECT d.id, l.id, d.en
	FROM dict d JOIN dict_lang l ON l.code = 'en'
	WHERE d.en <> '';

-- 数据已迁完,删除旧表(若不存在则跳过)
DROP TABLE IF EXISTS dict;
