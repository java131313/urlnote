//! CJK tokenization for full-text search.
//!
//! D1 (SQLite FTS5) cannot load external tokenizer extensions, so the worker
//! tokenizes with real segmenters — jieba-rs (Chinese, embedded dictionary)
//! and Vibrato/IPADIC (Japanese) — and stores space-joined tokens in an
//! `ascii`-tokenized FTS5 table (`ascii` splits on whitespace only, so our
//! word tokens survive intact; case is folded to lowercase at both index and
//! query time).
//!
//! The IPADIC dictionary is ~8MB zstd-compressed and exceeds the Workers
//! script size limit when embedded, so it ships as a static asset and is
//! fetched + decompressed (ruzstd, pure Rust) lazily once per isolate; if it
//! fails to load we degrade to Chinese-only tokens rather than failing the
//! request. Both segmenters run over the whole text and their token sets are
//! unioned: han characters are shared by both languages, so script-based
//! routing can't tell a kanji-only Japanese sentence from Chinese — while
//! redundant tokens only cost a little index space and add recall.

use std::cell::RefCell;
use std::io::Cursor;

use jieba_rs::Jieba;
use vibrato::Tokenizer as JaTokenizer;
use worker::{console_log, Env, Method, Request, RequestInit};

thread_local! {
    static JIEBA: RefCell<Option<Jieba>> = const { RefCell::new(None) };
    static JA_TOKENIZER: RefCell<Option<JaTokenizer>> = const { RefCell::new(None) };
}

const JA_DICT_ASSET: &str = "/dict/system_ipadic.dic.zst";

fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{4e00}'..='\u{9fff}'     // CJK 统一表意文字
        | '\u{3040}'..='\u{30ff}'   // 平假名 + 片假名
        | '\u{f900}'..='\u{faff}')  // 兼容表意文字
}

/// Split long text into sentence-boundary-aligned chunks. The viterbi lattice
/// memory of the segmenters grows with input length, so multi-hundred-KB
/// notes must be segmented in slices (the 128MB isolate limit is unforgiving).
/// Returns (char_base, chunk) pairs.
fn split_chunks(text: &str, max_chars: usize) -> Vec<(usize, &str)> {
    const BREAKS: [char; 10] = ['\n', '。', '！', '？', ';', '；', '.', '!', '｡', '・'];
    let mut out = Vec::new();
    let mut start_byte = 0usize;
    let mut start_char = 0usize;
    let mut chars_since_start = 0usize;
    let mut break_point: Option<(usize, usize)> = None; // (byte_end, chars_incl_break)
    for (bi, ch) in text.char_indices() {
        chars_since_start += 1;
        if BREAKS.contains(&ch) {
            break_point = Some((bi + ch.len_utf8(), chars_since_start));
        }
        if chars_since_start >= max_chars {
            let (end_byte, end_chars) = break_point.unwrap_or((bi, chars_since_start - 1));
            out.push((start_char, &text[start_byte..end_byte]));
            start_char += end_chars;
            chars_since_start -= end_chars;
            start_byte = end_byte;
            break_point = None;
        }
    }
    if start_byte < text.len() {
        out.push((start_char, &text[start_byte..]));
    }
    out
}

const SEG_CHUNK_CHARS: usize = 4000;

/// Call f(char_base, chunk) per sentence-aligned chunk.
fn for_each_chunk(text: &str, mut f: impl FnMut(usize, &str)) {
    if text.chars().count() <= SEG_CHUNK_CHARS {
        f(0, text);
        return;
    }
    for (base, chunk) in split_chunks(text, SEG_CHUNK_CHARS) {
        f(base, chunk);
    }
}

fn push_token(out: &mut Vec<String>, raw: &str) {
    // keep word characters (letters incl. CJK, digits, underscore) and drop
    // punctuation — this also strips FTS5 syntax chars (* " ( ) : - ^ …)
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if cleaned.is_empty() {
        return;
    }
    out.push(cleaned.to_lowercase());
    // Single-CJK-character redundancy: index and query tokenize through the
    // same two segmenters, but their cuts don't always agree (e.g. jieba
    // splits 故宫 into 故/宫 while IPADIC keeps it whole), which would make
    // AND queries miss. Single chars always agree, so adding them guarantees
    // recall; word tokens still drive precision and bm25 ranking.
    for c in cleaned.chars() {
        if is_cjk(c) {
            out.push(c.to_string());
        }
    }
}

