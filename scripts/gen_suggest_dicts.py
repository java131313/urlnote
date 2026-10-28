#!/usr/bin/env python3
"""Generate the editor suggestion word lists (Chinese + Japanese).

Sources:
- Chinese: the jieba-rs dictionary (same one the search tokenizer embeds),
  format `word freq tag` per line, ranked by freq.
- Japanese: the IPADIC CSVs from the mecab-ipadic distribution (the same
  dictionary the search tokenizer ships, via vibrato), ranked by cost
  (lower cost = more frequent).

Output: one word per line, rust/frontend/static/dict/suggest_{zh,ja}.txt
Words are filtered to 2..=8 chars, must contain at least one CJK char, no
spaces/punctuation. Top N per language (default 30000).
"""

import csv
import glob
import io
import re
import sys
import urllib.request

JIEBA_DICT = glob.glob(
    f"{__file__}/../../rust/target/**/jieba-rs-0.6.*/src/data/dict.txt",
    recursive=True,
)
# registry path fallback (target dir is cleaned sometimes)
import os

def find_jieba_dict() -> str:
    cands = []
    home = os.path.expanduser("~/.cargo/registry/src")
    for d in glob.glob(f"{home}/*/jieba-rs-0.6*/src/data/dict.txt"):
        cands.append(d)
    if cands:
        return cands[0]
    raise SystemExit("jieba dict.txt not found; `cargo fetch` first")

CJK = re.compile(r"[一-鿿぀-ヿ豈-﫿]")
BAD = re.compile(r"[\s,，、。\.\"'“”‘’\(\)（）\[\]【】\{\}<>《》!！?？:：;；]")

TOP_N = 30000

# 手动附加词:不出现在 jieba 词典的高频专名/人名(重新生成时保留)
EXTRA_ZH = ["康晓宇"]


def ok(word: str) -> bool:
    return (
        2 <= len(word) <= 8
        and CJK.search(word)
        and not BAD.search(word)
        and not word.isascii()
    )


def gen_zh() -> list[str]:
    words = []
    with open(find_jieba_dict(), encoding="utf-8") as f:
        for line in f:
            parts = line.split()
            if len(parts) < 2:
                continue
            w, freq = parts[0], parts[1]
            try:
                fr = int(freq)
            except ValueError:
                continue
            if ok(w):
                words.append((fr, w))
    words.sort(key=lambda x: -x[0])
    seen, out = set(), []
    for _, w in words:
        if w not in seen:
            seen.add(w)
            out.append(w)
        if len(out) >= TOP_N:
            break
    for w in EXTRA_ZH:  # 附加词去重后并入(无需排序,前端会排)
        if w not in seen and ok(w):
            out.append(w)
    return out


def gen_ja(csv_paths: list[str]) -> list[str]:
    words = []
    for p in csv_paths:
        with open(p, encoding="euc-jp", errors="ignore") as f:
            for row in csv.reader(f):
                if len(row) < 4:
                    continue
                surface, cost = row[0], row[3]
                try:
                    c = int(cost)
                except ValueError:
                    continue
                if ok(surface):
                    words.append((c, surface))
    words.sort(key=lambda x: x[0])
    seen, out = set(), []
    for _, w in words:
        if w not in seen:
            seen.add(w)
            out.append(w)
        if len(out) >= TOP_N:
            break
    for w in EXTRA_ZH:  # 附加词去重后并入(无需排序,前端会排)
        if w not in seen and ok(w):
            out.append(w)
    return out


def main():
    out_dir = os.path.join(os.path.dirname(__file__), "../rust/frontend/static/dict")
    os.makedirs(out_dir, exist_ok=True)

    zh = gen_zh()
    with open(f"{out_dir}/suggest_zh.txt", "w", encoding="utf-8") as f:
        f.write("\n".join(zh) + "\n")
    print(f"zh: {len(zh)} words")

    ipadic_dir = sys.argv[1] if len(sys.argv) > 1 else "/tmp/ipadic-mecab-2_7_0"
    csvs = sorted(glob.glob(f"{ipadic_dir}/*.csv"))
    if not csvs:
        raise SystemExit(f"no IPADIC csv found in {ipadic_dir}; extract the tarball there")
    ja = gen_ja(csvs)
    with open(f"{out_dir}/suggest_ja.txt", "w", encoding="utf-8") as f:
        f.write("\n".join(ja) + "\n")
    print(f"ja: {len(ja)} words")


if __name__ == "__main__":
    main()
