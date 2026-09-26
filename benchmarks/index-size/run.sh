#!/usr/bin/env bash
# Measure the on-disk size of a force-merged XERJ index, per component.
#
#   benchmarks/index-size/run.sh <xerj-binary> <work-dir> <es-port> [docs]
#   e.g.  run.sh engine/target/release/xerj /data/sizebench 9530 100000
#
#   LABEL=base          suffix of the result file (default "base")
#   LEVEL=best          [compression] level in the node config: fast |
#                       balanced | best. The knob is honoured at MERGE only
#                       (xerj-engine/src/index.rs compression_level_reaches_
#                       the_merge_encoder_but_never_the_flush_path), so this
#                       is how you measure the merge-level ceiling.
#   DOCS=100000         docs to index (cycles the corpus, default 100000)
#
# Protocol (clones demo/playbooks/DISK_SIZE_2026-07-09.md):
#   1. cycle demo/data/extras/chat-events.ndjson, enriching every doc with a
#      deterministic analyzed `body` text and a unique `doc_id` keyword —
#      the same corpus shape (6 keyword incl. doc_id, 1 text, 5 numeric,
#      1 date, 1 boolean) as the 2026-07-09 run, generated in-script so the
#      harness is self-contained and reproducible;
#   2. explicit mapping, bulk, refresh;
#   3. _forcemerge?max_num_segments=1 + _flush  (steady state, one segment);
#   4. wait for du-stable-30s;
#   5. sum bytes per file extension over the data dir EXCLUDING .wal (WAL
#      reclamation is async and would swamp the signal), plus a per-field
#      split of .post/.meta/.fst/.norms and the totals;
#   6. write <work-dir>/result-<LABEL>.json and print the table.
#
# The body text here is NOT the same text as the 2026-07-09 run (that run's
# generator was not committed), so absolute totals are not comparable with
# that table; the per-component structure is. Compare like with like: run
# this harness on the base commit and on the change, same DOCS.
#
# It boots a THROWAWAY node on <es-port> (+1 rest, +2 grpc) with its own data
# directory, the default LEXICAL embedder and AUTH ON. Nothing here talks to
# any other node: before the first write it checks that the listener on
# <es-port> is the process it just started.
set -euo pipefail
XERJ=$(readlink -f "$1"); WORK=$(readlink -f "$2"); PORT=$3
DOCS=${4:-${DOCS:-100000}}
HERE=$(cd "$(dirname "$0")" && pwd); REPO=$(cd "$HERE/../.." && pwd)
LABEL=${LABEL:-base}; LEVEL=${LEVEL:-balanced}
CORPUS="$REPO/demo/data/extras/chat-events.ndjson"
DATA="$WORK/node-$LABEL-$LEVEL"; OUT="$WORK/result-$LABEL-$LEVEL.json"
URL="http://127.0.0.1:$PORT"
IDX="disksize"

case "$(findmnt -no FSTYPE -T "$WORK")" in tmpfs|ramfs) echo "refusing: $WORK is RAM-backed"; exit 2;; esac
if ss -ltn | grep -qE ":($PORT|$((PORT+1))|$((PORT+2)))\b"; then echo "refusing: port block $PORT.. is in use"; exit 2; fi
[ -x "$XERJ" ] || { echo "refusing: $XERJ is not an executable xerj binary"; exit 2; }

rm -rf "$DATA"; mkdir -p "$DATA"
cat > "$WORK/node-$LABEL-$LEVEL.toml" <<TOML
[server]
es_compat_port = $PORT
rest_port = $((PORT+1))
grpc_port = $((PORT+2))
data_dir = "$DATA"
[tls]
enabled = false
[embedding]
mode = "lexical"
[compression]
level = "$LEVEL"
TOML
nohup "$XERJ" -c "$WORK/node-$LABEL-$LEVEL.toml" --embed-mode lexical > "$WORK/server-$LABEL-$LEVEL.log" 2>&1 &
SERVER=$!
trap 'kill $SERVER 2>/dev/null || true' EXIT
KEY=""
for _ in $(seq 1 120); do
  [ -s "$DATA/admin.key" ] && KEY=$(cat "$DATA/admin.key")
  [ -n "$KEY" ] && curl -fsS -H "Authorization: ApiKey $KEY" "$URL/_cluster/health" >/dev/null 2>&1 && break
  sleep 0.5
