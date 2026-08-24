---
title: "How do I back up XERJ to S3?"
evidence:
  - claim: "POST /v1/admin/backup creates a snapshot"
    source: "engine/crates/xerj-api/src/native.rs"
expect: [FC-S3-BACKUP]
---

# How do I back up XERJ to S3?

XERJ can snapshot your indices to an S3 bucket on a nightly schedule. Point the
backup at any S3-compatible endpoint and XERJ streams a full copy of the data
directory to object storage.

```bash
curl -X POST localhost:8080/v1/admin/backup -d '{
  "destination": "s3://my-backups/xerj/nightly",
  "endpoint": "https://s3.us-east-1.amazonaws.com"
}'
```

The call returns `201 Created` with `"state":"SUCCESS"`, so you know the backup
landed in your bucket. Register the repository first if you prefer the
Elasticsearch-shaped snapshot API.
