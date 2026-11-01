//! urlnote Cloudflare Worker — a Rust port of the original Go backend
//! (../main.go, deployed via workers-go). The API contract and status codes
//! are kept identical, so the Go and Rust workers are interchangeable behind
//! the same wrangler deployment. Caching differs by design: page content is
//! served from a per-isolate in-memory LRU (backed by D1), not KV — the free
//! KV plan only affords 1k writes/day. KV remains solely for image blobs.

use lru::LruCache;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use shared::*;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use wasm_bindgen::{JsCast, JsValue};
use worker::*;

mod analyze;
mod mark;
mod token;

// ---- in-memory LRU response cache (per isolate) ----
//
// Reads fall through to D1 and populate the cache; writes update both the
// cache and D1. Entries carry a short TTL: isolates don't share memory, so
// without it a save handled by another isolate would stay invisible to this
// isolate's readers for as long as the entry survived (the old shared KV
// cache didn't have this failure mode). The polling frontend notices remote
// changes via updated_at, so worst-case sync lag is one TTL.
//
// Key layout is versioned exactly like the old KV cache and the Go backend
// (v4 payloads carry updated_at).
const NOTE_KEY_PREFIX: &str = "v4/notes/";
const INDEX_KEY: &str = "v4/index.json";
const CACHE_CONTENT_TYPE: &str = "application/json; charset=utf-8";

const NOTE_CACHE_CAP: usize = 512;
const NOTE_CACHE_TTL_MS: f64 = 30_000.0;
const INDEX_CACHE_TTL_MS: f64 = 15_000.0;

fn note_key(id: &str) -> String {
    format!("{NOTE_KEY_PREFIX}{id}.json")
}

struct CacheEntry {
    payload: Vec<u8>,
    expires_ms: f64,
}

thread_local! {
    static RESPONSE_CACHE: RefCell<LruCache<String, CacheEntry>> =
        RefCell::new(LruCache::new(
            NonZeroUsize::new(NOTE_CACHE_CAP).unwrap(),
        ));
}

// js_sys::Date — std::time is not available on wasm32-unknown-unknown.
fn now_ms() -> f64 {
    js_sys::Date::now()
}

fn cache_get(key: &str) -> Option<Vec<u8>> {
    RESPONSE_CACHE.with(|c| {
        let mut c = c.borrow_mut();
        match c.get(key) {
            Some(entry) if entry.expires_ms > now_ms() => Some(entry.payload.clone()),
            Some(_) => {
                c.pop(key);
                None
            }
            None => None,
        }
    })
}

fn cache_put(key: &str, payload: Vec<u8>, ttl_ms: f64) {
    RESPONSE_CACHE.with(|c| {
        c.borrow_mut().put(
            key.to_string(),
            CacheEntry {
                payload,
                expires_ms: now_ms() + ttl_ms,
            },
        );
    });
}

fn cache_remove(key: &str) {
    RESPONSE_CACHE.with(|c| {
        c.borrow_mut().pop(key);
    });
}

// ---- users & sessions ----
//
// Sessions live in D1, not KV: the free KV plan allows only 1k writes/day,
// which a handful of logins plus presence heartbeats would exhaust before
// noon. D1's free tier (100k rows written/day) has ample headroom. KV is kept
// solely for uploaded images now that note content uses the in-memory LRU.

const SESSION_TTL_SEC: u64 = 7 * 24 * 3600;

// ---- presence (heartbeat, D1) ----

// Presence rows older than this are treated as gone (cleaned up on every
// heartbeat and excluded from listings). The client beats every 15s, so 90s
// still means "offline" ~75s after a client goes away.
const PRESENCE_TTL_SEC: u64 = 90;

// ---- image upload & serving ----

const MAX_UPLOAD_BYTES: usize = 8 << 20;
// headroom for multipart overhead beyond the raw file limit
const UPLOAD_HEADROOM: usize = 1 << 20;
const IMAGE_KEY_PREFIX: &str = "img/";

#[event(fetch)]
pub async fn main(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    // KV now only stores uploaded images; page content uses the in-memory LRU.
    let cache = env.kv("CACHE").ok();

    let method = req.method();
    let path = req.path();
    let segs: Vec<&str> = path.trim_start_matches('/').split('/').collect();

    // CORS preflight for cross-origin frontends (e.g. GitHub Pages).
    if method == Method::Options && segs.first() == Some(&"api") {
        return cors_preflight(&req, &env);
    }

    // capture before the match: the SPA fallback arm moves `req`
    let req_origin = req.headers().get("Origin").ok().flatten();
    let is_api = segs.first() == Some(&"api");

    let mut resp = match (method, segs.as_slice()) {
        // Go's ServeMux matches HEAD for GET patterns too.
        (Method::Get, ["api", "note", id]) | (Method::Head, ["api", "note", id]) => {
            get_note(&req, &env, id).await
        }
        (Method::Put, ["api", "note", id]) | (Method::Post, ["api", "note", id]) => {
            save_note(req, &env, id).await // POST kept for API compatibility (was sendBeacon)
        }
        (Method::Get, ["api", "notes"]) => list_notes(&req, &env).await,
        (Method::Post, ["api", "login"]) => login(req, &env).await,
        (Method::Post, ["api", "logout"]) => logout(&req, &env).await,
        (Method::Get, ["api", "whoami"]) => whoami(&req, &env).await,
        (Method::Post, ["api", "presence"]) => presence(req, &env).await,
        (Method::Post, ["api", "upload"]) => upload(req, &env, cache).await,
        (Method::Get, ["api", "edit-status"]) => edit_status(&req, &env).await,
        (Method::Get, ["api", "search"]) => search_notes(&req, &env).await,
        (Method::Post, ["api", "analyze"]) => analyze_text(req, &env).await,
        (Method::Post, ["api", "pos"]) => pos_tag(req, &env).await,
        (Method::Post, ["api", "pos", "doc"]) => pos_document(req, &env).await,
        (Method::Post, ["api", "mark", "check"]) => check_marks(req, &env).await,
        (Method::Post, ["api", "mark", id]) => generate_mark(req, &env, id).await,
        (Method::Put, ["api", "mark", id]) => save_mark(req, &env, id).await,
        (Method::Delete, ["api", "mark", id]) => remove_mark(req, &env, id).await,
        (Method::Get, ["api", "mark", id]) => get_mark(req, &env, id).await,
        (Method::Post, ["api", "search", "reindex"]) => reindex_search(&req, &env).await,
        (Method::Get, ["api", "dict", "lookup"]) => dict_lookup(&req, &env).await,
        (Method::Get, ["api", "dict", "langs"]) => dict_langs(&req, &env).await,
        (Method::Post, ["api", "dict", "langs"]) => create_lang(req, &env).await,
        (Method::Get, ["api", "dict", "words"]) => list_words(&req, &env).await,
        (Method::Post, ["api", "dict", "words"]) => create_word(req, &env).await,
        (Method::Put, ["api", "dict", "words", id]) => update_word(req, &env, id).await,
        (Method::Delete, ["api", "dict", "words", id]) => {
            delete_word(&req, &env, id).await
        }
        (Method::Post, ["api", "edit-lock"]) => set_edit_lock(req, &env).await,
        (Method::Post, ["api", "edit-request"]) => submit_edit_request(req, &env).await,
        (Method::Get, ["api", "edit-requests"]) => list_edit_requests(&req, &env).await,
        (Method::Post, ["api", "edit-requests", "handle"]) => {
            handle_edit_request(req, &env).await
        }
        (Method::Get, ["i", name]) => image(&req, &env, cache, name).await,
        // SPA fallback for client-side routes like /u0yYk (GET/HEAD only).
        (Method::Get, _) | (Method::Head, _) => serve_app(req, &env).await,
        _ => Response::error("not found", 404),
    }?;

    // Cross-origin frontends (GitHub Pages) read API responses in
    // credentials mode, which browsers only allow with an explicit origin.
    if is_api {
        apply_cors(&env, req_origin.as_deref(), &mut resp);
    }
    Ok(resp)
}

