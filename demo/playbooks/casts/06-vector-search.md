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
        "_id": "e71f1a61-464a-4384-a7cf-25659a5ae30f",
        "_index": "vstore",
        "_primary_term": 1,
        "_seq_no": 1782803695122171,
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
        "_id": "9b678c6d-4f1e-4cb2-b452-37a4d9b18563",
        "_index": "vstore",
        "_primary_term": 1,
        "_seq_no": 1782803695122171,
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
        "_id": "a748e623-75fe-4c68-9ab7-6cf17ecd68b6",
        "_index": "vstore",
        "_primary_term": 1,
        "_seq_no": 1782803695122171,
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
        "_id": "e71f1a61-464a-4384-a7cf-25659a5ae30f",
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
        "_id": "9b678c6d-4f1e-4cb2-b452-37a4d9b18563",
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
        "_id": "a748e623-75fe-4c68-9ab7-6cf17ecd68b6",
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

