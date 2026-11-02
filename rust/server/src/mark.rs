// notes_mark lifecycle, MongoDB/GridFS port: the annotated copy's content
// lives in the "markcontents" bucket; notes_mark holds metadata. A background
// sweep (plus the admin endpoint) keeps marks consistent with notes.

use futures_util::TryStreamExt;
use mongodb::bson::doc;
use shared::MarkInfo;

use crate::db::{doc_objid, doc_str, AppState, BUCKET_MARKS};
use crate::token;

pub fn ensure_ja() {
    token::ensure_ja();
}

/// (re)generate the marked copy of note `id`. No-op if the note is gone.
pub async fn generate(st: &AppState, id: &str) -> Result<bool, String> {
    let note = st
        .col("notes")
        .find_one(doc! { "_id": id })
        .await
        .map_err(|e| e.to_string())?;
    let Some(note) = note else { return Ok(false) };
    let content = match doc_objid(&note, "content_id") {
        Some(oid) => String::from_utf8_lossy(&st.gridfs_get(crate::db::BUCKET_CONTENTS, oid).await.unwrap_or_default()).into_owned(),
        None => String::new(),
    };
    let updated_at = doc_str(&note, "updated_at");
    let marked = token::annotate_markdown(&content);
    let new_id = st
        .gridfs_put(BUCKET_MARKS, id, marked.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let old = st
        .col("notes_mark")
        .find_one(doc! { "_id": id })
        .await
        .ok()
        .flatten()
        .and_then(|d| doc_objid(&d, "content_id"));
    st.col("notes_mark")
        .update_one(
            doc! { "_id": id },
            doc! { "$set": {
                "content_id": new_id,
                "source_updated_at": updated_at,
                "updated_at": crate::util::now_str(),
            } },
        )
        .with_options(mongodb::options::UpdateOptions::builder().upsert(true).build())
        .await
        .map_err(|e| e.to_string())?;
    if let Some(old_id) = old {
        st.gridfs_delete(BUCKET_MARKS, old_id).await;
    }
    Ok(true)
}

pub async fn remove(st: &AppState, id: &str) -> Result<(), String> {
    let old = st
        .col("notes_mark")
        .find_one(doc! { "_id": id })
        .await
        .ok()
        .flatten()
        .and_then(|d| doc_objid(&d, "content_id"));
    st.col("notes_mark")
        .delete_one(doc! { "_id": id })
        .await
        .map_err(|e| e.to_string())?;
    if let Some(old_id) = old {
        st.gridfs_delete(BUCKET_MARKS, old_id).await;
    }
    Ok(())
}

pub async fn get(st: &AppState, id: &str) -> Result<Option<MarkInfo>, String> {
    let doc = st
        .col("notes_mark")
        .find_one(doc! { "_id": id })
        .await
        .map_err(|e| e.to_string())?;
    match doc {
        None => Ok(None),
        Some(d) => {
            let content = match doc_objid(&d, "content_id") {
                Some(oid) => String::from_utf8_lossy(&st.gridfs_get(BUCKET_MARKS, oid).await.unwrap_or_default()).into_owned(),
                None => String::new(),
            };
            Ok(Some(MarkInfo {
                content,
                source_updated_at: doc_str(&d, "source_updated_at"),
            }))
        }
    }
}

/// Save user edits made inside the mark editor: re-annotate, store, and
/// blank source_updated_at so the sweep leaves hand-edited marks alone.
pub async fn save(st: &AppState, id: &str, content: &str) -> Result<bool, String> {
    let annotated = token::annotate_markdown(content);
    let new_id = st
        .gridfs_put(BUCKET_MARKS, id, annotated.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let res = st
        .col("notes_mark")
        .update_one(
            doc! { "_id": id },
            doc! { "$set": {
                "content_id": new_id,
                "source_updated_at": "",
                "updated_at": crate::util::now_str(),
            } },
        )
        .await
        .map_err(|e| e.to_string())?;
    if res.matched_count == 0 {
        return Ok(false);
    }
    Ok(true)
}

/// Never clobber a mark the user edited by hand (source_updated_at = "").
pub async fn regenerate_auto(st: &AppState, id: &str) -> Result<(), String> {
    let existing = st
        .col("notes_mark")
        .find_one(doc! { "_id": id })
        .await
        .map_err(|e| e.to_string())?;
    if existing
        .map(|d| doc_str(&d, "source_updated_at").is_empty())
        .unwrap_or(false)
    {
        return Ok(());
    }
    generate(st, id).await.map(|_| ())
}

/// Consistency sweep: orphan marks (note deleted) removed; stale marks
/// (note edited after the mark was taken) regenerated.
/// Returns (orphans_removed, marks_regenerated, scanned).
pub async fn sweep(st: &AppState, limit: i64) -> Result<(i64, i64, i64), String> {
    let notes = st.col("notes");
    let marks = st.col("notes_mark");
    let mut cursor = marks.find(doc! {}).limit(limit).await.map_err(|e| e.to_string())?;
    let mut removed = 0i64;
    let mut regenerated = 0i64;
    while let Some(m) = cursor.try_next().await.map_err(|e| e.to_string())? {
        let id = doc_str(&m, "_id");
        let note = notes
            .find_one(doc! { "_id": &id })
            .await
            .map_err(|e| e.to_string())?;
        match note {
            None => {
                if remove(st, &id).await.is_ok() {
                    removed += 1;
                }
            }
            Some(n) => {
                let src = doc_str(&m, "source_updated_at");
                if !src.is_empty() && src != doc_str(&n, "updated_at")
                    && generate(st, &id).await.unwrap_or(false) {
                        regenerated += 1;
                    }
            }
        }
    }
    Ok((removed, regenerated, removed + regenerated))
}
