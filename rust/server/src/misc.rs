// auth, presence, edit-lock, image upload/serving — the small endpoints.

use axum::extract::{Multipart, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use chrono::{Duration, Utc};
use mongodb::bson::{doc, DateTime};
use shared::*;
use std::collections::BTreeMap;

use crate::auth::{can_edit_now, is_admin, my_request_status, require_user};
use crate::db::{doc_str, AppState, BUCKET_IMAGES};
use crate::util::{
    ct_eq, json_error, json_vec, new_image_key, new_token, now_str, password_hash,
    sniff_image, MAX_UPLOAD_BYTES, PRESENCE_TTL_SEC, SESSION_TTL_SEC,
};

pub async fn login(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    let username = body.username.trim().to_string();
    let user = st
        .col("users")
        .find_one(doc! { "_id": &username })
        .await
        .ok()
        .flatten();
    let ok = user
        .as_ref()
        .map(|u| ct_eq(&password_hash(&username, &body.password), &doc_str(u, "password_hash")))
        .unwrap_or(false);
    if !ok {
        return Err(json_error(StatusCode::UNAUTHORIZED, "用户名或密码错误"));
    }
    let token = new_token();
    let expires = DateTime::from_millis((Utc::now() + Duration::seconds(SESSION_TTL_SEC)).timestamp_millis());
    st.col("sessions")
        .insert_one(doc! { "_id": &token, "username": &username, "expires_at": expires })
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;

    // SameSite=None is required for cross-origin deployments (frontend on
    // GitHub Pages); browsers only attach the cookie cross-site with
    // SameSite=None, which in turn requires Secure (HTTPS).
    let https = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("https"))
        .unwrap_or(false);
    let cookie = format!(
        "session={token}; Path=/; Max-Age={SESSION_TTL_SEC}; HttpOnly{}",
        if https { "; Secure; SameSite=None" } else { "; SameSite=Lax" }
    );
    let payload = serde_json::to_vec(&LoginResponse {
        ok: true,
        user: User { username },
    })
    .unwrap();
    Ok(crate::util::response_with(
        StatusCode::OK,
        &[
            ("content-type", "application/json; charset=utf-8"),
            ("set-cookie", &cookie),
        ],
        payload,
    ))
}

pub async fn logout(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    if let Some(token) = crate::util::cookie_value(&headers, "session") {
        if valid_token(&token) {
            let _ = st.col("sessions").delete_one(doc! { "_id": token }).await;
        }
    }
    let payload = serde_json::to_vec(&OkResponse { ok: true }).unwrap();
    Ok(crate::util::response_with(
        StatusCode::OK,
        &[
            ("content-type", "application/json; charset=utf-8"),
            ("set-cookie", "session=; Path=/; Max-Age=0"),
        ],
        payload,
    ))
}

pub async fn whoami(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    let user = require_user(&st, &headers).await?;
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&WhoamiResponse {
            user: User { username: user },
        })
        .unwrap(),
    ))
}

pub async fn presence(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PresenceRequest>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    let user = require_user(&st, &headers).await?;
    if !valid_note_id(&body.note_id) {
        return Err(json_error(StatusCode::BAD_REQUEST, "bad request"));
    }
    let now = now_str();
    st.col("presence")
        .update_one(
            doc! { "_id": format!("{}/{}", body.note_id, user) },
            doc! { "$set": {
                "note_id": body.note_id.clone(),
                "username": user.clone(),
                "updated_at": now,
            } },
        )
        .with_options(mongodb::options::UpdateOptions::builder().upsert(true).build())
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;

    // opportunistic cleanup of stale rows (also excluded below)
    let cutoff = (Utc::now() - Duration::seconds(PRESENCE_TTL_SEC))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let _ = st
        .col("presence")
        .delete_many(doc! { "updated_at": { "$lt": cutoff.clone() } })
        .await;

    let mut cursor = st
        .col("presence")
        .find(doc! { "note_id": &body.note_id, "updated_at": { "$gte": cutoff } })
        .sort(doc! { "username": 1 })
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    let mut users = Vec::new();
    use futures_util::TryStreamExt;
    while let Some(d) = cursor
        .try_next()
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?
    {
        users.push(doc_str(&d, "username"));
    }
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&PresenceResponse { users }).unwrap(),
    ))
}

// ---- edit lock & access requests ----

pub async fn edit_status(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    let user = require_user(&st, &headers).await?;
    let locked = crate::auth::edit_locked(&st).await;
    let request = if is_admin(&user) {
        "none".to_string()
    } else {
        my_request_status(&st, &user).await
    };
    let can_edit = is_admin(&user) || !locked || request == "approved";
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&EditStatusResponse {
            locked,
            is_admin: is_admin(&user),
            can_edit,
            request,
        })
        .unwrap(),
    ))
}

pub async fn set_edit_lock(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<EditLockRequest>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    let user = require_user(&st, &headers).await?;
    if !is_admin(&user) {
        return Err(json_error(StatusCode::FORBIDDEN, "forbidden"));
    }
    st.col("edit_lock")
        .update_one(
            doc! { "_id": "lock" },
            doc! { "$set": { "locked": body.locked, "updated_at": now_str() } },
        )
        .with_options(mongodb::options::UpdateOptions::builder().upsert(true).build())
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&OkResponse { ok: true }).unwrap(),
    ))
}

