// Dictionary endpoints (MongoDB port of the D1 implementation).
// Collections: dict_lang {_id: code}, dict_group {_id: i64, key},
// dict_word {_id: ObjectId, seq: i64 (admin-facing id), group_id, lang, word}
// with a unique (lang, word) index — same "one word belongs to exactly one
// meaning group per language" rule as the SQL version.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use futures_util::TryStreamExt;
use mongodb::bson::{doc, Bson, Document};
use serde::Deserialize;
use shared::*;
use std::collections::{BTreeMap, HashSet};

use crate::auth::{is_admin, require_user};
use crate::db::{doc_i64, doc_str, next_seq, AppState};
use crate::util::{json_error, json_vec, regex_escape};

const DICT_LOOKUP_MAX_CHARS: usize = 32;
const DICT_LOOKUP_LIMIT: i64 = 8;

fn word_item(d: &Document, trans: BTreeMap<String, String>) -> DictWordItem {
    DictWordItem {
        id: doc_i64(d, "seq"),
        group_id: doc_i64(d, "group_id"),
        lang: doc_str(d, "lang"),
        word: doc_str(d, "word"),
        trans,
    }
}

/// Fetch words matching `filter` plus their groups' translations.
async fn words_with_trans(
    st: &AppState,
    filter: Document,
    limit: i64,
) -> Result<Vec<DictWordItem>, String> {
    let mut cursor = st
        .col("dict_word")
        .find(filter)
        .sort(doc! { "word": 1 })
        .limit(limit * 4)
        .await
        .map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    while let Some(d) = cursor.try_next().await.map_err(|e| e.to_string())? {
        rows.push(d);
    }
    rows.truncate(limit as usize);
    let group_ids: Vec<i64> = rows.iter().map(|r| doc_i64(r, "group_id")).collect();
    let mut groups: BTreeMap<i64, BTreeMap<String, String>> = BTreeMap::new();
    if !group_ids.is_empty() {
        let mut cursor = st
            .col("dict_word")
            .find(doc! { "group_id": { "$in": &group_ids } })
            .await
            .map_err(|e| e.to_string())?;
        while let Some(d) = cursor.try_next().await.map_err(|e| e.to_string())? {
            let gid = doc_i64(&d, "group_id");
            groups
                .entry(gid)
                .or_default()
                .insert(doc_str(&d, "lang"), doc_str(&d, "word"));
        }
    }
    Ok(rows
        .iter()
        .map(|r| {
            let own_lang = doc_str(r, "lang");
            let own_word = doc_str(r, "word");
            let trans = groups
                .get(&doc_i64(r, "group_id"))
                .map(|t| {
                    t.iter()
                        .filter(|(k, v)| !(**k == own_lang && **v == own_word))
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect()
                })
                .unwrap_or_default();
            word_item(r, trans)
        })
        .collect())
}

pub async fn dict_langs(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    require_user(&st, &headers).await?;
    let mut cursor = st
        .col("dict_lang")
        .find(doc! {})
        .sort(doc! { "ord": 1 })
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    let mut langs = Vec::new();
    while let Some(d) = cursor
        .try_next()
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?
    {
        langs.push(DictLang {
            code: doc_str(&d, "_id"),
            name: doc_str(&d, "name"),
            badge: doc_str(&d, "badge"),
        });
    }
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&DictLangList { langs }).unwrap(),
    ))
}

pub async fn dict_lookup(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    require_user(&st, &headers).await?;
    let empty =
        || json_vec(StatusCode::OK, serde_json::to_vec(&DictLookupResponse { words: vec![] }).unwrap());
    let Some(query) = q.get("q") else { return Ok(empty()) };
    let query = query.trim();
    if query.is_empty() || query.chars().count() > DICT_LOOKUP_MAX_CHARS {
        return Ok(empty());
    }
    let lang = q.get("lang").cloned().unwrap_or_default();
    let lang = lang.trim().to_string();
    if !lang.is_empty() && !valid_lang_code(&lang) {
        return Ok(empty());
    }
    let mut filter = doc! { "word": { "$regex": format!("^{}", regex_escape(query)) } };
    if !lang.is_empty() {
        filter.insert("lang", lang);
    }
    let mut words = words_with_trans(&st, filter, DICT_LOOKUP_LIMIT)
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e))?;
    // exact hits first, then ascending word (same contract as the D1 version)
    words.sort_by(|a, b| {
        (b.word == query)
            .cmp(&(a.word == query))
            .then_with(|| a.word.cmp(&b.word))
    });
    words.truncate(DICT_LOOKUP_LIMIT as usize);
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&DictLookupResponse { words }).unwrap(),
    ))
}

