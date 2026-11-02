#!/usr/bin/env python3
"""生成 scripts/dict_seed.sql —— 四语词典种子数据。

数据来源:
  1. 本文件内嵌的 TRANSLATIONS 对照词组(简中/繁中/日/英一一对应)
  2. rust/frontend/static/dict/suggest_{zh,ja}.txt —— suggest 功能的静态
     高频词表(把它们搬进 dict_word,每词一个词义组,暂无译词)

生成的 SQL 全部使用 INSERT OR IGNORE + 稳定 key,可重复执行不重复入库。
用法:  python3 scripts/gen_dict_seed.py > scripts/dict_seed.sql
"""

import json
import pathlib

ROOT = pathlib.Path(__file__).resolve().parent.parent

# 从共享的 scripts/dict_translations.json 读取(MongoDB 版播种器也用它)
with open(ROOT / "scripts" / "dict_translations.json", encoding="utf-8") as _f:
    TRANSLATIONS = [
        (d["zh_cn"], d["zh_tw"], d["ja"], d["en"])
        for d in json.load(_f)
    ]

LANGS = ["zh_cn", "zh_tw", "ja", "en"]

# 每个 VALUES 块的最大行数(控制单条语句体积)
CHUNK = 1000


def sql_str(s: str) -> str:
    return "'" + s.replace("'", "''") + "'"


def values_block(rows, col_names: str = "word") -> str:
    # SQLite 不支持 AS v(col, ...) 列别名列表,VALUES 列固定为 column1..n
    return "(\n\tVALUES\n" + ",\n".join("\t(" + r + ")" for r in rows) + f"\n) AS v"


def emit_wordlist(out, code: str, rel_path: str) -> None:
    words = [
        ln.strip()
        for ln in (ROOT / rel_path).read_text(encoding="utf-8").splitlines()
        if ln.strip()
    ]
    out.append(f"-- {code} 高频词表({len(words)} 词,来源 {rel_path}),每词一个词义组")
    for i in range(0, len(words), CHUNK):
        chunk = words[i : i + CHUNK]
        block = values_block([sql_str(w) for w in chunk])
        out.append(
            f"INSERT OR IGNORE INTO dict_group (key)\n"
            f"SELECT 'w:{code}:' || v.column1 FROM {block};"
        )
        out.append(
            f"INSERT OR IGNORE INTO dict_word (group_id, lang_id, word)\n"
            f"SELECT g.id, l.id, v.column1 FROM {block}\n"
            f"JOIN dict_lang l ON l.code = '{code}'\n"
            f"JOIN dict_group g ON g.key = 'w:{code}:' || v.column1;"
        )


def main() -> None:
    out = [
        "-- 四语词典种子数据(由 scripts/gen_dict_seed.py 生成,请勿手改)。",
        "-- 应用到远程库:  npx wrangler d1 execute urlnote --remote --file=./scripts/dict_seed.sql",
        "-- 应用到本地库:  npx wrangler d1 execute urlnote --local  --file=./scripts/dict_seed.sql",
        "-- 全部为 INSERT OR IGNORE + 稳定 key,重复执行不会重复入库。",
        "",
        "-- 确保内置语言存在(迁移/ schema.sql 已含,这里幂等补一遍)",
        "INSERT OR IGNORE INTO dict_lang (code, name, badge, ord) VALUES",
        "\t('zh_cn', '简体中文', '简', 1),",
        "\t('zh_tw', '繁體中文', '繁', 2),",
        "\t('ja',    '日本語',   '日', 3),",
        "\t('en',    'English',  '英', 4);",
        "",
        "-- 对照词组:每组一行四语同义词",
    ]
    for zh_cn, zh_tw, ja, en in TRANSLATIONS:
        key = "t:" + zh_cn
        pairs = ",\n\t".join(
            f"({sql_str(c)},{sql_str(w)})"
            for c, w in zip(LANGS, (zh_cn, zh_tw, ja, en))
            if w
        )
        out.append(f"INSERT OR IGNORE INTO dict_group (key) VALUES ({sql_str(key)});")
        out.append(
            "INSERT OR IGNORE INTO dict_word (group_id, lang_id, word)\n"
            f"SELECT g.id, l.id, v.column2 FROM (\n\tVALUES\n\t{pairs}\n) AS v\n"
            f"JOIN dict_lang l ON l.code = v.column1\n"
            f"JOIN dict_group g ON g.key = {sql_str(key)};"
        )
    out.append("")
    emit_wordlist(out, "zh_cn", "rust/frontend/static/dict/suggest_zh.txt")
    out.append("")
    emit_wordlist(out, "ja", "rust/frontend/static/dict/suggest_ja.txt")
    out.append(
        "-- 清理空组:词表词与对照词因 (语言,词) 唯一被合并后,可能留下无词组"
    )
    out.append(
        "DELETE FROM dict_group WHERE NOT EXISTS "
        "(SELECT 1 FROM dict_word WHERE dict_word.group_id = dict_group.id);"
    )
    print("\n\n".join(out))


if __name__ == "__main__":
    main()
