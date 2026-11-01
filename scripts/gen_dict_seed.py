#!/usr/bin/env python3
"""生成 scripts/dict_seed.sql —— 四语词典种子数据。

数据来源:
  1. 本文件内嵌的 TRANSLATIONS 对照词组(简中/繁中/日/英一一对应)
  2. rust/frontend/static/dict/suggest_{zh,ja}.txt —— suggest 功能的静态
     高频词表(把它们搬进 dict_word,每词一个词义组,暂无译词)

生成的 SQL 全部使用 INSERT OR IGNORE + 稳定 key,可重复执行不重复入库。
用法:  python3 scripts/gen_dict_seed.py > scripts/dict_seed.sql
"""

import pathlib

ROOT = pathlib.Path(__file__).resolve().parent.parent

# (zh_cn, zh_tw, ja, en) —— 同一行相同意思
TRANSLATIONS = [
    ("中国", "中國", "中国", "China"),
    ("美国", "美國", "アメリカ", "America"),
    ("日本", "日本", "日本", "Japan"),
    ("英国", "英國", "イギリス", "Britain"),
    ("法国", "法國", "フランス", "France"),
    ("德国", "德國", "ドイツ", "Germany"),
    ("世界", "世界", "世界", "world"),
    ("国家", "國家", "国", "country"),
    ("城市", "城市", "都市", "city"),
    ("语言", "語言", "言語", "language"),
    ("中文", "中文", "中国語", "Chinese"),
    ("英文", "英文", "英語", "English"),
    ("日语", "日語", "日本語", "Japanese"),
    ("汉字", "漢字", "漢字", "kanji"),
    ("学习", "學習", "学習", "study"),
    ("工作", "工作", "仕事", "work"),
    ("生活", "生活", "生活", "life"),
    ("时间", "時間", "時間", "time"),
    ("今天", "今天", "今日", "today"),
    ("明天", "明天", "明日", "tomorrow"),
    ("昨天", "昨天", "昨日", "yesterday"),
    ("早上", "早上", "朝", "morning"),
    ("晚上", "晚上", "夜", "night"),
    ("谢谢", "謝謝", "ありがとう", "thank you"),
    ("再见", "再見", "さようなら", "goodbye"),
    ("你好", "你好", "こんにちは", "hello"),
    ("爱", "愛", "愛", "love"),
    ("书", "書", "本", "book"),
    ("电影", "電影", "映画", "movie"),
    ("音乐", "音樂", "音楽", "music"),
    ("电脑", "電腦", "パソコン", "computer"),
    ("手机", "手機", "携帯電話", "mobile phone"),
    ("网络", "網絡", "ネットワーク", "network"),
    ("服务器", "伺服器", "サーバー", "server"),
    ("数据库", "資料庫", "データベース", "database"),
    ("程序", "程式", "プログラム", "program"),
    ("软件", "軟體", "ソフトウェア", "software"),
    ("硬件", "硬體", "ハードウェア", "hardware"),
    ("搜索", "搜尋", "検索", "search"),
    ("文件", "檔案", "ファイル", "file"),
    ("图片", "圖片", "画像", "image"),
    ("编辑", "編輯", "編集", "edit"),
    ("保存", "儲存", "保存", "save"),
    ("删除", "刪除", "削除", "delete"),
    ("登录", "登入", "ログイン", "login"),
    ("注销", "登出", "ログアウト", "logout"),
    ("密码", "密碼", "パスワード", "password"),
    ("用户", "使用者", "ユーザー", "user"),
    ("管理员", "管理員", "管理者", "administrator"),
    ("朋友", "朋友", "友達", "friend"),
    ("家人", "家人", "家族", "family"),
    ("天气", "天氣", "天気", "weather"),
    ("食物", "食物", "食べ物", "food"),
    ("水", "水", "水", "water"),
    ("钱", "錢", "お金", "money"),
    ("新闻", "新聞", "ニュース", "news"),
    ("问题", "問題", "問題", "question"),
    ("答案", "答案", "答え", "answer"),
    ("名字", "名字", "名前", "name"),
    ("地址", "地址", "住所", "address"),
    ("电话", "電話", "電話", "telephone"),
    ("电子邮件", "電子郵件", "メール", "email"),
    ("公司", "公司", "会社", "company"),
    ("学校", "學校", "学校", "school"),
    ("医院", "醫院", "病院", "hospital"),
    ("车站", "車站", "駅", "station"),
    ("机场", "機場", "空港", "airport"),
    ("银行", "銀行", "銀行", "bank"),
    ("商店", "商店", "商店", "shop"),
    ("价格", "價格", "価格", "price"),
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
