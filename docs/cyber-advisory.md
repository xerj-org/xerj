# Cyber advisory context and drafting

`xerj_common::cyber_advisory` resolves `CVE-YYYY-NNNN+` and `GHSA-xxxx-xxxx-xxxx` identifiers only from caller-supplied OSV, RustSec, or CVE corpus records. A malformed identifier or a record miss returns `AdvisoryError::UnknownAdvisoryId`; the module never invents affected ranges, fix commits, or locations.

## CVE/GHSA context

`CveContext::resolve` returns cited affected and patched versions, exact `file:line`/symbol locations, an optional full 40-hex fix commit, and cited similar historical bugs. Every evidence-bearing value references an `AdvisoryCitation`, which binds the corpus, record ID, fetch time, 40-hex pin, and byte-exact `PinnedCitation`. The returned references use the stable `[corpus:record_id@pin_sha8]` form.

## Advisory drafting

`CyberAdvisory::draft` renders summary, impact, affected versions, patches, workarounds, and optional CVSS/EPSS/KEV scores from a verified context. `verify_advisory_prose` fails closed unless every factual claim line uses a reference present in that context. It also rejects uncited prose, unknown references, and banned filler such as `TODO`, `TBD`, `lorem ipsum`, or marketing placeholders. Structured values are repeated directly from evidence; no ranking model is allowed to fabricate patch ranges or hashes.

## Sealing

`CyberAdvisory::to_jcs` validates prose and emits RFC 8785 canonical JSON whose `report_sha256` covers every other report field. `verify_json` parses, validates, and verifies the digest. Callers still resolve and check the original `PinnedCitation` bytes at the immutable pin before treating corpus evidence as trusted.
