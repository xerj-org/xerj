# Import an archived agent transcript for decision retrieval

`xerj sessions import` is the explicit preparation step for a single Claude Code
transcript, Codex rollout JSONL file, claude-mem store or MemPalace palace. It runs locally, calls no model or node, and exports text
turns through XERJ's existing native PII plugin. Review the export, then give its
folder to ordinary `xerj autoindex`. This covers four explicit formats for
[#1793](https://github.com/xerj-org/xerj/issues/1793) and
[#2874](https://github.com/xerj-org/xerj/issues/2874), not automatic transcript
discovery, a live conversation view or incremental sync.

## Try the synthetic example

From the repository root with a built binary and Python 3:

```sh
(cd engine && cargo build --locked --release -j 32 -p xerj-server)
python3 tools/session-import/smoke.py --binary engine/target/release/xerj
```

The example uses only the committed synthetic transcript, prepares a private
export, starts a private loopback node on unused ports, dry-runs autoindex,
indexes and queries the decision, repeats indexing to check idempotence, then
checks graceful restart. It removes its own temporary data at exit. It neither
contacts nor removes any existing node/index. The node runs in default lexical
mode; indexing explicitly uses `--no-semantic`. No semantic-quality claim is made.

## Import your own archived transcript

Choose a stopped/archived file explicitly; this command does not inspect your
home directory or discover other sessions. Use a private destination directory
whose parent you control. The output file must not exist, even if empty.

```sh
set -eu
SESSION_EXPORT=$(mktemp -d)
xerj sessions import --format claude-code \
  --input /path/to/archived-session.jsonl \
  --output "$SESSION_EXPORT/records.jsonl"
```

Success prints a JSON summary containing the schema version, exported-record
count, physical source-line count, skipped-record count, skipped-block count,
and SHA-256 of the source bytes consumed and the exact exported bytes. Blank
lines count as source lines but not skipped records. Skipped blocks count the
non-text blocks inspected in non-meta user/assistant messages. Skipped records
count other top-level records, `isMeta` messages and messages with no text.

The redactor handles the existing email, IP, credit-card, US SSN and phone
patterns. It does not remove every credential, person name, repository path or
private fact. Check the exported JSONL before indexing it. Raw tool payloads,
tool results, thinking and arbitrary message metadata are excluded, including
fields the importer does not recognize. The importer does not change the input.

After review, against your running node:

```sh
set -eu
xerj autoindex "$SESSION_EXPORT" --prefix session-history --dry-run
# Check the estimate and prefix ownership before indexing.
xerj autoindex "$SESSION_EXPORT" --prefix session-history
xerj autoindex map --prefix session-history
xerj search --prefix session-history "why did we drop polling"
```

Only put prepared exports in that folder. Ordinary `autoindex` still accepts
raw JSONL; pointing it at the original transcript folder does **not** activate
this preparation or redaction. Use a prefix you own; follow the normal
autoindex estimate/approval contract for large exports.

## Claude Code record and source contract

One output line is one text-bearing `user` or `assistant` event. `message.content`
may be a string or an array of `text`/recognized non-text blocks. Text blocks
retain their order. Each output object has the same key set:

| Fields | Meaning |
|---|---|
| `session_schema_version`, `session_format` | Export schema 1, `claude-code` |
| `id`, `session_id`, `message_id` | Canonical validated UUIDs; ID is session/message UUID |
| `parent_id` | Explicit `parentUuid`, or null; it may reference a skipped event |
| `role`, `title`, `body` | Event role and redacted text; title is the first text line, capped at 120 characters |
| `source_file`, `source_line`, `source_byte_offset` | Redacted original basename, 1-based physical line, 0-based byte offset |
| `files` | Redacted `file_path` values explicitly requested by Read/Write/Edit/MultiEdit blocks in this event |
| `cwd`, `git_branch`, `timestamp` | Explicit metadata or null; timestamp must be RFC 3339 |
| `is_sidechain`, `is_summary` | Explicit branch/compaction flags, default false |

File paths are references in a tool request, not proof that a change succeeded.
No files are opened through these references, and no commit is inferred from
prose, shell commands or branch names. File references in tool-only events are
not exported in this text-only version. Autoindex citations still identify the
prepared file/record; the `source_*` fields locate the original event, whose
byte offset is meaningful only for that archived source digest. Redaction may
change the original filename/path spelling. Query the stored record with
`_source: true` to read all source metadata.

The importer preserves all text-bearing branches in file order. It does not
reconstruct the active conversation, merge forks or claim that a discarded
alternative is current. Compaction summaries are marked, not treated as original
turns. Non-text attachments are not OCRed, and tool results are not searched.

## Refusals, reruns and limits

Exit 0 is a complete export; 2 is invalid CLI usage; 1 is an import or
publication/reporting error. If stdout fails after publication, stderr explicitly
says the export was published: inspect and reuse that complete file rather than
expecting a second import to overwrite it. Diagnostics name the line and schema problem, without
echoing the source payload. Invalid JSON (including a truncated tail), UUIDs,
roles, timestamps or flags, unsupported content blocks, mixed session IDs,
repeated message UUIDs and a changed source refuse the whole export. There is
no fallback to unredacted generic JSONL/text. Metadata-only input also refuses.

Input and output are capped at 512 MiB each, source lines at 1 MiB including
newline, collected turn text at 256 KiB and message UUIDs at 100,000. These are
admission limits, not a measured peak-memory claim. Parsing is line by line.
The temporary output contains only redacted, allowlisted records, is hidden
and is removed on ordinary failure. It is synced before no-clobber publication;
the tempfile implementation may leave an extra hidden hard link on platforms
that need a link/unlink fallback or after an interruption. This is not a
transactional directory-durability guarantee. On Unix the new export has mode 0600. No existing output,
including a concurrent writer's file or a symlink, is replaced.

An unchanged import rerun refuses the existing output; reuse that snapshot for
unchanged autoindex reruns. For an updated source or parser version, prepare a
new export folder and index it under a new prefix/state directory. This version
does not refresh or remove a previous snapshot. Retain the old snapshot/index
until the new one has been verified, then use the normal ownership-aware
autoindex remove workflow. A killed process can leave a hidden sanitized temp
file; it is not a complete export. Use a new destination after interruption.

## Format evidence and verification boundary

Claude Code documents its on-disk JSONL as an internal format that can change:
[session storage](https://code.claude.com/docs/en/sessions#where-transcripts-are-stored).
This adapter supports the explicit shapes above, not every client version,
`stream-json`, rendered `/export` text or Cursor logs. Codex uses the separate
explicit adapter below.

Reference retrieved before implementation: Anthropic's MIT-licensed SDK at
`0e89b298e771259378a6cd3734e00d323dff5703`,
[`sessions.py:897–929`](https://github.com/anthropics/claude-agent-sdk-python/blob/0e89b298e771259378a6cd3734e00d323dff5703/src/claude_agent_sdk/_internal/sessions.py#L897-L929)
parses typed UUID-bearing events, and
[`sessions.py:1248–1285`](https://github.com/anthropics/claude-agent-sdk-python/blob/0e89b298e771259378a6cd3734e00d323dff5703/src/claude_agent_sdk/_internal/sessions.py#L1248-L1285)
distinguishes visible messages and session identities. Approach only; no code
copied. XERJ intentionally refuses malformed input instead of silently skipping
it, and exports branch flags rather than choosing the SDK's active chain.

All checked-in fixtures are synthetic. No real personal transcript or real
Claude client release is claimed validated by those fixtures. Unit and CLI
tests cover redaction, provenance, filtering, size limits, malformed tails,
unsupported shapes, no-clobber publication and recovery.

## Codex rollout adapter

Select an archived, uncompressed rollout JSONL file explicitly. The command
does not scan `~/.codex`, read the history database, resolve other rollouts or
run Codex. Use the same private-output and review-before-indexing steps above:

```sh
set -eu
SESSION_EXPORT=$(mktemp -d)
xerj sessions import --format codex \
  --input /path/to/archived-rollout.jsonl \
  --output "$SESSION_EXPORT/records.jsonl"
```

The file must start (ignoring blank lines) with one `session_meta` containing a
UUID `payload.id`. Only `response_item` / `payload.type=message` with role `user`
or `assistant` exports text. Ordered `input_text` and `output_text` blocks are
joined with newlines; `input_image` and `input_audio` increment skipped blocks.
Unknown message roles, phases, block types, response item types or top-level
record types refuse the whole snapshot. Channel-bearing messages also refuse;
reasoning is not admitted as message text. All envelopes need an RFC 3339
timestamp and object payload. This is a deliberately bounded subset of an
internal format, not a claim of support for every Codex version.

`event_msg` records are skipped, including the duplicate user/assistant text
they can contain. System/developer messages, reasoning, tool calls and results,
agent-to-agent messages, compaction payloads, world-state and usage/security
records are not exported. The summary uses the same definitions above: skipped
blocks count non-text attachments in admitted user/assistant messages; skipped
records count every nonblank record not exported. No raw metadata, encrypted
content, provider message ID, annotations, tool argument, image or audio URL is
included. A user-role text block can itself contain injected context or private
facts; this adapter does not distinguish that text from a human prompt. Review
the export; the native five-pattern PII redactor is not a general secret filter.

Codex output keeps schema 1 and the shared text/source fields, with these
format-specific meanings:

| Field | Codex meaning |
|---|---|
| `session_format`, `session_id` | `codex`, canonical thread UUID from `session_meta.payload.id` (not a root-session or provider message ID) |
| `message_id` | `line-<physical-line>-<SHA256 of original line bytes, including newline if present>`; an archive event locator, not a Codex message ID |
| `id` | `codex/<session_id>/<message_id>`; identical archived input repeats identically, repeated equal messages at different lines remain distinct |
| `parent_id`, `is_sidechain` | null: no parent chain or branch status inferred |
| `files` | Empty array; shell commands, patches and prose are not parsed into file/commit associations |
| `cwd`, `git_branch` | Redacted session `cwd` and `git.branch`; each `turn_context` replaces the cwd (missing/null becomes unknown). Branch is a session-creation snapshot, not a per-turn observation |
| `message_phase` | Explicit `commentary`, `partial_answer`, `final_answer`, or null when absent; this additional field is Codex-only |
| `is_summary` | false; compaction summaries and replacement histories are omitted |
| `timestamp`, `source_*` | Envelope timestamp and original basename/physical line/byte offset |

The exporter preserves physical message order, including text before a rollback
or compaction; it does not reconstruct the current model context or active
conversation. A non-null `history_base` refuses because a single file cannot
resolve an external inherited prefix. Fork/subagent metadata is not followed,
and no complete-conversation claim is made for an individual file. Newer
`retained_context`, inter-agent and realtime record envelopes are unsupported.
The `codex exec --json` event stream, `history.jsonl` prompt history, compressed
rollouts and app-server JSON-RPC are separate formats and are unsupported.

All original size limits, source-change detection, sanitized staging, no-clobber
publication, exit codes and new-folder/prefix recovery rules apply. The 100,000
message limit counts user/assistant response messages (including attachment-only
messages); there is no invented UUID requirement or deduplication by provider ID.
Existing Claude Code exports retain their previous field meanings and key set;
no existing index or export is upgraded or rewritten.

Reference retrieved before implementation: OpenAI Codex, Apache-2.0, pinned at
`970111f6cb62cc02fe36a82ea7d71b37a2889c97`:

- [`recorder.rs:2127–2160`](https://github.com/openai/codex/blob/970111f6cb62cc02fe36a82ea7d71b37a2889c97/codex-rs/rollout/src/recorder.rs#L2127-L2160): timestamped flattened rollout envelope.
- [`rollout_payload.rs:30–73`](https://github.com/openai/codex/blob/970111f6cb62cc02fe36a82ea7d71b37a2889c97/codex-rs/history/src/rollout_payload.rs#L30-L73): tagged payloads distinguish responses, events, context and compaction.
- [`protocol.rs:3279–3356`](https://github.com/openai/codex/blob/970111f6cb62cc02fe36a82ea7d71b37a2889c97/codex-rs/protocol/src/protocol.rs#L3279-L3356): thread identity, cwd, forks and inherited history.
- [`models.rs:883–915`](https://github.com/openai/codex/blob/970111f6cb62cc02fe36a82ea7d71b37a2889c97/codex-rs/protocol/src/models.rs#L883-L915) and [`1052–1083`](https://github.com/openai/codex/blob/970111f6cb62cc02fe36a82ea7d71b37a2889c97/codex-rs/protocol/src/models.rs#L1052-L1083): text/attachment blocks and optional response IDs/phases.

These are format evidence, not copied implementation. The official
[non-interactive guide](https://developers.openai.com/codex/noninteractive)
documents `exec --json` events; that stream is not the disk rollout adapter.
Fixtures are synthetic, not personal transcripts or a live-client run.
Run the Codex fixture through the same private-node lifecycle check:

```sh
python3 tools/session-import/smoke.py --binary engine/target/release/xerj --format codex
```

## claude-mem store adapter

[claude-mem](https://github.com/thedotmack/claude-mem) is a Claude Code plugin
that has a model condense each session into observations and summaries, kept in
one SQLite file. Pass that file explicitly; the command does not look for it,
start or stop the claude-mem worker, or change the store's contents:

```sh
set -eu
SESSION_EXPORT=$(mktemp -d)
xerj sessions import --format claude-mem \
  --input ~/.claude-mem/claude-mem.db \
  --output "$SESSION_EXPORT/records.jsonl"
```

The store is opened read-only, not immutable, inside one read transaction, so
rows a running worker still holds in `claude-mem.db-wal` are included and all
three tables come from the same snapshot. SQLite may create the store's
`-wal` and `-shm` side files to read it, and needs the `-shm` file or a
writable folder to read a WAL database; on a read-only copy without either,
copy the `.db`, `-wal` and `-shm` files together. The 512 MiB input cap counts
the `.db` and `-wal` files together. A file with no `observations` table
refuses as not a claude-mem store. A store from before claude-mem's session ID
rename (its `sdk_session_id` columns) refuses with a request to start
claude-mem once, which migrates it.

Three tables export, each ordered by `created_at_epoch` then row id:

| Table | `role` | `body` |
|---|---|---|
| `user_prompts` | `user` | `prompt_text`, what was typed |
| `observations` | `observation` | `title`, `subtitle`, `narrative`, `text`, then each of `facts` as a `- ` line |
| `session_summaries` | `summary` | `Request:`, `Investigated:`, `Learned:`, `Completed:`, `Next steps:`, `Notes:` lines |

Observations and summaries are what claude-mem's model wrote about a session,
not what was said in it: they carry `is_summary: true`, and a search hit on one
is a model's account to check against the code, not a quote. A row whose text
columns are all empty counts as a skipped record. Session metadata (the stored
first prompt and custom title), content hashes, token counts, the `metadata`
column, `pending_messages` and the Chroma vector store are not exported.

claude-mem output keeps schema 1 and the shared text/source fields, with these
format-specific meanings:

| Field | claude-mem meaning |
|---|---|
| `session_format`, `session_id` | `claude-mem`; the session ID claude-mem recorded for that row's platform (`content_session_id`, a UUID for Claude Code), joined through `sdk_sessions` on `memory_session_id` for observations and summaries. A non-UUID value is kept as redacted text; no session row gives null |
| `id`, `message_id` | `claude-mem/<role>/<row id>` and `<role>/<row id>`: stable for a row across exports of the same store |
| `parent_id` | null |
| `files` | `files_modified` (summaries: `files_edited`) then `files_read`, deduplicated, as claude-mem recorded them |
| `project`, `memory_session_id` | claude-mem's project name and its own session key, as redacted text |
| `observation_type`, `concepts` | Observations only: claude-mem's type (`decision`, `bugfix` and so on) and concept tags, as redacted text |
| `prompt_number` | The prompt within the session the row belongs to, or null |
| `timestamp` | `created_at_epoch` as RFC 3339 UTC with milliseconds |
| `source_file`, `source_table`, `source_line` | Store basename, table, and SQLite row id (there is no line or byte offset in a database) |

`source_lines` in the summary counts rows read and `source_sha256` hashes those
rows' selected columns in export order, so two exports of an unchanged store
match. Columns later claude-mem migrations added are read as null on an older
store; a missing required column, a value that is not text or an integer, or a
`facts`, `concepts` or `files_*` value that is not a JSON string array refuses
the whole export. More than 100,000 rows refuses rather than truncating. The
redaction, size limits, staging, no-clobber publication, exit codes and
new-folder/prefix rerun rules above apply unchanged.

Reference retrieved before implementation: claude-mem, Apache-2.0, pinned at
`999a0c4010a12befecb1686e695126c04a4faa6e`:

- [`SessionStore.ts:1273-1325`](https://github.com/thedotmack/claude-mem/blob/999a0c4010a12befecb1686e695126c04a4faa6e/src/services/sqlite/SessionStore.ts#L1273-L1325): base `sdk_sessions`, `observations` and `session_summaries` tables and their session keys.
- [`SessionStore.ts:1697-1712`](https://github.com/thedotmack/claude-mem/blob/999a0c4010a12befecb1686e695126c04a4faa6e/src/services/sqlite/SessionStore.ts#L1697-L1712): `user_prompts`, keyed by `content_session_id` and `session_db_id`, with no project column.
- [`SessionStore.ts:1812-1821`](https://github.com/thedotmack/claude-mem/blob/999a0c4010a12befecb1686e695126c04a4faa6e/src/services/sqlite/SessionStore.ts#L1812-L1821): migration 17, which renamed `sdk_session_id` to `memory_session_id` and `claude_session_id` to `content_session_id`.
- [`SessionStore.ts:4166-4180`](https://github.com/thedotmack/claude-mem/blob/999a0c4010a12befecb1686e695126c04a4faa6e/src/services/sqlite/SessionStore.ts#L4166-L4180): observation fields, with `facts`, `concepts` and `files_*` stored as `JSON.stringify` arrays.

These are format evidence, not copied implementation. The fixture
(`engine/crates/xerj-autoindex/tests/fixtures/claude-mem.sql`) is a synthetic
store in that column shape, not a live claude-mem install. Run it through the
private-node lifecycle check:

```sh
python3 tools/session-import/smoke.py --binary engine/target/release/xerj --format claude-mem
```

## MemPalace palace adapter

[MemPalace](https://github.com/MemPalace/mempalace) files conversation and
project text verbatim as drawers, organised into wings, rooms and halls. Pass
the palace directory (MemPalace's default is `~/.mempalace/palace`) or its
`sqlite_exact.sqlite3` file explicitly; the command does not look for it or
change it:

```sh
set -eu
SESSION_EXPORT=$(mktemp -d)
xerj sessions import --format mempalace \
  --input ~/.mempalace/palace \
  --output "$SESSION_EXPORT/records.jsonl"
```

Only MemPalace's `sqlite_exact` backend is read. A palace on the default Chroma
backend (a `chroma.sqlite3` and no `sqlite_exact.sqlite3`) refuses with that
reason rather than exporting nothing, and a palace holding both stores refuses,
as MemPalace itself does (pass the store file to read it anyway). The store is opened the way MemPalace's
own read-only reader opens it: with both `-wal` and `-shm` present it is opened
read-only so a live writer's uncheckpointed drawers are included; with neither
it is opened immutable so no side files are created; with only one it refuses.
All drawers come from one read transaction. An immutable open trusts the file
not to change, so if the store or its side files change while it is read (a
writer started), the import refuses and publishes nothing; rerun when the writer
is idle. The 512 MiB input cap counts the store and its `-wal` file together.

Only the `mempalace_drawers` collection exports, ordered by `created_at` then
drawer ID. Closets (MemPalace's pointer index over drawers) and other
collections are not read, and registry and sentinel rows, which share the
drawers collection as bookkeeping, count as skipped records. A drawer MemPalace
stored escaped (flagged `_mempalace_escaped`) is decoded first; a NUL in it
becomes a space and an unpaired surrogate U+FFFD, as MemPalace's own search
index and MCP output render them. Embeddings, entity lists and other metadata
are not exported.

MemPalace output keeps schema 1 and the shared text/source fields, with these
format-specific meanings:

| Field | MemPalace meaning |
|---|---|
| `session_format`, `role`, `is_summary` | `mempalace`, `drawer`, `false`: a drawer is mined text, not a model's condensation |
| `session_id`, `parent_id` | null: drawers are keyed by palace location, not by agent session |
| `id`, `message_id` | `mempalace/drawer/<key>` and `drawer/<key>`, where `<key>` is the first 32 hex digits of the SHA-256 of MemPalace's drawer ID, so a `mempalace compact` that renumbers rows keeps the IDs |
| `drawer_id` | MemPalace's drawer ID, as redacted text |
| `wing`, `room`, `hall`, `added_by`, `content_date` | Drawer metadata, as redacted text when present |
| `origin_file` | The file MemPalace mined the drawer from, as redacted text |
| `files` | `origin_file` for a project drawer; empty for a conversation drawer (`ingest_mode: convos`), whose origin is a transcript |
| `chunk_index`, `line_start`, `line_end` | MemPalace's chunk position, integers or null |
| `timestamp` | `authored_at` (when a conversation was written), else `filed_at` (when it was mined), else the row's `created_at`, as RFC 3339 UTC with milliseconds; null when none is an RFC 3339 instant |
| `source_file`, `source_table`, `source_line` | Store basename, `documents`, and SQLite row id |

`source_lines` counts drawer-collection rows read, bookkeeping included, and
`source_sha256` hashes them in export order, so two exports of an unchanged
palace match. A missing `mempalace_drawers` collection, a `metadata_json` that
is not a JSON object or a non-text column refuses the whole export; more than
100,000 rows refuses rather than truncating. The redaction, size limits,
staging, no-clobber publication, exit codes and new-folder/prefix rerun rules
above apply unchanged.

Reference retrieved before implementation: MemPalace, MIT, pinned at
`4f7745afbd9d0197c6475b4c7ac846d4932eaeda`:

- [`backends/sqlite_exact.py:57`](https://github.com/MemPalace/mempalace/blob/4f7745afbd9d0197c6475b4c7ac846d4932eaeda/mempalace/backends/sqlite_exact.py#L57) and [`:1923-1951`](https://github.com/MemPalace/mempalace/blob/4f7745afbd9d0197c6475b4c7ac846d4932eaeda/mempalace/backends/sqlite_exact.py#L1923-L1951): the store filename and its `collections` and `documents` tables.
- [`backends/sqlite_exact.py:1790-1817`](https://github.com/MemPalace/mempalace/blob/4f7745afbd9d0197c6475b4c7ac846d4932eaeda/mempalace/backends/sqlite_exact.py#L1790-L1817): the read-only open rule for WAL side files.
- [`backends/sqlite_exact.py:290-294`](https://github.com/MemPalace/mempalace/blob/4f7745afbd9d0197c6475b4c7ac846d4932eaeda/mempalace/backends/sqlite_exact.py#L290-L294): registry and sentinel rows excluded as bookkeeping.
- [`config.py:288`](https://github.com/MemPalace/mempalace/blob/4f7745afbd9d0197c6475b4c7ac846d4932eaeda/mempalace/config.py#L288) and [`palace/collection.py:218`](https://github.com/MemPalace/mempalace/blob/4f7745afbd9d0197c6475b4c7ac846d4932eaeda/mempalace/palace/collection.py#L218): the drawers and closets collection names.
- [`stored_text.py:40-41`](https://github.com/MemPalace/mempalace/blob/4f7745afbd9d0197c6475b4c7ac846d4932eaeda/mempalace/stored_text.py#L40-L41) and [`:85-89`](https://github.com/MemPalace/mempalace/blob/4f7745afbd9d0197c6475b4c7ac846d4932eaeda/mempalace/stored_text.py#L85-L89): the escape flag, prefix and decoder.

These are format evidence, not copied implementation. The fixture
(`engine/crates/xerj-autoindex/tests/fixtures/mempalace.sql`) is a synthetic
store in that column shape, not a live MemPalace install. Run it through the
private-node lifecycle check:

```sh
python3 tools/session-import/smoke.py --binary engine/target/release/xerj --format mempalace
```
