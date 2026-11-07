//! 画图(画板):整文档读写。元素数组(strokes/shapes/text)由前端回放渲染,
//! 个人单用户不做增量同步。_id 为客户端生成的短字符串(与 notes 同惯例)。

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use futures_util::TryStreamExt;
use mongodb::bson::{doc, Document};
use serde::{Deserialize, Serialize};
use shared::{valid_note_id, ErrorResponse};

use crate::auth::require_user;
use crate::db::{doc_i64, doc_str, AppState};
use crate::util::{json_error, json_vec, now_str};

/// 列表条目(画廊)。
#[derive(Clone, Debug, Serialize)]
struct DrawingMeta {
    id: String,
    name: String,
    updated_at: String,
}

/// 完整文档(GET 响应与 PUT 回显)。
#[derive(Clone, Debug, Serialize)]
struct DrawingDoc {
    id: String,
    name: String,
    width: i64,
    height: i64,
    /// 元素数组:pen/eraser 折线、line/rect/ellipse 图形、text 文本(前端定义结构)。
    elements: Vec<serde_json::Value>,
    updated_at: String,
    author: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DrawingSave {
    #[serde(default)]
    name: String,
    #[serde(default)]
    width: Option<i64>,
    #[serde(default)]
    height: Option<i64>,
    #[serde(default)]
    elements: Vec<serde_json::Value>,
}

fn to_bson_elements(elements: &[serde_json::Value]) -> mongodb::bson::Bson {
    mongodb::bson::to_bson(elements).unwrap_or_else(|_| mongodb::bson::Bson::Array(Vec::new()))
}

pub async fn list_drawings(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    require_user(&st, &headers).await?;
    let mut cursor = st
        .col("drawings")
        .find(doc! {})
        .sort(doc! { "updated_at": -1 })
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    let mut drawings = Vec::new();
    while let Some(d) = cursor
        .try_next()
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?
    {
        drawings.push(DrawingMeta {
            id: doc_str(&d, "_id"),
            name: doc_str(&d, "name"),
            updated_at: doc_str(&d, "updated_at"),
        });
    }
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&serde_json::json!({ "drawings": drawings })).unwrap(),
    ))
}

/// GET /api/drawing/{id} —— 不存在则返回空画板(沿用 notes 的"打开即建"语义)。
pub async fn get_drawing(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    require_user(&st, &headers).await?;
    if !valid_note_id(&id) {
        return Err(json_error(StatusCode::BAD_REQUEST, "bad request"));
    }
    if let Some(d) = st
        .col("drawings")
        .find_one(doc! { "_id": &id })
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?
    {
        let elements: Vec<serde_json::Value> = mongodb::bson::from_bson(
            d.get("elements").cloned().unwrap_or(mongodb::bson::Bson::Array(Vec::new())),
        )
        .unwrap_or_default();
        return Ok(json_vec(
            StatusCode::OK,
            serde_json::to_vec(&DrawingDoc {
                id,
                name: doc_str(&d, "name"),
                width: { let w = doc_i64(&d, "width"); if w > 0 { w } else { 960 } },
                height: { let h = doc_i64(&d, "height"); if h > 0 { h } else { 600 } },
                elements,
                updated_at: doc_str(&d, "updated_at"),
                author: doc_str(&d, "author"),
            })
            .unwrap(),
        ));
    }
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&DrawingDoc {
            id,
            name: String::new(),
            width: 960,
            height: 600,
            elements: Vec::new(),
            updated_at: String::new(),
            author: String::new(),
        })
        .unwrap(),
    ))
}

/// PUT /api/drawing/{id} —— upsert。
pub async fn save_drawing(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<DrawingSave>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    let user = require_user(&st, &headers).await?;
    if !valid_note_id(&id) {
        return Err(json_error(StatusCode::BAD_REQUEST, "bad request"));
    }
    let name: String = body.name.trim().chars().take(120).collect();
    let now = now_str();
    let update = doc! {
        "$set": {
            "name": name.clone(),
            "width": body.width.unwrap_or(960),
            "height": body.height.unwrap_or(600),
            "elements": to_bson_elements(&body.elements),
            "updated_at": now.clone(),
        },
        "$setOnInsert": { "created_at": now.clone(), "author": user.clone() },
    };
    st.col("drawings")
        .update_one(doc! { "_id": &id }, update)
        .upsert(true)
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&DrawingDoc {
            id,
            name,
            width: body.width.unwrap_or(960),
            height: body.height.unwrap_or(600),
            elements: body.elements,
            updated_at: now,
            author: user,
        })
        .unwrap(),
    ))
}

pub async fn delete_drawing(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    require_user(&st, &headers).await?;
    if !valid_note_id(&id) {
        return Err(json_error(StatusCode::BAD_REQUEST, "bad request"));
    }
    st.col("drawings")
        .delete_one(doc! { "_id": &id })
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&serde_json::json!({ "ok": true })).unwrap(),
    ))
}