fn zh_tokenize(text: &str, out: &mut Vec<String>) {
    JIEBA.with(|j| {
        let mut j = j.borrow_mut();
        if j.is_none() {
            *j = Some(Jieba::new());
        }
        if let Some(jb) = j.as_ref() {
            for tok in jb.cut(text, false) {
                push_token(out, tok.as_ref());
            }
        }
    });
}

async fn fetch_dict_asset(env: &Env) -> Option<Vec<u8>> {
    let assets = env.assets("ASSETS").ok()?;
    let req = Request::new_with_init(
        &format!("https://assets.urlnote{JA_DICT_ASSET}"),
        RequestInit::new().with_method(Method::Get),
    )
    .ok()?;
    let mut resp = assets.fetch_request(req).await.ok()?;
    if resp.status_code() != 200 {
        console_log!("ja dictionary asset missing: {}", resp.status_code());
        return None;
    }
    resp.bytes().await.ok()
}

// init_ja loads and caches the Japanese tokenizer; failures leave it absent
// and callers degrade to Chinese-only tokens.
async fn init_ja(env: &Env) -> bool {
    let Some(bytes) = fetch_dict_asset(env).await else {
        return false;
    };
    let decoded = ruzstd::StreamingDecoder::new(Cursor::new(bytes));
    let Ok(decoder) = decoded else {
        console_log!("ja dictionary zstd decode failed");
        return false;
    };
    let Ok(dict) = vibrato::Dictionary::read(decoder) else {
        console_log!("ja dictionary parse failed");
        return false;
    };
    JA_TOKENIZER.with(|j| *j.borrow_mut() = Some(JaTokenizer::new(dict)));
    true
}

fn ja_tokenize(text: &str, out: &mut Vec<String>) {
    JA_TOKENIZER.with(|j| {
        let mut j = j.borrow_mut();
        if let Some(tk) = j.as_mut() {
            let mut worker = tk.new_worker();
            worker.reset_sentence(text);
            worker.tokenize();
            for i in 0..worker.num_tokens() {
                push_token(out, worker.token(i).surface());
            }
        }
    });
}

/// Ensure the Japanese tokenizer is loaded; false = degrade to zh-only.
pub async fn ensure_ja(env: &Env) -> bool {
    let loaded = JA_TOKENIZER.with(|j| j.borrow().is_some());
    if !loaded {
        if !init_ja(env).await {
            console_log!("ja tokenizer unavailable; zh-only tokens");
        }
    }
    JA_TOKENIZER.with(|j| j.borrow().is_some())
}

/// Chinese segmentation with POS tags and char offsets, for the analyzer.
pub fn zh_tag(text: &str) -> Vec<(String, String, usize, usize)> {
    zh_tag_inner(text, false)
}

/// HMM-enabled variant used by the analyzer's duplicate detector.
pub fn zh_tag_hmm(text: &str) -> Vec<(String, String, usize, usize)> {
    zh_tag_inner(text, true)
}

fn zh_tag_inner(text: &str, hmm: bool) -> Vec<(String, String, usize, usize)> {
    let mut out = Vec::new();
    JIEBA.with(|j| {
        let mut j = j.borrow_mut();
        if j.is_none() {
            *j = Some(Jieba::new());
        }
        if let Some(jb) = j.as_ref() {
            for_each_chunk(text, |base, chunk| {
                let mut pos = 0usize;
                for t in jb.tag(chunk, hmm) {
                    let start = chunk[pos..].find(t.word).map(|i| pos + i).unwrap_or(pos);
                    let end = start + t.word.len();
                    let tag = correct_zh_cat(t.word, zh_cat(t.tag));
                    out.push((
                        t.word.to_string(),
                        tag.to_string(),
                        base + chunk[..start].chars().count(),
                        base + chunk[..end].chars().count(),
                    ));
                    pos = end;
                }
            });
        }
    });
    out
}