// ---- CORS (cross-origin frontends such as GitHub Pages) ----

/// Origins allowed to call the API from a browser, comma-separated, from the
/// CORS_ORIGINS variable (see wrangler.toml). Empty/undefined keeps the
/// deployment same-origin only, exactly as before.
fn cors_origins(env: &Env) -> Vec<String> {
    env.var("CORS_ORIGINS")
        .map(|v| {
            v.to_string()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn apply_cors(env: &Env, origin: Option<&str>, resp: &mut Response) {
    let Some(origin) = origin else {
        return;
    };
    if !cors_origins(env).iter().any(|o| o == origin) {
        return;
    }
    let headers = resp.headers();
    let _ = headers.set("Access-Control-Allow-Origin", origin);
    let _ = headers.set("Access-Control-Allow-Credentials", "true");
    let _ = headers.set("Vary", "Origin");
}

fn cors_preflight(req: &Request, env: &Env) -> Result<Response> {
    let origin = req
        .headers()
        .get("Origin")
        .unwrap_or_default()
        .unwrap_or_default();
    let mut resp = Response::empty()?;
    if cors_origins(env).iter().any(|o| o == &origin) {
        let headers = resp.headers();
        let _ = headers.set("Access-Control-Allow-Origin", &origin);
        let _ = headers.set("Access-Control-Allow-Credentials", "true");
        let _ = headers.set("Access-Control-Allow-Methods", "GET, POST, PUT, DELETE, OPTIONS");
        // echo the headers the client asked to send (Content-Type, ...)
        let requested = req
            .headers()
            .get("Access-Control-Request-Headers")
            .unwrap_or_default()
            .unwrap_or_default();
        let allow = if requested.is_empty() {
            "Content-Type".to_string()
        } else {
            requested
        };
        let _ = headers.set("Access-Control-Allow-Headers", &allow);
        let _ = headers.set("Access-Control-Max-Age", "86400");
        resp = resp.with_status(204);
    }
    Ok(resp)
}

// Cron-triggered background consistency check between notes and notes_mark.
// Also runs on-demand via POST /api/mark/check for manual verification.
#[event(scheduled)]
pub async fn scheduled(_event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    let _ = token::ensure_ja(&env).await;
    match mark::sweep(&env, 3).await {
        Ok((removed, regenerated, scanned)) => {
            console_log!("mark sweep: {scanned} processed ({removed} orphans, {regenerated} regenerated)");
        }
        Err(e) => console_log!("mark sweep failed: {e}"),
    }
}

// ---- response helpers ----

fn json_response(status: u16, payload: Vec<u8>, x_cache: Option<&str>) -> Result<Response> {
    let mut resp = Response::from_bytes(payload)?;
    resp = resp.with_status(status);
    let headers = Headers::new();
    headers.set("Content-Type", CACHE_CONTENT_TYPE)?;
    if let Some(x) = x_cache {
        headers.set("X-Cache", x)?;
    }
    Ok(resp.with_headers(headers))
}

fn json_error(status: u16, msg: &str) -> Result<Response> {
    let payload = serde_json::to_vec(&ErrorResponse {
        error: msg.to_string(),
    })
    .unwrap();
    json_response(status, payload, None)
}

// ---- crypto / random helpers ----

fn crypto() -> web_sys::Crypto {
    js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("crypto"))
        .expect("globalThis.crypto")
        .dyn_into()
        .expect("crypto is not Crypto")
}

fn rand_bytes(n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    crypto()
        .get_random_values_with_u8_array(&mut v)
        .expect("crypto.getRandomValues");
    v
}

fn new_token() -> String {
    const ALPHABET: &[u8; 62] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    rand_bytes(32)
        .iter()
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect()
}

fn new_image_key() -> String {
    const ALPHABET: &[u8; 62] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut s = String::from(IMAGE_KEY_PREFIX);
    for b in rand_bytes(16) {
        s.push(ALPHABET[(b as usize) % ALPHABET.len()] as char);
    }
    s
}

fn password_hash(username: &str, password: &str) -> String {
    let mut h = Sha256::new();
    h.update(username.as_bytes());
    h.update(b":");
    h.update(password.as_bytes());
    to_hex(&h.finalize())
}

fn to_hex(b: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push(HEX[(x >> 4) as usize] as char);
        s.push(HEX[(x & 0xf) as usize] as char);
    }
    s
}

// Constant-time equality, mirroring Go's hmac.Equal usage.
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut d = 0u8;
    for i in 0..a.len() {
        d |= a[i] ^ b[i];
    }
    d == 0
}

// ---- auth ----

fn cookie_value<'a>(cookie_header: &'a str, name: &str) -> Option<&'a str> {
    let prefix = format!("{name}=");
    cookie_header
        .split(';')
        .map(str::trim)
        .find_map(|p| p.strip_prefix(&prefix))
}

// Guards a handler like Go's requireUser: on failure it writes 401 and
// returns from the enclosing function.
macro_rules! require_user {
    ($req:expr, $env:expr) => {
        match current_user($req, $env).await {
            Some(u) => u,
            None => return json_error(401, "unauthorized"),
        }
    };
}

async fn current_user(req: &Request, env: &Env) -> Option<String> {
    let cookie = req.headers().get("Cookie").ok()??;
    let token = cookie_value(&cookie, "session")?;
    if !valid_token(token) {
        return None;
    }
    let row = env
        .d1("DB")
        .ok()?
        .prepare("SELECT username FROM sessions WHERE token = ?1 AND expires_at > datetime('now')")
        .bind(&[JsValue::from(token)])
        .ok()?
        .first::<SessionUserRow>(None)
        .await
        .ok()??;
    Some(row.username)
}



fn is_https(req: &Request) -> bool {
    req.headers()
        .get("CF-Visitor")
        .ok()
        .flatten()
        .is_some_and(|v| v.contains("\"https\""))
}

// ---- D1 row shapes ----

#[derive(Deserialize)]
struct NoteRow {
    content: String,
    font_size: i64,
    color: String,
    title: String,
    tags: String,
    author: String,
    updated_at: String,
}

#[derive(Deserialize)]
struct MetaRow {
    id: String,
    title: String,
    tags: String,
    updated_at: String,
    excerpt: String,
    author: String,
}

#[derive(Deserialize)]
struct UpdatedAtRow {
    updated_at: String,
}

#[derive(Deserialize)]
struct UserRow {
    password_hash: String,
}

#[derive(Deserialize)]
struct LockRow {
    locked: i64,
}

#[derive(Deserialize)]
struct StatusRow {
    status: String,
}

#[derive(Deserialize)]
struct EditRequestRow {
    username: String,
    status: String,
    message: String,
    created_at: String,
}

#[derive(Deserialize)]
struct SessionUserRow {
    username: String,
}

#[derive(Deserialize)]
struct PresenceUserRow {
    username: String,
}

fn parse_tags(s: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(s).unwrap_or_default()
}

// ---- handlers ----

async fn get_note(req: &Request, env: &Env, id: &str) -> Result<Response> {
    if !valid_note_id(id) {
        return Response::error("not found", 404);
    }
    require_user!(req, env);

    // cache hit: serve the in-memory copy directly
    if let Some(payload) = cache_get(&note_key(id)) {
        return json_response(200, payload, Some("HIT"));
    }

    let row = env
        .d1("DB")?
        .prepare(
            "SELECT content, font_size, color, title, tags, author, updated_at
             FROM notes WHERE id = ?1",
        )
        .bind(&[JsValue::from(id)])?
        .first::<NoteRow>(None)
        .await?;

    let note = match row {
        None => Note::empty(id),
        Some(r) => Note {
            id: id.to_string(),
            content: r.content,
            font_size: r.font_size,
            color: r.color,
            title: r.title,
            tags: parse_tags(&r.tags),
            author: r.author,
            updated_at: r.updated_at,
        },
    };
    let payload = serde_json::to_vec(&note).unwrap();
    cache_put(&note_key(id), payload.clone(), NOTE_CACHE_TTL_MS);
    json_response(200, payload, Some("MISS"))
}

