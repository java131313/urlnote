//! 画图(drawings)——与 urlnote-server 的 /api/drawing(s) 契约一致,
//! 存储在 D1(SQLite):elements 为 JSON 文本,按 id 整文档 upsert。
//! 前端(react DrawPage / Leptos draw.rs / 桌面版)共用同一协议。

use serde::{Deserialize, Serialize};
use shared::valid_note_id;
use wasm_bindgen::JsValue;
use worker::{Env, Request, Response, Result};

use crate::{current_user, json_error};

#[derive(Deserialize, Serialize)]
struct DrawMetaRow {
    id: String,
    name: String,
    updated_at: String,
}

#[derive(Deserialize)]
struct DrawRow {
    id: String,
    name: String,
    width: i64,
    height: i64,
    /// JSON 文本(元素数组)。
    elements: String,
    updated_at: String,
}

#[derive(Deserialize)]
struct DrawSave {
    #[serde(default)]
    name: String,
    #[serde(default)]
    width: Option<i64>,
    #[serde(default)]
    height: Option<i64>,
    #[serde(default)]
    elements: Vec<serde_json::Value>,
}

/// GET /api/drawings —— 画廊列表(updated_at 倒序)。
pub async fn list_drawings(req: &Request, env: &Env) -> Result<Response> {
    if current_user(req, env).await.is_none() {
        return json_error(401, "unauthorized");
    }
    let rows = env
        .d1("DB")?
        .prepare("SELECT id, name, updated_at FROM drawings ORDER BY updated_at DESC")
        .all()
        .await?
        .results::<DrawMetaRow>()?;
    Response::from_json(&serde_json::json!({ "drawings": rows }))
}

/// GET /api/drawing/{id} —— 不存在则返回空画板(打开即建,同 notes 语义)。
pub async fn get_drawing(req: &Request, env: &Env, id: &str) -> Result<Response> {
    if current_user(req, env).await.is_none() {
        return json_error(401, "unauthorized");
    }
    if !valid_note_id(id) {
        return json_error(400, "bad request");
    }
    let row = env
        .d1("DB")?
        .prepare("SELECT id, name, width, height, elements, updated_at FROM drawings WHERE id = ?1")
        .bind(&[JsValue::from(id)])?
        .first::<DrawRow>(None)
        .await?;
    match row {
        Some(r) => {
            let elements: serde_json::Value =
                serde_json::from_str(&r.elements).unwrap_or(serde_json::json!([]));
            Response::from_json(&serde_json::json!({
                "id": r.id, "name": r.name, "width": r.width, "height": r.height,
                "elements": elements, "updated_at": r.updated_at,
            }))
        }
        None => Response::from_json(&serde_json::json!({
            "id": id, "name": "", "width": 960, "height": 600,
            "elements": [], "updated_at": "",
        })),
    }
}

/// PUT/POST /api/drawing/{id} —— upsert(POST 兼容 sendBeacon 场景)。
pub async fn save_drawing(mut req: Request, env: &Env, id: &str) -> Result<Response> {
    let user = match current_user(&req, env).await {
        Some(u) => u,
        None => return json_error(401, "unauthorized"),
    };
    if !valid_note_id(id) {
        return json_error(400, "bad request");
    }
    let body: DrawSave = match req.json().await {
        Ok(b) => b,
        Err(_) => return json_error(400, "bad request"),
    };
    let name: String = body.name.trim().chars().take(120).collect();
    let width = body.width.unwrap_or(960);
    let height = body.height.unwrap_or(600);
    let elements = serde_json::to_string(&body.elements).unwrap_or_else(|_| "[]".into());
    let updated_at = env
        .d1("DB")?
        .prepare(
            "INSERT INTO drawings (id, name, width, height, elements, author, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, width = excluded.width,
                 height = excluded.height, elements = excluded.elements,
                 author = excluded.author, updated_at = CURRENT_TIMESTAMP
             RETURNING updated_at",
        )
        .bind(&[
            JsValue::from(id),
            JsValue::from(name.as_str()),
            JsValue::from(width as f64),
            JsValue::from(height as f64),
            JsValue::from(elements.as_str()),
            JsValue::from(user.as_str()),
        ])?
        .first::<UpdatedRow>(None)
        .await?
        .map(|r| r.updated_at)
        .unwrap_or_default();
    Response::from_json(&serde_json::json!({
        "id": id, "name": name, "width": width, "height": height,
        "elements": body.elements, "updated_at": updated_at, "author": user,
    }))
}

/// POST /api/drawing/{id}/delete
pub async fn delete_drawing(req: &Request, env: &Env, id: &str) -> Result<Response> {
    if current_user(req, env).await.is_none() {
        return json_error(401, "unauthorized");
    }
    if !valid_note_id(id) {
        return json_error(400, "bad request");
    }
    env.d1("DB")?
        .prepare("DELETE FROM drawings WHERE id = ?1")
        .bind(&[JsValue::from(id)])?
        .run()
        .await?;
    Response::from_json(&serde_json::json!({ "ok": true }))
}

#[derive(Deserialize)]
struct UpdatedRow {
    updated_at: String,
}
