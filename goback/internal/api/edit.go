package api

import (
	"encoding/json"
	"net/http"
	"strings"

	"gorm.io/gorm/clause"

	"urlnote/goback/internal/store"
	"urlnote/goback/shared"
)

// ---- edit lock ----
//
// The lock lives in PostgreSQL: edit_lock is a single-row flag,
// edit_requests holds one row per user (pending until approved; reject/
// revoke delete the row so the user can apply again). Admin (the seeded
// "admin" account) is always allowed to edit and is the only one who can
// flip the lock or handle requests. All lock-state reads fail open
// (unlocked): a database hiccup must not brick editing for everyone — the
// write path still fails loudly if the database is actually down.

func isAdmin(user string) bool { return user == shared.AdminUsername }

func (s *Server) editLocked() bool {
	var lock store.EditLock
	if err := s.DB.Select("locked").Where("id = 1").First(&lock).Error; err != nil {
		return false
	}
	return lock.Locked != 0
}

func (s *Server) myRequestStatus(user string) string {
	var req store.EditRequest
	if err := s.DB.Select("status").Where("username = ?", user).First(&req).Error; err != nil {
		return "none"
	}
	return req.Status
}

func (s *Server) canEditNow(user string) bool {
	return isAdmin(user) || !s.editLocked() || s.myRequestStatus(user) == "approved"
}

func (s *Server) editStatus(w http.ResponseWriter, r *http.Request) {
	user, ok := s.requireUser(w, r)
	if !ok {
		return
	}
	locked := s.editLocked()
	request := "none"
	if !isAdmin(user) {
		request = s.myRequestStatus(user)
	}
	writeJSON(w, http.StatusOK, shared.EditStatusResponse{
		Locked:  locked,
		IsAdmin: isAdmin(user),
		CanEdit: isAdmin(user) || !locked || request == "approved",
		Request: request,
	})
}

func (s *Server) setEditLock(w http.ResponseWriter, r *http.Request) {
	user, ok := s.requireUser(w, r)
	if !ok {
		return
	}
	if !isAdmin(user) {
		jsonError(w, http.StatusForbidden, "forbidden")
		return
	}
	var body shared.EditLockRequest
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
		jsonError(w, http.StatusBadRequest, "bad request")
		return
	}
	locked := 0
	if body.Locked {
		locked = 1
	}
	err := s.DB.Clauses(clause.OnConflict{
		Columns:   []clause.Column{{Name: "id"}},
		DoUpdates: clause.AssignmentColumns([]string{"locked", "updated_at"}),
	}).Create(&store.EditLock{ID: 1, Locked: locked, UpdatedAt: nowStr()}).Error
	if err != nil {
		jsonError(w, http.StatusInternalServerError, "lock failed")
		return
	}
	writeJSON(w, http.StatusOK, shared.OkResponse{OK: true})
}

func (s *Server) submitEditRequest(w http.ResponseWriter, r *http.Request) {
	user, ok := s.requireUser(w, r)
	if !ok {
		return
	}
	var body shared.EditRequestSubmit
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
		jsonError(w, http.StatusBadRequest, "bad request")
		return
	}
	// Admin never needs approval; an existing approval is left untouched.
	if isAdmin(user) || s.myRequestStatus(user) == "approved" {
		writeJSON(w, http.StatusOK, shared.OkResponse{OK: true})
		return
	}
	msg := []rune(strings.TrimSpace(body.Message))
	if len(msg) > 200 {
		msg = msg[:200]
	}
	// INSERT OR REPLACE equivalent: upsert the pending row.
	err := s.DB.Clauses(clause.OnConflict{
		Columns: []clause.Column{{Name: "username"}},
		DoUpdates: clause.Set([]clause.Assignment{
			{Column: clause.Column{Name: "status"}, Value: "pending"},
			{Column: clause.Column{Name: "message"}, Value: string(msg)},
			{Column: clause.Column{Name: "created_at"}, Value: nowStr()},
		}),
	}).Create(&store.EditRequest{Username: user, Status: "pending", Message: string(msg), CreatedAt: nowStr()}).Error
	if err != nil {
		jsonError(w, http.StatusInternalServerError, "request failed")
		return
	}
	writeJSON(w, http.StatusOK, shared.OkResponse{OK: true})
}

func (s *Server) listEditRequests(w http.ResponseWriter, r *http.Request) {
	user, ok := s.requireUser(w, r)
	if !ok {
		return
	}
	if !isAdmin(user) {
		jsonError(w, http.StatusForbidden, "forbidden")
		return
	}
	var rows []store.EditRequest
	if err := s.DB.Order("created_at DESC").Find(&rows).Error; err != nil {
		jsonError(w, http.StatusInternalServerError, "list failed")
		return
	}
	items := make([]shared.EditRequestItem, 0, len(rows))
	for _, row := range rows {
		items = append(items, shared.EditRequestItem{
			Username: row.Username, Status: row.Status,
			Message: row.Message, CreatedAt: row.CreatedAt,
		})
	}
	writeJSON(w, http.StatusOK, shared.EditRequestList{Requests: items})
}

func (s *Server) handleEditRequest(w http.ResponseWriter, r *http.Request) {
	user, ok := s.requireUser(w, r)
	if !ok {
		return
	}
	if !isAdmin(user) {
		jsonError(w, http.StatusForbidden, "forbidden")
		return
	}
	var body shared.EditHandleRequest
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
		jsonError(w, http.StatusBadRequest, "bad request")
		return
	}
	target := strings.TrimSpace(body.Username)
	if target == "" || len([]rune(target)) > 32 || isAdmin(target) {
		jsonError(w, http.StatusBadRequest, "bad request")
		return
	}
	var err error
	switch body.Action {
	case "approve":
		// Upsert: approving a pending row keeps its message; re-approving is idempotent.
		err = s.DB.Clauses(clause.OnConflict{
			Columns: []clause.Column{{Name: "username"}},
			DoUpdates: clause.Set([]clause.Assignment{
				{Column: clause.Column{Name: "status"}, Value: "approved"},
			}),
		}).Create(&store.EditRequest{Username: target, Status: "approved", Message: "", CreatedAt: nowStr()}).Error
	case "reject", "revoke":
		err = s.DB.Where("username = ?", target).Delete(&store.EditRequest{}).Error
	default:
		jsonError(w, http.StatusBadRequest, "bad request")
		return
	}
	if err != nil {
		jsonError(w, http.StatusInternalServerError, "handle failed")
		return
	}
	writeJSON(w, http.StatusOK, shared.OkResponse{OK: true})
}
