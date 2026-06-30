# Analyzers — _analyze

_Use case doc: analyzers_

Inspect tokenization the way ES does.

### ✅ standard analyzer

```bash
curl -s -XPOST "http://localhost:9200/_analyze" \
  -H 'content-type: application/json' \
  -d '{"analyzer":"standard","text":"The Quick, Brown Fox!"}'
```

```json
{
  "tokens": [
    {
      "end_offset": 3,
      "position": 0,
      "start_offset": 0,
      "token": "the",
      "type": "word"
    },
    {
      "end_offset": 9,
      "position": 1,
      "start_offset": 4,
      "token": "quick",
      "type": "word"
    },
    {
      "end_offset": 16,
      "position": 2,
      "start_offset": 11,
      "token": "brown",
      "type": "word"
    },
    {
      "end_offset": 20,
      "position": 3,
      "start_offset": 17,
      "token": "fox",
      "type": "word"
    }
  ]
}
```

_HTTP 200_

### ✅ keyword analyzer

```bash
curl -s -XPOST "http://localhost:9200/_analyze" \
  -H 'content-type: application/json' \
  -d '{"analyzer":"keyword","text":"The Quick, Brown Fox!"}'
```

```json
{
  "tokens": [
    {
      "end_offset": 21,
      "position": 0,
      "start_offset": 0,
      "token": "The Quick, Brown Fox!",
      "type": "word"
    }
  ]
}
```

_HTTP 200_

### ✅ whitespace analyzer

```bash
curl -s -XPOST "http://localhost:9200/_analyze" \
  -H 'content-type: application/json' \
  -d '{"analyzer":"whitespace","text":"The Quick, Brown Fox!"}'
```

```json
{
  "tokens": [
    {
      "end_offset": 3,
      "position": 0,
      "start_offset": 0,
      "token": "The",
      "type": "word"
    },
    {
      "end_offset": 10,
      "position": 1,
      "start_offset": 4,
      "token": "Quick,",
      "type": "word"
    },
    {
      "end_offset": 16,
      "position": 2,
      "start_offset": 11,
      "token": "Brown",
      "type": "word"
    },
    {
      "end_offset": 21,
      "position": 3,
      "start_offset": 17,
      "token": "Fox!",
      "type": "word"
    }
  ]
}
```

_HTTP 200_

