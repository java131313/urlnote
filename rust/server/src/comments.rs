// 批注(锚定文字):统一存 MongoDB,document 用 "note/<id>" / "oss/<key>" 区分来源。

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use futures_util::TryStreamExt;
use mongodb::bson::{doc, Document};
use shared::*;
use shared::valid_note_id;

use crate::auth::{can_edit_now, is_admin, require_user};
use crate::db::{doc_str, AppState};
use crate::util::{json_error, json_vec, now_str};

fn to_item(d: &Document) -> CommentItem {
    CommentItem {
        id: d.get("_id").and_then(|v| v.as_object_id()).map(|o| o.to_hex()).unwrap_or_default(),
        doc: doc_str(d, "doc"),
        quote: doc_str(d, "quote"),
        text: doc_str(d, "text"),
        author: doc_str(d, "author"),
        created_at: doc_str(d, "created_at"),
        start: d.get("start").and_then(|v| v.as_i64()).unwrap_or(0) as usize,
        end: d.get("end").and_then(|v| v.as_i64()).unwrap_or(0) as usize,
    }
}

pub async fn list_comments(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    require_user(&st, &headers).await?;
    let doc_ref = q.get("doc").cloned().unwrap_or_default();
    if !valid_doc_ref(&doc_ref) {
        return Err(json_error(StatusCode::BAD_REQUEST, "bad request"));
    }
    let mut cursor = st
        .col("comments")
        .find(doc! { "doc": &doc_ref })
        .sort(doc! { "created_at": 1 })
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    let mut comments = Vec::new();
    while let Some(d) = cursor
        .try_next()
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?
    {
        comments.push(to_item(&d));
    }
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&CommentList { comments }).unwrap(),
    ))
}

pub async fn create_comment(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CommentUpsert>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    let user = require_user(&st, &headers).await?;
    if !valid_doc_ref(&body.doc) || body.text.trim().is_empty() {
        return Err(json_error(StatusCode::BAD_REQUEST, "bad request"));
    }
    let quote: String = body.quote.chars().take(200).collect();
    let text: String = body.text.trim().chars().take(2000).collect();
    // id 由客户端生成(锚点先入文档),合法则采用,否则服务端生成
    let id = mongodb::bson::oid::ObjectId::parse_str(&body.id)
        .unwrap_or_else(|_| mongodb::bson::oid::ObjectId::new());
    let item = CommentItem {
        id: id.to_hex(),
        doc: body.doc.clone(),
        quote,
        text,
        author: user,
        created_at: now_str(),
        start: body.start,
        end: body.end,
    };
    st.col("comments")
        .insert_one(doc! {
            "_id": id,
            "doc": item.doc.clone(),
            "quote": item.quote.clone(),
            "text": item.text.clone(),
            "author": item.author.clone(),
            "created_at": item.created_at.clone(),
            "start": item.start as i64,
            "end": item.end as i64,
        })
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&item).unwrap(),
    ))
}

pub async fn delete_comment(
    State(st): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    let user = require_user(&st, &headers).await?;
    let Ok(oid) = mongodb::bson::oid::ObjectId::parse_str(&id) else {
        return Err(json_error(StatusCode::BAD_REQUEST, "bad request"));
    };
    // 作者本人或 admin 可删
    let existing = st
        .col("comments")
        .find_one(doc! { "_id": oid })
        .await
        .ok()
        .flatten();
    match existing {
        Some(d) if doc_str(&d, "author") == user || is_admin(&user) => {
            let _ = st.col("comments").delete_one(doc! { "_id": oid }).await;
            Ok(json_vec(
                StatusCode::OK,
                serde_json::to_vec(&OkResponse { ok: true }).unwrap(),
            ))
        }
        _ => Err(json_error(StatusCode::FORBIDDEN, "forbidden")),
    }
}

// ---- note-scoped comments:存在便签的 notes_mark 文档里(comments 数组),
// 不占用 comments 集合;文档库仍用全局集合 ----

fn note_doc_filter(id: &str) -> Result<mongodb::bson::Document, (StatusCode, Json<ErrorResponse>)> {
    if !valid_note_id(id) {
        return Err(json_error(StatusCode::BAD_REQUEST, "bad request"));
    }
    Ok(doc! { "_id": id })
}

