//! Rule-based grammar/morphology analysis for Chinese and Japanese, built on
//! the same segmenters that power search (jieba POS tags + Vibrato/IPADIC).
//!
//! These are deterministic lint rules, not a full grammar engine — they catch
//! the common, high-confidence mistakes (duplicated words/punctuation,
//! 的/地/得 misuse, doubled particles, mixed polite/plain Japanese styles,
//! unbalanced brackets) and always pair a problem with a concrete correction
//! when one exists.

use shared::Issue;

use crate::token;

// analyzer offsets are char offsets (both segmenters emit them relative to
// the full text) so the client can apply suggestions by spreading the
// string into code points

fn is_punct(c: char) -> bool {
    matches!(c,
        ',' | '。' | '、' | '，' | '．' | '！' | '？' | '；' | '：' | '!' | '?'
        | ';' | ':' | '.')
}

// ---- general rules (both languages) ----

/// Adjacent identical words: "的的", "とてもとても", accidental double typing.
fn push_dup_issue(
    issues: &mut Vec<Issue>,
    _text: &str,
    word: &str,
    start_char: usize,
    end_char: usize,
) {
    let problem = if word.chars().count() == 1 {
        format!("「{word}」连续出现")
    } else {
        format!("词语「{word}」重复")
    };
    issues.push(Issue {
        start: start_char,
        end: end_char,
        quote: format!("{word}{word}"),
        problem,
        suggestion: word.to_string(),
        lang: String::new(),
    });
}

fn check_duplicated_words(issues: &mut Vec<Issue>, text: &str) {
    // dictionary-based pass: single-char and >=3-char repeats are almost
    // always mistakes; 2-char dictionary words may be legitimate ABAB
    // reduplication (研究研究), so they are skipped here
    let tokens = token::zh_tag(text);
    for w in tokens.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        let punct_only = a.0.chars().all(is_punct);
        let n = a.0.chars().count();
        if a.0 == b.0 && !a.0.is_empty() && !punct_only && (n == 1 || n >= 3) {
            push_dup_issue(issues, text, &a.0, a.2, b.3);
        }
    }
    // HMM pass: repeats among out-of-vocabulary words (e.g. 很好很好) are
    // not dictionary reduplication and worth flagging
    for w in token::zh_tag_hmm(text).windows(2) {
        let (a, b) = (&w[0], &w[1]);
        if a.0 == b.0
            && a.1 == "i"
            && !a.0.is_empty()
            && !a.0.chars().all(is_punct)
            && (a.2, a.3) != (b.2, b.3)
        {
            push_dup_issue(issues, text, &a.0, a.2, b.3);
        }
    }
}

/// Repeated punctuation: "，，" "。。" "！！" (doubled) — keep one.
fn check_punctuation(issues: &mut Vec<Issue>, text: &str) {
    // byte → char offset map, built once (O(n)) — hundreds of issues would
    // otherwise make this O(n²)
    let mut byte_to_char = vec![0u32; text.len() + 2];
    let mut n = 0u32;
    for (bi, _ch) in text.char_indices() {
        byte_to_char[bi] = n;
        n += 1;
    }
    byte_to_char[text.len()] = n;
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut i = 0;
    while i + 1 < chars.len() {
        let (bi, c) = chars[i];
        let (bj, d) = chars[i + 1];
        if c == d && is_punct(c) {
            // find the run length
            let mut j = i + 1;
            while j < chars.len() && chars[j].1 == c {
                j += 1;
            }
            issues.push(Issue {
                start: byte_to_char[bi] as usize,
                end: byte_to_char[chars[j - 1].0] as usize + 1,
                quote: chars[i..j].iter().map(|(_, ch)| ch).collect(),
                problem: "标点符号重复".to_string(),
                suggestion: c.to_string(),
                lang: String::new(),
            });
            i = j;
        } else if is_punct(c) && is_punct(d) && c != d && !(c == '.' && d == '.') {
            // mixed punctuation pair like "，。" usually a slip; suggest the
            // stronger terminal mark
            let keep = if d == '。' || d == '！' || d == '？' { d } else { c };
            issues.push(Issue {
                start: byte_to_char[bi] as usize,
                end: byte_to_char[bj] as usize + 1,
                quote: format!("{c}{d}"),
                problem: "连续使用两个不同的标点".to_string(),
                suggestion: keep.to_string(),
                lang: String::new(),
            });
            i += 2;
        } else {
            i += 1;
        }
    }
}