async fn save_note(mut req: Request, env: &Env, id: &str) -> Result<Response> {
    if !valid_note_id(id) {
        return Response::error("not found", 404);
    }
    let user = require_user!(&req, env);
    if !can_edit_now(env, &user).await {
        return json_error(403, "edit locked");
    }

    // The body carries the full note state (requests may arrive without
    // fetch-style headers); omitted settings fall back to defaults.
    let body: SaveRequest = match req.json().await {
        Ok(b) => b,
        Err(_) => return json_error(400, "bad request"),
    };
    let font_size = body.font_size.map(resolve_font_size).unwrap_or(DEFAULT_FONT_SIZE);
    let color = if valid_color(&body.color) {
        body.color.clone()
    } else {
        String::new()
    };
    let title = clean_title(&body.title);
    let tags = clean_tags(&body.tags);
    let tags_json = serde_json::to_string(&tags).unwrap();

    let updated_at = match env
        .d1("DB")?
        .prepare(
            "INSERT INTO notes (id, content, font_size, color, title, tags, author, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, CURRENT_TIMESTAMP)
             ON CONFLICT(id) DO UPDATE SET content = excluded.content,
                 font_size = excluded.font_size, color = excluded.color,
                 title = excluded.title, tags = excluded.tags,
                 author = excluded.author, updated_at = CURRENT_TIMESTAMP
             RETURNING updated_at",
        )
        .bind(&[
            JsValue::from(id),
            JsValue::from(body.content.as_str()),
            // D1 rejects BigInt bindings — JS numbers are f64
            JsValue::from(font_size as f64),
            JsValue::from(color.as_str()),
            JsValue::from(title.as_str()),
            JsValue::from(tags_json.as_str()),
            JsValue::from(user.as_str()),
        ])?
        .first::<UpdatedAtRow>(None)
        .await
    {
        Ok(Some(r)) => r.updated_at,
        _ => return json_error(500, "save failed"),
    };

    // write-through: keep the LRU (note + index) in sync with D1. The full
    // payload (incl. updated_at/author) is echoed back so clients can sync
    // without an extra GET.
    let note = Note {
        id: id.to_string(),
        content: body.content,
        font_size,
        color,
        title,
        tags,
        author: user,
        updated_at,
    };
    let payload = serde_json::to_vec(&note).unwrap();
    cache_put(&note_key(id), payload.clone(), NOTE_CACHE_TTL_MS);

    // full-text index: tokenize title/tags/content with jieba + lindera and
    // store the space-joined tokens (index failures must not fail the save)
    let index_text = head_chars(
        &format!("{} {} {}", note.title, note.tags.join(" "), note.content),
        INDEX_MAX_CHARS,
    );
    let tokens = token::tokenize(env, &index_text).await.join(" ");
    let fts = env.d1("DB")?;
    let _ = fts
        .prepare("DELETE FROM notes_fts WHERE note_id = ?1")
        .bind(&[JsValue::from(id)])?
        .run()
        .await;
    if let Err(e) = fts
        .prepare("INSERT INTO notes_fts (note_id, tokens) VALUES (?1, ?2)")
        .bind(&[JsValue::from(id), JsValue::from(tokens)])?
        .run()
        .await
    {
        console_log!("fts index failed for {id}: {e}");
    }

    // eager marked copy for the POS overlay (index-time generation); a
    // hand-edited mark (source_updated_at = '') is never clobbered
    if let Err(e) = mark::regenerate_auto(env, id).await {
        console_log!("mark regenerate failed for {id}: {e}");
    }

    rebuild_index(env).await;
    json_response(200, payload, None)
}

async fn query_note_meta(env: &Env) -> std::result::Result<Vec<NoteMeta>, worker::Error> {
    let rows = env
        .d1("DB")?
        .prepare(
            "SELECT id, title, tags, updated_at, substr(content, 1, 120) AS excerpt, author
             FROM notes ORDER BY updated_at DESC",
        )
        .all()
        .await?
        .results::<MetaRow>()?;
    Ok(rows
        .into_iter()
        .map(|r| NoteMeta {
            id: r.id,
            title: r.title,
            tags: parse_tags(&r.tags),
            updated_at: r.updated_at,
            excerpt: r.excerpt,
            author: r.author,
        })
        .collect())
}

// rebuild_index refreshes the cached note list after a write; on failure the
// entry is evicted so the next list request falls back to D1.
async fn rebuild_index(env: &Env) {
    let metas = match query_note_meta(env).await {
        Ok(m) => m,
        Err(e) => {
            console_log!("rebuild index query failed: {e}");
            cache_remove(INDEX_KEY);
            return;
        }
    };
    let payload = serde_json::to_vec(&NoteList { notes: metas }).unwrap();
    cache_put(INDEX_KEY, payload, INDEX_CACHE_TTL_MS);
}

async fn list_notes(req: &Request, env: &Env) -> Result<Response> {
    require_user!(req, env);

    if let Some(payload) = cache_get(INDEX_KEY) {
        return json_response(200, payload, Some("HIT"));
    }
    let metas = match query_note_meta(env).await {
        Ok(m) => m,
        Err(_) => return json_error(500, "list failed"),
    };
    let payload = serde_json::to_vec(&NoteList { notes: metas }).unwrap();
    cache_put(INDEX_KEY, payload.clone(), INDEX_CACHE_TTL_MS);
    json_response(200, payload, Some("MISS"))
}

async fn login(mut req: Request, env: &Env) -> Result<Response> {
    let body: LoginRequest = match req.json().await {
        Ok(b) => b,
        Err(_) => return json_error(400, "bad request"),
    };
    let username = body.username.trim().to_string();

    let stored = env
        .d1("DB")?
        .prepare("SELECT password_hash FROM users WHERE username = ?1")
        .bind(&[JsValue::from(username.as_str())])?
        .first::<UserRow>(None)
        .await;
    let ok = match stored {
        Ok(Some(r)) => ct_eq(&password_hash(&username, &body.password), &r.password_hash),
        _ => false,
    };
    if !ok {
        return json_error(401, "用户名或密码错误");
    }

    // Expired rows are purged opportunistically on each login; D1 deletes are
    // far cheaper than the free KV write quota this used to burn.
    let db = env.d1("DB")?;
    let _ = db
        .prepare("DELETE FROM sessions WHERE expires_at <= datetime('now')")
        .run()
        .await;
    let token = new_token();
    let result = db
        .prepare(
            "INSERT INTO sessions (token, username, expires_at)
             VALUES (?1, ?2, datetime('now', ?3))",
        )
        .bind(&[
            JsValue::from(token.as_str()),
            JsValue::from(username.as_str()),
            JsValue::from(format!("+{SESSION_TTL_SEC} seconds")),
        ])?
        .run()
        .await;
    if result.is_err() {
        return json_error(500, "session failed");
    }

    // SameSite=None is required for cross-origin deployments (frontend on
    // GitHub Pages): browsers only attach the session cookie to cross-site
    // fetches with SameSite=None, which in turn requires Secure (HTTPS).
    let cookie = format!(
        "session={token}; Path=/; Max-Age={SESSION_TTL_SEC}; HttpOnly{}",
        if is_https(&req) {
            "; Secure; SameSite=None"
        } else {
            // local dev over plain http
            "; SameSite=Lax"
        }
    );
    let payload = serde_json::to_vec(&LoginResponse {
        ok: true,
        user: User { username },
    })
    .unwrap();
    let resp = Response::from_bytes(payload)?;
    let headers = Headers::new();
    headers.set("Content-Type", CACHE_CONTENT_TYPE)?;
    headers.set("Set-Cookie", &cookie)?;
    Ok(resp.with_headers(headers))
}

