// Note CRUD, listing and full-text search. Content lives in GridFS
// (bucket "notecontents"); the notes collection holds metadata, the excerpt
// and the search tokens.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use mongodb::bson::doc;
use serde::Deserialize;
use shared::*;

use crate::auth::{can_edit_now, require_user};
use crate::db::{doc_i64, doc_objid, doc_str, doc_str_array, AppState, BUCKET_CONTENTS};
use crate::token;
use crate::util::{head_chars, json_error, json_vec, now_str, INDEX_MAX_CHARS};

#[derive(Deserialize)]
pub struct SaveQuery {
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub font_size: Option<f64>,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

fn note_from_doc(id: &str, d: &mongodb::bson::Document, content: String) -> Note {
    Note {
        id: id.to_string(),
        content,
        font_size: doc_i64(d, "font_size"),
        color: doc_str(d, "color"),
        title: doc_str(d, "title"),
        tags: doc_str_array(d, "tags"),
        author: doc_str(d, "author"),
        updated_at: doc_str(d, "updated_at"),
    }
}

pub async fn get_note(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    if !valid_note_id(&id) {
        return Err(json_error(StatusCode::NOT_FOUND, "not found"));
    }
    require_user(&st, &headers).await?;
    let doc = st
        .col("notes")
        .find_one(doc! { "_id": &id })
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    let note = match doc {
        None => Note::empty(&id),
        Some(d) => {
            let content = match doc_objid(&d, "content_id") {
                Some(oid) => String::from_utf8_lossy(&st.gridfs_get(BUCKET_CONTENTS, oid).await.unwrap_or_default()).into_owned(),
                None => String::new(),
            };
            note_from_doc(&id, &d, content)
        }
    };
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&note).unwrap(),
    ))
}

pub async fn save_note(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<SaveQuery>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    if !valid_note_id(&id) {
        return Err(json_error(StatusCode::NOT_FOUND, "not found"));
    }
    let user = require_user(&st, &headers).await?;
    if !can_edit_now(&st, &user).await {
        return Err(json_error(StatusCode::FORBIDDEN, "edit locked"));
    }

    let font_size = body.font_size.map(resolve_font_size).unwrap_or(DEFAULT_FONT_SIZE);
    let color = if valid_color(&body.color) { body.color.clone() } else { String::new() };
    let title = clean_title(&body.title);
    let tags = clean_tags(&body.tags);
    let updated_at = now_str();

    // content → GridFS (new file first, swap, then drop the old one)
    let content_id = st
        .gridfs_put(BUCKET_CONTENTS, &id, body.content.as_bytes())
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e))?;

    let index_text = head_chars(
        &format!("{} {} {}", title, tags.join(" "), body.content),
        INDEX_MAX_CHARS,
    );
    let tokens = token::tokenize(&index_text);
    let excerpt: String = body.content.chars().take(120).collect();

    let old = st
        .col("notes")
        .find_one(doc! { "_id": &id })
        .await
        .ok()
        .flatten();
    let old_content = old.as_ref().and_then(|d| doc_objid(d, "content_id"));

    let new_doc = doc! {
        "_id": &id,
        "title": title.clone(),
        "tags": tags.clone(),
        "font_size": font_size,
        "color": color.clone(),
        "author": user.clone(),
        "excerpt": excerpt,
        "tokens": tokens,
        "content_id": content_id,
        "updated_at": updated_at.clone(),
    };
    st.col("notes")
        .replace_one(doc! { "_id": &id }, new_doc)
        .with_options(mongodb::options::ReplaceOptions::builder().upsert(true).build())
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;

    if let Some(old_id) = old_content {
        st.gridfs_delete(BUCKET_CONTENTS, old_id).await;
    }

    // eager marked copy for the POS overlay (never clobbers a hand-edited mark)
    if let Err(e) = crate::mark::regenerate_auto(&st, &id).await {
        eprintln!("mark regenerate failed for {id}: {e}");
    }

    let note = Note {
        id: id.clone(),
        content: body.content,
        font_size,
        color,
        title,
        tags,
        author: user,
        updated_at,
    };
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&note).unwrap(),
    ))
}

pub async fn list_notes(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    require_user(&st, &headers).await?;
    let mut cursor = st
        .col("notes")
        .find(doc! {})
        .sort(doc! { "updated_at": -1 })
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    let mut notes = Vec::new();
    use futures_util::TryStreamExt;
    while let Some(d) = cursor
        .try_next()
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?
    {
        notes.push(NoteMeta {
            id: doc_str(&d, "_id"),
            title: doc_str(&d, "title"),
            tags: doc_str_array(&d, "tags"),
            updated_at: doc_str(&d, "updated_at"),
            excerpt: doc_str(&d, "excerpt"),
            author: doc_str(&d, "author"),
        });
    }
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&NoteList { notes }).unwrap(),
    ))
}

pub async fn search_notes(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    require_user(&st, &headers).await?;
    let empty = || json_vec(StatusCode::OK, serde_json::to_vec(&NoteList { notes: vec![] }).unwrap());
    let Some(query) = q.get("q") else { return Ok(empty()) };
    let query = query.trim();
    if query.is_empty() {
        return Ok(empty());
    }
    let terms = token::tokenize(&head_chars(query, 64));
    if terms.is_empty() {
        return Ok(empty());
    }
    let filter = doc! { "tokens": { "$all": &terms } };
    let mut cursor = st
        .col("notes")
        .find(filter)
        .sort(doc! { "updated_at": -1 })
        .limit(50)
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    let mut notes = Vec::new();
    use futures_util::TryStreamExt;
    while let Some(d) = cursor
        .try_next()
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?
    {
        notes.push(NoteMeta {
            id: doc_str(&d, "_id"),
            title: doc_str(&d, "title"),
            tags: doc_str_array(&d, "tags"),
            updated_at: doc_str(&d, "updated_at"),
            excerpt: doc_str(&d, "excerpt"),
            author: doc_str(&d, "author"),
        });
    }
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&NoteList { notes }).unwrap(),
    ))
}

#[derive(serde::Serialize)]
struct ReindexResponse {
    indexed: i64,
}

/// Rebuild search tokens for every note (admin; replaces the chunked
/// worker version — a native server has no per-request CPU budget).
pub async fn reindex_search(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    let user = require_user(&st, &headers).await?;
    if !crate::auth::is_admin(&user) {
        return Err(json_error(StatusCode::FORBIDDEN, "forbidden"));
    }
    crate::mark::ensure_ja();
    let mut cursor = st
        .col("notes")
        .find(doc! {})
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    let mut count = 0i64;
    use futures_util::TryStreamExt;
    while let Some(d) = cursor
        .try_next()
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?
    {
        let id = doc_str(&d, "_id");
        let content = match doc_objid(&d, "content_id") {
            Some(oid) => String::from_utf8_lossy(&st.gridfs_get(BUCKET_CONTENTS, oid).await.unwrap_or_default()).into_owned(),
            None => continue,
        };
        let text = head_chars(
            &format!(
                "{} {} {}",
                doc_str(&d, "title"),
                doc_str_array(&d, "tags").join(" "),
                content
            ),
            INDEX_MAX_CHARS,
        );
        let tokens = token::tokenize(&text);
        let _ = st
            .col("notes")
            .update_one(doc! { "_id": &id }, doc! { "$set": { "tokens": tokens } })
            .await;
        count += 1;
    }
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&ReindexResponse { indexed: count }).unwrap(),
    ))
}