/// Adjacent repeated 2/3/4-char n-grams at character level, catching
/// repeats whose tokens the segmenter doesn't know (很好很好). 2-char ABAB
/// verb reduplication (研究研究) is legitimate and is exempted when the
/// repeated pair is a single dictionary verb.
fn check_repeated_ngrams(issues: &mut Vec<Issue>, text: &str) {
    let tags = token::zh_tag(text);
    let is_abab_verb = |byte_start: usize, gram: &str| -> bool {
        gram.chars().count() == 2
            && tags
                .iter()
                .any(|(_w, t, s, e)| *s == byte_start && *e == byte_start + gram.len() && (t == "v" || t == "vn"))
    };
    let chars: Vec<char> = text.chars().collect();
    let byte_at: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    let mut reported = Vec::new();
    for n in [4usize, 3, 2] {
        if chars.len() < 2 * n {
            continue;
        }
        let mut i = 0;
        while i + 2 * n <= chars.len() {
            let seg: String = chars[i..i + n].iter().collect();
            let ok = chars[i..i + n].iter().all(|&c| !is_punct(c) && !c.is_whitespace());
            if ok && chars[i..i + n] == chars[i + n..i + 2 * n] {
                if n == 2 {
                    let bs = byte_at[i];
                    let gram: String = chars[i..i + n].iter().collect();
                    if is_abab_verb(bs, &gram) {
                        i += 2 * n;
                        continue;
                    }
                }
                let key = (i, i + 2 * n);
                if !reported.iter().any(|&(s, e)| key.0 >= s && key.1 <= e) {
                    reported.push(key);
                    issues.push(Issue {
                        start: i,
                        end: i + 2 * n,
                        quote: format!("{seg}{seg}"),
                        problem: format!("「{seg}」重复出现"),
                        suggestion: seg.clone(),
                        lang: String::new(),
                    });
                }
                i += 2 * n;
            } else {
                i += 1;
            }
        }
    }
}

/// Unbalanced CJK brackets and quotes.
fn check_brackets(issues: &mut Vec<Issue>, text: &str) {
    const PAIRS: [(char, char); 6] = [
        ('（', '）'),
        ('(', ')'),
        ('「', '」'),
        ('『', '』'),
        ('【', '】'),
        ('《', '》'),
    ];
    for &(open, close) in &PAIRS {
        let opens = text.chars().filter(|&c| c == open).count();
        let closes = text.chars().filter(|&c| c == close).count();
        if opens != closes {
            issues.push(Issue {
                start: 0,
                end: 0,
                quote: String::new(),
                problem: if opens > closes {
                    format!("「{open}」比「{close}」多 {n} 处", n = opens - closes)
                } else {
                    format!("「{close}」比「{open}」多 {n} 处", n = closes - opens)
                },
                suggestion: format!("请配对使用 {open}{close}"),
                lang: String::new(),
            });
        }
    }
}

// ---- Chinese rules (jieba POS tags) ----

