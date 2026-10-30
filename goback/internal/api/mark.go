package api

import (
	"context"
	"encoding/json"
	"net/http"

	"urlnote/goback/internal/store"
	"urlnote/goback/shared"
)

// ---- notes_mark lifecycle ----
//
// The annotated copy of a note lives in its own table while notes always
// keeps the clean original. A background sweep (plus a manual admin
// endpoint) keeps the two consistent: notes edited after marking get their
// mark regenerated, and marks whose note was deleted are removed.

// generateMarkRow regenerates the marked copy of note id. No-op if the note is
// gone; returns false in that case.
func (s *Server) generateMarkRow(ctx context.Context, id string) bool {
	note, found, err := s.DB.NoteForMark(ctx, id)
	if err != nil || !found {
		return false
	}
	marked := s.Ana.AnnotateMarkdown(note.Content)
	err = s.DB.UpsertMark(ctx, store.NoteMark{
		ID: id, Content: marked, SourceUpdatedAt: note.UpdatedAt, UpdatedAt: nowStr(),
	})
	return err == nil
}

// regenerateMarkAuto refreshes the marked copy from the note's current
// content, but never clobbers a mark the user edited by hand
// (an empty source_updated_at). Used by save and reindex.
func (s *Server) regenerateMarkAuto(ctx context.Context, id string) {
	mark, found, err := s.DB.GetMark(ctx, id)
	if err == nil && found && mark.SourceUpdatedAt == "" {
		return
	}
	s.generateMarkRow(ctx, id)
}

// sweepMarks runs the consistency sweep:
//   - orphan marks (note deleted) are removed;
//   - stale marks (note edited after the mark was taken) are regenerated.
//
// Returns (orphansRemoved, marksRegenerated, scanned).
func (s *Server) sweepMarks(ctx context.Context, limit int) (int64, int64, int64) {
	// orphans: mark rows with no matching note
	orphans, _ := s.DB.OrphanMarkIDs(ctx, limit)
	var removed int64
	for _, id := range orphans {
		if err := s.DB.DeleteMark(ctx, id); err == nil {
			removed++
		}
	}

	// stale marks
	remaining := limit - int(removed)
	if remaining < 0 {
		remaining = 0
	}
	stale, _ := s.DB.StaleMarkIDs(ctx, remaining)
	var regenerated int64
	for _, id := range stale {
		if s.generateMarkRow(ctx, id) {
			regenerated++
		}
	}
	return removed, regenerated, removed + regenerated
}

// ---- HTTP handlers ----

func (s *Server) generateMark(w http.ResponseWriter, r *http.Request) {
	user, ok := s.requireUser(w, r)
	if !ok {
		return
	}
	id := r.PathValue("id")
	if !validID(id) {
		jsonError(w, http.StatusBadRequest, "bad request")
		return
	}
	if !s.canEditNow(r.Context(), user) {
		jsonError(w, http.StatusForbidden, "edit locked")
		return
	}
	if s.generateMarkRow(r.Context(), id) {
		writeJSON(w, http.StatusOK, shared.OkResponse{OK: true})
		return
	}
	exists, err := s.DB.NoteExists(r.Context(), id)
	if err != nil {
		jsonError(w, http.StatusInternalServerError, "mark failed")
		return
	}
	if !exists {
		jsonError(w, http.StatusNotFound, "note not found")
		return
	}
	jsonError(w, http.StatusInternalServerError, "mark failed")
}

func (s *Server) saveMark(w http.ResponseWriter, r *http.Request) {
	user, ok := s.requireUser(w, r)
	if !ok {
		return
	}
	id := r.PathValue("id")
	if !validID(id) {
		jsonError(w, http.StatusBadRequest, "bad request")
		return
	}
	if !s.canEditNow(r.Context(), user) {
		jsonError(w, http.StatusForbidden, "edit locked")
		return
	}
	var body shared.AnalyzeRequest
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
		jsonError(w, http.StatusBadRequest, "bad request")
		return
	}
	// The content is re-annotated (POS HTML spans) before storing, so
	// notes_mark always holds the annotated document; source_updated_at is
	// blanked so the consistency sweep leaves hand-edited marks alone.
	rows, err := s.DB.UpdateMarkContent(r.Context(), id, s.Ana.AnnotateMarkdown(body.Content), nowStr())
	if err != nil {
		jsonError(w, http.StatusInternalServerError, "save failed")
		return
	}
	if rows == 0 {
		jsonError(w, http.StatusNotFound, "no mark")
		return
	}
	writeJSON(w, http.StatusOK, shared.OkResponse{OK: true})
}

func (s *Server) removeMark(w http.ResponseWriter, r *http.Request) {
	user, ok := s.requireUser(w, r)
	if !ok {
		return
	}
	id := r.PathValue("id")
	if !validID(id) {
		jsonError(w, http.StatusBadRequest, "bad request")
		return
	}
	if !s.canEditNow(r.Context(), user) {
		jsonError(w, http.StatusForbidden, "edit locked")
		return
	}
	_ = s.DB.DeleteMark(r.Context(), id)
	writeJSON(w, http.StatusOK, shared.OkResponse{OK: true})
}

func (s *Server) getMark(w http.ResponseWriter, r *http.Request) {
	if _, ok := s.requireUser(w, r); !ok {
		return
	}
	id := r.PathValue("id")
	if !validID(id) {
		jsonError(w, http.StatusBadRequest, "bad request")
		return
	}
	mark, found, err := s.DB.GetMark(r.Context(), id)
	if err != nil || !found {
		jsonError(w, http.StatusNotFound, "no mark")
		return
	}
	writeJSON(w, http.StatusOK, shared.MarkInfo{
		Content:         mark.Content,
		SourceUpdatedAt: mark.SourceUpdatedAt,
	})
}

// SweepForLog runs the periodic consistency sweep; it exists so the worker
// entry (cron trigger) can drive it without knowing the internals.
func (s *Server) SweepForLog(ctx context.Context) (removed, regenerated, scanned int64) {
	return s.sweepMarks(ctx, 3)
}

// checkMarks is the manual admin sweep endpoint.
func (s *Server) checkMarks(w http.ResponseWriter, r *http.Request) {
	user, ok := s.requireUser(w, r)
	if !ok {
		return
	}
	if !isAdmin(user) {
		jsonError(w, http.StatusForbidden, "forbidden")
		return
	}
	removed, regenerated, scanned := s.sweepMarks(r.Context(), 5)
	writeJSON(w, http.StatusOK, map[string]any{
		"scanned":         scanned,
		"orphans_removed": removed,
		"regenerated":     regenerated,
	})
}
