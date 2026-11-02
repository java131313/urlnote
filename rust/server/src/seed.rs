// `urlnote-server seed` — load the dictionary into MongoDB from the shared
// translation table and the static word lists. Idempotent: unique (lang,
// word) index + stable group keys; words already present are moved into
// their translation group; empty groups are swept at the end.

use futures_util::TryStreamExt;
use mongodb::bson::{doc, Bson};
use serde::Deserialize;

use crate::db::{doc_i64, AppState};

const LANGS: [&str; 4] = ["zh_cn", "zh_tw", "ja", "en"];

#[derive(Deserialize)]
struct TransRow {
    zh_cn: String,
    zh_tw: String,
    ja: String,
    en: String,
}

fn default_translations_path() -> String {
    for cand in [
        "../scripts/dict_translations.json",
        "scripts/dict_translations.json",
    ] {
        if std::path::Path::new(cand).exists() {
            return cand.to_string();
        }
    }
    "../scripts/dict_translations.json".to_string()
}

fn default_wordlist(lang: &str) -> String {
    let file = match lang {
        "zh_cn" => "suggest_zh.txt",
        "ja" => "suggest_ja.txt",
        _ => return String::new(),
    };
    for cand in [
        format!("../frontend/static/dict/{file}"),
        format!("frontend/static/dict/{file}"),
    ] {
        if std::path::Path::new(&cand).exists() {
            return cand;
        }
    }
    format!("../frontend/static/dict/{file}")
}

async fn ensure_langs(st: &AppState) {
    let seeds = [
        ("zh_cn", "简体中文", "简"),
        ("zh_tw", "繁體中文", "繁"),
        ("ja", "日本語", "日"),
        ("en", "English", "英"),
    ];
    for (i, (code, name, badge)) in seeds.iter().enumerate() {
        let _ = st
            .col("dict_lang")
            .update_one(
                doc! { "_id": code },
                doc! { "$setOnInsert": { "name": name, "badge": badge, "ord": (i as i64) + 1 } },
            )
            .with_options(mongodb::options::UpdateOptions::builder().upsert(true).build())
            .await;
    }
}

async fn group_by_key(st: &AppState, key: &str) -> Result<i64, String> {
    if let Some(g) = st
        .col("dict_group")
        .find_one(doc! { "key": key })
        .await
        .map_err(|e| e.to_string())?
    {
        return Ok(doc_i64(&g, "_id"));
    }
    let id = crate::db::next_seq(&st.db, "dict_group").await?;
    st.col("dict_group")
        .insert_one(doc! { "_id": id, "key": key })
        .await
        .map_err(|e| e.to_string())?;
    Ok(id)
}

/// Insert (lang, word) into the group; if the word already exists (e.g. a
/// seeded wordlist word with its own group), move it here instead.
async fn upsert_into_group(st: &AppState, group_id: i64, lang: &str, word: &str) -> Result<(), String> {
    let existing = st
        .col("dict_word")
        .find_one(doc! { "lang": lang, "word": word })
        .await
        .map_err(|e| e.to_string())?;
    match existing {
        Some(d) => {
            let mut set = doc! {};
            if doc_i64(&d, "group_id") != group_id {
                set.insert("group_id", group_id);
            }
            if d.get("seq").is_none() {
                set.insert("seq", crate::db::next_seq(&st.db, "dict_word").await?);
            }
            if !set.is_empty() {
                let _ = st
                    .col("dict_word")
                    .update_one(doc! { "_id": d.get("_id").cloned().unwrap_or(Bson::Null) }, doc! { "$set": set })
                    .await;
            }
            Ok(())
        }
        None => {
            let seq = crate::db::next_seq(&st.db, "dict_word").await?;
            st.col("dict_word")
                .insert_one(doc! { "seq": seq, "group_id": group_id, "lang": lang, "word": word })
                .await
                .map_err(|e| e.to_string())?;
            Ok(())
        }
    }
}

pub async fn run(st: &AppState) -> Result<(), String> {
    ensure_langs(st).await;

    // 1. translation groups
    let tpath = std::env::var("TRANSLATIONS_PATH").unwrap_or_else(|_| default_translations_path());
    let rows: Vec<TransRow> =
        serde_json::from_str(&std::fs::read_to_string(&tpath).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    for r in &rows {
        let key = format!("t:{}", r.zh_cn);
        let gid = group_by_key(st, &key).await?;
        for lang in LANGS {
            let word = match lang {
                "zh_cn" => &r.zh_cn,
                "zh_tw" => &r.zh_tw,
                "ja" => &r.ja,
                _ => &r.en,
            };
            if !word.is_empty() {
                upsert_into_group(st, gid, lang, word).await?;
            }
        }
    }
    println!("translation groups: {}", rows.len());

    // 2. static word lists — one group per word (no translations)
    for lang in ["zh_cn", "ja"] {
        let path = default_wordlist(lang);
        let Ok(text) = std::fs::read_to_string(&path) else {
            eprintln!("wordlist missing: {path}");
            continue;
        };
        let words: Vec<&str> = text.lines().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
        for w in &words {
            // skip words that already live in a translation group
            let exists = st
                .col("dict_word")
                .find_one(doc! { "lang": lang, "word": w })
                .await
                .ok()
                .flatten()
                .is_some();
            if exists {
                continue;
            }
            let gid = group_by_key(st, &format!("w:{lang}:{w}")).await?;
            upsert_into_group(st, gid, lang, w).await?;
        }
        println!("{lang} wordlist: {} words", words.len());
    }

    // 3. sweep empty groups (wordlist words absorbed into translation groups
    //    leave their per-word groups behind)
    let mut swept = 0i64;
    let mut cursor = st.col("dict_group").find(doc! {}).await.map_err(|e| e.to_string())?;
    while let Some(g) = cursor.try_next().await.map_err(|e| e.to_string())? {
        let gid = doc_i64(&g, "_id");
        let has = st
            .col("dict_word")
            .find_one(doc! { "group_id": gid })
            .await
            .ok()
            .flatten()
            .is_some();
        if !has {
            let _ = st.col("dict_group").delete_one(doc! { "_id": gid }).await;
            swept += 1;
        }
    }
    println!("empty groups swept: {swept}");
    Ok(())
}