/// 的 / 地 / 得 misuse, the classic trio:
/// - 形容词 + 的 + 动词 → 地 (慢慢地走)
/// - 动词 + 的 + 副/形 → 得 (跑得很快)
/// - 形容词 + 地 + 名词 → 的 (美丽的风景)
fn check_de_di(issues: &mut Vec<Issue>, text: &str) {
    let tags = token::zh_tag(text);
    for w in tags.windows(3) {
        let (a, m, b) = (&w[0], &w[1], &w[2]);
        let aux = m.0.as_str();
        if m.1 != "uj" && !(aux == "的" || aux == "地" || aux == "得") {
            continue;
        }
        let adj_or_adv = matches!(a.1.as_str(), "a" | "d");
        let fix = |want: &str, why: &str, issues: &mut Vec<Issue>| {
            issues.push(Issue {
                start: m.2,
                end: m.3,
                quote: m.0.clone(),
                problem: format!("「{aux}」应为「{want}」:{why}"),
                suggestion: want.to_string(),
                lang: "zh".to_string(),
            });
        };
        match aux {
            "的" => {
                if adj_or_adv && b.1 == "v" {
                    fix("地", "形容词修饰动词作状语应用「地」", issues);
                } else if a.1 == "v" && (b.1 == "d" || b.1 == "a") {
                    fix("得", "动词后接程度补语应用「得」", issues);
                }
            }
            "地" => {
                if adj_or_adv && b.1 == "n" {
                    fix("的", "形容词修饰名词应用「的」", issues);
                }
            }
            _ => {}
        }
    }
    // jieba often keeps 「慢慢的」 whole instead of splitting off 「的」 — catch
    // word-final 的/地/得 on adjectives/verbs via the surrounding tokens
    for w in tags.windows(2) {
        let (x, y) = (&w[0], &w[1]);
        let mut last = x.0.chars().last().unwrap_or('\0');
        if !matches!(last, '的' | '地' | '得') {
            continue;
        }
        let tail_start = x.3 - last.len_utf8();
        let mut corrected = x.0.clone();
        match (x.1.as_str(), y.1.as_str(), last) {
            ("a" | "d", "v", '的') => {
                corrected.pop();
                corrected.push('地');
                last = '地';
                issues.push(Issue {
                    start: tail_start,
                    end: x.3,
                    quote: x.0.clone(),
                    problem: "「的」应为「地」:形容词修饰动词作状语应用「地」".to_string(),
                    suggestion: corrected,
                    lang: "zh".to_string(),
                });
            }
            ("v", "d" | "a", '的') => {
                corrected.pop();
                corrected.push('得');
                issues.push(Issue {
                    start: tail_start,
                    end: x.3,
                    quote: x.0.clone(),
                    problem: "「的」应为「得」:动词后接程度补语应用「得」".to_string(),
                    suggestion: corrected,
                    lang: "zh".to_string(),
                });
            }
            ("a", "n", '地') => {
                corrected.pop();
                corrected.push('的');
                issues.push(Issue {
                    start: tail_start,
                    end: x.3,
                    quote: x.0.clone(),
                    problem: "「地」应为「的」:形容词修饰名词应用「的」".to_string(),
                    suggestion: corrected,
                    lang: "zh".to_string(),
                });
            }
            _ => {}
        }
        let _ = last;
    }
}

// ---- Japanese rules (IPADIC POS) ----

fn is_particle(pos: &str) -> bool {
    pos.starts_with("助詞")
}

/// Doubled particles: "をを" "にに" "のの" and particle runs like "にはは".
fn check_ja_particles(issues: &mut Vec<Issue>, text: &str) {
    let toks = token::ja_tokens(text);
    for w in toks.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        if is_particle(&a.1) && is_particle(&b.1) {
            if a.0 == b.0 {
                issues.push(Issue {
                    start: a.2,
                    end: b.3,
                    quote: format!("{}{}", a.0, b.0),
                    problem: format!("助詞「{}」重复", a.0),
                    suggestion: a.0.clone(),
                    lang: "ja".to_string(),
                });
            } else {
                issues.push(Issue {
                    start: a.2,
                    end: b.3,
                    quote: format!("{}{}", a.0, b.0),
                    problem: "两个助詞连续出现，请确认语序".to_string(),
                    suggestion: format!("{}{}", a.0, b.0),
                    lang: "ja".to_string(),
                });
            }
        }
    }
}

/// Mixed polite (です・ます) and plain (だ・である) endings in one passage.
fn check_ja_style(issues: &mut Vec<Issue>, text: &str) {
    let toks = token::ja_tokens(text);
    let mut polite = 0usize;
    let mut plain = 0usize;
    for (surface, pos, _, _) in &toks {
        if pos.starts_with("助動詞") || pos.starts_with("動詞") || pos.starts_with("形容詞") {
            match surface.as_str() {
                "です" | "ます" | "でした" | "ました" | "ません" => polite += 1,
                "だ" | "である" | "だった" | "だっ" => plain += 1,
                _ => {}
            }
        }
    }
    if polite >= 2 && plain >= 1 {
        issues.push(Issue {
            start: 0,
            end: 0,
            quote: String::new(),
            problem: format!(
                "です・ます调({polite} 处)与だ・である调({plain} 处)混用,建议统一文体"
            ),
            suggestion: "统一为です・ます调或だ・である调".to_string(),
            lang: "ja".to_string(),
        });
    }
}

/// Analyze `text` and return all detected issues, ordered by position.
pub fn analyze(text: &str) -> Vec<Issue> {
    let mut issues = Vec::new();
    if text.trim().is_empty() {
        return issues;
    }
    check_punctuation(&mut issues, text);
    check_repeated_ngrams(&mut issues, text);
    check_duplicated_words(&mut issues, text);
    check_brackets(&mut issues, text);
    check_de_di(&mut issues, text);
    check_ja_particles(&mut issues, text);
    check_ja_style(&mut issues, text);
    issues.sort_by_key(|i| (i.start, i.end));
    issues.dedup_by(|a, b| a.start == b.start && a.end == b.end && a.suggestion == b.suggestion);
    issues.truncate(100);
    issues
}
