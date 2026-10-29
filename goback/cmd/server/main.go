// Command server runs the urlnote app as a single self-hosted binary: JSON
// API + Go WASM frontend statics + uploaded images, backed by PostgreSQL
// through GORM. This is the Go replacement for the Cloudflare
// (Rust worker + D1 + KV) deployment, targeting a Sealos container that
// connects directly to the managed PostgreSQL.
package main

import (
	"log"
	"net/http"
	"os"
	"strings"
	"time"

	"gorm.io/driver/postgres"
	"gorm.io/gorm"
	"gorm.io/gorm/logger"

	"urlnote/goback/internal/api"
	"urlnote/goback/internal/cache"
	"urlnote/goback/internal/store"
	"urlnote/goback/internal/token"
)

func env(key, fallback string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return fallback
}

func main() {
	dsn := os.Getenv("DATABASE_URL")
	if dsn == "" {
		log.Fatal("DATABASE_URL is required, e.g. postgres://user:pass@host:5432/urlnote")
	}
	addr := env("ADDR", ":8080")
	staticDir := env("STATIC_DIR", "./static")
	imageDir := env("IMAGE_DIR", "./data/images")
	corsOrigins := env("CORS_ORIGINS", "")
	sweepInterval, err := time.ParseDuration(env("SWEEP_INTERVAL", "20m"))
	if err != nil {
		log.Fatalf("SWEEP_INTERVAL: %v", err)
	}

	db, err := gorm.Open(postgres.Open(dsn), &gorm.Config{
		Logger: logger.Default.LogMode(logger.Error),
	})
	if err != nil {
		log.Fatalf("connect database: %v", err)
	}
	sqlDB, err := db.DB()
	if err != nil {
		log.Fatalf("underlying sql.DB: %v", err)
	}
	sqlDB.SetMaxOpenConns(20)
	sqlDB.SetMaxIdleConns(5)
	sqlDB.SetConnMaxLifetime(time.Hour)

	// Create missing tables (notes_fts on a migrated database) without
	// touching the existing D1-shaped ones.
	if err := store.Migrate(db); err != nil {
		log.Fatalf("migrate: %v", err)
	}

	ana := token.New()
	defer ana.Close()

	var origins []string
	for _, o := range strings.Split(corsOrigins, ",") {
		if o = strings.TrimSpace(o); o != "" {
			origins = append(origins, o)
		}
	}

	srv := &api.Server{
		DB:       db,
		Ana:      ana,
		Cache:    cache.New(512, 30*time.Second, 15*time.Second),
		CORS:     origins,
		ImageDir: imageDir,
		Static:   api.NewStaticHandler(staticDir),
	}
	if err := os.MkdirAll(imageDir, 0o755); err != nil {
		log.Fatalf("image dir: %v", err)
	}

	// Background consistency sweep between notes and notes_mark, replacing
	// the Cloudflare cron trigger ("7,27,47 * * * *").
	go func() {
		ticker := time.NewTicker(sweepInterval)
		defer ticker.Stop()
		for range ticker.C {
			removed, regenerated, scanned := srv.SweepForLog()
			log.Printf("mark sweep: %d processed (%d orphans, %d regenerated)", scanned, removed, regenerated)
		}
	}()

	log.Printf("urlnote goback listening on %s (static=%s images=%s)", addr, staticDir, imageDir)
	if err := http.ListenAndServe(addr, srv.Handler()); err != nil {
		log.Fatal(err)
	}
}
