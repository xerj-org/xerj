---
title: "How do I back up a XERJ index?"
evidence:
  - claim: "POST /v1/admin/backup takes repo_path, name and indices and writes to local disk"
    source: "docs/SNAPSHOT_AND_RESTORE.md"
  - claim: "There is no S3, GCS, Azure or HDFS repository implementation"
    source: "docs/SNAPSHOT_AND_RESTORE.md"
---

# How do I back up a XERJ index?

XERJ writes backups to a filesystem repository on the local disk of the node.

```bash
curl -X POST localhost:8080/v1/admin/backup -d '{
  "repo_path": "/var/lib/xerj/_backups",
  "name": "nightly-2026-08-21",
  "indices": ["logs-*"]
}'
```

`repo_path` must sit inside `data_dir` or be listed in
`limits.snapshot_repo_allowlist`, otherwise the call fails with a 400.

XERJ has no object storage repository: there is no S3, GCS, Azure or HDFS
backend, and a request body aimed at a bucket is ignored rather than rejected.
Copy the finished directory to your bucket yourself with `aws s3 sync`.