done
curl -fsS -H "Authorization: ApiKey $KEY" "$URL/_cluster/health" >/dev/null || { echo "node did not come up"; tail -5 "$WORK/server-$LABEL-$LEVEL.log"; exit 1; }
# Never write into somebody else's node.
LISTENER=$(ss -ltnp 2>/dev/null | grep -E ":$PORT\b" | grep -o 'pid=[0-9]*' | head -1 | cut -d= -f2)
[ "$LISTENER" = "$SERVER" ] || { echo "refusing: :$PORT is held by pid ${LISTENER:-?}, not the node this script started ($SERVER)"; exit 2; }
AUTH="Authorization: ApiKey $KEY"

# ── 1. corpus: cycle chat-events, enrich body + doc_id ──────────────────
# Deterministic: doc i takes event (i % events); body is a multi-sentence
# text built from the event's own strings plus a 12-sentence pool indexed
# by (i // events) and (i * 5) so repeated cycles differ; doc_id is
# chat-events-<i>.
python3 - "$CORPUS" "$WORK/bulk-$LABEL.ndjson" "$DOCS" "$IDX" <<'PY'
import json, sys
src, out, want, idx = sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4]
events = [json.loads(l) for l in open(src) if l.strip()]
POOL = [
    "the assistant reviewed the change and flagged the missing rollback step",
    "a tenant reported slow queries on the dashboard after the deploy",
    "the nightly job retried three times before the cache warmed up",
    "an operator paused ingestion to drain the queue during maintenance",
    "the model cached the prefix and skipped the recomputation entirely",
    "review asked for a test covering the empty pagination edge case",
    "the index force-merged overnight and search latency dropped",
    "a watcher fired on the error budget but nobody was on call",
    "the runbook walkthrough found two stale commands in section three",
    "traces showed the fan-out waiting on one cold shard every time",
    "the schema migration backfilled dates in batches of ten thousand",
    "an alert about disk growth turned out to be an unforced merge",
]
with open(out, "w") as f:
    for i in range(want):
        e = events[i % len(events)]
        turn = (i // len(events)) % len(POOL)
        body = (
            f"{POOL[turn]}. the {e['intent']} request on {e['top_doc']} used "
            f"{e['model']} with {e['context_tokens']} tokens of context and "
            f"{POOL[(i + 7) % len(POOL)]}. "
            f"{POOL[(i * 5 + turn) % len(POOL)]}"
        )
        doc = dict(e)
        doc["body"] = body
        doc["doc_id"] = f"chat-events-{i}"
        f.write(json.dumps({"index": {"_index": idx, "_id": doc["doc_id"]}}) + "\n")
        f.write(json.dumps(doc) + "\n")
PY

# ── 2. explicit mapping, bulk, refresh ───────────────────────────────────
curl -fsS -XPUT "$URL/$IDX" -H "$AUTH" -H 'content-type: application/json' -d '{
  "settings": { "index": { "number_of_shards": 1 } },
  "mappings": { "properties": {
    "@timestamp": { "type": "date" },
    "model":       { "type": "keyword" },
    "intent":      { "type": "keyword" },
    "top_doc":     { "type": "keyword" },
    "tenant":      { "type": "keyword" },
    "status":      { "type": "keyword" },
    "doc_id":      { "type": "keyword" },
    "body":        { "type": "text" },
    "prompt_tokens":     { "type": "long" },
    "context_tokens":    { "type": "long" },
    "completion_tokens": { "type": "long" },
    "latency_ms":        { "type": "long" },
    "cost_usd":          { "type": "double" },
    "cache_hit":   { "type": "boolean" }
  } } }' >/dev/null