pub async fn submit_edit_request(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<EditRequestSubmit>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    let user = require_user(&st, &headers).await?;
    if is_admin(&user) || my_request_status(&st, &user).await == "approved" {
        return Ok(json_vec(
            StatusCode::OK,
            serde_json::to_vec(&OkResponse { ok: true }).unwrap(),
        ));
    }
    let message: String = body.message.trim().chars().take(200).collect();
    st.col("edit_requests")
        .update_one(
            doc! { "_id": &user },
            doc! { "$set": { "status": "pending", "message": message, "created_at": now_str() } },
        )
        .with_options(mongodb::options::UpdateOptions::builder().upsert(true).build())
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&OkResponse { ok: true }).unwrap(),
    ))
}

pub async fn list_edit_requests(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    let user = require_user(&st, &headers).await?;
    if !is_admin(&user) {
        return Err(json_error(StatusCode::FORBIDDEN, "forbidden"));
    }
    let mut cursor = st
        .col("edit_requests")
        .find(doc! {})
        .sort(doc! { "created_at": -1 })
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    let mut requests = Vec::new();
    use futures_util::TryStreamExt;
    while let Some(d) = cursor
        .try_next()
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?
    {
        requests.push(EditRequestItem {
            username: doc_str(&d, "_id"),
            status: doc_str(&d, "status"),
            message: doc_str(&d, "message"),
            created_at: doc_str(&d, "created_at"),
        });
    }
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&EditRequestList { requests }).unwrap(),
    ))
}

pub async fn handle_edit_request(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<EditHandleRequest>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    let user = require_user(&st, &headers).await?;
    if !is_admin(&user) {
        return Err(json_error(StatusCode::FORBIDDEN, "forbidden"));
    }
    let target = body.username.trim();
    if target.is_empty() || target.chars().count() > 32 || is_admin(target) {
        return Err(json_error(StatusCode::BAD_REQUEST, "bad request"));
    }
    match body.action.as_str() {
        // Upsert: re-approving is idempotent.
        "approve" => {
            st.col("edit_requests")
                .update_one(
                    doc! { "_id": target },
                    doc! { "$set": { "status": "approved", "created_at": now_str() } },
                )
                .with_options(mongodb::options::UpdateOptions::builder().upsert(true).build())
                .await
                .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
        }
        "reject" | "revoke" => {
            let _ = st.col("edit_requests").delete_one(doc! { "_id": target }).await;
        }
        _ => return Err(json_error(StatusCode::BAD_REQUEST, "bad request")),
    }
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&OkResponse { ok: true }).unwrap(),
    ))
}

// ---- image upload & serving ----

pub async fn upload(
    State(st): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    let user = require_user(&st, &headers).await?;
    if !can_edit_now(&st, &user).await {
        return Err(json_error(StatusCode::FORBIDDEN, "edit locked"));
    }
    let mut succ_map: BTreeMap<String, String> = BTreeMap::new();
    let mut err_files: Vec<String> = Vec::new();
    while let Ok(Some(field)) = multipart.next_field().await {
        let Some(name) = field.name().map(|s| s.to_string()) else { continue };
        if name != "file[]" && name != "file" {
            continue;
        }
        let file_name = field.file_name().unwrap_or("file").to_string();
        let data = field.bytes().await.unwrap_or_default();
        if data.is_empty() || data.len() > MAX_UPLOAD_BYTES {
            err_files.push(file_name);
            continue;
        }
        let Some((mime, ext)) = sniff_image(&data) else {
            err_files.push(file_name);
            continue;
        };
        let key = format!("{}{}", new_image_key(), ext);
        let _ = mime; // mime is recovered from the extension on /i/{name}
        match st.gridfs_put(BUCKET_IMAGES, &key, &data).await {
            Ok(_) => {
                succ_map.insert(file_name, format!("/i/{}", &key["img/".len()..]));
            }
            Err(e) => {
                eprintln!("gridfs put {key} failed: {e}");
                err_files.push(file_name);
            }
        }
    }
    Ok(json_vec(
        StatusCode::OK,
        serde_json::to_vec(&UploadResponse {
            msg: String::new(),
            code: 0,
            data: UploadData { err_files, succ_map },
        })
        .unwrap(),
    ))
}

const IMAGE_EXT_MIME: [(&str, &str); 5] = [
    (".png", "image/png"),
    (".jpg", "image/jpeg"),
    (".gif", "image/gif"),
    (".webp", "image/webp"),
    (".avif", "image/avif"),
];

pub async fn image(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    require_user(&st, &headers).await?;
    if !valid_image_name(&name) {
        return Err(json_error(StatusCode::NOT_FOUND, "not found"));
    }
    let ext = &name[name.find('.').unwrap()..];
    let Some((mime, _)) = IMAGE_EXT_MIME.iter().find(|(e, _)| *e == ext) else {
        return Err(json_error(StatusCode::NOT_FOUND, "not found"));
    };
    let file = st
        .bucket(BUCKET_IMAGES)
        .find_one(doc! { "filename": format!("img/{name}") })
        .await
        .ok()
        .flatten();
    let Some(file) = file else {
        return Err(json_error(StatusCode::NOT_FOUND, "not found"));
    };
    let file_id = match file.id {
        mongodb::bson::Bson::ObjectId(o) => o,
        _ => return Err(json_error(StatusCode::NOT_FOUND, "not found")),
    };
    let bytes = st
        .gridfs_get(BUCKET_IMAGES, file_id)
        .await
        .map_err(|_| json_error(StatusCode::NOT_FOUND, "not found"))?;
    Ok(crate::util::response_with(
        StatusCode::OK,
        &[
            ("content-type", *mime),
            ("cache-control", "public, max-age=31536000, immutable"),
        ],
        bytes,
    ))
}
