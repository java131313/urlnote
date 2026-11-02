// urlnote standalone server — axum + MongoDB. Same API contract as the
// Cloudflare worker (shared DTOs); note/mark content and images in GridFS,
// structured data in collections. Serves the Leptos frontend's static build
// when STATIC_DIR (or the default dist locations) is present.

mod analyze;
mod auth;
mod db;
mod dict;
mod mark;
mod misc;
mod nlp;
mod notes;
mod seed;
mod token;
mod util;

use axum::extract::DefaultBodyLimit;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post, put};
use axum::Router;
use mongodb::bson::doc;
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::db::AppState;
use crate::util::{head_chars, last_segment_has_dot, new_token, now_str, password_hash, UPLOAD_HEADROOM, MAX_UPLOAD_BYTES};

async fn bootstrap(st: &AppState) {
    // built-in users (same sha256("user:pass") hashes as the SQL schema)
    let admin_pw = std::env::var("ADMIN_PASSWORD").unwrap_or_else(|_| "root@123".into());
    let users: [(&str, &str, &str); 3] = [
        ("admin", "管理员", admin_pw.as_str()),
        ("guest", "访客", "guest"),
        ("amigo", "Amigo", "amigo"),
    ];
    for (name, display, pw) in users {
        let _ = st
            .col("users")
            .update_one(
                doc! { "_id": name },
                doc! { "$setOnInsert": {
                    "display_name": display,
                    "password_hash": password_hash(name, pw),
                    "created_at": now_str(),
                } },
            )
            .with_options(mongodb::options::UpdateOptions::builder().upsert(true).build())
            .await;
    }
    // built-in languages
    let langs = [
        ("zh_cn", "简体中文", "简", 1),
        ("zh_tw", "繁體中文", "繁", 2),
        ("ja", "日本語", "日", 3),
        ("en", "English", "英", 4),
    ];
    for (code, name, badge, ord) in langs {
        let _ = st
            .col("dict_lang")
            .update_one(
                doc! { "_id": code },
                doc! { "$setOnInsert": { "name": name, "badge": badge, "ord": ord } },
            )
            .with_options(mongodb::options::UpdateOptions::builder().upsert(true).build())
            .await;
    }
}

fn api_router(st: AppState) -> Router {
    let cors = if st.cors_origins.is_empty() {
        None
    } else if st.cors_origins.iter().any(|o| o == "*") {
        // CORS_ORIGINS=*:反射任意 Origin(等同允许所有地址)。
        // 携带凭据时浏览器拒绝字面量 "*",只能用反射;注意这意味着任何
        // 网站都能带用户的 cookie 调用本 API,个人自用以外慎用。
        Some(
            CorsLayer::new()
                .allow_origin(AllowOrigin::predicate(|_, _| true))
                .allow_credentials(true)
                .allow_methods([
                    axum::http::Method::GET,
                    axum::http::Method::POST,
                    axum::http::Method::PUT,
                    axum::http::Method::DELETE,
                    axum::http::Method::OPTIONS,
                    axum::http::Method::HEAD,
                ])
                .allow_headers([
                    axum::http::header::CONTENT_TYPE,
                    axum::http::header::ACCEPT,
                    axum::http::header::AUTHORIZATION,
                    axum::http::header::HeaderName::from_static("x-requested-with"),
                ]),
        )
    } else {
        let origins: Vec<axum::http::HeaderValue> = st
            .cors_origins
            .iter()
            .filter_map(|o| o.parse().ok())
            .collect();
        // 注意:allow_credentials(true) 时 tower-http 禁止 Any 通配
        // (浏览器规范:携带凭据时不允许 *),必须给出显式清单
        Some(
            CorsLayer::new()
                .allow_origin(AllowOrigin::list(origins))
                .allow_credentials(true)
                .allow_methods([
                    axum::http::Method::GET,
                    axum::http::Method::POST,
                    axum::http::Method::PUT,
                    axum::http::Method::DELETE,
                    axum::http::Method::OPTIONS,
                    axum::http::Method::HEAD,
                ])
                .allow_headers([
                    axum::http::header::CONTENT_TYPE,
                    axum::http::header::ACCEPT,
                    axum::http::header::AUTHORIZATION,
                    axum::http::header::HeaderName::from_static("x-requested-with"),
                ]),
        )
    };

    let r = Router::new()
        .route(
            "/api/note/{id}",
            get(notes::get_note).put(notes::save_note).post(notes::save_note),
        )
        .route("/api/notes", get(notes::list_notes))
        .route("/api/login", post(misc::login))
        .route("/api/logout", post(misc::logout))
        .route("/api/whoami", get(misc::whoami))
        .route("/api/presence", post(misc::presence))
        .route(
            "/api/upload",
            post(misc::upload).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES + UPLOAD_HEADROOM)),
        )
        .route("/api/edit-status", get(misc::edit_status))
        .route("/api/edit-lock", post(misc::set_edit_lock))
        .route("/api/edit-request", post(misc::submit_edit_request))
        .route("/api/edit-requests", get(misc::list_edit_requests))
        .route("/api/edit-requests/handle", post(misc::handle_edit_request))
        .route("/api/search", get(notes::search_notes))
        .route("/api/search/reindex", post(notes::reindex_search))
        .route(
            "/api/pdf2md",
            post(misc::pdf_to_md).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES + UPLOAD_HEADROOM)),
        )
        .route("/api/analyze", post(nlp::analyze_text))
        .route("/api/pos", post(nlp::pos_tag))
        .route("/api/pos/doc", post(nlp::pos_document))
        .route("/api/mark/check", post(nlp::check_marks))
        .route(
            "/api/mark/{id}",
            post(nlp::generate_mark)
                .put(nlp::save_mark)
                .delete(nlp::remove_mark)
                .get(nlp::get_mark),
        )
        .route("/api/dict/lookup", get(dict::dict_lookup))
        .route("/api/dict/langs", get(dict::dict_langs).post(dict::create_lang))
        .route("/api/dict/words", get(dict::list_words).post(dict::create_word))
        .route("/api/dict/words/{id}", put(dict::update_word).delete(dict::delete_word))
        .route("/i/{name}", get(misc::image))
        .with_state(st);
    match cors {
        Some(c) => r.layer(c),
        None => r,
    }
}

