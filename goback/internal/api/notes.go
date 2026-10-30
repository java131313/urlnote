package api

import (
	"context"
	"encoding/json"
	"net/http"
	"strings"

	"urlnote/goback/internal/store"
	"urlnote/goback/internal/token"
	"urlnote/goback/shared"
)

// cleanTags normalizes user-provided tags: trimmed, deduplicated, capped at
// 10 entries, each at most 24 characters.
func cleanTags(tags []string) []string {
	out := make([]string, 0, len(tags))
	seen := map[string]bool{}
	for _, t := range tags {
		t = strings.TrimSpace(t)
		if t == "" || seen[t] {
			continue
		}
		r := []rune(t)
		if len(r) > 24 {
			t = string(r[:24])
		}
		seen[t] = true
		out = append(out, t)
		if len(out) >= 10 {
			break
		}
	}
	return out
}

func cleanTitle(title string) string {
	r := []rune(strings.TrimSpace(title))
	if len(r) > 200 {
		return string(r[:200])
	}
	return string(r)
}

func parseTags(s string) []string {
	var tags []string
	if err := json.Unmarshal([]byte(s), &tags); err != nil {
		return []string{}
	}
	if tags == nil {
		return []string{}
	}
	return tags
}

func (s *Server) getNote(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	if !validID(id) {
		http.NotFound(w, r)
		return
	}
	if _, ok := s.requireUser(w, r); !ok {
		return
	}

	// cache hit: serve the cached copy directly
	if payload, ok := s.Cache.Get(noteKey(id)); ok {
		s.writeRawJSON(w, http.StatusOK, payload, "HIT")
		return
	}

	row, found, err := s.DB.GetNote(r.Context(), id)
	if err != nil {
		jsonError(w, http.StatusInternalServerError, "read failed")
		return
	}
	note := shared.EmptyNote(id)
	if found {
		note = shared.Note{
			ID:        id,
			Content:   row.Content,
			FontSize:  row.FontSize,
			Color:     row.Color,
			Title:     row.Title,
			Tags:      parseTags(row.Tags),
			Author:    row.Author,
			UpdatedAt: row.UpdatedAt,
		}
	}
	payload, _ := json.Marshal(note)
	s.Cache.Put(noteKey(id), payload, s.Cache.NoteTTL())
	s.writeRawJSON(w, http.StatusOK, payload, "MISS")
}

func (s *Server) saveNote(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	if !validID(id) {
		http.NotFound(w, r)
		return
	}
	user, ok := s.requireUser(w, r)
	if !ok {
		return
	}
	if !s.canEditNow(r.Context(), user) {
		jsonError(w, http.StatusForbidden, "edit locked")
		return
	}

	// The body carries the full note state (requests may arrive without
	// fetch-style headers); omitted settings fall back to defaults.
	var body shared.SaveRequest
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
		jsonError(w, http.StatusBadRequest, "bad request")
		return
	}
	fontSize := shared.DefaultFontSize
	if body.FontSize != nil {
		fontSize = shared.ResolveFontSize(*body.FontSize)
	}
	color := body.Color
	if !validColor(color) {
		color = ""
	}
	title := cleanTitle(body.Title)
	tags := cleanTags(body.Tags)
	tagsJSON, _ := json.Marshal(tags)
	updatedAt := nowStr()

	note := store.Note{
		ID:        id,
		Content:   body.Content,
		Title:     title,
		Tags:      string(tagsJSON),
		Author:    user,
		FontSize:  fontSize,
		Color:     color,
		UpdatedAt: updatedAt,
	}
	// INSERT ... ON CONFLICT(id) DO UPDATE — the D1 upsert, verbatim.
	if err := s.DB.SaveNote(r.Context(), note); err != nil {
		jsonError(w, http.StatusInternalServerError, "save failed")
		return
	}

	// The full payload (incl. updated_at/author) is echoed back so clients
	// can sync without an extra GET.
	full := shared.Note{
		ID: id, Content: body.Content, FontSize: fontSize, Color: color,
		Title: title, Tags: tags, Author: user, UpdatedAt: updatedAt,
	}
	payload, _ := json.Marshal(full)
	s.Cache.Put(noteKey(id), payload, s.Cache.NoteTTL())

	// full-text index: tokenize title/tags/content (head of very long notes)
	// and store the space-joined tokens. Index failures must not fail the save.
	head := []rune(title + " " + strings.Join(tags, " ") + " " + body.Content)
	if len(head) > token.IndexMaxChars {
		head = head[:token.IndexMaxChars]
	}
	toks := s.Ana.Tokenize(string(head))
	// Index failures must not fail the save (the row write already succeeded).
	_ = s.DB.ReplaceFts(r.Context(), id, strings.Join(toks, " "))

	// eager marked copy for the POS overlay; a hand-edited mark
	// (source_updated_at = '') is never clobbered
	s.regenerateMarkAuto(r.Context(), id)

	s.rebuildIndex(r.Context())
	s.writeRawJSON(w, http.StatusOK, payload, "")
}

// queryNoteMeta loads metadata of all notes, newest first.
func (s *Server) queryNoteMeta(ctx context.Context) ([]shared.NoteMeta, error) {
	rows, err := s.DB.ListNoteMeta(ctx)
	if err != nil {
		return nil, err
	}
	metas := make([]shared.NoteMeta, 0, len(rows))
	for _, m := range rows {
		metas = append(metas, shared.NoteMeta{
			ID: m.ID, Title: m.Title, Tags: parseTags(m.Tags),
			UpdatedAt: m.UpdatedAt, Excerpt: m.Excerpt, Author: m.Author,
		})
	}
	return metas, nil
}

// rebuildIndex refreshes the cached note list after a write.
func (s *Server) rebuildIndex(ctx context.Context) {
	metas, err := s.queryNoteMeta(ctx)
	if err != nil {
		s.Cache.Delete(indexKey)
		return
	}
	payload, _ := json.Marshal(shared.NoteList{Notes: metas})
	s.Cache.Put(indexKey, payload, s.Cache.IndexTTL())
}

func (s *Server) listNotes(w http.ResponseWriter, r *http.Request) {
	if _, ok := s.requireUser(w, r); !ok {
		return
	}
	if payload, ok := s.Cache.Get(indexKey); ok {
		s.writeRawJSON(w, http.StatusOK, payload, "HIT")
		return
	}
	metas, err := s.queryNoteMeta(r.Context())
	if err != nil {
		jsonError(w, http.StatusInternalServerError, "list failed")
		return
	}
	payload, _ := json.Marshal(shared.NoteList{Notes: metas})
	s.Cache.Put(indexKey, payload, s.Cache.IndexTTL())
	s.writeRawJSON(w, http.StatusOK, payload, "MISS")
}
