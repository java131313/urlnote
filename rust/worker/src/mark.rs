//! notes_mark lifecycle: the annotated copy of a note lives in its own
//! table while `notes` always keeps the clean original. A cron-driven
//! sweep (plus a manual admin endpoint) keeps the two consistent:
//! notes edited after marking get their mark regenerated, and marks whose
//! note was deleted are removed.

use serde::Deserialize;
use shared::MarkInfo;
use wasm_bindgen::JsValue;
use worker::*;

use crate::token;

#[derive(Deserialize)]
struct NoteSrcRow {
    id: String,
    content: String,
    updated_at: String,
}

#[derive(Deserialize)]
struct MarkMetaRow {
    id: String,
    source_updated_at: String,
}

/// (re)generate the marked copy of note `id`. No-op if the note is gone.
pub async fn generate(env: &Env, id: &str) -> Result<bool> {
    let db = env.d1("DB")?;
    let row = db
        .prepare("SELECT id, content, updated_at FROM notes WHERE id = ?1")
        .bind(&[JsValue::from(id)])?
        .first::<NoteSrcRow>(None)
        .await?;
    let Some(note) = row else { return Ok(false) };
    let marked = token::annotate_markdown(&note.content);
    db.prepare(
        "INSERT INTO notes_mark (id, content, source_updated_at, updated_at)
         VALUES (?1, ?2, ?3, CURRENT_TIMESTAMP)
         ON CONFLICT(id) DO UPDATE SET content = excluded.content,
             source_updated_at = excluded.source_updated_at,
             updated_at = CURRENT_TIMESTAMP",
    )
    .bind(&[
        JsValue::from(id),
        JsValue::from(marked),
        JsValue::from(note.updated_at.as_str()),
    ])?
    .run()
    .await?;
    Ok(true)
}

pub async fn remove(env: &Env, id: &str) -> Result<()> {
    env.d1("DB")?
        .prepare("DELETE FROM notes_mark WHERE id = ?1")
        .bind(&[JsValue::from(id)])?
        .run()
        .await?;
    Ok(())
}

pub async fn get(env: &Env, id: &str) -> Result<Option<MarkInfo>> {
    #[derive(Deserialize)]
    struct Row {
        content: String,
        source_updated_at: String,
    }
    let row = env
        .d1("DB")?
        .prepare("SELECT content, source_updated_at FROM notes_mark WHERE id = ?1")
        .bind(&[JsValue::from(id)])?
        .first::<Row>(None)
        .await?;
    Ok(row.map(|r| MarkInfo {
        content: r.content,
        source_updated_at: r.source_updated_at,
    }))
}

/// Consistency sweep, chunked for the free-plan CPU budget:
/// - orphan marks (note deleted) are removed;
/// - stale marks (note edited after the mark was taken) are regenerated.
/// Returns (orphans_removed, marks_regenerated, scanned).
pub async fn sweep(env: &Env, limit: i64) -> Result<(i64, i64, i64)> {
    let db = env.d1("DB")?;

    // orphans: mark rows with no matching note
    let orphans = db
        .prepare(
            "SELECT m.id AS id, m.source_updated_at AS source_updated_at
             FROM notes_mark m LEFT JOIN notes n ON n.id = m.id
             WHERE n.id IS NULL LIMIT ?1",
        )
        .bind(&[JsValue::from(limit as f64)])?
        .all()
        .await?
        .results::<MarkMetaRow>()?;
    let mut removed = 0i64;
    for o in &orphans {
        if remove(env, &o.id).await.is_ok() {
            removed += 1;
        }
    }

    // stale marks
    let stale = db
        .prepare(
            "SELECT m.id AS id, m.source_updated_at AS source_updated_at
             FROM notes_mark m JOIN notes n ON n.id = m.id
             WHERE m.source_updated_at != '' AND m.source_updated_at != n.updated_at
             ORDER BY m.updated_at LIMIT ?1",
        )
        .bind(&[JsValue::from((limit - removed).max(0) as f64)])?
        .all()
        .await?
        .results::<MarkMetaRow>()?;
    let mut regenerated = 0i64;
    for s in &stale {
        if generate(env, &s.id).await.unwrap_or(false) {
            regenerated += 1;
        }
    }

    Ok((removed, regenerated, removed + regenerated))
}

/// Save user edits made inside the mark editor. The content is re-annotated
/// (POS HTML spans) before storing, so notes_mark always holds the annotated
/// document; `source_updated_at` is blanked so the consistency sweep leaves
/// hand-edited marks alone. Only touches notes_mark.
pub async fn save(env: &Env, id: &str, content: &str) -> Result<bool> {
    #[derive(Deserialize)]
    struct IdRow {
        id: String,
    }
    let annotated = token::annotate_markdown(content);
    let row = env
        .d1("DB")?
        .prepare(
            "UPDATE notes_mark SET content = ?2, source_updated_at = '',
             updated_at = CURRENT_TIMESTAMP WHERE id = ?1 RETURNING id",
        )
        .bind(&[JsValue::from(id), JsValue::from(annotated)])?
        .first::<IdRow>(None)
        .await?;
    Ok(row.is_some())
}

/// Index-time/eager regeneration used by save and reindex: refresh the
/// marked copy from the note's current content, but never clobber a mark
/// the user edited by hand (source_updated_at = '').
pub async fn regenerate_auto(env: &Env, id: &str) -> Result<()> {
    #[derive(Deserialize)]
    struct SrcRow {
        source_updated_at: String,
    }
    let db = env.d1("DB")?;
    let existing = db
        .prepare("SELECT source_updated_at FROM notes_mark WHERE id = ?1")
        .bind(&[JsValue::from(id)])?
        .first::<SrcRow>(None)
        .await?;
    if existing
        .map(|r| r.source_updated_at.is_empty())
        .unwrap_or(false)
    {
        return Ok(());
    }
    generate(env, id).await.map(|_| ())
}
