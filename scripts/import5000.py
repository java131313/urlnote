#!/usr/bin/env python3
"""把 5000 个常见中文词连同 繁/日/英 对照写入 urlnote 词典(MongoDB)。

数据来源:
  简: rust/frontend/static/dict/suggest_zh.txt 前 5000(按词频排序)
  繁/英: npm cc-cedict 包([traditional, simplified, pinyin, gloss])
  日: suggest_ja.txt 精确匹配(中日同形词)

两步执行:
  python3 import5000.py prep   # 生成 /tmp/import5000.json(含需跳过的已有对照词)
  python3 import5000.py import # 走 admin API 写入(并发 8)
"""
import json, ssl, subprocess, sys, urllib.request, concurrent.futures

# macOS 自带的 Python 缺证书链:优先 certifi,否则降级为不校验
try:
    import certifi
    SSL_CTX = ssl.create_default_context(cafile=certifi.where())
except ImportError:
    SSL_CTX = ssl.create_default_context()
    SSL_CTX.check_hostname = False
    SSL_CTX.verify_mode = ssl.CERT_NONE

import os
# 连接信息一律走环境变量,勿把真实口令写进仓库:
#   MONGODB_URI   直连 mongo(仅 prep/--prebuild 需要)
#   API_BASE_URL  后端地址;ADMIN_PASSWORD 管理员密码
MONGO = os.environ.get("MONGODB_URI", "mongodb://user:pass@host:27017/?directConnection=true")
API = os.environ.get("API_BASE_URL", "https://your-server.example.com")
ROOT = "/Users/kangxiaoyu/program/www/urlnote"
CEDICT_JS = "/tmp/cccedict/package/data/all.js"
JA_LIST = f"{ROOT}/rust/frontend/static/dict/suggest_ja.txt"
ZH_LIST = f"{ROOT}/rust/frontend/static/dict/suggest_zh.txt"
OUT = "/tmp/import5000.json"
N = 5000


def mongo_eval(js: str) -> str:
    r = subprocess.run(
        ["mongosh", "--quiet", MONGO, "--eval", js],
        capture_output=True, text=True, timeout=300,
    )
    if r.returncode != 0:
        raise RuntimeError(r.stderr[:500])
    return r.stdout


def load_candidates():
    words = []
    seen = set()
    for line in open(ZH_LIST, encoding="utf-8"):
        w = line.strip()
        if not w or w in seen:
            continue
        seen.add(w)
        words.append(w)
        if len(words) >= N:
            break
    # CEDICT: export default [[trad, simp, pinyin, gloss], ...]
    raw = open(CEDICT_JS, encoding="utf-8").read()
    data = json.loads(raw[len("export default "):])["all"]
    by_simp = {}
    for trad, simp, _py, gloss, _x, _y in data:
        if simp not in by_simp:
            by_simp[simp] = (trad, gloss)
    ja_set = {l.strip() for l in open(JA_LIST, encoding="utf-8") if l.strip()}
    return words, by_simp, ja_set


def build_entries():
    words, by_simp, ja_set = load_candidates()
    # 已有对照的词(所在词义组词数>1)整组跳过,避免把已有词从新组里"搬走"
    skip = set()
    for i in range(0, len(words), 500):
        chunk = words[i:i + 500]
        arr = ",".join(json.dumps(w, ensure_ascii=False) for w in chunk)
        out = mongo_eval(
            "const ws=[" + arr + "];"
            "const rows=db.getSiblingDB('urlnote').dict_word.find({lang:'zh_cn',word:{$in:ws}},{word:1,group_id:1}).toArray();"
            "const gids=[...new Set(rows.map(r=>r.group_id))];"
            "const multi=db.getSiblingDB('urlnote').dict_word.aggregate([{$match:{group_id:{$in:gids}}},{$group:{_id:'$group_id',n:{$sum:1}}}]).toArray().filter(g=>g.n>1).map(g=>Number(g._id));"
            "print(JSON.stringify(rows.filter(r=>multi.includes(Number(r.group_id))).map(r=>r.word)));"
        )
        skip.update(json.loads(out))
    entries, no_cedict = [], []
    for w in words:
        if w in skip:
            continue
        ce = by_simp.get(w)
        if not ce:
            no_cedict.append(w)
            continue
        trad, gloss = ce
        if isinstance(gloss, list):
            gloss = gloss[0] if gloss else ""
        gloss = str(gloss).strip()
        if not gloss or len(gloss) > 64:
            gloss = gloss[:64]
        trans = {"zh_tw": trad, "en": gloss}
        if w in ja_set:
            trans["ja"] = w
        entries.append({"lang": "zh_cn", "word": w, "trans": trans})
    return entries, skip, no_cedict


