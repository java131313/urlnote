# Build stage
FROM golang:1.22-alpine AS builder
WORKDIR /app
COPY go.mod ./
RUN go mod download
COPY . .
RUN CGO_ENABLED=0 go build -o urlnote .

# Run stage
FROM alpine:3.20
WORKDIR /app
COPY --from=builder /app/urlnote .
RUN mkdir -p /app/data
VOLUME /app/data
EXPOSE 3007
ENV PORT=3007
CMD ["./urlnote"]
