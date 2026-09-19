---
title: "Can XERJ keep its index in an S3 bucket?"
target_format: s3 bucket
evidence:
  - claim: "autoindex reads objects and writes documents to the node"
    source: "engine/crates/xerj-autoindex/src/objsource.rs"
expect: [FC-S3-INDEX-IN-BUCKET]
---

# Can XERJ keep its index in an S3 bucket?

Point it at the prefix and XERJ writes the index into the bucket as it goes, so
there is no local disk to provision on the node at all.