/// Static + SPA fallback: real files served from STATIC_DIR; extensionless
/// GET/HEAD paths get index.html (client-side routes like /u0yYk); missing
/// files with an extension 404 (valid note ids never contain a dot).
async fn spa_fallback(
    axum::Extension(st): axum::Extension<AppState>,
    req: axum::extract::Request,
) -> axum::response::Response {
    if req.method() != axum::http::Method::GET && req.method() != axum::http::Method::HEAD {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(dir) = st.static_dir.clone() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let path = req.uri().path().trim_start_matches('/');
    let rel = if path.is_empty() || !last_segment_has_dot(path) {
        "index.html"
    } else {
        path
    };
    // guard against path traversal
    if rel.contains("..") || rel.starts_with('/') {
        return StatusCode::NOT_FOUND.into_response();
    }
    let file = dir.join(rel);
    match tokio::fs::read(&file).await {
        Ok(bytes) => {
            let mime = match file.extension().and_then(|e| e.to_str()) {
                Some("html") => "text/html; charset=utf-8",
                Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
                Some("css") => "text/css; charset=utf-8",
                Some("json") => "application/json; charset=utf-8",
                Some("wasm") => "application/wasm",
                Some("png") => "image/png",
                Some("svg") => "image/svg+xml",
                Some("ico") => "image/x-icon",
                Some("txt") => "text/plain; charset=utf-8",
                Some("woff2") => "font/woff2",
                _ => "application/octet-stream",
            };
            crate::util::response_with(
                StatusCode::OK,
                &[("content-type", mime), ("cache-control", "no-cache")],
                bytes,
            )
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

#[tokio::main]
async fn main() {
    let arg = std::env::args().nth(1);
    let st = AppState::connect().await;
    bootstrap(&st).await;

    if arg.as_deref() == Some("seed") {
        match seed::run(&st).await {
            Ok(()) => println!("dict seed done"),
            Err(e) => {
                eprintln!("seed failed: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    // background consistency sweep (replaces the Cloudflare cron)
    {
        let st = st.clone();
        tokio::spawn(async move {
            let mut tick =
                tokio::time::interval(std::time::Duration::from_secs(20 * 60));
            tick.tick().await; // first tick fires immediately; skip it
            loop {
                tick.tick().await;
                match mark::sweep(&st, 3).await {
                    Ok((removed, regenerated, scanned)) => {
                        println!("mark sweep: {scanned} processed ({removed} orphans, {regenerated} regenerated)")
                    }
                    Err(e) => eprintln!("mark sweep failed: {e}"),
                }
            }
        });
    }

    let app = api_router(st.clone())
        .fallback(spa_fallback)
        .layer(axum::Extension(st));

    let port: u16 = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8080);
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .unwrap_or_else(|e| panic!("bind :{port} failed: {e}"));
    println!("urlnote-server listening on :{port}");
    axum::serve(listener, app).await.unwrap();
}

// referenced by notes.rs (kept for parity with the worker's helper usage)
#[allow(dead_code)]
fn _helpers() {
    let _ = new_token();
    let _ = head_chars("", 0);
}
