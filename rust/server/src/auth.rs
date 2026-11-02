// Session auth and the edit-lock helpers (same contract as the worker).

use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use chrono::Utc;
use mongodb::bson::doc;
use shared::{valid_token, ADMIN_USERNAME};

use crate::db::{doc_bool, AppState};
use crate::util::{cookie_value, json_error};

pub fn is_admin(user: &str) -> bool {
    user == ADMIN_USERNAME
}

/// Resolve the session cookie to a username. Sessions carry a BSON DateTime
/// `expires_at`; the TTL index purges them lazily, and the explicit compare
/// keeps races honest.
pub async fn current_user(st: &AppState, headers: &HeaderMap) -> Option<String> {
    let token = cookie_value(headers, "session")?;
    if !valid_token(&token) {
        return None;
    }
    let now = mongodb::bson::DateTime::from_millis(Utc::now().timestamp_millis());
    let sess = st
        .col("sessions")
        .find_one(doc! { "_id": token, "expires_at": { "$gt": now } })
        .await
        .ok()??;
    sess.get_str("username").ok().map(|s| s.to_string())
}

/// require_user for handlers: 401 with the shared error shape on failure.
pub async fn require_user(
    st: &AppState,
    headers: &HeaderMap,
) -> Result<String, (StatusCode, Json<shared::ErrorResponse>)> {
    current_user(st, headers)
        .await
        .ok_or_else(|| json_error(StatusCode::UNAUTHORIZED, "unauthorized"))
}

pub async fn edit_locked(st: &AppState) -> bool {
    st.col("edit_lock")
        .find_one(doc! { "_id": "lock" })
        .await
        .ok()
        .flatten()
        .map(|d| doc_bool(&d, "locked"))
        .unwrap_or(false)
}

pub async fn my_request_status(st: &AppState, user: &str) -> String {
    st.col("edit_requests")
        .find_one(doc! { "_id": user })
        .await
        .ok()
        .flatten()
        .map(|d| crate::db::doc_str(&d, "status"))
        .filter(|s| s == "pending" || s == "approved")
        .unwrap_or_else(|| "none".to_string())
}

pub async fn can_edit_now(st: &AppState, user: &str) -> bool {
    is_admin(user) || !edit_locked(st).await || my_request_status(st, user).await == "approved"
}
