-- 全文搜索:worker 内的 jieba-rs(中文)+ vibrato/ipadic(日语)分词后,
-- 以空格连接的词元存入 FTS5。D1 不能加载外部分词器扩展,
-- 故用 tokenize='ascii'(仅按空白切分,词元不会被二次拆分);
-- 大小写已在写入和查询两端统一折叠。
CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
	note_id UNINDEXED,
	tokens,
	tokenize = 'ascii'
);
