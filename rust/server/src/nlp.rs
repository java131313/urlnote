// Grammar analysis / POS endpoints and the notes_mark HTTP handlers.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::Serialize;
use shared::*;

use crate::auth::{can_edit_now, is_admin, require_user};
use crate::db::AppState;
use crate::{analyze, mark, token};
use crate::util::{json_error, json_vec};

const POS_MAX_CHARS: usize = 30_000;

fn is_punct(tag: &str, w: &str) -> bool {
    tag.starts_with('x')
        && w
            .chars()
            .all(|c| c.is_ascii_punctuation() || ('\u{3000}'..='\u{303f}').contains(&c))
}

pub async fn analyze_text(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AnalyzeRequest>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    require_user(&st, &headers).await?;
    token::ensure_ja();
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&AnalyzeResponse {
            issues: analyze::analyze(&body.content),
        })
        .unwrap(),
    ))
}

pub async fn pos_tag(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AnalyzeRequest>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    require_user(&st, &headers).await?;
    token::ensure_ja();
    let text: String = body.content.chars().take(POS_MAX_CHARS).collect();
    let mut tokens = Vec::new();
    for (w, tag, s, e) in token::zh_tag(&text) {
        if is_punct(&tag, &w) {
            continue;
        }
        tokens.push(PosToken { w, cat: tag, s, e });
    }
    for (w, pos, s, e) in token::ja_tokens(&text) {
        // vibrato mangles pure-han (Chinese) runs — keep only kana-bearing
        // tokens here; jieba owns the Chinese side
        if !token::has_kana(&w) {
            continue;
        }
        let cat = match pos.split(',').next() {
            Some("名詞") => {
                if pos.contains("代名詞") { "r" } else { "n" }
            }
            Some("動詞") => "v",
            Some("形容詞") | Some("連体詞") => "a",
            Some("副詞") => "d",
            Some("助詞") => "u",
            Some("助動詞") => "aux",
            Some("接続詞") => "c",
            _ => continue, // 記号/未知語 etc.
        };
        tokens.push(PosToken { w, cat: cat.to_string(), s, e });
    }
    tokens.sort_by_key(|t| (t.s, t.e));
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&PosResponse {
            tokens,
            tagged_chars: text.chars().count(),
        })
        .unwrap(),
    ))
}

pub async fn pos_document(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AnalyzeRequest>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    require_user(&st, &headers).await?;
    token::ensure_ja();
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&PosDocResponse {
            markdown: token::annotate_markdown(&body.content),
        })
        .unwrap(),
    ))
}

pub async fn generate_mark(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    let user = require_user(&st, &headers).await?;
    if !shared::valid_note_id(&id) {
        return Err(json_error(StatusCode::BAD_REQUEST, "bad request"));
    }
    if !can_edit_now(&st, &user).await {
        return Err(json_error(StatusCode::FORBIDDEN, "edit locked"));
    }
    mark::ensure_ja();
    match mark::generate(&st, &id).await {
        Ok(true) => Ok(json_vec(StatusCode::OK, serde_json::to_vec(&OkResponse { ok: true }).unwrap())),
        Ok(false) => Err(json_error(StatusCode::NOT_FOUND, "note not found")),
        Err(_) => Err(json_error(StatusCode::INTERNAL_SERVER_ERROR, "mark failed")),
    }
}

pub async fn save_mark(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<AnalyzeRequest>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    let user = require_user(&st, &headers).await?;
    if !shared::valid_note_id(&id) {
        return Err(json_error(StatusCode::BAD_REQUEST, "bad request"));
    }
    if !can_edit_now(&st, &user).await {
        return Err(json_error(StatusCode::FORBIDDEN, "edit locked"));
    }
    match mark::save(&st, &id, &body.content).await {
        Ok(true) => Ok(json_vec(StatusCode::OK, serde_json::to_vec(&OkResponse { ok: true }).unwrap())),
        Ok(false) => Err(json_error(StatusCode::NOT_FOUND, "no mark")),
        Err(_) => Err(json_error(StatusCode::INTERNAL_SERVER_ERROR, "save failed")),
    }
}

pub async fn remove_mark(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    let user = require_user(&st, &headers).await?;
    if !shared::valid_note_id(&id) {
        return Err(json_error(StatusCode::BAD_REQUEST, "bad request"));
    }
    if !can_edit_now(&st, &user).await {
        return Err(json_error(StatusCode::FORBIDDEN, "edit locked"));
    }
    mark::remove(&st, &id)
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e))?;
    Ok(json_vec(StatusCode::OK, serde_json::to_vec(&OkResponse { ok: true }).unwrap()))
}

pub async fn get_mark(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    require_user(&st, &headers).await?;
    if !shared::valid_note_id(&id) {
        return Err(json_error(StatusCode::BAD_REQUEST, "bad request"));
    }
    match mark::get(&st, &id).await.map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e))? {
        Some(info) => Ok(json_vec(StatusCode::OK, serde_json::to_vec(&info).unwrap())),
        None => Err(json_error(StatusCode::NOT_FOUND, "no mark")),
    }
}

pub async fn check_marks(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    let user = require_user(&st, &headers).await?;
    if !is_admin(&user) {
        return Err(json_error(StatusCode::FORBIDDEN, "forbidden"));
    }
    mark::ensure_ja();
    let (removed, regenerated, scanned) = mark::sweep(&st, 5)
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e))?;
    #[derive(Serialize)]
    struct SweepResult {
        scanned: i64,
        orphans_removed: i64,
        regenerated: i64,
    }
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&SweepResult {
            scanned,
            orphans_removed: removed,
            regenerated,
        })
        .unwrap(),
    ))
}