def login() -> str:
    req = urllib.request.Request(
        API + "/api/login",
        data=json.dumps({"username": "admin", "password": os.environ.get("ADMIN_PASSWORD", "root@123")}).encode(),
        headers={"Content-Type": "application/json"},
    )
    resp = urllib.request.urlopen(req, timeout=30, context=SSL_CTX)
    for h, v in resp.headers.items():
        if h.lower() == "set-cookie":
            return v.split(";")[0]
    raise RuntimeError("no set-cookie")


def post_one(cookie: str, e: dict) -> str:
    req = urllib.request.Request(
        API + "/api/dict/words",
        data=json.dumps(e, ensure_ascii=False).encode(),
        headers={"Content-Type": "application/json", "Cookie": cookie},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=60, context=SSL_CTX) as r:
            return "ok" if r.status == 200 else f"http{r.status}"
    except urllib.error.HTTPError as ex:
        return f"http{ex.code}"
    except Exception as ex:
        return f"err:{ex}"


def main():
    if sys.argv[1:] == ["prep"]:
        entries, skip, no_cedict = build_entries()
        json.dump(entries, open(OUT, "w", encoding="utf-8"), ensure_ascii=False)
        print(f"候选 {N} → 跳过已有对照 {len(skip)},无 CEDICT {len(no_cedict)},待导入 {len(entries)}")
        print("样例:", json.dumps(entries[:3], ensure_ascii=False))
        return
    entries = json.load(open(OUT, encoding="utf-8"))
    # --prebuild:直连 mongo 预建带 key 的组(仅当集群外部可直连 mongo 时;
    # 给旧版后端绕开 null-key 组 bug)。默认关闭:新版后端 replace_one
    # 创建的组无 key 字段,直接 import 即可。
    if sys.argv[1:] == ["import", "--prebuild"]:
        n = len(entries)
        base_js = (
            "const c=db.getSiblingDB('urlnote').counters;"
            f"const r=c.findOneAndUpdate({{_x:0}}".replace('{_x:0}', "{_id:'dict_group'}") + ","
            f"{{$inc:{{seq:{n}}}}},".replace('seq', 'seq')
            + "{upsert:true,returnDocument:'after'});"
            + "print(Number(r.seq)-" + str(n) + "+1);"
        )
        base = int(mongo_eval(base_js).strip().splitlines()[-1])
        group_docs = ",".join(
            json.dumps({"_id": base + i, "key": f"imp:{e['word']}"}, ensure_ascii=False)
            for i, e in enumerate(entries)
        )
        mongo_eval("db.getSiblingDB('urlnote').dict_group.insertMany([" + group_docs + "],{ordered:false});")
        for i, e in enumerate(entries):
            e["group_id"] = base + i
        print(f"预建 {n} 个组,起始 id {base}", flush=True)
    cookie = login()
    stats = {}
    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as ex:
        futs = [ex.submit(post_one, cookie, e) for e in entries]
        for i, f in enumerate(concurrent.futures.as_completed(futs), 1):
            r = f.result()
            stats[r] = stats.get(r, 0) + 1
            if i % 500 == 0:
                print(f"{i}/{len(entries)} {stats}", flush=True)
    print("完成:", stats)


if __name__ == "__main__":
    main()
