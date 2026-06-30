# Vector search — kNN, no Pinecone

_Use case doc: vectors / playbooks/vector-search_

dense_vector field + kNN retrieval are first-class in the same engine.

### ✅ create vector index

```bash
curl -s -XPUT "http://localhost:9200/vstore" \
  -H 'content-type: application/json' \
  -d '{"mappings":{"properties":{"text":{"type":"text"},"embedding":{"type":"dense_vector","dims":4}}}}'
```

```json
{
  "acknowledged": true,
  "shards_acknowledged": true,
  "index": "vstore"
}
```

_HTTP 200_

### ✅ bulk index vectors

```bash
curl -s -XPOST "http://localhost:9200/vstore/_bulk" \
  -H 'content-type: application/x-ndjson' \
  --data-binary $'{"index":{}}\n{"text":"red apple","embedding":[0.9,0.1,0.0,0.1]}\n{"index":{}}\n{"text":"green pear","embedding":[0.1,0.9,0.1,0.0]}\n{"index":{}}\n{"text":"blue car","embedding":[0.0,0.1,0.9,0.1]}\n'
```

```json
{
  "errors": false,
  "items": [
    {
      "index": {
        "_id": "8b5da400-7a47-416e-8b12-59757a428ec1",
        "_index": "vstore",
        "_primary_term": 1,
        "_seq_no": 1782804405305297,
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
    {
      "index": {
        "_id": "f48582cf-265d-4ca8-8b09-40d8d70d742b",
        "_index": "vstore",
        "_primary_term": 1,
        "_seq_no": 1782804405305298,
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
    {
      "index": {
        "_id": "94795f4a-86a1-4650-9831-8c7fc1c7d923",
        "_index": "vstore",
        "_primary_term": 1,
        "_seq_no": 1782804405305298,
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
  ],
  "took": 0
}
```

_HTTP 200_

### ✅ refresh

```bash
curl -s -XPOST "http://localhost:9200/vstore/_refresh"
```

```json
{
  "_shards": {
    "failed": 0,
    "successful": 1,
    "total": 1
  }
}
```

_HTTP 200_

### ✅ kNN nearest to [0.9,0.1,0,0.1]

```bash
curl -s -XPOST "http://localhost:9200/vstore/_search" \
  -H 'content-type: application/json' \
  -d '{"knn":{"field":"embedding","query_vector":[0.9,0.1,0,0.1],"k":2,"num_candidates":10}}'
```

```json
{
  "_shards": {
    "failed": 0,
    "skipped": 0,
    "successful": 1,
    "total": 1
  },
  "hits": {
    "hits": [
      {
        "_id": "8b5da400-7a47-416e-8b12-59757a428ec1",
        "_index": "vstore",
        "_score": 1,
        "_source": {
          "embedding": [
            0.9,
            0.1,
            0,
            0.1
          ],
          "text": "red apple"
        }
      },
      {
        "_id": "f48582cf-265d-4ca8-8b09-40d8d70d742b",
        "_index": "vstore",
        "_score": 0.608433723449707,
        "_source": {
          "embedding": [
            0.1,
            0.9,
            0.1,
            0
          ],
          "text": "green pear"
        }
      },
      {
        "_id": "94795f4a-86a1-4650-9831-8c7fc1c7d923",
        "_index": "vstore",
        "_score": 0.5120481848716736,
        "_source": {
          "embedding": [
            0,
            0.1,
            0.9,
            0.1
          ],
          "text": "blue car"
        }
      }
    ],
    "max_score": 1,
    "total": {
      "relation": "eq",
      "value": 3
    }
  },
  "timed_out": false,
  "took": 0
}
```

_HTTP 200_

