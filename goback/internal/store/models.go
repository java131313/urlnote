// Package store defines the GORM models for the urlnote PostgreSQL schema.
//
// The schema was migrated from Cloudflare D1 (SQLite), so timestamps are
// TEXT columns holding UTC "YYYY-MM-DD HH:MM:SS" values — the models keep
// them as strings to stay byte-compatible with rows written by the Rust
// worker. notes_fts replaces D1's FTS5 virtual table with a real table
// (note_id, tokens); search uses to_tsvector('simple', tokens) @@ query.
package store

import "gorm.io/gorm"

type Note struct {
	ID        string `gorm:"column:id;type:text;primaryKey"`
	Content   string `gorm:"column:content;type:text;not null;default:''"`
	Title     string `gorm:"column:title;type:text;not null;default:''"`
	Tags      string `gorm:"column:tags;type:text;not null;default:'[]'"`
	Author    string `gorm:"column:author;type:text;not null;default:''"`
	FontSize  int    `gorm:"column:font_size;type:integer;not null;default:16"`
	Color     string `gorm:"column:color;type:text;not null;default:''"`
	UpdatedAt string `gorm:"column:updated_at;type:text;not null"`
}

func (Note) TableName() string { return "notes" }

type User struct {
	Username     string `gorm:"column:username;type:text;primaryKey"`
	DisplayName  string `gorm:"column:display_name;type:text;not null;default:''"`
	PasswordHash string `gorm:"column:password_hash;type:text;not null"`
	CreatedAt    string `gorm:"column:created_at;type:text;not null;default:''"`
}

func (User) TableName() string { return "users" }

type EditLock struct {
	ID        int    `gorm:"column:id;type:integer;primaryKey"`
	Locked    int    `gorm:"column:locked;type:integer;not null;default:0"`
	UpdatedAt string `gorm:"column:updated_at;type:text;not null;default:''"`
}

func (EditLock) TableName() string { return "edit_lock" }

type EditRequest struct {
	Username  string `gorm:"column:username;type:text;primaryKey"`
	Status    string `gorm:"column:status;type:text;not null;default:'pending'"`
	Message   string `gorm:"column:message;type:text;not null;default:''"`
	CreatedAt string `gorm:"column:created_at;type:text;not null;default:''"`
}

func (EditRequest) TableName() string { return "edit_requests" }

type Session struct {
	Token     string `gorm:"column:token;type:text;primaryKey"`
	Username  string `gorm:"column:username;type:text;not null"`
	ExpiresAt string `gorm:"column:expires_at;type:text;not null"`
}

func (Session) TableName() string { return "sessions" }

type Presence struct {
	NoteID    string `gorm:"column:note_id;type:text;primaryKey"`
	Username  string `gorm:"column:username;type:text;primaryKey"`
	UpdatedAt string `gorm:"column:updated_at;type:text;not null;default:''"`
}

func (Presence) TableName() string { return "presence" }

type NoteMark struct {
	ID              string `gorm:"column:id;type:text;primaryKey"`
	Content         string `gorm:"column:content;type:text;not null"`
	SourceUpdatedAt string `gorm:"column:source_updated_at;type:text;not null"`
	UpdatedAt       string `gorm:"column:updated_at;type:text;not null;default:''"`
}

func (NoteMark) TableName() string { return "notes_mark" }

// NoteFts is the full-text index: space-joined pre-segmented tokens per note.
// D1 used an FTS5 virtual table which does not exist in PostgreSQL; a plain
// table plus to_tsvector('simple', ...) at query time is the equivalent.
type NoteFts struct {
	NoteID string `gorm:"column:note_id;type:text;primaryKey"`
	Tokens string `gorm:"column:tokens;type:text"`
}

func (NoteFts) TableName() string { return "notes_fts" }

// Migrate creates missing tables (notes_fts on a fresh migrated database)
// and aligns columns on the existing D1-shaped ones. It never drops data.
func Migrate(db *gorm.DB) error {
	return db.AutoMigrate(&Note{}, &User{}, &EditLock{}, &EditRequest{},
		&Session{}, &Presence{}, &NoteMark{}, &NoteFts{})
}
