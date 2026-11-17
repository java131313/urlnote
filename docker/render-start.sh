#!/bin/bash
# Render 免费档单 Pod 启动:FerretDB(127.0.0.1:27017)→ urlnote-server。
# 数据在 Neon PG,FERRETDB_POSTGRESQL_URL 指向 Neon 的 direct(非池化)连接串。
set -u

: "${FERRETDB_POSTGRESQL_URL:?请设置 FERRETDB_POSTGRESQL_URL(Neon direct connection string)}"

/usr/local/bin/ferretdb \
  --listen-addr=127.0.0.1:27017 \
  --postgresql-url="$FERRETDB_POSTGRESQL_URL" \
  --telemetry=disable &
FDB_PID=$!
trap "kill $FDB_PID 2>/dev/null" EXIT

# 等 FerretDB 监听就绪(mongodb driver 自带重试,这里只是避免噪音)
for _ in $(seq 1 60); do
  if (echo > /dev/tcp/127.0.0.1/27017) 2>/dev/null; then break; fi
  sleep 0.5
done

export MONGODB_URI="${MONGODB_URI:-mongodb://127.0.0.1:27017}"
exec /app/urlnote-server
