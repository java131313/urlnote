.PHONY: build dev deploy create-db init-db init-db-local clean

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
	rm -rf rust/frontend/static/ffmpeg
	mkdir -p rust/frontend/static/ffmpeg/ffmpeg rust/frontend/static/ffmpeg/core
	cp -R rust/frontend/node_modules/@ffmpeg/ffmpeg/dist/esm/. rust/frontend/static/ffmpeg/ffmpeg/
	cp rust/frontend/node_modules/@ffmpeg/core/dist/esm/ffmpeg-core.js rust/frontend/node_modules/@ffmpeg/core/dist/esm/ffmpeg-core.wasm rust/frontend/static/ffmpeg/core/
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
	# the same for ffmpeg.wasm(/media 播放器核心)与 import 转换库
	rm -rf rust/frontend/dist/ffmpeg
	cp -R rust/frontend/static/ffmpeg rust/frontend/dist/ffmpeg
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
	# ffmpeg 核心(31MB)超过 CF 静态资产单文件上限:wrangler 上传时排除,
	# CF 部署的媒体页回退从 GitHub Pages 加载核心(rust/frontend/src/media.rs)
	printf 'ffmpeg/\n' > rust/frontend/dist/.assetsignore

rust-worker:
	cd rust/worker && worker-build --release

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
