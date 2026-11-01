# Opt-in Claude Code session handoff

`xerj init --memory` connects the existing Claude Code transcript importer
(#2406) to SessionEnd and SessionStart. This is the Claude Code part of
[#2645](https://github.com/xerj-org/xerj/issues/2645); Codex's existing offline
importer (#2413) is unchanged. Codex lifecycle integration is deferred.

On macOS/Linux, from the trusted project folder, with your local node already
running:

```sh
xerj init --memory --url http://localhost:9200 --dry-run
xerj init --memory --url http://localhost:9200
```

Restart the Claude Code session to load the hooks. This option manages only
`.claude/settings.local.json` and the private `.claude/xerj-memory/` directory;
ordinary `xerj init` keeps its existing MCP/teaching-file behavior. It does not
start a node, alter other agents or install hooks unless explicitly requested.
Existing settings and unrelated hook groups are retained. Settings backups
are kept inside the private data directory. An edited, duplicated or unowned
matching hook is refused rather than claimed or silently overwritten.

The node URL must be loopback, with no credentials, path, query or fragment.
Keys are never saved in configuration: `XERJ_API_KEY`, or the existing local
admin-key discovery, supplies auth at runtime. Redirects and HTTP proxies are
disabled for hook memory requests. A moved project needs a fresh configuration;
existing configuration bound to another canonical project path is refused.

## What happens

- SessionEnd writes a private queue entry and starts a detached worker. It
  prints nothing and does not wait for import or indexing. Independent delivery
  filenames prevent an arriving event from being erased by an older job.
- The worker uses the existing native PII-redacting importer. It saves an
  immutable content-addressed JSONL archive and replaces this session's current
  export only after the complete transcript validates. Tool results, thinking
  and tool payloads stay excluded; explicitly referenced file paths are kept
  under the existing importer contract. Different-session input is refused.
- Current exports are dry-run first, then reconciled with ordinary autoindex
  using one worker, `--no-graph --no-semantic`, the printed project prefix and a
  private journal. Opting in authorizes these automatic runs (`--max-minutes 0`);
  the subprocess has a shared 60-second dry-run/index deadline. The private
  `last-estimate.txt` records the dry-run output. No model is downloaded by the
  indexer. Server-side memory embedding follows the node's configured mode;
  the default is lexical hashing, not neural understanding.
- After indexing, the last four main-conversation text turns are excerpted
  into one `/_memory/<namespace>` entry per session. Archive filename/line and
  message ID accompany each excerpt. Repeated events reconcile and upsert the
  same session identity, without adding duplicate corpus rows or memory IDs.
- SessionStart reads the namespace's three most recent entries, including
  explicit notes you stored there. It prints at most 15 lines / 4096 UTF-8 bytes
  of quoted historical text. This is not an LLM summary, a decision classifier
  or an instruction source. On a node/read failure it can use the last locally
  saved session excerpt, explicitly labeled Cached. Nothing is printed when
  neither source is available. The memory request has a 500 ms total timeout.
- The full redacted conversation stays in the current export/search index;
  short excerpts are just a handoff. Original transcript files are never changed.

Hook stdin follows the [official Claude Code hook protocol](https://code.claude.com/docs/en/hooks):
`hook_event_name`, `cwd`, `session_id`, `transcript_path`. `cwd` must belong to
the configured project; supported transcript/session UUID rules are those of
`xerj sessions import --format claude-code`. Hook commands use quoted absolute
binary/project paths and never interpret event data as shell commands.

## Failures, retention and removal

Hooks always exit 0 and keep errors out of the agent session. Inspect
`.claude/xerj-memory/status.json`, `pending/`, and `last-estimate.txt` to see
whether the background job finished. Failed jobs remain queued; a later
SessionStart or SessionEnd attempts another drain. Each drain considers at most
eight jobs and checks its 90-second window between jobs. It is not a daemon.
Malformed/growing transcripts, offline nodes and indexing failures keep the
last valid export/receipt. Node publication, memory upsert and local receipt
are separate steps: a failure may leave indexed data before a receipt is
committed; replay reconciles the same identities rather than duplicating them.

Automatic capture is limited to 16 MiB per transcript; larger transcripts need
explicit manual import. The importer still enforces its existing per-line,
per-turn and record limits. There is no automatic retention purge: archives,
queue entries and node data can accumulate. The PII redactor handles its
existing email/IP/card/SSN/phone patterns; **it does not remove every secret or
personal name**. Only enable this for sessions you intend to retain locally.
Private folders use mode 0700 and new data files mode 0600. Hook/settings/state
symlinks are refused. The project and its configuration are trusted, owned by
the operator; this is not a sandbox against another process running as you.

```sh
xerj init --memory --remove --dry-run
xerj init --memory --remove
```

Removal refuses a busy worker and tells you to retry, preserving the current
settings. Its lease remains with an autoindex child if the supervisor dies.
Once removal succeeds, stale hook commands are silent no-ops. Removal keeps
archives, exports, pending jobs, backups, catalog/index data and memories.
Restore an edited managed hook from your private settings backup before
removing; unrelated settings are never restored wholesale.

To delete data intentionally, first disable the hooks. The setup output and
private config name the exact prefix/namespace; substitute those values below:

- Run `xerj autoindex remove --prefix <printed-prefix> --root <absolute-project>/.claude/xerj-memory/exports --url <node-url>` and inspect its ownership plan before confirming. This removes only the managed export corpus, not other project indices.
- Delete the dedicated memory namespace with the documented `DELETE /_memory/<printed-namespace>` API, using the same node/auth. This also deletes any manual notes you put in that namespace.
- After node data has been handled, delete this project's `.claude/xerj-memory/` directory if you also want to erase archives, cache, pending work and backups. Re-enabling afterward starts fresh; leaving data in place permits retries and reuse.

## Reproduce without personal sessions

```sh
python3 tools/session-memory/smoke.py --binary engine/target/release/xerj
python3 tools/session-memory/smoke.py --binary engine/target/release/xerj --authenticated
```

The script uses synthetic transcripts, a copied executable, quoted paths and a
private temporary node/data/project. It invokes the commands actually installed
in Claude settings, checks the searchable records and memory, repeats an event,
changes input, tests an offline cached start and malformed-tail refusal, restarts
the node, retries pending work and removes hooks while checking data retention.
It stops its own node and removes its own temporary files. It does not launch
Claude Code, use your transcripts or prove a specific host version's shutdown
behavior. Windows, remote nodes and Codex lifecycle hooks are unsupported here.

Reference implementation read before writing: `init.rs` configuration merge
and backup behavior, `sessions.rs` offline redaction/publication, `memory_api.rs`
namespaced store/list, and `state.rs` plus `journal_lock_inheritance.rs` for the
open-file-description lock lifetime. Exact pinned locations and measured
results are recorded in [VERIFICATION.md](VERIFICATION.md). No peer code was
copied. Claude Code's hook documentation supplies the wire/event contract;
its SessionEnd budget is why import/index work lives in a separate process.
