// Package store is the urlnote data access layer for Cloudflare D1
// (SQLite). It replaces the former GORM/PostgreSQL implementation: every
// statement is written in SQLite dialect (? placeholders, ON CONFLICT upserts,
// substr(), FTS5 MATCH/bm25) so it runs unchanged on D1 and on a local
// wrangler D1 database.
//
// Timestamps stay TEXT columns holding UTC "YYYY-MM-DD HH:MM:SS" values —
// byte-compatible with rows written by the Rust worker and schema.sql's
// datetime('now') defaults.
package store

import "database/sql"

type Note struct {
	ID        string
	Content   string
	Title     string
	Tags      string
	Author    string
	FontSize  int
	Color     string
	UpdatedAt string
}

// NoteMeta is one entry of the notes list / search result.
type NoteMeta struct {
	ID        string
	Title     string
	Tags      string
	UpdatedAt string
	Excerpt   string
	Author    string
}

type EditRequest struct {
	Username  string
	Status    string
	Message   string
	CreatedAt string
}

type NoteMark struct {
	ID              string
	Content         string
	SourceUpdatedAt string
	UpdatedAt       string
}

// Store wraps the database handle. A single instance is created per Worker
// request (the D1 binding is resolved from the per-request env).
type Store struct {
	db *sql.DB
}

func New(db *sql.DB) *Store { return &Store{db: db} }
