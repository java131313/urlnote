import { Client } from "pg";

// 目标 PostgreSQL 建表语句（源 schema.sql + migrations/0001-0007 的对应移植；
// notes_fts 是 SQLite FTS5 虚拟表，无法也不需迁移）。全部 IF NOT EXISTS，
// 可重复执行。时间列保留 TEXT，与 D1 datetime('now') 的
// "YYYY-MM-DD HH:MM:SS" UTC 格式保持一致。
const PG_SCHEMA = `
CREATE TABLE IF NOT EXISTS notes (
  id         TEXT PRIMARY KEY,
  content    TEXT NOT NULL DEFAULT '',
  title      TEXT NOT NULL DEFAULT '',
  tags       TEXT NOT NULL DEFAULT '[]',
  author     TEXT NOT NULL DEFAULT '',
  font_size  INTEGER NOT NULL DEFAULT 16,
  color      TEXT NOT NULL DEFAULT '',
  updated_at TEXT NOT NULL DEFAULT (to_char(now() AT TIME ZONE 'utc', 'YYYY-MM-DD HH24:MI:SS'))
);
CREATE TABLE IF NOT EXISTS users (
  username      TEXT PRIMARY KEY,
  display_name  TEXT NOT NULL DEFAULT '',
  password_hash TEXT NOT NULL,
  created_at    TEXT NOT NULL DEFAULT (to_char(now() AT TIME ZONE 'utc', 'YYYY-MM-DD HH24:MI:SS'))
);
CREATE TABLE IF NOT EXISTS edit_lock (
  id         INTEGER PRIMARY KEY CHECK (id = 1),
  locked     INTEGER NOT NULL DEFAULT 0,
  updated_at TEXT NOT NULL DEFAULT (to_char(now() AT TIME ZONE 'utc', 'YYYY-MM-DD HH24:MI:SS'))
);
CREATE TABLE IF NOT EXISTS edit_requests (
  username   TEXT PRIMARY KEY,
  status     TEXT NOT NULL DEFAULT 'pending',
  message    TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL DEFAULT (to_char(now() AT TIME ZONE 'utc', 'YYYY-MM-DD HH24:MI:SS'))
);
CREATE TABLE IF NOT EXISTS sessions (
  token      TEXT PRIMARY KEY,
  username   TEXT NOT NULL,
  expires_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS presence (
  note_id    TEXT NOT NULL,
  username   TEXT NOT NULL,
  updated_at TEXT NOT NULL DEFAULT (to_char(now() AT TIME ZONE 'utc', 'YYYY-MM-DD HH24:MI:SS')),
  PRIMARY KEY (note_id, username)
);
CREATE TABLE IF NOT EXISTS notes_mark (
  id                TEXT PRIMARY KEY,
  content           TEXT NOT NULL,
  source_updated_at TEXT NOT NULL,
  updated_at        TEXT NOT NULL DEFAULT (to_char(now() AT TIME ZONE 'utc', 'YYYY-MM-DD HH24:MI:SS'))
);
`;

const TABLES = [
  "notes",
  "users",
  "edit_lock",
  "edit_requests",
  "sessions",
  "presence",
  "notes_mark",
];

// GET：返回目标 PostgreSQL 各表当前行数（只读，用于核对迁移进度）
async function status(request, env) {
  if (!env.MIGRATION_TOKEN) {
    return Response.json({ success: false, message: "Migration endpoint is not configured" }, { status: 503 });
  }
  if (request.headers.get("Authorization") !== `Bearer ${env.MIGRATION_TOKEN}`) {
    return Response.json({ success: false, message: "Unauthorized" }, { status: 401 });
  }

  const client = new Client({ connectionString: env.HYPERDRIVE.connectionString });
  try {
    await client.connect();
    const counts = {};
    for (const table of TABLES) {
      const result = await client.query(`SELECT count(*)::int AS c FROM "${table}"`);
      counts[table] = result.rows[0].c;
    }
    return Response.json({ success: true, counts });
  } catch (error) {
    console.error("Status check failed:", error);
    return Response.json({ success: false, message: "Status check failed; the target tables may not exist yet" }, { status: 500 });
  } finally {
    await client.end();
  }
}

export default {
  async fetch(request, env) {
    if (request.method === "GET") {
      return status(request, env);
    }
    if (request.method !== "POST") {
      return Response.json({ success: false, message: "POST required" }, { status: 405 });
    }

    if (!env.MIGRATION_TOKEN) {
      console.error("MIGRATION_TOKEN is not configured");
      return Response.json(
        { success: false, message: "Migration endpoint is not configured" },
        { status: 503 }
      );
    }

    if (request.headers.get("Authorization") !== `Bearer ${env.MIGRATION_TOKEN}`) {
      return Response.json({ success: false, message: "Unauthorized" }, { status: 401 });
    }

    // 步骤拆分：?step=schema 只建表；?step=data 只灌数据；不带参数两步都做
    const step = new URL(request.url).searchParams.get("step") ?? "all";
    if (!["all", "schema", "data"].includes(step)) {
      return Response.json(
        { success: false, message: "step must be one of: all, schema, data" },
        { status: 400 }
      );
    }

    const client = new Client({ connectionString: env.HYPERDRIVE.connectionString });
    try {
      await client.connect();

      if (step === "schema" || step === "all") {
        await client.query(PG_SCHEMA);
      }

      if (step === "schema") {
        return Response.json({
          success: true,
          message: "表结构同步完成（7 张表，IF NOT EXISTS，可重复执行）",
        });
      }

      const migrated = {};
      for (const table of TABLES) {
        const { results: rows } = await env.DB.prepare(`SELECT * FROM "${table}"`).all();
        const columns = rows.length > 0 ? Object.keys(rows[0]) : [];
        let inserted = 0;
        let skipped = 0;

        if (columns.length > 0) {
          const quotedColumns = columns.map((column) => `"${column.replaceAll('"', '""')}"`);
          const placeholders = columns.map((_, index) => `$${index + 1}`).join(", ");
          const query =
            `INSERT INTO "${table}" (${quotedColumns.join(", ")}) ` +
            `VALUES (${placeholders}) ON CONFLICT DO NOTHING`;

          for (const row of rows) {
            const values = columns.map((column) => row[column]);
            const result = await client.query(query, values);
            if (result.rowCount === 1) {
              inserted++;
            } else {
              skipped++;
            }
          }
        }

        migrated[table] = { sourceRows: rows.length, inserted, skipped };
      }

      return Response.json({
        success: true,
        message: "D1 → PostgreSQL 迁移完成",
        migrated,
      });
    } catch (error) {
      console.error("D1 to PostgreSQL migration failed:", error);
      return Response.json(
        { success: false, message: "Migration failed; check the worker logs" },
        { status: 500 }
      );
    } finally {
      await client.end();
    }
  },
};
