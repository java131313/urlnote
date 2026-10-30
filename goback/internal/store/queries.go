package store

import (
	"context"
	"database/sql"
	"errors"
)

// ---- notes ----

// GetNote returns the note row, found=false when it does not exist.
func (s *Store) GetNote(ctx context.Context, id string) (Note, bool, error) {
	var n Note
	err := s.db.QueryRowContext(ctx,
		`SELECT id, content, title, tags, author, font_size, color, updated_at
		 FROM notes WHERE id = ?`, id).
		Scan(&n.ID, &n.Content, &n.Title, &n.Tags, &n.Author, &n.FontSize, &n.Color, &n.UpdatedAt)
	if errors.Is(err, sql.ErrNoRows) {
		return Note{}, false, nil
	}
	if err != nil {
		return Note{}, false, err
	}
	return n, true, nil
}

// SaveNote upserts the full note row (INSERT ... ON CONFLICT(id) DO UPDATE).
func (s *Store) SaveNote(ctx context.Context, n Note) error {
	_, err := s.db.ExecContext(ctx,
		`INSERT INTO notes (id, content, title, tags, author, font_size, color, updated_at)
		 VALUES (?, ?, ?, ?, ?, ?, ?, ?)
		 ON CONFLICT(id) DO UPDATE SET
		   content = excluded.content, title = excluded.title, tags = excluded.tags,
		   author = excluded.author, font_size = excluded.font_size,
		   color = excluded.color, updated_at = excluded.updated_at`,
		n.ID, n.Content, n.Title, n.Tags, n.Author, n.FontSize, n.Color, n.UpdatedAt)
	return err
}

