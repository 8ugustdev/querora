-- Cached introspection snapshots (per source) — enables drift reports.
CREATE TABLE IF NOT EXISTS catalog_cache (
  source_id   TEXT PRIMARY KEY,
  catalog_json TEXT NOT NULL,
  cached_at   TEXT NOT NULL
);