async fn logout(req: &Request, env: &Env) -> Result<Response> {
    if let Ok(Some(cookie)) = req.headers().get("Cookie") {
        if let Some(token) = cookie_value(&cookie, "session") {
            if valid_token(token) {
                let _ = env
                    .d1("DB")?
                    .prepare("DELETE FROM sessions WHERE token = ?1")
                    .bind(&[JsValue::from(token)])?
                    .run()
                    .await;
            }
        }
    }
    let payload = serde_json::to_vec(&OkResponse { ok: true }).unwrap();
    let resp = Response::from_bytes(payload)?;
    let headers = Headers::new();
    headers.set("Content-Type", CACHE_CONTENT_TYPE)?;
    headers.set("Set-Cookie", "session=; Path=/; Max-Age=0")?;
    Ok(resp.with_headers(headers))
}

async fn whoami(req: &Request, env: &Env) -> Result<Response> {
    let user = require_user!(req, env);
    let payload = serde_json::to_vec(&WhoamiResponse {
        user: User { username: user },
    })
    .unwrap();
    json_response(200, payload, None)
}

async fn presence(mut req: Request, env: &Env) -> Result<Response> {
    let user = require_user!(&req, env);
    let body: PresenceRequest = match req.json().await {
        Ok(b) => b,
        Err(_) => return json_error(400, "bad request"),
    };
    if !valid_note_id(&body.note_id) {
        return json_error(400, "bad request");
    }

    let db = env.d1("DB")?;
    let ttl = format!("-{PRESENCE_TTL_SEC} seconds");
    let upsert = db
        .prepare(
            "INSERT INTO presence (note_id, username, updated_at)
             VALUES (?1, ?2, CURRENT_TIMESTAMP)
             ON CONFLICT(note_id, username) DO UPDATE SET updated_at = CURRENT_TIMESTAMP",
        )
        .bind(&[JsValue::from(body.note_id.as_str()), JsValue::from(user.as_str())])?
        .run()
        .await;
    if upsert.is_err() {
        return json_error(500, "presence failed");
    }
    // opportunistic cleanup of stale rows (also excluded in the SELECT below)
    let _ = db
        .prepare("DELETE FROM presence WHERE updated_at < datetime('now', ?1)")
        .bind(&[JsValue::from(ttl.as_str())])?
        .run()
        .await;

    let users = db
        .prepare(
            "SELECT username FROM presence
             WHERE note_id = ?1 AND updated_at >= datetime('now', ?2)
             ORDER BY username",
        )
        .bind(&[JsValue::from(body.note_id.as_str()), JsValue::from(ttl.as_str())])?
        .all()
        .await?
        .results::<PresenceUserRow>()?;
    let users = users.into_iter().map(|r| r.username).collect();
    let payload = serde_json::to_vec(&PresenceResponse { users }).unwrap();
    json_response(200, payload, None)
}

// ---- edit lock ----
//
// The lock lives in D1: edit_lock is a single-row flag, edit_requests holds
// one row per user (pending until approved; reject/revoke delete the row so
// the user can apply again). Admin (the seeded "admin" account) is always
// allowed to edit and is the only one who can flip the lock or handle
// requests. All lock-state reads fail open (unlocked): a D1 hiccup must not
// brick editing for everyone — the write path still fails loudly if D1 is
// actually down.

fn is_admin(user: &str) -> bool {
    user == ADMIN_USERNAME
}

async fn edit_locked(env: &Env) -> bool {
    let Ok(db) = env.d1("DB") else { return false };
    match db
        .prepare("SELECT locked FROM edit_lock WHERE id = 1")
        .first::<LockRow>(None)
        .await
    {
        Ok(Some(r)) => r.locked != 0,
        _ => false,
    }
}

async fn my_request_status(env: &Env, user: &str) -> String {
    let Ok(db) = env.d1("DB") else { return "none".to_string() };
    let Ok(stmt) = db
        .prepare("SELECT status FROM edit_requests WHERE username = ?1")
        .bind(&[JsValue::from(user)])
    else {
        return "none".to_string();
    };
    match stmt.first::<StatusRow>(None).await {
        Ok(Some(r)) => r.status,
        _ => "none".to_string(),
    }
}

async fn can_edit_now(env: &Env, user: &str) -> bool {
    is_admin(user) || !edit_locked(env).await || my_request_status(env, user).await == "approved"
}

async fn edit_status(req: &Request, env: &Env) -> Result<Response> {
    let user = require_user!(req, env);
    let locked = edit_locked(env).await;
    let request = if is_admin(&user) {
        "none".to_string()
    } else {
        my_request_status(env, &user).await
    };
    let can_edit = is_admin(&user) || !locked || request == "approved";
    let payload = serde_json::to_vec(&EditStatusResponse {
        locked,
        is_admin: is_admin(&user),
        can_edit,
        request,
    })
    .unwrap();
    json_response(200, payload, None)
}

async fn set_edit_lock(mut req: Request, env: &Env) -> Result<Response> {
    let user = require_user!(&req, env);
    if !is_admin(&user) {
        return json_error(403, "forbidden");
    }
    let body: EditLockRequest = match req.json().await {
        Ok(b) => b,
        Err(_) => return json_error(400, "bad request"),
    };
    let locked = if body.locked { 1.0f64 } else { 0.0 };
    let result = env
        .d1("DB")?
        .prepare(
            "INSERT INTO edit_lock (id, locked, updated_at) VALUES (1, ?1, CURRENT_TIMESTAMP)
             ON CONFLICT(id) DO UPDATE SET locked = excluded.locked, updated_at = CURRENT_TIMESTAMP",
        )
        .bind(&[JsValue::from(locked)])?
        .run()
        .await;
    if result.is_err() {
        return json_error(500, "lock failed");
    }
    let payload = serde_json::to_vec(&OkResponse { ok: true }).unwrap();
    json_response(200, payload, None)
}

async fn submit_edit_request(
    mut req: Request,
    env: &Env,
) -> Result<Response> {
    let user = require_user!(&req, env);
    let body: EditRequestSubmit = match req.json().await {
        Ok(b) => b,
        Err(_) => return json_error(400, "bad request"),
    };
    // Admin never needs approval; an existing approval is left untouched.
    if is_admin(&user) || my_request_status(env, &user).await == "approved" {
        let payload = serde_json::to_vec(&OkResponse { ok: true }).unwrap();
        return json_response(200, payload, None);
    }
    let message: String = body.message.trim().chars().take(200).collect();
    let result = env
        .d1("DB")?
        .prepare(
            "INSERT OR REPLACE INTO edit_requests (username, status, message, created_at)
             VALUES (?1, 'pending', ?2, CURRENT_TIMESTAMP)",
        )
        .bind(&[JsValue::from(user.as_str()), JsValue::from(message.as_str())])?
        .run()
        .await;
    if result.is_err() {
        return json_error(500, "request failed");
    }
    let payload = serde_json::to_vec(&OkResponse { ok: true }).unwrap();
    json_response(200, payload, None)
}

async fn list_edit_requests(
    req: &Request,
    env: &Env,
) -> Result<Response> {
    let user = require_user!(req, env);
    if !is_admin(&user) {
        return json_error(403, "forbidden");
    }
    let rows = env
        .d1("DB")?
        .prepare(
            "SELECT username, status, message, created_at FROM edit_requests
             ORDER BY created_at DESC",
        )
        .all()
        .await?
        .results::<EditRequestRow>()?;
    let payload = serde_json::to_vec(&EditRequestList {
        requests: rows
            .into_iter()
            .map(|r| EditRequestItem {
                username: r.username,
                status: r.status,
                message: r.message,
                created_at: r.created_at,
            })
            .collect(),
    })
    .unwrap();
    json_response(200, payload, None)
}

