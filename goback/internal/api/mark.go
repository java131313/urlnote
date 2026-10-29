package api

import (
	"encoding/json"
	"net/http"

	"gorm.io/gorm"
	"gorm.io/gorm/clause"

	"urlnote/goback/internal/store"
	"urlnote/goback/shared"
)

// ---- notes_mark lifecycle ----
//
// The annotated copy of a note lives in its own table while notes always
// keeps the clean original. A background sweep (plus a manual admin
// endpoint) keeps the two consistent: notes edited after marking get their
// mark regenerated, and marks whose note was deleted are removed.

// generateMark regenerates the marked copy of note id. No-op if the note is
// gone; returns false in that case.
func (s *Server) generateMarkRow(id string) bool {
	var note store.Note
	if err := s.DB.Select("id", "content", "updated_at").Where("id = ?", id).First(&note).Error; err != nil {
		return false
	}
	marked := s.Ana.AnnotateMarkdown(note.Content)
	err := s.DB.Clauses(clause.OnConflict{
		Columns: []clause.Column{{Name: "id"}},
		DoUpdates: clause.Set([]clause.Assignment{
			{Column: clause.Column{Name: "content"}, Value: marked},
			{Column: clause.Column{Name: "source_updated_at"}, Value: note.UpdatedAt},
			{Column: clause.Column{Name: "updated_at"}, Value: nowStr()},
		}),
	}).Create(&store.NoteMark{ID: id, Content: marked, SourceUpdatedAt: note.UpdatedAt, UpdatedAt: nowStr()}).Error
	return err == nil
}

// regenerateMarkAuto refreshes the marked copy from the note's current
// content, but never clobbers a mark the user edited by hand
// (source_updated_at = ''). Used by save and reindex.
func (s *Server) regenerateMarkAuto(id string) {
	var mark store.NoteMark
	if err := s.DB.Select("source_updated_at").Where("id = ?", id).First(&mark).Error; err == nil && mark.SourceUpdatedAt == "" {
		return
	}
	s.generateMarkRow(id)
}

// sweepMarks runs the consistency sweep:
//   - orphan marks (note deleted) are removed;
//   - stale marks (note edited after the mark was taken) are regenerated.
//
// Returns (orphansRemoved, marksRegenerated, scanned).
func (s *Server) sweepMarks(limit int) (int64, int64, int64) {
	// orphans: mark rows with no matching note
	var orphans []string
	s.DB.Table("notes_mark").
		Select("notes_mark.id").
		Joins("LEFT JOIN notes ON notes.id = notes_mark.id").
		Where("notes.id IS NULL").
		Limit(limit).
		Pluck("notes_mark.id", &orphans)
	var removed int64
	for _, id := range orphans {
		if err := s.DB.Where("id = ?", id).Delete(&store.NoteMark{}).Error; err == nil {
			removed++
		}
	}

	// stale marks
	remaining := limit - int(removed)
	if remaining < 0 {
		remaining = 0
	}
	var stale []string
	s.DB.Table("notes_mark").
		Select("notes_mark.id").
		Joins("JOIN notes ON notes.id = notes_mark.id").
		Where("notes_mark.source_updated_at <> '' AND notes_mark.source_updated_at <> notes.updated_at").
		Order("notes_mark.updated_at").
		Limit(remaining).
		Pluck("notes_mark.id", &stale)
	var regenerated int64
	for _, id := range stale {
		if s.generateMarkRow(id) {
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
	if !idRe.MatchString(id) {
		jsonError(w, http.StatusBadRequest, "bad request")
		return
	}
	if !s.canEditNow(user) {
		jsonError(w, http.StatusForbidden, "edit locked")
		return
	}
	if s.generateMarkRow(id) {
		writeJSON(w, http.StatusOK, shared.OkResponse{OK: true})
	} else if err := s.DB.Select("id").Where("id = ?", id).First(&store.Note{}).Error; err == gorm.ErrRecordNotFound {
		jsonError(w, http.StatusNotFound, "note not found")
	} else {
		jsonError(w, http.StatusInternalServerError, "mark failed")
	}
}

func (s *Server) saveMark(w http.ResponseWriter, r *http.Request) {
	user, ok := s.requireUser(w, r)
	if !ok {
		return
	}
	id := r.PathValue("id")
	if !idRe.MatchString(id) {
		jsonError(w, http.StatusBadRequest, "bad request")
		return
	}
	if !s.canEditNow(user) {
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
	res := s.DB.Model(&store.NoteMark{}).
		Where("id = ?", id).
		Updates(map[string]any{
			"content":            s.Ana.AnnotateMarkdown(body.Content),
			"source_updated_at": "",
			"updated_at":          nowStr(),
		})
	if res.Error != nil {
		jsonError(w, http.StatusInternalServerError, "save failed")
		return
	}
	if res.RowsAffected == 0 {
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
	if !idRe.MatchString(id) {
		jsonError(w, http.StatusBadRequest, "bad request")
		return
	}
	if !s.canEditNow(user) {
		jsonError(w, http.StatusForbidden, "edit locked")
		return
	}
	s.DB.Where("id = ?", id).Delete(&store.NoteMark{})
	writeJSON(w, http.StatusOK, shared.OkResponse{OK: true})
}

func (s *Server) getMark(w http.ResponseWriter, r *http.Request) {
	if _, ok := s.requireUser(w, r); !ok {
		return
	}
	id := r.PathValue("id")
	if !idRe.MatchString(id) {
		jsonError(w, http.StatusBadRequest, "bad request")
		return
	}
	var mark store.NoteMark
	if err := s.DB.Select("content", "source_updated_at").Where("id = ?", id).First(&mark).Error; err != nil {
		jsonError(w, http.StatusNotFound, "no mark")
		return
	}
	writeJSON(w, http.StatusOK, shared.MarkInfo{
		Content:         mark.Content,
		SourceUpdatedAt: mark.SourceUpdatedAt,
	})
}

// SweepForLog runs the periodic consistency sweep; it exists so the main
// package can drive the background ticker without knowing the internals.
func (s *Server) SweepForLog() (removed, regenerated, scanned int64) {
	return s.sweepMarks(3)
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
	removed, regenerated, scanned := s.sweepMarks(5)
	writeJSON(w, http.StatusOK, map[string]any{
		"scanned":          scanned,
		"orphans_removed":  removed,
		"regenerated":      regenerated,
	})
}
