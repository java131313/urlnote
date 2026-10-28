# Some shells export a stale GOROOT pointing at a different Go version than
# the `go` binary on PATH, which breaks the toolchain — always use the
# go binary's built-in GOROOT.
unexport GOROOT

.PHONY: build build-go frontend wasm dev deploy create-db init-db init-db-local clean

# ---------------------------------------------------------------------------
# Rust pipeline (default) — this is what wrangler dev/deploy builds:
#   rust/worker    Cloudflare Worker (workers-rs) compiled to WASM
#   rust/frontend  Leptos CSR app compiled to WASM, built by Trunk
# The Go backend (main.go) is kept in the repo as reference/rollback; its
# build targets remain at the bottom of this file.
# ---------------------------------------------------------------------------

build: rust-static rust-frontend rust-worker

rust-static:
	npm --prefix rust/frontend ci --no-audit --no-fund
	rm -rf rust/frontend/static/vditor
	mkdir -p rust/frontend/static/vditor
	cp -R rust/frontend/node_modules/vditor/dist rust/frontend/static/vditor/dist
	# import 转换库：mammoth(docx→html) / turndown(html→md) / pdfjs(pdf 文本)
	rm -rf rust/frontend/static/mammoth rust/frontend/static/turndown rust/frontend/static/pdfjs
	mkdir -p rust/frontend/static/mammoth rust/frontend/static/turndown rust/frontend/static/pdfjs
	cp rust/frontend/node_modules/mammoth/mammoth.browser.min.js rust/frontend/static/mammoth/
	cp rust/frontend/node_modules/turndown/dist/turndown.js rust/frontend/static/turndown/
	cp rust/frontend/node_modules/pdfjs-dist/legacy/build/pdf.min.js \
	   rust/frontend/node_modules/pdfjs-dist/legacy/build/pdf.worker.min.js \
	   rust/frontend/static/pdfjs/

rust-frontend:
	cd rust/frontend && trunk build --release
	# Trunk 0.22 no longer auto-copies static/ — stage vditor's runtime
	# assets into the dist output so /vditor/* URLs resolve.
	rm -rf rust/frontend/dist/vditor
	cp -R rust/frontend/static/vditor rust/frontend/dist/vditor
	# the same for the import conversion libraries (/mammoth /turndown /pdfjs)
	rm -rf rust/frontend/dist/mammoth rust/frontend/dist/turndown rust/frontend/dist/pdfjs
	cp -R rust/frontend/static/mammoth rust/frontend/dist/mammoth
	cp -R rust/frontend/static/turndown rust/frontend/dist/turndown
	cp -R rust/frontend/static/pdfjs rust/frontend/dist/pdfjs
	# The compressed IPADIC dictionary for the search tokenizer is fetched by
	# the worker through the ASSETS binding at runtime (embedding it would
	# blow the Workers script size limit).
	mkdir -p rust/frontend/dist/dict
	cp -R rust/frontend/static/dict/. rust/frontend/dist/dict/

rust-worker:
	cd rust/worker && worker-build --release

# ---------------------------------------------------------------------------
# Go pipeline (kept for reference / rollback — see "Rollback" in README.md).
# Requires temporarily pointing wrangler.toml back at ./build/worker.mjs and
# the ./frontend/dist assets directory.
# ---------------------------------------------------------------------------

build-go: frontend
	go run github.com/syumai/workers-go/cmd/workers-assets-gen -mode=go
	GOOS=js GOARCH=wasm go build -trimpath -o ./build/app.wasm .

frontend:
	npm --prefix frontend ci
	rm -rf frontend/public/vditor
	mkdir -p frontend/public/vditor
	cp -R frontend/node_modules/vditor/dist frontend/public/vditor/dist
	npm --prefix frontend run build

dev:
	npx wrangler dev

deploy:
	npx wrangler deploy

# One-time database setup: create the D1 database, then paste the printed
# database_id into wrangler.toml and run `make init-db`.
create-db:
	npx wrangler d1 create urlnote

init-db:
	npx wrangler d1 execute urlnote --remote --file=./schema.sql

init-db-local:
	npx wrangler d1 execute urlnote --local --file=./schema.sql

clean:
	rm -rf build rust/target rust/worker/build rust/frontend/dist rust/frontend/node_modules rust/frontend/static/vditor rust/frontend/static/mammoth rust/frontend/static/turndown rust/frontend/static/pdfjs frontend/node_modules frontend/dist