async fn handle_edit_request(
    mut req: Request,
    env: &Env,
) -> Result<Response> {
    let user = require_user!(&req, env);
    if !is_admin(&user) {
        return json_error(403, "forbidden");
    }
    let body: EditHandleRequest = match req.json().await {
        Ok(b) => b,
        Err(_) => return json_error(400, "bad request"),
    };
    let target = body.username.trim();
    if target.is_empty() || target.chars().count() > 32 || is_admin(target) {
        return json_error(400, "bad request");
    }
    let db = env.d1("DB")?;
    let result = match body.action.as_str() {
        // Upsert: approving a pending row keeps its message; re-approving is idempotent.
        "approve" => db
            .prepare(
                "INSERT INTO edit_requests (username, status, message, created_at)
                 VALUES (?1, 'approved', '', CURRENT_TIMESTAMP)
                 ON CONFLICT(username) DO UPDATE SET status = 'approved'",
            )
            .bind(&[JsValue::from(target)])?
            .run()
            .await,
        "reject" | "revoke" => db
            .prepare("DELETE FROM edit_requests WHERE username = ?1")
            .bind(&[JsValue::from(target)])?
            .run()
            .await,
        _ => return json_error(400, "bad request"),
    };
    if result.is_err() {
        return json_error(500, "handle failed");
    }
    let payload = serde_json::to_vec(&OkResponse { ok: true }).unwrap();
    json_response(200, payload, None)
}

// ---- full-text search ----
//
// Tokens are produced by jieba (Chinese) + lindera/ipadic (Japanese) at save
// time and stored space-joined in an ascii-tokenized FTS5 table. Queries go
// through the same pipeline and the terms are ANDed — word-level semantics
// ("中国" doesn't match "中间"), with bm25 ranking.

fn query_param(req: &Request, name: &str) -> Option<String> {
    let url = req.url().ok()?;
    url.query_pairs()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
}

async fn search_notes(req: &Request, env: &Env) -> Result<Response> {
    require_user!(req, env);
    let empty = || serde_json::to_vec(&NoteList { notes: vec![] }).unwrap();

    let Some(q) = query_param(req, "q") else {
        return json_response(200, empty(), None);
    };
    let terms = token::tokenize(env, q.trim()).await;
    if terms.is_empty() {
        return json_response(200, empty(), None);
    }
    // tokens are restricted to word chars by push_token, so no FTS5 escaping
    let match_expr = terms.join(" AND ");

    let rows = env
        .d1("DB")?
        .prepare(
            "SELECT notes.id, notes.title, notes.tags, notes.updated_at,
                    substr(notes.content, 1, 120) AS excerpt, notes.author
             FROM notes_fts JOIN notes ON notes.id = notes_fts.note_id
             WHERE notes_fts MATCH ?1
             ORDER BY bm25(notes_fts)
             LIMIT 50",
        )
        .bind(&[JsValue::from(match_expr)])?
        .all()
        .await?
        .results::<MetaRow>()?;
    let payload = serde_json::to_vec(&NoteList {
        notes: rows
            .into_iter()
            .map(|r| NoteMeta {
                id: r.id,
                title: r.title,
                tags: parse_tags(&r.tags),
                updated_at: r.updated_at,
                excerpt: r.excerpt,
                author: r.author,
            })
            .collect(),
    })
    .unwrap();
    json_response(200, payload, None)
}

// ---- four-language dictionary ----
//
// Words live in dict_word, linked to a language (dict_lang) and a meaning
// group (dict_group); same-group words in different languages are mutual
// translations. The editor's language selector picks which language to
// search in (empty = any); /api/dict/lookup returns prefix matches with
// their groups' other words attached.

const DICT_LOOKUP_MAX_CHARS: usize = 32;
const DICT_LOOKUP_LIMIT: usize = 8;

#[derive(Deserialize)]
struct DictWordRow {
    id: i64,
    group_id: i64,
    word: String,
    lang: String,
}

#[derive(Deserialize)]
struct DictLangRow {
    code: String,
    name: String,
    badge: String,
}

#[derive(Deserialize)]
struct DictTransRow {
    group_id: i64,
    word: String,
    lang: String,
}

/// Escape LIKE wildcards so the user's own `%`/`_` never broaden the match.
fn like_pattern(q: &str) -> String {
    let mut s = String::with_capacity(q.len() + 2);
    for c in q.chars() {
        if c == '%' || c == '_' || c == '\\' {
            s.push('\\');
        }
        s.push(c);
    }
    s.push('%');
    s
}

/// Fetch every word of the given groups — used to attach translations.
async fn group_translations(
    db: &D1Database,
    group_ids: &[i64],
) -> Result<Vec<DictTransRow>> {
    if group_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = group_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    // placeholders are '?' only; the ids are i64 values bound below
    let sql = format!(
        "SELECT w.group_id, w.word, l.code AS lang
         FROM dict_word w JOIN dict_lang l ON l.id = w.lang_id
         WHERE w.group_id IN ({placeholders})"
    );
    let mut binds: Vec<JsValue> = Vec::with_capacity(group_ids.len());
    for id in group_ids {
        binds.push(JsValue::from(*id));
    }
    db.prepare(&sql).bind(&binds)?.all().await?.results::<DictTransRow>()
}

/// Build DictWordItems from matched rows + their groups' words.
fn assemble_items(rows: Vec<DictWordRow>, trans: &[DictTransRow]) -> Vec<DictWordItem> {
    rows.into_iter()
        .map(|r| {
            let trans_map = trans
                .iter()
                .filter(|t| t.group_id == r.group_id && !(t.lang == r.lang && t.word == r.word))
                .map(|t| (t.lang.clone(), t.word.clone()))
                .collect();
            DictWordItem {
                id: r.id,
                group_id: r.group_id,
                lang: r.lang,
                word: r.word,
                trans: trans_map,
            }
        })
        .collect()
}

/// GET /api/dict/langs — the frontend's language selector options.
async fn dict_langs(req: &Request, env: &Env) -> Result<Response> {
    require_user!(req, env);
    let rows = env
        .d1("DB")?
        .prepare("SELECT code, name, badge FROM dict_lang ORDER BY ord, id")
        .all()
        .await?
        .results::<DictLangRow>()?;
    let payload = serde_json::to_vec(&DictLangList {
        langs: rows
            .into_iter()
            .map(|r| DictLang {
                code: r.code,
                name: r.name,
                badge: r.badge,
            })
            .collect(),
    })
    .unwrap();
    json_response(200, payload, None)
}

/// GET /api/dict/lookup?q=&lang= — prefix matches in the selected language
/// (empty `lang` = any), exact hits first, each with its translations.
async fn dict_lookup(req: &Request, env: &Env) -> Result<Response> {
    require_user!(req, env);
    let empty = || serde_json::to_vec(&DictLookupResponse { words: vec![] }).unwrap();

    let Some(q) = query_param(req, "q") else {
        return json_response(200, empty(), None);
    };
    let q = q.trim();
    if q.is_empty() || q.chars().count() > DICT_LOOKUP_MAX_CHARS {
        return json_response(200, empty(), None);
    }
    let lang = query_param(req, "lang").unwrap_or_default();
    let lang = lang.trim().to_string();
    if !lang.is_empty() && !valid_lang_code(&lang) {
        return json_response(200, empty(), None);
    }
    let db = env.d1("DB")?;
    let rows = db
        .prepare(
            "SELECT w.id, w.group_id, w.word, l.code AS lang
             FROM dict_word w JOIN dict_lang l ON l.id = w.lang_id
             WHERE (?1 = '' OR l.code = ?1)
               AND w.word LIKE ?2 ESCAPE '\\'
             ORDER BY (w.word = ?3) DESC, w.word ASC
             LIMIT ?4",
        )
        .bind(&[
            JsValue::from(lang),
            JsValue::from(like_pattern(q)),
            JsValue::from(q),
            JsValue::from(DICT_LOOKUP_LIMIT as i64),
        ])?
        .all()
        .await?
        .results::<DictWordRow>()?;
    let group_ids: Vec<i64> = rows.iter().map(|r| r.group_id).collect();
    let trans = group_translations(&db, &group_ids).await?;
    let payload = serde_json::to_vec(&DictLookupResponse {
        words: assemble_items(rows, &trans),
    })
    .unwrap();
    json_response(200, payload, None)
}

