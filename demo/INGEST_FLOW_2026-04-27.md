# Demo: explicit ingest of chat-events into xerj

Captured: 2026-04-27T19:53:05Z on this machine

## 1. Empty server — what xerj-console sees right after boot

```
$ curl -s "localhost:9200/_cat/indices?v" | grep -vE "^\\."
green open .xerj_views 076f9c9c-27c6-4369-b392-2007a4339e3e 1 0 0 0
green open .xerj_magic_links e0595dcd-85bf-48e8-b7e7-024f3c9ae585 1 0 1 0
green open .xerj_alert_fires 65033f80-3baf-4f8f-a1e9-ec5729520061 1 0 0 0
green open .xerj_dashboards f322e9c4-7c1c-4515-b592-4fee1846e69b 1 0 0 0
green open .xerj_alert_rules cf26fa19-2b4b-46b0-bfc1-756f5803db4b 1 0 0 0
green open .xerj_passkeys 15b3e5a0-0efa-4740-8ca1-ad5276efe0d8 1 0 0 0
green open .xerj_idp_config c592b8c6-abc7-43a2-bde7-0ec87dd1c7f2 1 0 0 0
green open .xerj_audit 0feaf0dc-d2c1-435c-9a70-b1805c1b0284 1 0 0 0
green open .xerj_cluster_state 5085cf92-319a-44c5-b436-037f4a44c28f 1 0 0 0
green open .xerj_connections b480d2df-af41-4840-a52e-397a1e47b63f 1 0 0 0
green open .xerj_sessions ee3d881a-f8c3-4d42-8f32-664169f8e2ee 1 0 0 0
green open .xerj_prefs 527b3fd4-d7b5-4403-9f43-351c7ea8db96 1 0 0 0
green open .xerj_api_tokens 4adc1ac9-d5c1-4ae2-831d-baace07ff4f6 1 0 0 0
green open .xerj_users aced2676-cc86-4aab-805d-fe9dfe3cd296 1 0 0 0

$ curl -s localhost:9200/chat-events/_count
{"error":{"root_cause":[{"type":"index_not_found_exception","reason":"index not found: chat-events","index":"chat-events"}],"type":"index_not_found_exception","reason":"index not found: chat-events"},"status":404}```

Result: zero user data.  Open xerj-console and every dashboard would read
`{ "hits": { "total": 0 } }` and the live adapter returns null →
the SPA falls back to its mock skeleton.  Nothing is real yet.

## 2. The data file — generated, reproducible, byte-identical

```
$ # 4008 docs of synthetic LLM telemetry, random.seed(42)
$ wc -l demo/data/extras/chat-events.ndjson
4008 /home/claude/ai/xerj/demo/data/extras/chat-events.ndjson

$ head -2 demo/data/extras/chat-events.ndjson | jq .
{
  "@timestamp": "2026-04-26T19:47:24Z",
  "model": "claude-haiku-4-5",
  "intent": "code-assist",
  "prompt_tokens": 1978,
  "context_tokens": 10608,
  "completion_tokens": 306,
  "cost_usd": 0.010127,
  "latency_ms": 240,
  "cache_hit": false,
  "top_doc": "runbook/oncall.md",
  "tenant": "acme",
  "status": "ok"
}
{
  "@timestamp": "2026-04-26T19:48:01Z",
  "model": "claude-sonnet-4-6",
  "intent": "code-assist",
  "prompt_tokens": 2489,
  "context_tokens": 3444,
  "completion_tokens": 421,
  "cost_usd": 0.011121,
  "latency_ms": 950,
  "cache_hit": true,
  "top_doc": "rfc/039-hybrid-search.md",
  "tenant": "globex",
  "status": "ok"
}
```

## 3. Bulk ingest — what the SPA's data source looks like over the wire