/// Japanese tokens with their IPADIC POS (first two feature fields), for the
/// analyzer. Char offsets included.
pub fn ja_tokens(text: &str) -> Vec<(String, String, usize, usize)> {
    let mut out = Vec::new();
    JA_TOKENIZER.with(|j| {
        let mut j = j.borrow_mut();
        if let Some(tk) = j.as_mut() {
            // char offsets relative to the full text
            for_each_chunk(text, |base, chunk| {
                let mut worker = tk.new_worker();
                worker.reset_sentence(chunk);
                worker.tokenize();
                for i in 0..worker.num_tokens() {
                    let tok = worker.token(i);
                    let surface = tok.surface().to_string();
                    let feature = tok.feature().to_string();
                    let pos = feature.split(',').take(2).collect::<Vec<_>>().join(",");
                    let start = base + char_pos(chunk, tok.range_byte().start);
                    let end = base + char_pos(chunk, tok.range_byte().end);
                    out.push((surface, pos, start, end));
                }
            });
        }
    });
    out
}

fn char_pos(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].chars().count()
}

// ---- annotated-markdown generation ----

const POSW_OPEN: &str = "<span class=\"posw cat-";
const POS_DOC_MAX_CHARS: usize = 30_000;

/// Remove `<span class="posw cat-*">…</span>` wrappers from a previous
/// annotation pass so re-generating is idempotent.
fn strip_existing_posw(md: &str) -> String {
    let mut out = md.to_string();
    loop {
        let Some(start) = out.find(POSW_OPEN) else { break };
        let Some(gt) = out[start..].find('>') else { break };
        out.replace_range(start..start + gt + 1, "");
        // our spans never nest, so the first following </span> closes it
        if let Some(close) = out[start..].find("</span>") {
            out.replace_range(start + close..start + close + "</span>".len(), "");
        }
    }
    out
}

/// Char ranges covered by code (fenced blocks and `inline` spans) — tokens
/// inside must not be annotated.
fn code_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut in_fence = false;
    let mut pos = 0usize;
    for line in text.split_inclusive('\n') {
        let chars = line.chars().count();
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            ranges.push((pos, pos + chars));
            in_fence = !in_fence;
            pos += chars;
            continue;
        }
        if in_fence {
            ranges.push((pos, pos + chars));
            pos += chars;
            continue;
        }
        // inline code: toggle on each backtick run (assumes well-formed input)
        let mut in_code = false;
        let mut seg = pos;
        let mut parts = line.split('`').peekable();
        while let Some(part) = parts.next() {
            let n = part.chars().count();
            if in_code {
                ranges.push((seg, seg + n));
            }
            in_code = !in_code;
            seg += n + 1; // +1 for the consumed backtick
        }
        let _ = parts;
        pos += chars;
    }
    ranges
}

fn in_ranges(ranges: &[(usize, usize)], s: usize, e: usize) -> bool {
    ranges.iter().any(|&(rs, re)| s < re && e > rs)
}

fn is_kana(c: char) -> bool {
    ('\u{3040}'..='\u{30ff}').contains(&c)
}

/// True when the token contains kana — i.e. it plausibly belongs to a
/// Japanese passage. Vibrato/IPADIC segments Chinese text into garbage
/// (「我慢」「一位优秀的工程师」), so annotation and POS endpoints only keep
/// its kana-bearing tokens and let jieba own pure-han runs.
pub fn has_kana(w: &str) -> bool {
    w.chars().any(is_kana)
}

/// Punctuation/whitespace-only tokens carry no POS information.
fn no_letters(w: &str) -> bool {
    !w.chars().any(|c| c.is_alphanumeric())
}

/// Kana-only tokens from the Chinese segmenter are noise on Japanese text —
/// the Japanese pass owns those.
fn all_kana(w: &str) -> bool {
    !w.is_empty() && w.chars().all(is_kana)
}

/// Tokens carrying markdown metacharacters would corrupt rendering when
/// wrapped (e.g. inside links or emphasis) — skip them.
fn has_md_meta(w: &str) -> bool {
    w.chars()
        .any(|c| matches!(c, '*' | '_' | '`' | '#' | '[' | ']' | '(' | ')' | '<' | '>' | '|' | '~' | '!'))
}

fn zh_cat(tag: &str) -> &'static str {
    match tag {
        // jieba's dictionary marks state words (很/太/最/都…) as z/zg and
        // distinguisher words (高兴/大型…) as b — both read more naturally
        // as adverb / adjective for display purposes
        "z" | "zg" => "d",
        "b" | "bl" => "a",
        // locatives (上/下/里/外) get their own bucket; time words → noun
        "f" => "f",
        "t" | "tg" => "n",
        "i" => "n", // HMM-discovered neologism (今天天气) — noun is the best guess
        _ => match tag.chars().next() {
            Some('n') => "n",
            Some('v') => "v",
            Some('a') => "a",
            Some('d') => "d",
            Some('u') => "u",
            Some('r') => "r",
            Some('m') => "m",
            Some('q') => "q",
            Some('c') => "c",
            Some('p') => "p",
            _ => "x",
        },
    }
}