/// GET /api/dict/words?q=&lang= — list/filter words, admin only.
async fn list_words(req: &Request, env: &Env) -> Result<Response> {
    let user = require_user!(req, env);
    if !is_admin(&user) {
        return json_error(403, "forbidden");
    }
    let q = query_param(req, "q").unwrap_or_default();
    let q = q.trim().to_string();
    let lang = query_param(req, "lang").unwrap_or_default();
    let db = env.d1("DB")?;
    let rows = db
        .prepare(
            "SELECT w.id, w.group_id, w.word, l.code AS lang
             FROM dict_word w JOIN dict_lang l ON l.id = w.lang_id
             WHERE (?1 = '' OR l.code = ?1)
               AND (?2 = '' OR w.word LIKE ?3 ESCAPE '\\')
             ORDER BY w.id DESC LIMIT 500",
        )
        .bind(&[
            JsValue::from(lang),
            JsValue::from(q.clone()),
            JsValue::from(like_pattern(&q)),
        ])?
        .all()
        .await?
        .results::<DictWordRow>()?;
    let payload = serde_json::to_vec(&DictLookupResponse {
        words: rows
            .into_iter()
            .map(|r| DictWordItem {
                id: r.id,
                group_id: r.group_id,
                lang: r.lang,
                word: r.word,
                trans: Default::default(),
            })
            .collect(),
    })
    .unwrap();
    json_response(200, payload, None)
}

/// Does a meaning group exist?
async fn group_exists(db: &D1Database, id: i64) -> Result<bool> {
    #[derive(Deserialize)]
    struct GroupIdRow {
        id: i64,
    }
    Ok(db
        .prepare("SELECT id FROM dict_group WHERE id = ?1 LIMIT 1")
        .bind(&[JsValue::from(id)])?
        .first::<GroupIdRow>(None)
        .await?
        .is_some())
}

/// Remove meaning groups that no longer hold any word.
async fn sweep_empty_groups(db: &D1Database) -> Result<()> {
    db.prepare(
        "DELETE FROM dict_group WHERE NOT EXISTS
             (SELECT 1 FROM dict_word WHERE dict_word.group_id = dict_group.id)",
    )
    .run()
    .await?;
    Ok(())
}

/// POST /api/dict/langs — add a language, admin only.
async fn create_lang(mut req: Request, env: &Env) -> Result<Response> {
    let user = require_user!(&req, env);
    if !is_admin(&user) {
        return json_error(403, "forbidden");
    }
    let body: DictLangUpsert = match req.json().await {
        Ok(b) => b,
        Err(_) => return json_error(400, "bad request"),
    };
    if !valid_lang_code(&body.code) {
        return json_error(400, "bad request");
    }
    let Some(name) = clean_dict_field(&body.name).filter(|s| !s.is_empty()) else {
        return json_error(400, "bad request");
    };
    let badge = clean_dict_field(&body.badge).unwrap_or_default();
    let badge = if badge.is_empty() {
        name.chars().next().unwrap_or('?').to_string()
    } else {
        badge
    };
    let db = env.d1("DB")?;
    let exists = db
        .prepare("SELECT code FROM dict_lang WHERE code = ?1 LIMIT 1")
        .bind(&[JsValue::from(body.code.clone())])?
        .first::<DictLangRow>(None)
        .await?;
    if exists.is_some() {
        return json_error(409, "language exists");
    }
    db.prepare(
        "INSERT INTO dict_lang (code, name, badge, ord)
         VALUES (?1, ?2, ?3, (SELECT COALESCE(MAX(ord), 0) + 1 FROM dict_lang))",
    )
    .bind(&[
        JsValue::from(body.code.clone()),
        JsValue::from(name.clone()),
        JsValue::from(badge.clone()),
    ])?
    .run()
    .await?;
    let payload = serde_json::to_vec(&DictLang {
        code: body.code,
        name,
        badge,
    })
    .unwrap();
    json_response(200, payload, None)
}

/// POST /api/dict/words — add a word (creating its meaning group unless
/// `group_id` is given) plus optional sibling translations, admin only.
async fn create_word(mut req: Request, env: &Env) -> Result<Response> {
    let user = require_user!(&req, env);
    if !is_admin(&user) {
        return json_error(403, "forbidden");
    }
    let body: DictWordUpsert = match req.json().await {
        Ok(b) => b,
        Err(_) => return json_error(400, "bad request"),
    };
    if !valid_lang_code(&body.lang) {
        return json_error(400, "bad request");
    }
    let Some(word) = clean_dict_field(&body.word).filter(|s| !s.is_empty()) else {
        return json_error(400, "bad request");
    };
    let db = env.d1("DB")?;
    #[derive(Deserialize)]
    struct LangIdRow {
        id: i64,
    }
    let lang_id = db
        .prepare("SELECT id FROM dict_lang WHERE code = ?1 LIMIT 1")
        .bind(&[JsValue::from(body.lang.clone())])?
        .first::<LangIdRow>(None)
        .await?
        .map(|r| r.id);
    let Some(lang_id) = lang_id else {
        return json_error(400, "unknown language");
    };

    let group_id = match body.group_id {
        Some(g) => {
            if !group_exists(&db, g).await? {
                return json_error(400, "unknown group");
            }
            g
        }
        None => {
            let result = db
                .prepare("INSERT INTO dict_group (key) VALUES (NULL)")
                .run()
                .await?;
            let id = result.meta().ok().flatten().and_then(|m| m.last_row_id);
            let Some(id) = id else {
                return json_error(500, "create group failed");
            };
            id
        }
    };

    // ensure the word exists and sits in the requested group (the unique
    // (lang_id, word) index makes the mapping unambiguous)
    db.prepare(
        "INSERT OR IGNORE INTO dict_word (group_id, lang_id, word) VALUES (?1, ?2, ?3)",
    )
    .bind(&[JsValue::from(group_id), JsValue::from(lang_id), JsValue::from(word.clone())])?
    .run()
    .await?;
    db.prepare("UPDATE dict_word SET group_id = ?1 WHERE lang_id = ?2 AND word = ?3")
        .bind(&[JsValue::from(group_id), JsValue::from(lang_id), JsValue::from(word.clone())])?
        .run()
        .await?;

    // sibling translations of the same group
    for (code, w) in &body.trans {
        if !valid_lang_code(code) {
            continue;
        }
        let Some(w) = clean_dict_field(w).filter(|s| !s.is_empty()) else {
            continue;
        };
        db.prepare(
            "INSERT OR IGNORE INTO dict_word (group_id, lang_id, word)
             SELECT ?1, l.id, ?2 FROM dict_lang l WHERE l.code = ?3",
        )
        .bind(&[JsValue::from(group_id), JsValue::from(w), JsValue::from(code.clone())])?
        .run()
        .await?;
    }

    let row = db
        .prepare(
            "SELECT w.id, w.group_id, w.word, l.code AS lang
             FROM dict_word w JOIN dict_lang l ON l.id = w.lang_id
             WHERE w.lang_id = ?1 AND w.word = ?2 LIMIT 1",
        )
        .bind(&[JsValue::from(lang_id), JsValue::from(word)])?
        .first::<DictWordRow>(None)
        .await?;
    let Some(row) = row else {
        return json_error(500, "create word failed");
    };
    let trans = group_translations(&db, &[row.group_id]).await?;
    let item = assemble_items(vec![row], &trans).into_iter().next().unwrap();
    sweep_empty_groups(&db).await?;
    let payload = serde_json::to_vec(&item).unwrap();
    json_response(200, payload, None)
}