// ListNoteMeta loads metadata of all notes, newest first. substr replaces
// PostgreSQL's left(content, 120).
func (s *Store) ListNoteMeta(ctx context.Context) ([]NoteMeta, error) {
	rows, err := s.db.QueryContext(ctx,
		`SELECT id, title, tags, updated_at, substr(content, 1, 120), author
		 FROM notes ORDER BY updated_at DESC`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanNoteMeta(rows)
}

func scanNoteMeta(rows *sql.Rows) ([]NoteMeta, error) {
	metas := []NoteMeta{}
	for rows.Next() {
		var m NoteMeta
		if err := rows.Scan(&m.ID, &m.Title, &m.Tags, &m.UpdatedAt, &m.Excerpt, &m.Author); err != nil {
			return nil, err
		}
		metas = append(metas, m)
	}
	return metas, rows.Err()
}

// NoteAt returns the note at the given offset (ORDER BY id); used by the
// chunked reindex endpoint. found=false past the end of the table.
func (s *Store) NoteAt(ctx context.Context, offset int) (Note, bool, error) {
	var n Note
	err := s.db.QueryRowContext(ctx,
		`SELECT id, content, title, tags, author, font_size, color, updated_at
		 FROM notes ORDER BY id LIMIT 1 OFFSET ?`, offset).
		Scan(&n.ID, &n.Content, &n.Title, &n.Tags, &n.Author, &n.FontSize, &n.Color, &n.UpdatedAt)
	if errors.Is(err, sql.ErrNoRows) {
		return Note{}, false, nil
	}
	if err != nil {
		return Note{}, false, err
	}
	return n, true, nil
}

// ---- users & sessions ----

func (s *Store) GetPasswordHash(ctx context.Context, username string) (string, bool, error) {
	var hash string
	err := s.db.QueryRowContext(ctx, `SELECT password_hash FROM users WHERE username = ?`, username).Scan(&hash)
	if errors.Is(err, sql.ErrNoRows) {
		return "", false, nil
	}
	if err != nil {
		return "", false, err
	}
	return hash, true, nil
}

// DeleteExpiredSessions purges rows whose expires_at has passed.
func (s *Store) DeleteExpiredSessions(ctx context.Context, now string) error {
	_, err := s.db.ExecContext(ctx, `DELETE FROM sessions WHERE expires_at <= ?`, now)
	return err
}

func (s *Store) CreateSession(ctx context.Context, token, username, expiresAt string) error {
	_, err := s.db.ExecContext(ctx,
		`INSERT INTO sessions (token, username, expires_at) VALUES (?, ?, ?)`,
		token, username, expiresAt)
	return err
}

// GetSessionUsername resolves an unexpired session token.
func (s *Store) GetSessionUsername(ctx context.Context, token, now string) (string, bool, error) {
	var username string
	err := s.db.QueryRowContext(ctx,
		`SELECT username FROM sessions WHERE token = ? AND expires_at > ?`, token, now).Scan(&username)
	if errors.Is(err, sql.ErrNoRows) {
		return "", false, nil
	}
	if err != nil {
		return "", false, err
	}
	return username, true, nil
}

func (s *Store) DeleteSession(ctx context.Context, token string) error {
	_, err := s.db.ExecContext(ctx, `DELETE FROM sessions WHERE token = ?`, token)
	return err
}

// ---- edit lock & edit requests ----

// EditLocked reports the global lock flag; a missing row means unlocked
// (the same fail-open behavior as the former GORM query).
func (s *Store) EditLocked(ctx context.Context) (bool, error) {
	var locked int
	err := s.db.QueryRowContext(ctx, `SELECT locked FROM edit_lock WHERE id = 1`).Scan(&locked)
	if errors.Is(err, sql.ErrNoRows) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	return locked != 0, nil
}

func (s *Store) SetEditLock(ctx context.Context, locked bool, now string) error {
	v := 0
	if locked {
		v = 1
	}
	_, err := s.db.ExecContext(ctx,
		`INSERT INTO edit_lock (id, locked, updated_at) VALUES (1, ?, ?)
		 ON CONFLICT(id) DO UPDATE SET locked = excluded.locked, updated_at = excluded.updated_at`,
		v, now)
	return err
}

// GetEditRequestStatus returns the request row status; found=false when the
// user never applied.
func (s *Store) GetEditRequestStatus(ctx context.Context, username string) (string, bool, error) {
	var status string
	err := s.db.QueryRowContext(ctx,
		`SELECT status FROM edit_requests WHERE username = ?`, username).Scan(&status)
	if errors.Is(err, sql.ErrNoRows) {
		return "", false, nil
	}
	if err != nil {
		return "", false, err
	}
	return status, true, nil
}

// SubmitEditRequest upserts a pending request for the user.
func (s *Store) SubmitEditRequest(ctx context.Context, username, message, now string) error {
	_, err := s.db.ExecContext(ctx,
		`INSERT INTO edit_requests (username, status, message, created_at) VALUES (?, 'pending', ?, ?)
		 ON CONFLICT(username) DO UPDATE SET
		   status = excluded.status, message = excluded.message, created_at = excluded.created_at`,
		username, message, now)
	return err
}

// ApproveEditRequest upserts an approved row, keeping an existing
// message/created_at (INSERT OR REPLACE semantics of the former code kept
// only status on conflict).
func (s *Store) ApproveEditRequest(ctx context.Context, username, now string) error {
	_, err := s.db.ExecContext(ctx,
		`INSERT INTO edit_requests (username, status, message, created_at) VALUES (?, 'approved', '', ?)
		 ON CONFLICT(username) DO UPDATE SET status = 'approved'`,
		username, now)
	return err
}

func (s *Store) DeleteEditRequest(ctx context.Context, username string) error {
	_, err := s.db.ExecContext(ctx, `DELETE FROM edit_requests WHERE username = ?`, username)
	return err
}

func (s *Store) ListEditRequests(ctx context.Context) ([]EditRequest, error) {
	rows, err := s.db.QueryContext(ctx,
		`SELECT username, status, message, created_at FROM edit_requests ORDER BY created_at DESC`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	items := []EditRequest{}
	for rows.Next() {
		var r EditRequest
		if err := rows.Scan(&r.Username, &r.Status, &r.Message, &r.CreatedAt); err != nil {
			return nil, err
		}
		items = append(items, r)
	}
	return items, rows.Err()
}

// ---- presence ----

func (s *Store) TouchPresence(ctx context.Context, noteID, username, now string) error {
	_, err := s.db.ExecContext(ctx,
		`INSERT INTO presence (note_id, username, updated_at) VALUES (?, ?, ?)
		 ON CONFLICT(note_id, username) DO UPDATE SET updated_at = excluded.updated_at`,
		noteID, username, now)
	return err
}

func (s *Store) DeleteStalePresence(ctx context.Context, cutoff string) error {
	_, err := s.db.ExecContext(ctx, `DELETE FROM presence WHERE updated_at < ?`, cutoff)
	return err
}

func (s *Store) ListPresenceUsers(ctx context.Context, noteID, cutoff string) ([]string, error) {
	rows, err := s.db.QueryContext(ctx,
		`SELECT username FROM presence WHERE note_id = ? AND updated_at >= ? ORDER BY username`,
		noteID, cutoff)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	users := []string{}
	for rows.Next() {
		var u string
		if err := rows.Scan(&u); err != nil {
			return nil, err
		}
		users = append(users, u)
	}
	return users, rows.Err()
}

// ---- notes_mark ----

// NoteForMark returns the fields needed to (re)generate a marked copy.
func (s *Store) NoteForMark(ctx context.Context, id string) (Note, bool, error) {
	var n Note
	err := s.db.QueryRowContext(ctx,
		`SELECT id, content, updated_at FROM notes WHERE id = ?`, id).
		Scan(&n.ID, &n.Content, &n.UpdatedAt)
	if errors.Is(err, sql.ErrNoRows) {
		return Note{}, false, nil
	}
	if err != nil {
		return Note{}, false, err
	}
	return n, true, nil
}

func (s *Store) NoteExists(ctx context.Context, id string) (bool, error) {
	var one int
	err := s.db.QueryRowContext(ctx, `SELECT 1 FROM notes WHERE id = ?`, id).Scan(&one)
	if errors.Is(err, sql.ErrNoRows) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	return true, nil
}

func (s *Store) GetMark(ctx context.Context, id string) (NoteMark, bool, error) {
	var m NoteMark
	err := s.db.QueryRowContext(ctx,
		`SELECT id, content, source_updated_at, updated_at FROM notes_mark WHERE id = ?`, id).
		Scan(&m.ID, &m.Content, &m.SourceUpdatedAt, &m.UpdatedAt)
	if errors.Is(err, sql.ErrNoRows) {
		return NoteMark{}, false, nil
	}
	if err != nil {
		return NoteMark{}, false, err
	}
	return m, true, nil
}

func (s *Store) UpsertMark(ctx context.Context, m NoteMark) error {
	_, err := s.db.ExecContext(ctx,
		`INSERT INTO notes_mark (id, content, source_updated_at, updated_at) VALUES (?, ?, ?, ?)
		 ON CONFLICT(id) DO UPDATE SET
		   content = excluded.content, source_updated_at = excluded.source_updated_at,
		   updated_at = excluded.updated_at`,
		m.ID, m.Content, m.SourceUpdatedAt, m.UpdatedAt)
	return err
}

// UpdateMarkContent stores a hand-edited mark: source_updated_at is blanked
// so the consistency sweep leaves it alone. rows=0 means no mark exists.
func (s *Store) UpdateMarkContent(ctx context.Context, id, content, now string) (int64, error) {
	res, err := s.db.ExecContext(ctx,
		`UPDATE notes_mark SET content = ?, source_updated_at = '', updated_at = ? WHERE id = ?`,
		content, now, id)
	if err != nil {
		return 0, err
	}
	return res.RowsAffected()
}

func (s *Store) DeleteMark(ctx context.Context, id string) error {
	_, err := s.db.ExecContext(ctx, `DELETE FROM notes_mark WHERE id = ?`, id)
	return err
}

func (s *Store) OrphanMarkIDs(ctx context.Context, limit int) ([]string, error) {
	return s.markIDs(ctx,
		`SELECT notes_mark.id FROM notes_mark
		 LEFT JOIN notes ON notes.id = notes_mark.id
		 WHERE notes.id IS NULL LIMIT ?`, limit)
}

func (s *Store) StaleMarkIDs(ctx context.Context, limit int) ([]string, error) {
	return s.markIDs(ctx,
		`SELECT notes_mark.id FROM notes_mark JOIN notes ON notes.id = notes_mark.id
		 WHERE notes_mark.source_updated_at <> '' AND notes_mark.source_updated_at <> notes.updated_at
		 ORDER BY notes_mark.updated_at LIMIT ?`, limit)
}

func (s *Store) markIDs(ctx context.Context, query string, limit int) ([]string, error) {
	rows, err := s.db.QueryContext(ctx, query, limit)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	ids := []string{}
	for rows.Next() {
		var id string
		if err := rows.Scan(&id); err != nil {
			return nil, err
		}
		ids = append(ids, id)
	}
	return ids, rows.Err()
}

// ---- full-text search (FTS5) ----

// ReplaceFts rewrites the index row for one note.
func (s *Store) ReplaceFts(ctx context.Context, noteID, tokens string) error {
	if _, err := s.db.ExecContext(ctx, `DELETE FROM notes_fts WHERE note_id = ?`, noteID); err != nil {
		return err
	}
	_, err := s.db.ExecContext(ctx, `INSERT INTO notes_fts (note_id, tokens) VALUES (?, ?)`, noteID, tokens)
	return err
}

// SearchNotes runs an FTS5 MATCH query; tokens are restricted to word
// characters by the tokenizer, so no FTS escaping is needed.
func (s *Store) SearchNotes(ctx context.Context, matchExpr string) ([]NoteMeta, error) {
	rows, err := s.db.QueryContext(ctx,
		`SELECT notes.id, notes.title, notes.tags, notes.updated_at,
		        substr(notes.content, 1, 120) AS excerpt, notes.author
		 FROM notes_fts JOIN notes ON notes.id = notes_fts.note_id
		 WHERE notes_fts MATCH ?
		 ORDER BY bm25(notes_fts)
		 LIMIT 50`, matchExpr)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	return scanNoteMeta(rows)
}