pub async fn list_words(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    let user = require_user(&st, &headers).await?;
    if !is_admin(&user) {
        return Err(json_error(StatusCode::FORBIDDEN, "forbidden"));
    }
    let mut filter = Document::new();
    if let Some(lang) = q.get("lang").filter(|l| !l.trim().is_empty()) {
        filter.insert("lang", lang.trim());
    }
    if let Some(query) = q.get("q").filter(|s| !s.trim().is_empty()) {
        filter.insert("word", doc! { "$regex": format!("^{}", regex_escape(query.trim())) });
    }
    let words = words_with_trans(&st, filter, 500)
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e))?;
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&DictLookupResponse { words }).unwrap(),
    ))
}

async fn group_exists(st: &AppState, id: i64) -> bool {
    st.col("dict_group")
        .find_one(doc! { "_id": id })
        .await
        .ok()
        .flatten()
        .is_some()
}

pub async fn create_lang(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<DictLangUpsert>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    let user = require_user(&st, &headers).await?;
    if !is_admin(&user) {
        return Err(json_error(StatusCode::FORBIDDEN, "forbidden"));
    }
    if !valid_lang_code(&body.code) {
        return Err(json_error(StatusCode::BAD_REQUEST, "bad request"));
    }
    let Some(name) = clean_dict_field(&body.name).filter(|s| !s.is_empty()) else {
        return Err(json_error(StatusCode::BAD_REQUEST, "bad request"));
    };
    let badge = clean_dict_field(&body.badge).unwrap_or_default();
    let badge = if badge.is_empty() {
        name.chars().next().unwrap_or('?').to_string()
    } else {
        badge
    };
    let existing = st
        .col("dict_lang")
        .find_one(doc! { "_id": &body.code })
        .await
        .ok()
        .flatten();
    if existing.is_some() {
        return Err(json_error(StatusCode::CONFLICT, "language exists"));
    }
    let ord = next_seq(&st.db, "dict_lang").await.unwrap_or(0);
    st.col("dict_lang")
        .insert_one(doc! { "_id": &body.code, "name": &name, "badge": &badge, "ord": ord })
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&DictLang { code: body.code, name, badge }).unwrap(),
    ))
}

pub async fn create_word(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<DictWordUpsert>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    let user = require_user(&st, &headers).await?;
    if !is_admin(&user) {
        return Err(json_error(StatusCode::FORBIDDEN, "forbidden"));
    }
    if !valid_lang_code(&body.lang) {
        return Err(json_error(StatusCode::BAD_REQUEST, "bad request"));
    }
    let Some(word) = clean_dict_field(&body.word).filter(|s| !s.is_empty()) else {
        return Err(json_error(StatusCode::BAD_REQUEST, "bad request"));
    };
    let lang_exists = st
        .col("dict_lang")
        .find_one(doc! { "_id": &body.lang })
        .await
        .ok()
        .flatten()
        .is_some();
    if !lang_exists {
        return Err(json_error(StatusCode::BAD_REQUEST, "unknown language"));
    }
    let group_id = match body.group_id {
        Some(g) => {
            if !group_exists(&st, g).await {
                return Err(json_error(StatusCode::BAD_REQUEST, "unknown group"));
            }
            g
        }
        None => next_seq(&st.db, "dict_group")
            .await
            .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e))?,
    };
    st.col("dict_group")
        .update_one(doc! { "_id": group_id }, doc! { "$setOnInsert": { "key": Bson::Null } })
        .with_options(mongodb::options::UpdateOptions::builder().upsert(true).build())
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;

    // ensure the word exists in the group; assign seq to new words
    let existing = st
        .col("dict_word")
        .find_one(doc! { "lang": &body.lang, "word": &word })
        .await
        .ok()
        .flatten();
    let saved = match existing {
        Some(mut d) => {
            if doc_i64(&d, "group_id") != group_id || d.get("seq").is_none() {
                let mut set = doc! { "group_id": group_id };
                if d.get("seq").is_none() {
                    let seq = next_seq(&st.db, "dict_word")
                        .await
                        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e))?;
                    set.insert("seq", seq);
                }
                let _ = st
                    .col("dict_word")
                    .update_one(doc! { "_id": d.get("_id").cloned().unwrap_or(Bson::Null) }, doc! { "$set": set })
                    .await;
                if let Ok(Some(fresh)) = st
                    .col("dict_word")
                    .find_one(doc! { "lang": &body.lang, "word": &word })
                    .await
                {
                    d = fresh;
                }
            }
            d
        }
        None => {
            let seq = next_seq(&st.db, "dict_word")
                .await
                .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e))?;
            st.col("dict_word")
                .insert_one(doc! { "seq": seq, "group_id": group_id, "lang": &body.lang, "word": &word })
                .await
                .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
            st.col("dict_word")
                .find_one(doc! { "lang": &body.lang, "word": &word })
                .await
                .ok()
                .flatten()
                .ok_or_else(|| json_error(StatusCode::INTERNAL_SERVER_ERROR, "create word failed"))?
        }
    };

    for (code, w) in &body.trans {
        if !valid_lang_code(code) {
            continue;
        }
        let Some(w) = clean_dict_field(w).filter(|s| !s.is_empty()) else {
            continue;
        };
        let _ = upsert_word_simple(&st, group_id, code, &w).await;
    }
    sweep_empty_groups(&st).await;

    let gid = doc_i64(&saved, "group_id");
    let mut trans: BTreeMap<String, String> = BTreeMap::new();
    let mut cursor = st
        .col("dict_word")
        .find(doc! { "group_id": gid })
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    while let Some(d) = cursor
        .try_next()
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?
    {
        let l = doc_str(&d, "lang");
        let w = doc_str(&d, "word");
        if !(l == doc_str(&saved, "lang") && w == doc_str(&saved, "word")) {
            trans.insert(l, w);
        }
    }
    let item = word_item(&saved, trans);
    Ok(json_vec(StatusCode::OK, serde_json::to_vec(&item).unwrap()))
}

