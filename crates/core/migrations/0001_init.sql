-- Querora app database schema. Secrets NEVER live here (Keychain only).
CREATE TABLE IF NOT EXISTS sources (
  id           TEXT PRIMARY KEY,
  name         TEXT NOT NULL,
  kind         TEXT NOT NULL,          -- postgres | mysql | sqlite | duckdb
  params_json  TEXT NOT NULL DEFAULT '{}', -- non-secret connection params only
  created_at   TEXT NOT NULL,
  updated_at   TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS semantic_versions (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  source_id    TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
  version      TEXT NOT NULL,          -- set on publish; empty for drafts
  status       TEXT NOT NULL CHECK (status IN ('draft', 'published')),
  graph_json   TEXT NOT NULL,
  created_at   TEXT NOT NULL,
  published_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_semantic_source_status
  ON semantic_versions (source_id, status);

CREATE TABLE IF NOT EXISTS chat_sessions (
  id               TEXT PRIMARY KEY,
  agent            TEXT NOT NULL,      -- claude | codex | pi | byok
  agent_session_id TEXT,               -- driver-native session id for resume
  agent_version    TEXT,
  title            TEXT NOT NULL DEFAULT '',
  created_at       TEXT NOT NULL,
  updated_at       TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS chat_messages (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id  TEXT NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
  role        TEXT NOT NULL,           -- user | agent | system | tool
  content_json TEXT NOT NULL,
  created_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_messages_session ON chat_messages (session_id);

CREATE TABLE IF NOT EXISTS audit_log (
  id      INTEGER PRIMARY KEY AUTOINCREMENT,
  ts      TEXT NOT NULL,
  actor   TEXT NOT NULL,               -- toolapi | driver:<agent> | dualmode
  tool    TEXT NOT NULL,
  summary TEXT NOT NULL DEFAULT ''
);
