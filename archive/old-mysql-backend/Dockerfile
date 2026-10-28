# Build stage
FROM golang:1.26-alpine AS builder
WORKDIR /app
ENV GO111MODULE=on \
    GOPROXY=https://goproxy.cn,direct
COPY go.mod go.sum ./
RUN go mod download
COPY . .
RUN CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o urlnote .

# Run stage
FROM alpine:3.21
RUN apk add --no-cache ca-certificates tzdata
WORKDIR /app
COPY --from=builder /app/urlnote .
EXPOSE 3007
ENV PORT=3007 \
    DB_HOST=mysql \
    DB_PORT=3306 \
    DB_USER=urlnote \
    DB_PASSWORD=urlnote \
    DB_NAME=urlnote
CMD ["./urlnote"]
