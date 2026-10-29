package api

import (
	"encoding/json"
	"net/http"

	"gorm.io/gorm/clause"

	"urlnote/goback/internal/store"
	"urlnote/goback/shared"
)

// presence records the caller's visit to a note (heartbeat; rows older than
// presenceTTLSec are treated as gone) and returns everyone currently on it.
func (s *Server) presence(w http.ResponseWriter, r *http.Request) {
	user, ok := s.requireUser(w, r)
	if !ok {
		return
	}
	var body shared.PresenceRequest
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil || !idRe.MatchString(body.NoteID) {
		jsonError(w, http.StatusBadRequest, "bad request")
		return
	}

	now := nowStr()
	err := s.DB.Clauses(clause.OnConflict{
		Columns:   []clause.Column{{Name: "note_id"}, {Name: "username"}},
		DoUpdates: clause.AssignmentColumns([]string{"updated_at"}),
	}).Create(&store.Presence{NoteID: body.NoteID, Username: user, UpdatedAt: now}).Error
	if err != nil {
		jsonError(w, http.StatusInternalServerError, "presence failed")
		return
	}
	// opportunistic cleanup of stale rows (also excluded in the SELECT below)
	cutoff := timeNowAdd(-presenceTTLSec)
	s.DB.Where("updated_at < ?", cutoff).Delete(&store.Presence{})

	var users []string
	if err := s.DB.Model(&store.Presence{}).
		Select("username").
		Where("note_id = ? AND updated_at >= ?", body.NoteID, cutoff).
		Order("username").Pluck("username", &users).Error; err != nil || users == nil {
		users = []string{}
	}
	writeJSON(w, http.StatusOK, shared.PresenceResponse{Users: users})
}
