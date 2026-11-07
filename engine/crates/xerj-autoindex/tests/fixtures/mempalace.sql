-- A small MemPalace sqlite_exact store (mempalace/backends/sqlite_exact.py schema at
-- MemPalace/mempalace 4f7745afbd9d0197c6475b4c7ac846d4932eaeda). DO_NOT_EXPORT marks
-- rows and columns the importer must never write: bookkeeping rows, closets, embeddings.
PRAGMA journal_mode = WAL;

CREATE TABLE meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
CREATE TABLE collections (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL UNIQUE,
  dimension INTEGER,
  created_at TEXT NOT NULL
);
CREATE TABLE documents (
  collection_id INTEGER NOT NULL,
  id TEXT NOT NULL,
  document TEXT NOT NULL,
  metadata_json TEXT NOT NULL,
  embedding BLOB NOT NULL,
  dim INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (collection_id, id),
  FOREIGN KEY(collection_id) REFERENCES collections(id) ON DELETE CASCADE
);

INSERT INTO meta VALUES ('schema_version', '1');
INSERT INTO collections VALUES (1, 'mempalace_drawers', 1, '2026-10-25T00:00:00.000000+00:00');
INSERT INTO collections VALUES (2, 'mempalace_closets', 1, '2026-10-25T00:00:00.000000+00:00');

-- A drawer mined from a project file.
INSERT INTO documents VALUES (
  1, 'drawer_watcher_design_0001',
  'We dropped polling because file events avoid repeated scans while the durable journal preserves recovery.',
  '{"wing":"watcher","room":"design","source_file":"/work/watcher/docs/decisions.md","chunk_index":0,"line_start":3,"line_end":4,"added_by":"miner","filed_at":"2026-10-25T00:00:01.000000+00:00"}',
  x'00', 1, '2026-10-25T00:00:01.000000+00:00', '2026-10-25T00:00:01.000000+00:00'
);
-- A drawer mined from a conversation, holding personal data the redactor must remove.
INSERT INTO documents VALUES (
  1, 'drawer_watcher_chat_0002',
  '> Why did we drop polling? Contact owner@example.com or 202-555-0147.
File events, with the journal for recovery.',
  '{"wing":"watcher","room":"chat","hall":"hall_facts","source_file":"/home/me/.claude/projects/watcher/session.jsonl","chunk_index":0,"added_by":"convo_miner","authored_at":"2026-10-20T09:30:00+00:00","filed_at":"2026-10-25T00:00:02.000000+00:00","entities":"DO_NOT_EXPORT_ENTITIES","ingest_mode":"convos"}',
  x'00', 1, '2026-10-25T00:00:02.000000+00:00', '2026-10-25T00:00:02.000000+00:00'
);
-- A drawer MemPalace stored escaped because its text held a NUL and a stray byte.
INSERT INTO documents VALUES (
  1, 'drawer_watcher_logs_0003',
  char(1) || 'mpesc:Build log one\0two \\ path \uDC80 end',
  '{"wing":"watcher","room":"logs","source_file":' || json_quote(char(1) || 'mpesc:/work/watcher/build\0.log') || ',"filed_at":"2026-10-25T00:00:03.000000+00:00","_mempalace_escaped":1}',
  x'00', 1, '2026-10-25T00:00:03.000000+00:00', '2026-10-25T00:00:03.000000+00:00'
);
-- Bookkeeping rows that share the drawers collection.
INSERT INTO documents VALUES (
  1, 'registry_watcher_0004', 'DO_NOT_EXPORT_REGISTRY',
  '{"wing":"watcher","room":"_registry","source_file":"/home/me/.claude/projects/watcher/session.jsonl","ingest_mode":"registry"}',
  x'00', 1, '2026-10-25T00:00:04.000000+00:00', '2026-10-25T00:00:04.000000+00:00'
);
INSERT INTO documents VALUES (
  1, 'sentinel_0005', 'DO_NOT_EXPORT_SENTINEL', '{"is_sentinel":true}',
  x'00', 1, '2026-10-25T00:00:05.000000+00:00', '2026-10-25T00:00:05.000000+00:00'
);
-- A closet: MemPalace's pointer index over drawers, not a memory of its own.
INSERT INTO documents VALUES (
  2, 'closet_watcher_0006', 'DO_NOT_EXPORT_CLOSET', '{"wing":"watcher","room":"design"}',
  x'00', 1, '2026-10-25T00:00:06.000000+00:00', '2026-10-25T00:00:06.000000+00:00'
);
