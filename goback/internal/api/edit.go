package api

import (
	"context"
	"encoding/json"
	"net/http"
	"strings"

	"urlnote/goback/shared"
)

// ---- edit lock ----
//
// The lock lives in D1: edit_lock is a single-row flag, edit_requests holds
// one row per user (pending until approved; reject/revoke delete the row so
// the user can apply again). Admin (the seeded "admin" account) is always
// allowed to edit and is the only one who can flip the lock or handle
// requests. All lock-state reads fail open (unlocked): a database hiccup must
// not brick editing for everyone — the write path still fails loudly if the
// database is actually down.

func isAdmin(user string) bool { return user == shared.AdminUsername }

func (s *Server) editLocked(ctx context.Context) bool {
	locked, err := s.DB.EditLocked(ctx)
	if err != nil {
		return false
	}
	return locked
}

func (s *Server) myRequestStatus(ctx context.Context, user string) string {
	status, _, err := s.DB.GetEditRequestStatus(ctx, user)
	if err != nil {
		return "none"
	}
	return status
}

func (s *Server) canEditNow(ctx context.Context, user string) bool {
	return isAdmin(user) || !s.editLocked(ctx) || s.myRequestStatus(ctx, user) == "approved"
}

func (s *Server) editStatus(w http.ResponseWriter, r *http.Request) {
	user, ok := s.requireUser(w, r)
	if !ok {
		return
	}
	locked := s.editLocked(r.Context())
	request := "none"
	if !isAdmin(user) {
		request = s.myRequestStatus(r.Context(), user)
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
	if err := s.DB.SetEditLock(r.Context(), body.Locked, nowStr()); err != nil {
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
	if isAdmin(user) || s.myRequestStatus(r.Context(), user) == "approved" {
		writeJSON(w, http.StatusOK, shared.OkResponse{OK: true})
		return
	}
	msg := []rune(strings.TrimSpace(body.Message))
	if len(msg) > 200 {
		msg = msg[:200]
	}
	// INSERT ... ON CONFLICT(username) DO UPDATE: upsert the pending row.
	if err := s.DB.SubmitEditRequest(r.Context(), user, string(msg), nowStr()); err != nil {
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
	rows, err := s.DB.ListEditRequests(r.Context())
	if err != nil {
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
		err = s.DB.ApproveEditRequest(r.Context(), target, nowStr())
	case "reject", "revoke":
		err = s.DB.DeleteEditRequest(r.Context(), target)
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
