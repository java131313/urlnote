package api

import (
	"encoding/json"
	"net/http"

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
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil || !validID(body.NoteID) {
		jsonError(w, http.StatusBadRequest, "bad request")
		return
	}

	now := nowStr()
	if err := s.DB.TouchPresence(r.Context(), body.NoteID, user, now); err != nil {
		jsonError(w, http.StatusInternalServerError, "presence failed")
		return
	}
	// opportunistic cleanup of stale rows (also excluded in the SELECT below)
	cutoff := timeNowAdd(-presenceTTLSec)
	_ = s.DB.DeleteStalePresence(r.Context(), cutoff)

	users, err := s.DB.ListPresenceUsers(r.Context(), body.NoteID, cutoff)
	if err != nil {
		users = []string{}
	}
	writeJSON(w, http.StatusOK, shared.PresenceResponse{Users: users})
}
