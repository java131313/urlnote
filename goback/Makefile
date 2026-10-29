# urlnote goback — Go rewrite (backend + WASM frontend).
#
# Common targets:
#   make build    — wasm frontend + server binary into ./bin
#   make dev      — build + run against $DATABASE_URL (local dev)
#   make test     — unit tests (token/analyze)
#   make docker   — build the all-in-one image
#   make run      — docker run against $DATABASE_URL (host.docker.internal
#                   reaches the host's postgres on macOS/Windows)
#
# macOS note: this machine's CLT install keeps C++ headers only inside the
# SDK, so cgo (gojieba) needs the extra include flags below. Linux/Docker
# builds don't need them.

unexport GOROOT

# Host-side cgo flags for macOS (no-op on Linux).
SDK := /Library/Developer/CommandLineTools/SDKs/MacOSX.sdk
ifneq (,$(wildcard $(SDK)/usr/include/c++/v1/algorithm))
export CGO_CXXFLAGS := -stdlib=libc++ -nostdinc++ -I$(SDK)/usr/include/c++/v1
endif

BIN := bin
SERVER := $(BIN)/urlnote-server
WASM := static/app.wasm

.PHONY: build wasm server dev test docker run clean

build: wasm server

wasm:
	GOOS=js GOARCH=wasm go build -trimpath -o $(WASM) ./web

server:
	CGO_ENABLED=1 go build -trimpath -o $(SERVER) ./cmd/server

dev: build
	DATABASE_URL=$${DATABASE_URL:?set DATABASE_URL, e.g. postgres://user@localhost:5432/urlnote_test?sslmode=disable} \
	ADDR=$${ADDR:-:8080} IMAGE_DIR=$${IMAGE_DIR:-./data/images} $(SERVER)

test:
	go test ./internal/token/ ./internal/analyze/

docker:
	docker build -t urlnote-goback .

run: docker
	docker rm -f urlnote-goback 2>/dev/null; \
	docker run -d --name urlnote-goback \
	  -p 8080:8080 \
	  -e DATABASE_URL=$${DATABASE_URL:?set DATABASE_URL} \
	  -v urlnote-data:/data \
	  urlnote-goback

clean:
	rm -rf $(BIN)