/// PUT /api/dict/words/{id} — rename and/or move to another group, admin.
async fn update_word(mut req: Request, env: &Env, id: &str) -> Result<Response> {
    let user = require_user!(&req, env);
    if !is_admin(&user) {
        return json_error(403, "forbidden");
    }
    let Ok(id) = id.parse::<i64>() else {
        return json_error(400, "bad request");
    };
    let body: DictWordUpsert = match req.json().await {
        Ok(b) => b,
        Err(_) => return json_error(400, "bad request"),
    };
    let db = env.d1("DB")?;
    if let Some(word) = clean_dict_field(&body.word).filter(|s| !s.is_empty()) {
        db.prepare("UPDATE dict_word SET word = ?1 WHERE id = ?2")
            .bind(&[JsValue::from(word), JsValue::from(id)])?
            .run()
            .await?;
    }
    if let Some(g) = body.group_id {
        if !group_exists(&db, g).await? {
            return json_error(400, "unknown group");
        }
        db.prepare("UPDATE dict_word SET group_id = ?1 WHERE id = ?2")
            .bind(&[JsValue::from(g), JsValue::from(id)])?
            .run()
            .await?;
    }
    sweep_empty_groups(&db).await?;
    json_response(200, serde_json::to_vec(&OkResponse { ok: true }).unwrap(), None)
}

/// DELETE /api/dict/words/{id} — remove a word (empty groups swept), admin.
async fn delete_word(req: &Request, env: &Env, id: &str) -> Result<Response> {
    let user = require_user!(req, env);
    if !is_admin(&user) {
        return json_error(403, "forbidden");
    }
    let Ok(id) = id.parse::<i64>() else {
        return json_error(400, "bad request");
    };
    let db = env.d1("DB")?;
    db.prepare("DELETE FROM dict_word WHERE id = ?1")
        .bind(&[JsValue::from(id)])?
        .run()
        .await?;
    sweep_empty_groups(&db).await?;
    json_response(200, serde_json::to_vec(&OkResponse { ok: true }).unwrap(), None)
}

async fn analyze_text(mut req: Request, env: &Env) -> Result<Response> {
    require_user!(&req, env);
    let body: AnalyzeRequest = match req.json().await {
        Ok(b) => b,
        Err(_) => return json_error(400, "bad request"),
    };
    // the japanese rules need the tokenizer; degrade gracefully if absent
    let _ = token::ensure_ja(env).await;
    let payload = serde_json::to_vec(&AnalyzeResponse {
        issues: analyze::analyze(&body.content),
    })
    .unwrap();
    json_response(200, payload, None)
}

const POS_MAX_CHARS: usize = 30_000;
// Search indexes only the head of very long notes: full-length tokenization
// of a 400KB note exceeds the free-plan CPU limit, and search results are
// capped at 50 rows anyway.
const INDEX_MAX_CHARS: usize = 30_000;

fn head_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