/// The bundled jieba dictionary mislabels a handful of very common words
/// (美丽 as a place name ns, 好吃 as a verb…). Patch the frequent ones —
/// the long tail keeps the dictionary tag.
fn correct_zh_cat(word: &str, cat: &'static str) -> &'static str {
    match word {
        "美丽" | "漂亮" | "好看" | "英俊" | "可爱" | "聪明" | "优秀" | "精彩" | "有趣"
        | "高兴" | "快乐" | "开心" | "难过" | "伤心" | "生气" | "害怕" | "热闹" | "安静"
        | "干净" | "新鲜" | "方便" | "重要" | "危险" | "安全" | "温暖" | "凉快" | "舒服"
        | "难受" | "好吃" | "好喝" | "好玩" | "容易" | "简单" | "困难" | "年轻" => "a",
        "正在" | "正" | "刚刚" | "常常" | "经常" | "时常" | "大声" | "小声" | "轻声" => "d",
        _ => cat,
    }
}

fn ja_cat(pos: &str) -> Option<&'static str> {
    match pos.split(',').next() {
        Some("名詞") => Some(if pos.contains("代名詞") { "r" } else { "n" }),
        Some("動詞") => Some("v"),
        Some("形容詞") | Some("連体詞") => Some("a"),
        Some("副詞") => Some("d"),
        Some("助詞") => Some("u"),
        Some("助動詞") => Some("aux"),
        Some("接続詞") => Some("c"),
        _ => None,
    }
}

/// Re-render `md` with every (Chinese + Japanese) word wrapped in a
/// `<span class="posw cat-*">` tag. Overlapping tokenizations are deduped so
/// the produced HTML is always properly nested.
pub fn annotate_markdown(md: &str) -> String {
    let src = strip_existing_posw(md);
    let mask = code_ranges(&src);

    let mut toks: Vec<(usize, usize, &'static str)> = Vec::new();
    for (w, tag, s, e) in zh_tag(&src) {
        if s >= POS_DOC_MAX_CHARS {
            break;
        }
        if no_letters(&w) || all_kana(&w) || has_md_meta(&w) || in_ranges(&mask, s, e) {
            continue;
        }
        toks.push((s, e, zh_cat(&tag)));
    }
    for (w, pos, s, e) in ja_tokens(&src) {
        if s >= POS_DOC_MAX_CHARS {
            break;
        }
        let Some(cat) = ja_cat(&pos) else { continue };
        if !has_kana(&w) || no_letters(&w) || has_md_meta(&w) || in_ranges(&mask, s, e) {
            continue; // pure-han tokens belong to the Chinese segmenter
        }
        toks.push((s, e, cat));
    }
    // dedupe overlaps, longest-first within the same start
    toks.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    let mut merged: Vec<(usize, usize, &'static str)> = Vec::with_capacity(toks.len());
    let mut last_end = 0usize;
    for t in toks {
        if t.0 >= last_end {
            last_end = t.1;
            merged.push(t);
        }
    }

    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len() + merged.len() * 42);
    let mut cursor = 0usize;
    for (s, e, cat) in merged {
        if s < cursor || e > chars.len() {
            continue;
        }
        out.extend(chars[cursor..s].iter());
        out.push_str("<span class=\"posw cat-");
        out.push_str(cat);
        out.push_str("\">");
        out.extend(chars[s..e].iter());
        out.push_str("</span>");
        cursor = e;
    }
    out.extend(chars[cursor..].iter());
    out
}

/// Tokenize with jieba + vibrato, union the token sets, dedupe.
pub async fn tokenize(env: &Env, text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for_each_chunk(text, |_, chunk| zh_tokenize(chunk, &mut out));

    let loaded = JA_TOKENIZER.with(|j| j.borrow().is_some());
    if !loaded {
        // isolate is single-threaded: two tasks racing here may both load,
        // which wastes one fetch/decode but is harmless
        if !init_ja(env).await {
            console_log!("ja tokenizer unavailable; zh-only tokens");
        }
    }
    for_each_chunk(text, |_, chunk| ja_tokenize(chunk, &mut out));

    out.sort();
    out.dedup();
    out
}
