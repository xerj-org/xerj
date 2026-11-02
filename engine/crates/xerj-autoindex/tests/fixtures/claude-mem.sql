-- Synthetic claude-mem store: the four tables `xerj sessions import --format claude-mem`
-- reads, in the column shape of thedotmack/claude-mem at 999a0c40 (SessionStore.ts).
-- Columns the importer never exports carry DO_NOT_EXPORT markers.
PRAGMA journal_mode = WAL;

CREATE TABLE sdk_sessions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  content_session_id TEXT NOT NULL,
  memory_session_id TEXT UNIQUE,
  project TEXT NOT NULL,
  platform_source TEXT NOT NULL DEFAULT 'claude',
  user_prompt TEXT,
  started_at TEXT NOT NULL,
  started_at_epoch INTEGER NOT NULL,
  status TEXT NOT NULL DEFAULT 'active',
  custom_title TEXT,
  cwd TEXT
);

CREATE TABLE observations (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  memory_session_id TEXT NOT NULL,
  project TEXT NOT NULL,
  text TEXT,
  type TEXT NOT NULL,
  created_at TEXT NOT NULL,
  created_at_epoch INTEGER NOT NULL,
  prompt_number INTEGER,
  title TEXT,
  subtitle TEXT,
  facts TEXT,
  narrative TEXT,
  concepts TEXT,
  files_read TEXT,
  files_modified TEXT,
  discovery_tokens INTEGER DEFAULT 0,
  content_hash TEXT,
  merged_into_project TEXT,
  metadata TEXT
);

CREATE TABLE session_summaries (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  memory_session_id TEXT UNIQUE NOT NULL,
  project TEXT NOT NULL,
  request TEXT,
  investigated TEXT,
  learned TEXT,
  completed TEXT,
  next_steps TEXT,
  files_read TEXT,
  files_edited TEXT,
  notes TEXT,
  created_at TEXT NOT NULL,
  created_at_epoch INTEGER NOT NULL,
  prompt_number INTEGER,
  discovery_tokens INTEGER DEFAULT 0
);

CREATE TABLE user_prompts (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  session_db_id INTEGER,
  content_session_id TEXT NOT NULL,
  prompt_number INTEGER NOT NULL,
  prompt_text TEXT NOT NULL,
  created_at TEXT NOT NULL,
  created_at_epoch INTEGER NOT NULL
);

INSERT INTO sdk_sessions VALUES (
  1, '550e8400-e29b-41d4-a716-446655440000', 'mem-0001', 'watcher', 'claude',
  'DO_NOT_EXPORT_SESSION_PROMPT', '2026-10-25T00:00:00.000Z', 1792886400000,
  'completed', 'DO_NOT_EXPORT_TITLE', '/work/watcher'
);

INSERT INTO user_prompts VALUES (
  1, 1, '550e8400-e29b-41d4-a716-446655440000', 1,
  'Why did we drop polling? Contact owner@example.com or 202-555-0147.',
  '2026-10-25T00:00:00.000Z', 1792886400000
);

INSERT INTO observations VALUES (
  1, 'mem-0001', 'watcher', NULL, 'decision', '2026-10-25T00:00:01.000Z', 1792886401000, 1,
  'Dropped polling for file events', 'Watcher design',
  '["File events avoid repeated scans","The durable journal preserves recovery"]',
  'We dropped polling because file events avoid repeated scans while the durable journal preserves recovery.',
  '["watcher","recovery"]', '["src/poll.rs"]', '["src/watch.rs"]', 1200,
  'DO_NOT_EXPORT_HASH', NULL, '{"note":"DO_NOT_EXPORT_METADATA"}'
);

INSERT INTO session_summaries VALUES (
  1, 'mem-0001', 'watcher', 'Explain why polling was dropped',
  'Compared polling with file events', 'Event loss is recoverable from the journal',
  'Switched the watcher to file events', 'Measure event latency',
  '["src/poll.rs"]', '["src/watch.rs"]', NULL,
  '2026-10-25T00:00:02.000Z', 1792886402000, 1, 900
);