/// trans-side upsert without response assembly
async fn upsert_word_simple(st: &AppState, group_id: i64, lang: &str, word: &str) -> Result<(), String> {
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
                let seq = next_seq(&st.db, "dict_word").await?;
                set.insert("seq", seq);
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
            let seq = next_seq(&st.db, "dict_word").await?;
            st.col("dict_word")
                .insert_one(doc! { "seq": seq, "group_id": group_id, "lang": lang, "word": word })
                .await
                .map_err(|e| e.to_string())?;
            Ok(())
        }
    }
}

#[derive(Deserialize)]
pub struct WordUpdate {
    #[serde(default)]
    pub word: String,
    #[serde(default)]
    pub group_id: Option<i64>,
}

async fn find_word_by_seq(st: &AppState, id: i64) -> Option<Document> {
    st.col("dict_word")
        .find_one(doc! { "seq": id })
        .await
        .ok()
        .flatten()
}

pub async fn update_word(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(body): Json<WordUpdate>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    let user = require_user(&st, &headers).await?;
    if !is_admin(&user) {
        return Err(json_error(StatusCode::FORBIDDEN, "forbidden"));
    }
    let Some(doc) = find_word_by_seq(&st, id).await else {
        return Err(json_error(StatusCode::BAD_REQUEST, "bad request"));
    };
    let oid = doc.get("_id").cloned().unwrap_or(Bson::Null);
    if let Some(word) = clean_dict_field(&body.word).filter(|s| !s.is_empty()) {
        let _ = st
            .col("dict_word")
            .update_one(doc! { "_id": &oid }, doc! { "$set": { "word": word } })
            .await;
    }
    if let Some(g) = body.group_id {
        if !group_exists(&st, g).await {
            return Err(json_error(StatusCode::BAD_REQUEST, "unknown group"));
        }
        let _ = st
            .col("dict_word")
            .update_one(doc! { "_id": &oid }, doc! { "$set": { "group_id": g } })
            .await;
    }
    sweep_empty_groups(&st).await;
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&OkResponse { ok: true }).unwrap(),
    ))
}

pub async fn delete_word(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    let user = require_user(&st, &headers).await?;
    if !is_admin(&user) {
        return Err(json_error(StatusCode::FORBIDDEN, "forbidden"));
    }
    let Some(doc) = find_word_by_seq(&st, id).await else {
        return Err(json_error(StatusCode::BAD_REQUEST, "bad request"));
    };
    let _ = st
        .col("dict_word")
        .delete_one(doc! { "_id": doc.get("_id").cloned().unwrap_or(Bson::Null) })
        .await;
    sweep_empty_groups(&st).await;
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&OkResponse { ok: true }).unwrap(),
    ))
}

async fn sweep_empty_groups(st: &AppState) {
    let groups = st.col("dict_group");
    let mut empties: HashSet<i64> = HashSet::new();
    if let Ok(mut cursor) = groups.find(doc! {}).await {
        while let Ok(Some(g)) = cursor.try_next().await {
            let gid = doc_i64(&g, "_id");
            let has = st
                .col("dict_word")
                .find_one(doc! { "group_id": gid })
                .await
                .ok()
                .flatten()
                .is_some();
            if !has {
                empties.insert(gid);
            }
        }
    }
    for gid in empties {
        let _ = groups.delete_one(doc! { "_id": gid }).await;
    }
}
