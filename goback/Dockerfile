# Multi-stage build for the urlnote Go rewrite (backend + WASM frontend).
#
#   stage 1 (wasm)    — compile web/ to app.wasm (pure Go, no cgo)
#   stage 2 (server)  — compile cmd/server with CGO (gojieba embeds cppjieba)
#   stage 3 (runtime) — debian slim + the binary + static assets
#
# Runtime env (see cmd/server/main.go):
#   DATABASE_URL   postgres://user:pass@host:5432/urlnote   (required)
#   ADDR           listen address, default :8080
#   IMAGE_DIR      uploaded-image storage, default /data/images in the image
#   STATIC_DIR     static assets dir, baked to /app/static
#   CORS_ORIGINS   comma-separated origins allowed to call the API
#   SWEEP_INTERVAL notes_mark consistency sweep cadence, default 20m

# ---- stage 1: WASM frontend ----
FROM golang:1.26-bookworm AS wasm
WORKDIR /src
COPY go.mod go.sum ./
COPY shared ./shared
COPY web ./web
RUN GOOS=js GOARCH=wasm go build -trimpath -o /out/app.wasm ./web

# ---- stage 2: server (cgo for gojieba) ----
FROM golang:1.26-bookworm AS server
WORKDIR /src
COPY go.mod go.sum ./
RUN go mod download
COPY . .
COPY --from=wasm /out/app.wasm ./static/app.wasm
RUN CGO_ENABLED=1 go build -trimpath -ldflags "-s -w" -o /out/urlnote-server ./cmd/server

# ---- stage 3: runtime ----
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=server /out/urlnote-server ./urlnote-server
COPY static ./static
ENV ADDR=:8080 \
    STATIC_DIR=/app/static \
    IMAGE_DIR=/data/images
VOLUME ["/data"]
EXPOSE 8080
ENTRYPOINT ["/app/urlnote-server"]