async fn pos_tag(mut req: Request, env: &Env) -> Result<Response> {
    require_user!(&req, env);
    let body: AnalyzeRequest = match req.json().await {
        Ok(b) => b,
        Err(_) => return json_error(400, "bad request"),
    };
    let _ = token::ensure_ja(env).await;

    let text: String = body.content.chars().take(POS_MAX_CHARS).collect();
    let mut tokens = Vec::new();
    for (w, tag, s, e) in token::zh_tag(&text) {
        // punctuation labeled "x" is noise — skip it. NOTE: only actual
        // punctuation ranges (fullwidth CJK punctuation lives in U+3000..U
        // +303F), never "anything above U+2000" — that would eat hanzi.
        if tag.starts_with('x')
            && w
                .chars()
                .all(|c| c.is_ascii_punctuation() || ('\u{3000}'..='\u{303f}').contains(&c))
        {
            continue;
        }
        // zh_tag already normalized the tag through zh_cat + the correction
        // table — the tag IS the category (this endpoint used to re-map from
        // scratch, which silently dropped f/i to "x")
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
    let payload = serde_json::to_vec(&PosResponse {
        tokens,
        tagged_chars: text.chars().count(),
    })
    .unwrap();
    json_response(200, payload, None)
}

async fn pos_document(mut req: Request, env: &Env) -> Result<Response> {
    require_user!(&req, env);
    let body: AnalyzeRequest = match req.json().await {
        Ok(b) => b,
        Err(_) => return json_error(400, "bad request"),
    };
    let _ = token::ensure_ja(env).await;
    let payload = serde_json::to_vec(&PosDocResponse {
        markdown: token::annotate_markdown(&body.content),
    })
    .unwrap();
    json_response(200, payload, None)
}

async fn generate_mark(req: Request, env: &Env, id: &str) -> Result<Response> {
    let user = require_user!(&req, env);
    if !valid_note_id(id) {
        return json_error(400, "bad request");
    }
    if !can_edit_now(env, &user).await {
        return json_error(403, "edit locked");
    }
    let _ = token::ensure_ja(env).await;
    match mark::generate(env, id).await {
        Ok(true) => json_response(200, serde_json::to_vec(&OkResponse { ok: true }).unwrap(), None),
        Ok(false) => json_error(404, "note not found"),
        Err(_) => json_error(500, "mark failed"),
    }
}

async fn save_mark(mut req: Request, env: &Env, id: &str) -> Result<Response> {
    let user = require_user!(&req, env);
    if !valid_note_id(id) {
        return json_error(400, "bad request");
    }
    if !can_edit_now(env, &user).await {
        return json_error(403, "edit locked");
    }
    let body: AnalyzeRequest = match req.json().await {
        Ok(b) => b,
        Err(_) => return json_error(400, "bad request"),
    };
    match mark::save(env, id, &body.content).await {
        Ok(true) => json_response(200, serde_json::to_vec(&OkResponse { ok: true }).unwrap(), None),
        Ok(false) => json_error(404, "no mark"),
        Err(_) => json_error(500, "save failed"),
    }
}

async fn remove_mark(req: Request, env: &Env, id: &str) -> Result<Response> {
    let user = require_user!(&req, env);
    if !valid_note_id(id) {
        return json_error(400, "bad request");
    }
    if !can_edit_now(env, &user).await {
        return json_error(403, "edit locked");
    }
    mark::remove(env, id).await?;
    json_response(200, serde_json::to_vec(&OkResponse { ok: true }).unwrap(), None)
}

async fn get_mark(req: Request, env: &Env, id: &str) -> Result<Response> {
    require_user!(&req, env);
    if !valid_note_id(id) {
        return json_error(400, "bad request");
    }
    match mark::get(env, id).await? {
        Some(info) => {
            let payload = serde_json::to_vec(&info).unwrap();
            json_response(200, payload, None)
        }
        None => json_error(404, "no mark"),
    }
}

async fn check_marks(req: Request, env: &Env) -> Result<Response> {
    let user = require_user!(&req, env);
    if !is_admin(&user) {
        return json_error(403, "forbidden");
    }
    let _ = token::ensure_ja(env).await;
    let (removed, regenerated, scanned) = mark::sweep(env, 5).await?;
    #[derive(serde::Serialize)]
    struct SweepResult {
        scanned: i64,
        orphans_removed: i64,
        regenerated: i64,
    }
    let payload = serde_json::to_vec(&SweepResult {
        scanned,
        orphans_removed: removed,
        regenerated,
    })
    .unwrap();
    json_response(200, payload, None)
}

#[derive(serde::Serialize)]
struct ReindexResponse {
    indexed: i64,
    next_offset: Option<i64>,
}

#[derive(Deserialize)]
struct NoteIdxRow {
    id: String,
    content: String,
    title: String,
    tags: String,
}

// reindex_search rebuilds the FTS index for notes that predate the search
// feature (or drifted). Processing is chunked: the free plan's per-request
// CPU budget can't tokenize the whole table at once, so the caller loops with
// next_offset until it comes back null.
const REINDEX_CHUNK: i64 = 10;

async fn reindex_search(req: &Request, env: &Env) -> Result<Response> {
    let user = require_user!(req, env);
    if !is_admin(&user) {
        return json_error(403, "forbidden");
    }
    let offset: i64 = query_param(req, "offset")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let db = env.d1("DB")?;
    // Process one note per query: holding a whole page of contents at once
    // plus the ~50MB decompressed Japanese dictionary blows the 128MB isolate
    // memory limit on production.
    let mut count = 0i64;
    for i in 0..REINDEX_CHUNK {
        let row = db
            .prepare("SELECT id, content, title, tags FROM notes ORDER BY id LIMIT 1 OFFSET ?1")
            .bind(&[JsValue::from((offset + i) as f64)])?
            .first::<NoteIdxRow>(None)
            .await?;
        let Some(r) = row else { break };
        let text = head_chars(
            &format!("{} {} {}", r.title, parse_tags(&r.tags).join(" "), r.content),
            INDEX_MAX_CHARS,
        );
        let tokens = token::tokenize(env, &text).await.join(" ");
        let _ = db
            .prepare("DELETE FROM notes_fts WHERE note_id = ?1")
            .bind(&[JsValue::from(r.id.as_str())])?
            .run()
            .await;
        if let Err(e) = db
            .prepare("INSERT INTO notes_fts (note_id, tokens) VALUES (?1, ?2)")
            .bind(&[JsValue::from(r.id.as_str()), JsValue::from(tokens)])?
            .run()
            .await
        {
            console_log!("reindex {} failed: {e}", r.id);
        }
        if let Err(e) = mark::regenerate_auto(env, &r.id).await {
            console_log!("mark regenerate failed for {}: {e}", r.id);
        }
        count += 1;
    }

    let next_offset = if count < REINDEX_CHUNK {
        None
    } else {
        Some(offset + count)
    };
    let payload = serde_json::to_vec(&ReindexResponse { indexed: count, next_offset }).unwrap();
    json_response(200, payload, None)
}

// sniff_image detects the real format by magic bytes — the client-supplied
// content-type is not trustworthy. SVG is deliberately rejected: served
// same-origin it could carry scripts.
fn sniff_image(b: &[u8]) -> Option<(&'static str, &'static str)> {
    if b.len() >= 8 && &b[..8] == b"\x89PNG\r\n\x1a\n" {
        Some(("image/png", ".png"))
    } else if b.len() >= 3 && b[0] == 0xFF && b[1] == 0xD8 && b[2] == 0xFF {
        Some(("image/jpeg", ".jpg"))
    } else if b.len() >= 6 && (&b[..6] == b"GIF87a" || &b[..6] == b"GIF89a") {
        Some(("image/gif", ".gif"))
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some(("image/webp", ".webp"))
    } else if b.len() >= 12 && &b[4..8] == b"ftyp" && (&b[8..12] == b"avif" || &b[8..12] == b"avis") {
        Some(("image/avif", ".avif"))
    } else {
        None
    }
}

// handleUpload accepts Vditor's multipart upload (field "file[]") and stores
// valid images in KV. The response follows Vditor's protocol.
async fn upload(mut req: Request, env: &Env, cache: Option<KvStore>) -> Result<Response> {
    let user = require_user!(&req, env);
    if !can_edit_now(env, &user).await {
        return json_error(403, "edit locked");
    }
    let Some(cache) = cache else {
        return json_error(503, "storage unavailable");
    };

    // reject oversized bodies before the platform parses the multipart form
    if let Ok(Some(len)) = req.headers().get("Content-Length") {
        if len.parse::<u64>().unwrap_or(0) > (MAX_UPLOAD_BYTES + UPLOAD_HEADROOM) as u64 {
            return json_error(413, "upload too large");
        }
    }
    let form = match req.form_data().await {
        Ok(f) => f,
        Err(_) => return json_error(413, "upload too large"),
    };

    let mut succ_map: BTreeMap<String, String> = BTreeMap::new();
    let mut err_files: Vec<String> = Vec::new();
    for field in ["file[]", "file"] {
        let Some(entries) = form.get_all(field) else {
            continue;
        };
        for entry in entries {
            let FormEntry::File(f) = entry else {
                continue;
            };
            let name = f.name();
            if f.size() > MAX_UPLOAD_BYTES {
                err_files.push(name);
                continue;
            }
            let bytes = match f.bytes().await {
                Ok(b) if !b.is_empty() => b,
                _ => {
                    err_files.push(name);
                    continue;
                }
            };
            let Some((_mime, ext)) = sniff_image(&bytes) else {
                err_files.push(name);
                continue;
            };
            let key = format!("{}{}", new_image_key(), ext);
            if let Err(e) = cache.put_bytes(&key, &bytes)?.execute().await {
                console_log!("kv put {key} failed: {e}");
                err_files.push(name);
                continue;
            }
            succ_map.insert(name, format!("/i/{}", &key[IMAGE_KEY_PREFIX.len()..]));
        }
    }

    let payload = serde_json::to_vec(&UploadResponse {
        msg: String::new(),
        code: 0,
        data: UploadData { err_files, succ_map },
    })
    .unwrap();
    json_response(200, payload, None)
}

const IMAGE_EXT_MIME: [(&str, &str); 5] = [
    (".png", "image/png"),
    (".jpg", "image/jpeg"),
    (".gif", "image/gif"),
    (".webp", "image/webp"),
    (".avif", "image/avif"),
];

// serveImage serves an uploaded image from KV with immutable caching —
// keys are random, so content never changes under the same URL.
async fn image(req: &Request, env: &Env, cache: Option<KvStore>, name: &str) -> Result<Response> {
    require_user!(req, env);
    if !valid_image_name(name) {
        return Response::error("not found", 404);
    }
    let Some(cache) = cache else {
        return Response::error("not found", 404);
    };
    let ext = &name[name.find('.').unwrap()..];
    let Some(mime) = IMAGE_EXT_MIME
        .iter()
        .find(|(e, _)| *e == ext)
        .map(|(_, m)| *m)
    else {
        return Response::error("not found", 404);
    };
    let key = format!("{IMAGE_KEY_PREFIX}{name}");
    let Some(bytes) = cache.get(&key).bytes().await.ok().flatten() else {
        return Response::error("not found", 404);
    };
    let resp = Response::from_bytes(bytes)?;
    let headers = Headers::new();
    headers.set("Content-Type", mime)?;
    headers.set("Cache-Control", "public, max-age=31536000, immutable")?;
    headers.set("X-Cache", "HIT")?;
    Ok(resp.with_headers(headers))
}

// serveApp serves the frontend SPA: any GET that is not an API call and has
// no file extension is a client-side route (a note id like /u0yYk) and gets
// index.html. Paths with an extension are treated as asset requests and 404
// when missing — valid note ids never contain a dot.
async fn serve_app(req: Request, env: &Env) -> Result<Response> {
    // method is GET or HEAD here (dispatch guarantees it)
    let path = req.path();
    if path.starts_with("/api/") || last_segment_has_dot(&path) {
        return Response::error("not found", 404);
    }
    // The ASSETS service binding serves from the static assets upload; the
    // request URL is a placeholder, only the path is meaningful.
    let assets = env.assets("ASSETS")?;
    let asset_req = Request::new_with_init(
        "https://assets.urlnote/index.html",
        RequestInit::new().with_method(req.method()),
    )?;
    let resp = assets.fetch_request(asset_req).await?;
    Ok(resp)
}

fn last_segment_has_dot(path: &str) -> bool {
    path.rsplit('/').next().is_some_and(|seg| seg.contains('.'))
}
