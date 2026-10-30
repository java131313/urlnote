package api

import (
	"encoding/json"
	"net/http"
	"strconv"
	"strings"

	"urlnote/goback/internal/token"
	"urlnote/goback/shared"
)

// ---- full-text search ----
//
// Tokens are produced by the segmenters at save time and stored space-joined
// in the D1 FTS5 table (tokenize='ascii'). Queries go through the same
// pipeline and the terms are ANDed — word-level semantics ("中国" doesn't
// match "中间"), ranked by bm25. Tokens are restricted to word chars by
// pushToken, so no FTS5 escaping is needed.

func (s *Server) searchNotes(w http.ResponseWriter, r *http.Request) {
	if _, ok := s.requireUser(w, r); !ok {
		return
	}
	empty, _ := json.Marshal(shared.NoteList{Notes: []shared.NoteMeta{}})

	q := r.URL.Query().Get("q")
	if q == "" {
		s.writeRawJSON(w, http.StatusOK, empty, "")
		return
	}
	terms := s.Ana.Tokenize(strings.TrimSpace(q))
	if len(terms) == 0 {
		s.writeRawJSON(w, http.StatusOK, empty, "")
		return
	}
	matchExpr := strings.Join(terms, " AND ")

	rows, err := s.DB.SearchNotes(r.Context(), matchExpr)
	if err != nil {
		jsonError(w, http.StatusInternalServerError, "search failed")
		return
	}
	metas := make([]shared.NoteMeta, 0, len(rows))
	for _, m := range rows {
		metas = append(metas, shared.NoteMeta{
			ID: m.ID, Title: m.Title, Tags: parseTags(m.Tags),
			UpdatedAt: m.UpdatedAt, Excerpt: m.Excerpt, Author: m.Author,
		})
	}
	payload, _ := json.Marshal(shared.NoteList{Notes: metas})
	s.writeRawJSON(w, http.StatusOK, payload, "")
}

// reindexSearch rebuilds the FTS index for notes that predate the search
// feature (or drifted). Processing is chunked (reindexChunk notes per call)
// to mirror the Rust worker's contract: the caller loops with next_offset
// until it comes back null.
const reindexChunk = 10

type reindexResponse struct {
	Indexed    int  `json:"indexed"`
	NextOffset *int `json:"next_offset"`
}

func (s *Server) reindexSearch(w http.ResponseWriter, r *http.Request) {
	user, ok := s.requireUser(w, r)
	if !ok {
		return
	}
	if !isAdmin(user) {
		jsonError(w, http.StatusForbidden, "forbidden")
		return
	}
	offset, _ := strconv.Atoi(r.URL.Query().Get("offset"))

	count := 0
	for i := 0; i < reindexChunk; i++ {
		row, found, err := s.DB.NoteAt(r.Context(), offset+i)
		if err != nil || !found {
			break
		}
		head := []rune(row.Title + " " + strings.Join(parseTags(row.Tags), " ") + " " + row.Content)
		if len(head) > token.IndexMaxChars {
			head = head[:token.IndexMaxChars]
		}
		toks := s.Ana.Tokenize(string(head))
		// Index failures are logged upstream in the Rust worker; keep
		// processing the remaining notes here.
		_ = s.DB.ReplaceFts(r.Context(), row.ID, strings.Join(toks, " "))
		s.regenerateMarkAuto(r.Context(), row.ID)
		count++
	}

	resp := reindexResponse{Indexed: count}
	if count == reindexChunk {
		next := offset + count
		resp.NextOffset = &next
	}
	payload, _ := json.Marshal(resp)
	s.writeRawJSON(w, http.StatusOK, payload, "")
}
