// Package shared defines the API JSON contract, mirroring
// rust/shared/src/lib.rs field-for-field so the Go server, the Go WASM
// frontend and any existing clients interoperate with identical payloads.
package shared

// Editor setting bounds — the frontend clamps to the same range.
const (
	DefaultFontSize = 16
	MinFontSize     = 10
	MaxFontSize     = 40
	AdminUsername   = "admin"
)

// ResolveFontSize rounds and clamps like the Rust worker: values <= 0 mean
// "use the default".
func ResolveFontSize(n float64) int {
	if n <= 0 {
		return DefaultFontSize
	}
	r := int(n + 0.5)
	if r < MinFontSize {
		return MinFontSize
	}
	if r > MaxFontSize {
		return MaxFontSize
	}
	return r
}

// Note is the full note representation: GET /api/note/{id} and the PUT/POST echo.
type Note struct {
	ID        string   `json:"id"`
	Content   string   `json:"content"`
	FontSize  int      `json:"font_size"`
	Color     string   `json:"color"`
	Title     string   `json:"title"`
	Tags      []string `json:"tags"`
	Author    string   `json:"author"`
	UpdatedAt string   `json:"updated_at"`
}

// EmptyNote is the payload for a note that has no row yet.
func EmptyNote(id string) Note {
	return Note{ID: id, FontSize: DefaultFontSize, Tags: []string{}}
}

// NoteMeta is one entry of GET /api/notes.
type NoteMeta struct {
	ID        string   `json:"id"`
	Title     string   `json:"title"`
	Tags      []string `json:"tags"`
	UpdatedAt string   `json:"updated_at"`
	Excerpt   string   `json:"excerpt"`
	Author    string   `json:"author"`
}

type NoteList struct {
	Notes []NoteMeta `json:"notes"`
}

// SaveRequest is the PUT/POST /api/note/{id} request body.
type SaveRequest struct {
	Content  string   `json:"content"`
	FontSize *float64 `json:"font_size"`
	Color    string   `json:"color"`
	Title    string   `json:"title"`
	Tags     []string `json:"tags"`
}

type LoginRequest struct {
	Username string `json:"username"`
	Password string `json:"password"`
}

type User struct {
	Username string `json:"username"`
}

type LoginResponse struct {
	OK   bool `json:"ok"`
	User User `json:"user"`
}

type WhoamiResponse struct {
	User User `json:"user"`
}

type OkResponse struct {
	OK bool `json:"ok"`
}

type ErrorResponse struct {
	Error string `json:"error"`
}

type PresenceRequest struct {
	NoteID string `json:"note_id"`
}

type PresenceResponse struct {
	Users []string `json:"users"`
}

// EditStatusResponse is the frontend's source of truth for edit permission.
type EditStatusResponse struct {
	Locked  bool   `json:"locked"`
	IsAdmin bool   `json:"is_admin"`
	CanEdit bool   `json:"can_edit"`
	Request string `json:"request"`
}

type EditLockRequest struct {
	Locked bool `json:"locked"`
}

type EditRequestSubmit struct {
	Message string `json:"message"`
}

type EditRequestItem struct {
	Username  string `json:"username"`
	Status    string `json:"status"`
	Message   string `json:"message"`
	CreatedAt string `json:"created_at"`
}

type EditRequestList struct {
	Requests []EditRequestItem `json:"requests"`
}

// EditHandleRequest action: "approve" | "reject" | "revoke"
// (reject/revoke both delete the row).
type EditHandleRequest struct {
	Username string `json:"username"`
	Action   string `json:"action"`
}

// ---- grammar/morphology analysis ----

type AnalyzeRequest struct {
	Content string `json:"content"`
}

// Issue is one detected problem. Start/End are char offsets into the
// analyzed content so the client can apply Suggestion in place; both 0 for
// whole-text warnings (no auto-fix).
type Issue struct {
	Start      int    `json:"start"`
	End        int    `json:"end"`
	Quote      string `json:"quote"`
	Problem    string `json:"problem"`
	Suggestion string `json:"suggestion"`
	Lang       string `json:"lang"`
}

type AnalyzeResponse struct {
	Issues []Issue `json:"issues"`
}

// ---- POS (part-of-speech) annotation ----

// PosToken is one token of the POS-tagged content. S/E are char offsets into
// the analyzed content (same convention as Issue). Cat is a normalized
// category key: n/v/a/d/u/r/m/c/p/aux/f/q/x.
type PosToken struct {
	W   string `json:"w"`
	Cat string `json:"cat"`
	S   int    `json:"s"`
	E   int    `json:"e"`
}

type PosResponse struct {
	Tokens      []PosToken `json:"tokens"`
	TaggedChars int        `json:"tagged_chars"`
}

// PosDocResponse is the note content re-rendered as markdown with POS
// annotations baked into `<span class="posw cat-*">` inline tags.
type PosDocResponse struct {
	Markdown string `json:"markdown"`
}

// UploadResponse is Vditor's upload protocol response.
type UploadResponse struct {
	Msg  string     `json:"msg"`
	Code int        `json:"code"`
	Data UploadData `json:"data"`
}

type UploadData struct {
	// Vditor's protocol uses camelCase here, unlike the rest of the API.
	ErrFiles []string          `json:"errFiles"`
	SuccMap  map[string]string `json:"succMap"`
}

// MarkInfo is the GET /api/mark/{id} response — the annotated copy from
// notes_mark.
type MarkInfo struct {
	Content         string `json:"content"`
	SourceUpdatedAt string `json:"source_updated_at"`
}
