//go:build js && wasm

// Command worker runs the urlnote Go backend as a Cloudflare Worker:
// the Go binary is compiled to WebAssembly (GOOS=js GOARCH=wasm) and
// executed by github.com/syumai/workers-go. It serves the JSON API backed by
// the D1 binding "DB", keeps uploaded images in the KV binding "CACHE",
// serves the SPA through the Static Assets binding "ASSETS", and runs the
// notes_mark consistency sweep from the cron trigger.
package main

import (
	"context"
	"database/sql"
	"log"
	"strings"
	"time"

	"github.com/syumai/workers-go"
	"github.com/syumai/workers-go/cloudflare"
	"github.com/syumai/workers-go/cloudflare/cron"
	_ "github.com/syumai/workers-go/cloudflare/d1" // registers the "d1" database/sql driver
	"github.com/syumai/workers-go/cloudflare/kv"

	"urlnote/goback/internal/api"
	"urlnote/goback/internal/store"
	"urlnote/goback/internal/token"
)

// sweepLimit mirrors the self-hosted sweep batch size (3 notes per cron run,
// matching the Rust worker's cron cadence of every 20 minutes).
const sweepLimit = 3

func corsOrigins() []string {
	var origins []string
	for _, o := range strings.Split(cloudflare.Getenv("CORS_ORIGINS"), ",") {
		if o = strings.TrimSpace(o); o != "" {
			origins = append(origins, o)
		}
	}
	return origins
}

func main() {
	// The D1 binding is resolved from the per-request env, so sql.Open must
	// stay inside the worker runtime (main runs once per request).
	db, err := sql.Open("d1", "DB")
	if err != nil {
		log.Printf("open D1 binding DB: %v", err)
		return
	}

	// KV is used for the response cache and uploaded images. Missing binding
	// degrades to no cache / no image storage instead of failing requests.
	cacheNS, err := kv.NewNamespace("CACHE")
	if err != nil {
		log.Printf("KV binding CACHE unavailable, running without cache: %v", err)
		cacheNS = nil
	}

	srv := &api.Server{
		DB:     store.New(db),
		Ana:    token.New(),
		Cache:  newKVCache(cacheNS),
		CORS:   corsOrigins(),
		Images: newKVImages(cacheNS),
		Static: newAssetsHandler(),
	}

	// Cron trigger ("7,27,47 * * * *" in wrangler-go.toml) replaces the
	// self-hosted ticker.
	cron.ScheduleTaskNonBlock(func(ctx context.Context) error {
		removed, regenerated, scanned := srv.SweepForLog(ctx)
		log.Printf("mark sweep: %d processed (%d orphans, %d regenerated)", scanned, removed, regenerated)
		return nil
	})

	workers.Serve(srv.Handler())
}

// noteTTL / indexTTL match the former in-memory cache values. Workers KV
// rejects expiration_ttl below 60s, so writes are clamped there.
const (
	noteTTL  = 30 * time.Second
	indexTTL = 15 * time.Second
	kvMinTTL = 60
)