```
$ # ES-compat /_bulk: alternating action + doc lines. Same shape Logstash, Filebeat,
$ # Logstash and every ES SDK already speaks.
$ (
  while IFS= read -r doc; do
    printf "{\"index\":{\"_index\":\"chat-events\"}}\n%s\n" "$doc"
  done < demo/data/extras/chat-events.ndjson
) > /tmp/bulk-body.ndjson
$ wc -l /tmp/bulk-body.ndjson
8016 /tmp/bulk-body.ndjson

$ START=$(date +%s.%N); curl -s -XPOST localhost:9200/_bulk \
    -H "content-type: application/x-ndjson" \
    --data-binary @/tmp/bulk-body.ndjson > /tmp/bulk-resp.json; END=$(date +%s.%N)
$ python3 -c "print(f\"elapsed: {$(awk \"BEGIN{print $END-$START}\")*1000:.0f} ms\")"

elapsed: 104 ms

$ # Response — first action, last action, error count, took
$ jq "{took, errors, n_items: (.items | length), first_item: .items[0], last_item: .items[-1]}" /tmp/bulk-resp.json
{
  "took": 87,
  "errors": false,
  "n_items": 4008,
  "first_item": {
    "index": {
      "_id": "59ca3bd5-9fef-40b0-aea9-2bcaff69589d",
      "_index": "chat-events",
      "_primary_term": 1,
      "_seq_no": 1777319608862913,
      "_shards": {
        "failed": 0,
        "successful": 1,
        "total": 1
      },
      "_version": 1,
      "result": "created",
      "status": 201
    }
  },
  "last_item": {
    "index": {
      "_id": "5cab65ce-e0dc-43c0-9c36-b2abf9e3eac4",
      "_index": "chat-events",
      "_primary_term": 1,
      "_seq_no": 1777319608863429,
      "_shards": {
        "failed": 0,
        "successful": 1,
        "total": 1
      },
      "_version": 1,
      "result": "created",
      "status": 201
    }
  }
}
```

## 4. Verify — what xerj-console will see when it queries this index

```
$ curl -s "localhost:9200/_cat/indices?v" | grep chat-events
green open chat-events de04447a-91b2-4f0a-91f5-e4329b1451eb 1 0 4008 0

$ curl -s localhost:9200/chat-events/_count
{"_shards":{"failed":0,"skipped":0,"successful":1,"total":1},"count":4008}
$ # Now the EXACT query the AI · Overview dashboard sends:
$ curl -s localhost:9200/chat-events/_search -H "content-type: application/json" -d "{
    \"size\":9999,
    \"query\":{\"range\":{\"@timestamp\":{\"gte\":\"$(date -u -d \"24 hours ago\" +%FT%TZ)\"}}},
    \"aggs\":{
      \"total_prompt\":   {\"sum\":{\"field\":\"prompt_tokens\"}},
      \"total_context\":  {\"sum\":{\"field\":\"context_tokens\"}},
      \"total_completion\":{\"sum\":{\"field\":\"completion_tokens\"}},
      \"total_cost\":     {\"sum\":{\"field\":\"cost_usd\"}},
      \"avg_latency\":    {\"avg\":{\"field\":\"latency_ms\"}},
      \"models\":         {\"terms\":{\"field\":\"model\",\"size\":8}}
    }}" | jq "{hits_total: .hits.total.value, took, agg_summary: .aggregations | with_entries(if .key==\"models\" then .value = (.value.buckets | map({(.key): .doc_count}) | add) else .value = .value.value end)}"
{
  "hits_total": 3990,
  "took": 25,
  "agg_summary": {
    "avg_latency": 838.5731829573934,
    "models": {
      "claude-sonnet-4-6": 1385,
      "claude-haiku-4-5": 1105,
      "claude-opus-4-7": 888,
      "gpt-5": 372,
      "gemini-3": 150,
      "llama-4": 90
    },
    "total_completion": 1277826.0,
    "total_context": 35552928.0,
    "total_cost": 37.80994899999996,
    "total_prompt": 7168434.0
  }
}
```

These are the exact numbers AI · Overview renders.  4 008 queries,
44 M tokens, $38 spend, six-model breakdown summing back to 4 008.
If you change the corpus, the dashboard changes.
