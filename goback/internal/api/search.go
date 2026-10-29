package api

import (
	"encoding/json"
	"net/http"
	"strconv"
	"strings"

	"urlnote/goback/internal/store"
	"urlnote/goback/internal/token"
	"urlnote/goback/shared"
)

// ---- full-text search ----
//
// Tokens are produced by jieba (Chinese) + kagome/IPADIC (Japanese) at save
// time and stored space-joined in the notes_fts table. Queries go through
// the same pipeline and the terms are ANDed — word-level semantics ("中国"
// doesn't match "中间"), ranked by ts_rank. tokens are restricted to word
// chars by pushToken, so no tsquery escaping is needed.

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
	matchExpr := strings.Join(terms, " & ")

	var rows []struct {
		ID        string
		Title     string
		Tags      string
		UpdatedAt string
		Excerpt   string
		Author    string
	}
	err := s.DB.Raw(`
		SELECT notes.id, notes.title, notes.tags, notes.updated_at,
		       left(notes.content, 120) AS excerpt, notes.author
		FROM notes_fts JOIN notes ON notes.id = notes_fts.note_id
		WHERE to_tsvector('simple', notes_fts.tokens) @@ to_tsquery('simple', ?)
		ORDER BY ts_rank(to_tsvector('simple', notes_fts.tokens), to_tsquery('simple', ?)) DESC
		LIMIT 50`, matchExpr, matchExpr).Scan(&rows).Error
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
// feature (or drifted). Processing is chunked (REINDEX_CHUNK notes per call)
// to mirror the Rust worker's contract: the caller loops with next_offset
// until it comes back null.
const reindexChunk = 10

type reindexResponse struct {
	Indexed    int   `json:"indexed"`
	NextOffset *int  `json:"next_offset"`
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
		var row store.Note
		if err := s.DB.Order("id").Offset(offset + i).Limit(1).First(&row).Error; err != nil {
			break
		}
		head := []rune(row.Title + " " + strings.Join(parseTags(row.Tags), " ") + " " + row.Content)
		if len(head) > token.IndexMaxChars {
			head = head[:token.IndexMaxChars]
		}
		toks := s.Ana.Tokenize(string(head))
		s.DB.Where("note_id = ?", row.ID).Delete(&store.NoteFts{})
		if err := s.DB.Create(&store.NoteFts{NoteID: row.ID, Tokens: strings.Join(toks, " ")}).Error; err != nil {
			// logged; continue with the next note
		}
		s.regenerateMarkAuto(row.ID)
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