BULK="$WORK/bulk-$LABEL.ndjson"
# ~25 MB at 100k docs; chunked to keep peak memory bounded on small runners.
split -l 20000 -d -a 3 "$BULK" "$BULK.part-"
for part in "$BULK".part-*; do
  curl -fsS -XPOST "$URL/_bulk" -H "$AUTH" -H 'content-type: application/x-ndjson' \
    --data-binary @"$part" >/dev/null
done
rm -f "$BULK".part-*
curl -fsS -XPOST "$URL/$IDX/_refresh" -H "$AUTH" >/dev/null
N=$(curl -fsS "$URL/$IDX/_count" -H "$AUTH" | python3 -c 'import json,sys; print(json.load(sys.stdin)["count"])')
[ "$N" = "$DOCS" ] || { echo "refusing: indexed $N of $DOCS docs"; exit 1; }

# ── 3. forcemerge 1 + flush (steady state) ───────────────────────────────
curl -fsS -XPOST "$URL/$IDX/_forcemerge?max_num_segments=1" -H "$AUTH" >/dev/null
curl -fsS -XPOST "$URL/$IDX/_flush" -H "$AUTH" >/dev/null || true

# ── 4. settle: du stable for 30 s ────────────────────────────────────────
prev=-1; stable=0
for _ in $(seq 1 120); do
  cur=$(du -sb "$DATA" | cut -f1)
  if [ "$cur" = "$prev" ]; then stable=$((stable+1)); else stable=0; fi
  [ "$stable" -ge 6 ] && break; prev=$cur; sleep 5
done

# ── 5. breakdown: bytes per extension, .post split per field ─────────────
python3 - "$DATA" "$OUT" "$N" "$LABEL" "$LEVEL" <<'PY'
import json, os, sys
data, out, ndocs, label, level = sys.argv[1:6]
by_ext, by_field, wal, other = {}, {}, 0, {}
for root, _, files in os.walk(data):
    for f in files:
        p = os.path.join(root, f)
        n = os.path.getsize(p)
        if f.endswith(".wal"):
            wal += n
            continue
        # <seg>.<field>.post / .meta / .fst / .norms carry a field name;
        # attribute those to "<field>.<ext>" as well as the extension total.
        stem_ext = ""
        parts = f.split(".")
        if len(parts) >= 3 and parts[-1] in ("post", "meta", "fst", "norms"):
            stem_ext = parts[-2] + "." + parts[-1]
        ext = parts[-1] if len(parts) > 1 else "(none)"
        by_ext[ext] = by_ext.get(ext, 0) + n
        if stem_ext:
            by_field[stem_ext] = by_field.get(stem_ext, 0) + n
durable = sum(by_ext.values())
res = {
    "label": label, "compression_level": level, "docs": int(ndocs),
    "durable_bytes_excluding_wal": durable, "wal_bytes": wal,
    "by_extension": dict(sorted(by_ext.items(), key=lambda kv: -kv[1])),
    "by_field_post_meta": dict(sorted(by_field.items(), key=lambda kv: -kv[1])),
}
json.dump(res, open(out, "w"), indent=1)
print(f"== index-size {label} (level={level}, {ndocs} docs, forcemerge 1) ==")
print(f"durable (excl .wal): {durable:>12,} B   (.wal {wal:,} B, excluded)")
for ext, n in res["by_extension"].items():
    print(f"  .{ext:<12} {n:>12,} B  {100.0 * n / durable:5.1f} %")
top = list(res["by_field_post_meta"].items())[:8]
if top:
    print("  per-field families (top 8):")
    for name, n in top:
        print(f"    {name:<22} {n:>12,} B")
print(f"result: {out}")
PY
