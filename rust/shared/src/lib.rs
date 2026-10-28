//! Shared API types and validation logic for urlnote.
//!
//! These types mirror the JSON contract of the original Go backend
//! (main.go) exactly — same field names, same shapes — so the Rust worker
//! and the Rust frontend (and any existing clients) interoperate with the
//! same payloads.

use serde::{Deserialize, Serialize};

// ---- editor setting bounds — the frontend clamps to the same range ----

pub const DEFAULT_FONT_SIZE: i64 = 16;
pub const MIN_FONT_SIZE: i64 = 10;
pub const MAX_FONT_SIZE: i64 = 40;

/// Round and clamp like the Go backend: values <= 0 mean "use the default".
pub fn resolve_font_size(n: f64) -> i64 {
    if n <= 0.0 {
        return DEFAULT_FONT_SIZE;
    }
    let r = n.round();
    if r < MIN_FONT_SIZE as f64 {
        MIN_FONT_SIZE
    } else if r > MAX_FONT_SIZE as f64 {
        MAX_FONT_SIZE
    } else {
        r as i64
    }
}

// ---- validation (Go used equivalent regexes) ----

/// `^[a-zA-Z0-9_-]{1,64}$` — note ids also arrive as URL path segments.
pub fn valid_note_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `^#[0-9a-fA-F]{6}$` — editor text color, empty means "follow site theme".
pub fn valid_color(c: &str) -> bool {
    let b = c.as_bytes();
    b.len() == 7
        && b[0] == b'#'
        && b[1..]
            .iter()
            .all(|x| x.is_ascii_hexdigit() && x.is_ascii())
}

/// `^[a-zA-Z0-9]{32}$` — session token.
pub fn valid_token(t: &str) -> bool {
    t.len() == 32 && t.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// `^[a-zA-Z0-9]{16}\.[a-z0-9]+$` — uploaded image file name.
pub fn valid_image_name(n: &str) -> bool {
    let b = n.as_bytes();
    let Some(dot) = n.find('.') else {
        return false;
    };
    let (stem, ext) = (&b[..dot], &b[dot + 1..]);
    stem.len() == 16
        && stem.iter().all(|x| x.is_ascii_alphanumeric())
        && !ext.is_empty()
        && ext.iter().all(|x| x.is_ascii_lowercase() || x.is_ascii_digit())
}

// ---- tag/title normalization (same rules as the Go cleanTags) ----

pub fn clean_tags(tags: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(tags.len());
    for t in tags {
        let t = t.trim();
        if t.is_empty() || out.iter().any(|x| x == t) {
            continue;
        }
        let capped: String = t.chars().take(24).collect();
        out.push(capped);
        if out.len() >= 10 {
            break;
        }
    }
    out
}

pub fn clean_title(title: &str) -> String {
    title.trim().chars().take(200).collect()
}

// ---- API payloads ----

/// The built-in account with edit-lock privileges (seeded in schema.sql).
pub const ADMIN_USERNAME: &str = "admin";

/// Full note representation: GET /api/note/{id} and the PUT/POST echo.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub content: String,
    pub font_size: i64,
    pub color: String,
    pub title: String,
    pub tags: Vec<String>,
    pub author: String,
    pub updated_at: String,
}

impl Note {
    /// Payload for a note that has no row in D1 yet (matches the Go backend).
    pub fn empty(id: &str) -> Note {
        Note {
            id: id.to_string(),
            content: String::new(),
            font_size: DEFAULT_FONT_SIZE,
            color: String::new(),
            title: String::new(),
            tags: Vec::new(),
            author: String::new(),
            updated_at: String::new(),
        }
    }
}

/// One entry of GET /api/notes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NoteMeta {
    pub id: String,
    pub title: String,
    pub tags: Vec<String>,
    pub updated_at: String,
    pub excerpt: String,
    pub author: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NoteList {
    pub notes: Vec<NoteMeta>,
}

/// PUT/POST /api/note/{id} request body.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SaveRequest {
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub font_size: Option<f64>,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoginRequest {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct User {
    pub username: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoginResponse {
    pub ok: bool,
    pub user: User,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WhoamiResponse {
    pub user: User,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OkResponse {
    pub ok: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PresenceRequest {
    #[serde(default)]
    pub note_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PresenceResponse {
    pub users: Vec<String>,
}

// ---- edit lock & access requests ----

/// GET /api/edit-status — the frontend's source of truth for edit permission.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EditStatusResponse {
    pub locked: bool,
    pub is_admin: bool,
    /// Current user may edit under the current lock state.
    pub can_edit: bool,
    /// This user's request state: "none" | "pending" | "approved".
    pub request: String,
}

/// POST /api/edit-lock request body (admin only).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EditLockRequest {
    pub locked: bool,
}

/// POST /api/edit-request request body.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EditRequestSubmit {
    #[serde(default)]
    pub message: String,
}

/// One row of GET /api/edit-requests (admin only).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EditRequestItem {
    pub username: String,
    /// "pending" | "approved"
    pub status: String,
    pub message: String,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EditRequestList {
    pub requests: Vec<EditRequestItem>,
}

/// POST /api/edit-requests/handle request body (admin only).
/// action: "approve" | "reject" | "revoke" (reject/revoke both delete the row).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EditHandleRequest {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub action: String,
}

// ---- grammar/morphology analysis ----

/// POST /api/analyze request body.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnalyzeRequest {
    #[serde(default)]
    pub content: String,
}

/// One detected problem. `start`/`end` are char offsets into the analyzed
/// content so the client can apply `suggestion` in place; both 0 for
/// whole-text warnings (no auto-fix).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Issue {
    pub start: usize,
    pub end: usize,
    pub quote: String,
    pub problem: String,
    pub suggestion: String,
    /// "zh" | "ja" | "" (language-specific vs general)
    pub lang: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnalyzeResponse {
    pub issues: Vec<Issue>,
}

// ---- POS (part-of-speech) annotation ----

/// One token of the POS-tagged content. `s`/`e` are char offsets into the
/// analyzed content (same convention as Issue). `cat` is a normalized
/// category key: n/v/a/d/u/r/m/c/p/aux/x.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PosToken {
    pub w: String,
    pub cat: String,
    pub s: usize,
    pub e: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PosResponse {
    pub tokens: Vec<PosToken>,
    /// how many chars of the content were actually tagged (long notes are
    /// truncated so the response stays a reasonable size)
    pub tagged_chars: usize,
}

/// POST /api/pos/doc response — the note content re-rendered as markdown with
/// POS annotations baked into `<span class="posw cat-*">` inline tags.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PosDocResponse {
    #[serde(default)]
    pub markdown: String,
}

/// Vditor's upload protocol response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UploadResponse {
    pub msg: String,
    pub code: i64,
    pub data: UploadData,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UploadData {
    // Vditor's protocol uses camelCase here, unlike the rest of the API
    #[serde(rename = "errFiles")]
    pub err_files: Vec<String>,
    #[serde(rename = "succMap")]
    pub succ_map: std::collections::BTreeMap<String, String>,
}

/// GET /api/mark/{id} response — the annotated copy from notes_mark.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MarkInfo {
    pub content: String,
    pub source_updated_at: String,
}