pub async fn list_note_comments(
    State(st): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    require_user(&st, &headers).await?;
    let filter = note_doc_filter(&id)?;
    let doc = st
        .col("notes_mark")
        .find_one(filter)
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    let doc_ref = format!("note/{id}");
    let comments: Vec<CommentItem> = doc
        .map(|d| {
            d.get_array("comments")
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_document())
                        .map(|d| CommentItem {
                            id: doc_str(d, "_id"),
                            doc: doc_ref.clone(),
                            quote: doc_str(d, "quote"),
                            text: doc_str(d, "text"),
                            author: doc_str(d, "author"),
                            created_at: doc_str(d, "created_at"),
                            start: d.get("start").and_then(|x| x.as_i64()).unwrap_or(0) as usize,
                            end: d.get("end").and_then(|x| x.as_i64()).unwrap_or(0) as usize,
                        })
                        .collect()
                })
                .unwrap_or_default()
        })
        .unwrap_or_default();
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&CommentList { comments }).unwrap(),
    ))
}

pub async fn create_note_comment(
    State(st): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(mut body): Json<CommentUpsert>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    let user = require_user(&st, &headers).await?;
    let filter = note_doc_filter(&id)?;
    if !can_edit_now(&st, &user).await {
        return Err(json_error(StatusCode::FORBIDDEN, "edit locked"));
    }
    if body.text.trim().is_empty() {
        return Err(json_error(StatusCode::BAD_REQUEST, "bad request"));
    }
    let cid = mongodb::bson::oid::ObjectId::parse_str(&body.id)
        .unwrap_or_else(|_| mongodb::bson::oid::ObjectId::new())
        .to_hex();
    // 没有标记副本时先按当前正文生成一个(空 content 的 mark 会让
    // 前端"标记副本"状态异常);generate 走 $set,不碰已有 comments
    let has_mark = st
        .col("notes_mark")
        .find_one(doc! { "_id": &id })
        .await
        .ok()
        .flatten()
        .is_some();
    if !has_mark {
        let _ = crate::mark::generate(&st, &id).await;
    }
    let entry = doc! {
        "_id": &cid,
        "quote": body.quote.chars().take(200).collect::<String>(),
        "text": body.text.trim().chars().take(2000).collect::<String>(),
        "author": &user,
        "created_at": crate::util::now_str(),
        "start": body.start as i64,
        "end": body.end as i64,
    };
    let _ = body.id.parse::<u64>();
    st.col("notes_mark")
        .update_one(
            filter,
            doc! {
                "$push": { "comments": entry.clone() },
                "$setOnInsert": {
                    "_id": &id,
                    "content": "",
                    "source_updated_at": "",
                    "updated_at": crate::util::now_str(),
                },
            },
        )
        .with_options(mongodb::options::UpdateOptions::builder().upsert(true).build())
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    let item = CommentItem {
        id: cid,
        doc: format!("note/{id}"),
        quote: doc_str(&entry, "quote"),
        text: doc_str(&entry, "text"),
        author: user,
        created_at: doc_str(&entry, "created_at"),
        start: body.start,
        end: body.end,
    };
    Ok(json_vec(StatusCode::OK, serde_json::to_vec(&item).unwrap()))
}

pub async fn delete_note_comment(
    State(st): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path((id, cid)): axum::extract::Path<(String, String)>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    let user = require_user(&st, &headers).await?;
    let filter = note_doc_filter(&id)?;
    // 作者本人或 admin 可删
    let existing = st
        .col("notes_mark")
        .find_one(filter.clone())
        .await
        .ok()
        .flatten();
    let allowed = existing
        .map(|d| {
            d.get_array("comments")
                .map(|arr| {
                    arr.iter().any(|v| {
                        v.as_document()
                            .map(|d| {
                                doc_str(d, "_id") == cid
                                    && (doc_str(d, "author") == user || is_admin(&user))
                            })
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false)
        })
        .unwrap_or(false);
    if !allowed {
        return Err(json_error(StatusCode::FORBIDDEN, "forbidden"));
    }
    st.col("notes_mark")
        .update_one(filter, doc! { "$pull": { "comments": { "_id": &cid } } })
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&OkResponse { ok: true }).unwrap(),
    ))
}
