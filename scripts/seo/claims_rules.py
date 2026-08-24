#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
claims_rules.py - the rule DATA for scripts/seo/factcheck.py.

This module holds no logic. It is the single place a writer or editor extends
when a new unsupportable claim is discovered. `factcheck.py` is the engine.

Every rule MUST carry:
    id        stable identifier, used by --explain and by CI suppressions
    title     one line, human readable
    intent    what the pattern is trying to catch, in words
    sev       ERROR | WARN            (heuristics go in WARN, never ERROR)
    reason    why the claim is not supportable, in the writer's language
    evidence  list of "<file>:<line>" citations into the two ledgers.
              A rule with no citation is rejected by --self-test.
    rewrite   a compliant sentence the writer can paste instead.

Matching fields (all optional except `pattern` for kind="pattern"):
    pattern   regex over NORMALISED text (lowercased, straight quotes,
              ASCII hyphens). Fires the rule.
    context   regex that must ALSO be present in scope for the rule to fire.
              Use it to bind a generic word ("bucket") to a risky context
              ("backup"). Keeps false positives down.
    requires  regex that must be present in scope or the rule fires.
              This is the "qualification required" form.
    exempt    list of regexes; if any matches in scope the rule does not fire.
              This is where the PERMITTED phrasings live.
    scope     "paragraph" (default) or "window"
    window    chars either side of the match when scope == "window"
    code      True to also scan fenced code blocks (default False).
              Set True where the trap lives in a copyable API example.

Sources of truth, in precedence order:
    1. LEDGER-readjudication   (ran the binary; overrules #2)
    2. LEDGER-capabilities     (read the code)
    3. RESEARCH-competitors-longtail  (THING matrix, honesty check)

Those three are the session working record that produced this ruleset.  They
are deliberately not committed, so the citations below are provenance labels
rather than paths you can open: they say which document and which line settled
a rule, not where to find it now.  Everything they established is baked into
the tables in this file, and those tables are what every gate reads.
"""

from __future__ import annotations

ERROR, WARN = "ERROR", "WARN"

RJ = "LEDGER-readjudication.md"
LC = "LEDGER-capabilities.md"
RC = "RESEARCH-competitors-longtail.md"


# ======================================================================================
# Reusable regex fragments
# ======================================================================================

# Honest framings that make an otherwise-banned word safe. These are deliberately
# explicit strings rather than a generic "any negation nearby", because a generic
# negation is trivially satisfied by an unrelated "not" elsewhere in the paragraph.
_NEG = r"(?:no|not|never|without|lacks?|lacking|cannot|can't|does not|doesn't|do not|is not|are not|has no|have no|there is no|there are no)"

def _neg_near(term, span=90):
    """`<negation> ... <term>` or `<term> ... is/are not` inside one sentence."""
    return (r"(?:" + _NEG + r"\b[^.\n]{0,%d}\b(?:%s)" % (span, term) +
            r"|\b(?:%s)\b[^.\n]{0,%d}\b(?:is|are|was|were)\s+not\b" % (term, span) +
            r"|\b(?:%s)\b[^.\n]{0,%d}\b(?:is|are)\s+(?:absent|inert|unimplemented|not implemented|not enforced|not supported)\b" % (term, span) +
            r")")


_BACKUP_CTX = r"back(?:\s|-)?up|backups|snapshot|restore|archive|repositor(?:y|ies)|disaster recovery|\bdr\b|retention"
_S3_TERMS = r"s3|object stor(?:e|age)|bucket|blob stor(?:e|age)|minio|gcs|google cloud storage|azure blob|cloud storage"

_NEURAL_QUALIFIER = (r"(?:--embed-mode[= ]neural|embed-mode\s+neural|neural embedd|neural mode|"
                     r"opt-in neural|neural is opt-in|opt into neural|onnx embedd|"
                     r"lexical by default|default embedder is lexical|feature[- ]hashing|"
                     r"lexical feature hashing|lexical, not (?:neural|semantic))")


# ======================================================================================
# 1. Banned / qualified claim rules
# ======================================================================================

RULES = [

    # ---------------------------------------------------------------- S3 / object store
    {
        "id": "FC-S3-BACKUP",
        "title": "S3 / object-store backup or snapshot destination",
        "intent": "any object-store noun (s3, bucket, object storage, blob storage, "
                  "minio, gcs, azure blob) inside a backup/snapshot/restore context",
        "sev": ERROR,
        "kind": "pattern",
        "pattern": r"\b(?:%s)\b" % _S3_TERMS,
        "context": r"\b(?:%s)\b" % _BACKUP_CTX,
        "exempt": [
            r"local disk|local filesystem|filesystem repositor|local-disk|writes? to disk|"
            r"on-disk repositor|\bdata_dir\b|repo_path",
            _neg_near(_S3_TERMS, 120),
            r"silently|201 success|reports success|appears to succeed|drops? the s3",
        ],
        "code": True,
        "reason": ("The endpoint EXISTS and returns `201 Created` with `\"state\":\"SUCCESS\"` "
                   "for the documented S3 body - but serde ignores the unknown `destination` "
                   "and `endpoint` keys and the backup lands on LOCAL DISK under "
                   "`<data_dir>/_backups`. A reviewer who curls the documented example sees a "
                   "success and concludes the feature works. There is no RepositoryType enum, "
                   "no URL-scheme dispatch, no feature flag and no env var; "
                   "`storage.backend = \"s3\"` is rejected at startup, and the only S3Backend in "
                   "the tree is a local-directory simulation whose every caller is under "
                   "`#[cfg(test)]`. `PUT /_snapshot/s3repo` resolves the BUCKET NAME to a "
                   "filesystem path and 400s."),
        "evidence": [RJ + ":21", RJ + ":44", RJ + ":73", LC + ":384", LC + ":613"],
        "rewrite": ("XERJ backs up to a filesystem repository. `POST /v1/admin/backup` takes "
                    "`repo_path` (default `<data_dir>/_backups`), `name` and `indices`; the path "
                    "must sit inside `data_dir` or in `limits.snapshot_repo_allowlist`. There is "
                    "no S3, GCS, Azure or HDFS repository - copy the finished directory to object "
                    "storage yourself."),
    },
    {
        "id": "FC-S3-INGEST",
        "title": "Native object-store ingest / indexing",
        "intent": "claims that autoindex or the engine reads directly from an object store",
        "sev": ERROR,
        "kind": "pattern",
        "pattern": r"\b(?:index|ingest|crawl|scan|autoindex|read)\w*\s+(?:\w+\s+){0,3}?(?:an?\s+)?(?:%s)\b" % _S3_TERMS,
        "exempt": [
            r"mount|mounted|s3fs|rclone|sync(?:ed)? (?:it )?(?:down|locally)|copy (?:it )?(?:down|locally)|"
            r"already on disk|local(?:ly)? first|filesystem walk",
            _neg_near(_S3_TERMS, 120),
        ],
        "reason": ("`xerj autoindex` walks a LOCAL FILESYSTEM. There is no object-store reader. "
                   "The THING coverage matrix marks 'S3 bucket' RED: 'Do not imply native S3 "
                   "ingest. Quickwit owns object-storage indexing.'"),
        "evidence": [RC + ":374", RJ + ":44"],
        "rewrite": ("Mount or sync the bucket to local disk first (s3fs, rclone, `aws s3 sync`), "
                    "then point `xerj autoindex` at the directory. XERJ does not read from object "
                    "storage itself."),
    },

    # ---------------------------------------------------------------- RBAC / SSO
    {
        "id": "FC-RBAC",
        "title": "RBAC / roles / fine-grained permissions",
        "intent": "role-based access control, role assignment, per-field or per-document "
                  "permissions, 'granular'/'fine-grained' permissions",
        "sev": ERROR,
        "kind": "pattern",
        "pattern": (r"(?:\brbac\b|role[- ]based access|\brole[- ]based\b|"
                    r"(?:fine[- ]grained|granular|per[- ]user|per[- ]role|per[- ]team)\s+"
                    r"(?:permission|access|authoriz|authoris|control|security)|"
                    r"assign(?:ing|ed)?\s+roles?\b|"
                    r"(?:document|field)[- ]level security|"
                    r"\brole mapping\b|\bsuperuser\b\s+(?:and|vs)\s+|"
                    r"privilege(?:s)?\s+(?:model|system|enforcement))"),
        "exempt": [
            _neg_near(r"rbac|role[- ]based|roles?|privileges?", 120),
            r"roles are stored but not enforced|enforced\"?\s*:\s*false|full superuser access|"
            r"api[- ]key (?:principal )?scop|principal scop|scoped (?:api )?keys?|"
            r"deferred|not role-based",
        ],
        "reason": ("Observed on a live node: `GET /_security/roles` returns the 6 seeded roles "
                   "plus `\"enforced\": false` and the verbatim warning 'roles are stored but NOT "
                   "enforced: every authenticated caller has full superuser access regardless of "
                   "any role assignment.' `_has_privileges` answers `has_all_requested: true` to "
                   "`cluster:[\"all\"]`. The store is not even ES-shaped: `PUT /_security/role/{name}` "
                   "with ES's `indices:[{names,privileges}]` is rejected with "
                   "`invalid type: map, expected a string`."),
        "evidence": [RJ + ":23", LC + ":412", LC + ":615"],
        "rewrite": ("Authorization in XERJ is API-key PRINCIPAL SCOPING, not roles: `authorize_index`, "
                    "`authorize_brain`, `authorize_memory_namespace` and `authorize_expression` confine "
                    "a Scoped key to its named indices and reserve the `.xerj-memory-*` namespace. "
                    "Roles are stored but not enforced - say 'scoped API keys', never 'RBAC'."),
    },
    {
        "id": "FC-SSO",
        "title": "SSO / SAML / OIDC / LDAP / Kerberos",
        "intent": "any enterprise identity-federation claim",
        "sev": ERROR,
        "kind": "pattern",
        "pattern": r"(?:\bsso\b|single sign[- ]on|\bsaml\b|\boidc\b|openid connect|\bldap\b|"
                   r"\bkerberos\b|active directory|identity provider|\bidp\b|scim\b)",
        "exempt": [
            _neg_near(r"sso|single sign[- ]on|saml|oidc|ldap|kerberos|identity provider", 140),
            r"no sso of any kind|no saml/oidc/ldap|elasticsearch gates|behind (?:a )?(?:paid|platinum)",
        ],
        "reason": ("There is no `_security/user` CRUD, no `_security/role_mapping`, no realms, "
                   "and no LDAP / SAML / OIDC / Kerberos anywhere in the tree. Re-adjudication "
                   "confirmed this against the running binary."),
        "evidence": [RJ + ":23", LC + ":418", LC + ":615"],
        "rewrite": ("XERJ authenticates with API keys (and mTLS at the transport). It has no SSO, "
                    "SAML, OIDC or LDAP integration - terminate identity at your proxy and pass a "
                    "scoped API key through."),
    },
    {
        "id": "FC-PRIV-ORACLE",
        "title": "`_has_privileges` presented as an authorization check",
        "intent": "telling readers to gate anything on the _has_privileges response",
        "sev": ERROR,
        "kind": "pattern",
        "code": True,
        "pattern": r"_has_privileges",
        "exempt": [r"answers?\s+true to everything|always returns true|stub|not an authorization oracle|"
                   r"do not treat|never treat"],
        "reason": "The endpoint answers `true` to everything. Its own source comment: "
                  "'Do not treat this endpoint as an authorization oracle.'",
        "evidence": [LC + ":263", LC + ":639"],
        "rewrite": ("`_has_privileges` exists for Kibana's handshake and answers `true` to every "
                    "request. Do not use it as an authorization oracle."),
    },

    # ---------------------------------------------------------------- HA / cluster
    {
        "id": "FC-HA",
        "title": "High availability, replication, failover, multi-node, multi-region",
        "intent": "any claim that data survives a node loss or moves between nodes",
        "sev": ERROR,
        "kind": "pattern",
        "pattern": (r"(?:\bhigh availability\b|\bha\s+(?:cluster|setup|deployment|mode|pair)\b|"
                    r"\bfail[- ]?over\b|\breplicat(?:e|es|ed|ion|ing)\b|\breplica set\b|"
                    r"\bmulti[- ]?node\b|\bmulti[- ]?region\b|\bactive[- ]active\b|"
                    r"\bthree[- ]node\b|\b3[- ]node\b|\bquorum\b|"
                    r"\bhorizontal(?:ly)? scal|\bscales? out\b|\bscale[- ]out\b|"
                    r"\bdistributed (?:search|index|engine|deployment|cluster)\b|"
                    r"\bcluster of\b|\bcluster nodes\b|\badd (?:more )?nodes\b)"),
        "exempt": [
            _neg_near(r"ha|high availability|replication|failover|multi[- ]node|distributed|cluster", 140),
            r"single[- ]node only|single node only|xerj is single[- ]node|"
            r"does not claim multi-node production readiness|"
            r"raft (?:metadata|leader election)|metadata log replication|"
            r"(?:logical|postgres|postgresql|mysql|cdc|wal2json|debezium)[- ]replication|"
            r"replication slot|logical[- ]replication|change data capture|"
            r"index data never moves|zero data[- ]plane callers|not implemented",
        ],
        "reason": ("Data-plane replication is absent: `WalReplicator`, `SearchCoordinator` and "
                   "`RegionManager` have zero references outside `crates/xerj-cluster/`, and "
                   "`engine.rs:873` pins `ShardRouter::new(1)`. The Raft crate IS a live boot path "
                   "(`main.rs:2059-2125` under `cluster.enabled = true`) - leader election and "
                   "METADATA log replication are reachable - but index data never moves. "
                   "Single-node is the only measured configuration. Cluster transport is plaintext "
                   "JSON with no mTLS and no per-node identity."),
        "evidence": [RJ + ":22", RJ + ":38", LC + ":614", LC + ":637"],
        "rewrite": ("XERJ runs single-node. Raft provides leader election and metadata log "
                    "replication when `cluster.enabled = true`, but index data never moves between "
                    "nodes: there is no data-plane replication, no failover and no multi-region "
                    "mode. Plan for backup-and-restore, not for HA."),
    },
    {
        "id": "FC-SHARDS",
        "title": "Sharding / number_of_shards greater than 1",
        "intent": "claims that XERJ shards an index across shards or nodes",
        "sev": ERROR,
        "kind": "pattern",
        "code": True,
        "pattern": (r"(?:\bshard(?:s|ing|ed)?\b|number_of_shards|"
                    r"\bsplit (?:the |your )?index across\b|\bpartition(?:s|ed|ing)? (?:the |your )?index\b)"),
        "exempt": [
            _neg_near(r"shard(?:s|ing|ed)?|number_of_shards", 140),
            r"single[- ]shard|one shard|xerj is single-shard|shardrouter::new\(1\)|"
            r"number_of_shards\"?\s*[:=]\s*\"?1\b|"
            r"logs a warn|warn(?:ing)?\b[^.\n]{0,60}ignored|active_shards_percent_as_number|"
            r"fabricat|synthesi[sz]ed|sliced scroll|slice(?:d)? scroll",
            r"\"?_?shards\"?\s*:\s*[\{\[]|\b_shards\b|\"successful\"|\"failed\"",
            r"does not claim multi[- ]node|post[- ]ga|"
            r"single[- ]node\b[^.\n]{0,90}(?:only|default|the default run)|"
            r"(?:default run|only configuration)\b[^.\n]{0,40}single[- ]node",
        ],
        "reason": ("`number_of_shards > 1` is NOT silently ignored - which is worse. It (a) logs "
                   "`WARN xerj is single-shard; number_of_shards=5 is ignored`, (b) is echoed back "
                   "verbatim by `_settings`, and (c) FABRICATES a healthy multi-shard topology: "
                   "`GET /_cluster/health/testidx` on a 5-shard/2-replica index returned "
                   "`\"active_primary_shards\":5,\"active_shards\":5,\"unassigned_shards\":2,"
                   "\"active_shards_percent_as_number\":500.0`. An operator checking cluster health "
                   "for shard confirmation gets a fabricated answer."),
        "evidence": [RJ + ":22", RJ + ":37", LC + ":637"],
        "rewrite": ("XERJ is single-shard. `number_of_shards` is accepted and echoed back for wire "
                    "compatibility, and it drives sliced-scroll partitioning, but only one shard "
                    "exists - `_cluster/health` will report a multi-shard topology that is not real."),
    },
    {
        "id": "FC-SLA",
        "title": "Availability SLA / uptime guarantee",
        "intent": "99.9x% availability, uptime guarantees, 'five nines'",
        "sev": ERROR,
        "kind": "pattern",
        "pattern": r"(?:\b99\.9\d*\s*%|\bfour nines\b|\bfive nines\b|availability sla|uptime (?:sla|guarantee)|"
                   r"\bsla\b[^.\n]{0,40}(?:availability|uptime))",
        "exempt": [r"forward[- ]looking|target, not|not measured uptime|no availability sla|we do not offer"],
        "reason": ("The 99.95% / 99.99% figures are forward-looking commercial targets on tiers "
                   "marked 'Q2 2026', sold against UNSHIPPED replication. Selling an availability "
                   "SLA on unshipped replication is the highest-risk claim on the site. There is no "
                   "measured uptime anywhere."),
        "evidence": [LC + ":505", LC + ":614"],
        "rewrite": ("Do not publish an availability number. Describe the durability mechanism that "
                    "exists - WAL + fsync policy + filesystem snapshots on a single node."),
    },

    # ---------------------------------------------------------------- semantics
    {
        "id": "FC-SEMANTIC",
        "title": "'Semantic search' without the opt-in-neural qualification",
        "intent": "semantic search / semantic similarity / semantic matching / vector meaning, "
                  "used without naming the neural opt-in or the lexical default",
        "sev": ERROR,
        "kind": "pattern",
        "pattern": (r"(?:semantic(?:ally)?\s+(?:search|similar(?:ity)?|match(?:ing|es)?|retrieval|"
                    r"relevance|understanding|index(?:ing)?)|"
                    r"\bsemantic search\b|\bsemantically\b|"
                    r"\bconcept(?:ual)?\s+(?:search|match|similarity)\b|"
                    r"\bsearch by meaning\b|\bmeaning[- ]based\b)"),
        "requires": _NEURAL_QUALIFIER,
        "reason": ("THE DEFAULT EMBEDDER IS LEXICAL FEATURE HASHING with no model and no semantics. "
                   "Observed end-to-end on a default node: two documents indexed into a "
                   "`semantic_text` field, then `{\"query\":{\"semantic\":{\"field\":\"text\","
                   "\"query\":\"car\"}}}` ranked 1st 'a canine barked loudly' (0.5169) and 2nd "
                   "'the automobile is red' (0.5000). The default embedder ranks an UNRELATED "
                   "sentence above the exact synonym - on this probe it is anti-correlated, not "
                   "weakly semantic. Neural is opt-in, downloaded, CPU-only and runs at ~15 docs/s."),
        "evidence": [RJ + ":29", LC + ":632", LC + ":277"],
        "rewrite": ("'Semantic search with `--embed-mode neural` (opt-in, downloaded MiniLM-class "
                    "model, CPU-only). The default embedder is lexical feature hashing and cannot "
                    "connect synonyms.' Put the qualifier in the SAME paragraph as the claim."),
    },
    {
        "id": "FC-MEANING",
        "title": "'Understands the meaning' / 'knows what you mean'",
        "intent": "anthropomorphic comprehension claims about the query or the corpus",
        "sev": ERROR,
        "kind": "pattern",
        "pattern": (r"(?:understand(?:s|ing)?\s+(?:the\s+)?(?:meaning|intent|context|what you mean|"
                    r"your (?:query|question|question's meaning))|"
                    r"knows what you mean|grasp(?:s)? the meaning|"
                    r"understand(?:s)? natural language|figures out what you meant)"),
        "requires": _NEURAL_QUALIFIER,
        "reason": ("Same probe as FC-SEMANTIC: on the default lexical feature-hashing embedder, "
                   "the query 'car' ranked 'a canine barked loudly' ABOVE 'the automobile is red'. "
                   "'Understands the meaning' is not a softer version of the semantic claim - it is "
                   "a stronger one, and it is false by default."),
        "evidence": [RJ + ":29", LC + ":632"],
        "rewrite": ("'XERJ matches your query lexically by default (BM25 plus feature-hashed "
                    "vectors). Run with `--embed-mode neural` to get a real embedding model.' "
                    "Never claim comprehension."),
    },
    {
        "id": "FC-EMBED-DEFAULT",
        "title": "Embeddings / vectors mentioned without the lexical-default disclosure",
        "intent": "standing rule 4 - 'lexical by default; neural is opt-in' must appear in any "
                  "article that mentions semantic search, embeddings or vectors",
        "sev": WARN,
        "kind": "pattern",
        "scope": "document",
        "pattern": r"(?:\bembedding(?:s)?\b|\bembedder\b|\bembed(?:s|ded)?\s+your\b|"
                   r"\bvector search\b|\bhybrid search\b|\bsemantic\w*\b)",
        "requires": _NEURAL_QUALIFIER,
        "reason": "Standing rule 4 of the ledger: \"'Lexical by default; neural is opt-in' appears "
                  "in any article that mentions semantic search, embeddings, or vectors.\"",
        "evidence": [LC + ":650", LC + ":632"],
        "rewrite": "Add one sentence: 'XERJ's default embedder is lexical feature hashing; the "
                   "neural embedder is opt-in via `--embed-mode neural`.'",
    },
    {
        "id": "FC-SEMANTIC-MATCH",
        "code": True,
        "title": "`match` on a semantic_text field described as semantic",
        "intent": "claims that a plain match query auto-upgrades to kNN",
        "sev": WARN,
        "kind": "pattern",
        "pattern": r"match\b[^.\n]{0,60}semantic_text|semantic_text[^.\n]{0,60}\bmatch\b",
        "exempt": [r"runs bm25|not knn|issue #363|does not auto[- ]upgrade|bm25, not"],
        "reason": "Known open defect on rc.18: `match` on a `semantic_text` field runs BM25, "
                  "NOT kNN (issue #363).",
        "evidence": [LC + ":302"],
        "rewrite": "State it: '`match` on a `semantic_text` field currently runs BM25, not kNN "
                   "(#363). Use an explicit `semantic` or `knn` clause.'",
    },
    {
        "id": "FC-LEARNED-FUSION",
        "code": True,
        "title": "Learned / trained rank fusion",
        "intent": "fusion: learned, learning-to-rank, trained rerankers inside XERJ",
        "sev": ERROR,
        "kind": "pattern",
        "pattern": r"(?:\"?learned\"?\s+fusion|fusion\s*[:=]\s*\"?learned|learning[- ]to[- ]rank|\bltr\b|"
                   r"trained rerank|learned rerank)",
        "exempt": [_neg_near(r"learned|ltr|learning[- ]to[- ]rank", 120), r"400 at parse time|rejected"],
        "reason": "`Learned` fusion is NOT implemented - it 400s at parse time in both string and "
                  "object form, with a defence-in-depth 400 in the engine and a conformance test "
                  "asserting the rejection.",
        "evidence": [LC + ":200"],
        "rewrite": "XERJ fuses with RRF (and weighted linear). `fusion: \"learned\"` is rejected "
                   "with a 400 - there is no learned or trained fusion.",
    },

    # ---------------------------------------------------------------- retracted benchmarks
    {
        "id": "FC-BENCH-8191",
        "title": "The retracted 81-of-91-cells / 1M-doc benchmark board",
        "intent": "the withdrawn denominator and corpus size",
        "sev": ERROR,
        "kind": "pattern",
        "pattern": (r"(?:\b81\s*(?:/|of|out of)\s*91\b|\b91\s+(?:measured\s+)?cells\b|"
                    r"\b1\s*m(?:illion)?[- ]doc(?:ument)?\s+corpus\b|"
                    r"\bwins?\s+81\b)"),
        "reason": ("`SCORECARD.md` says 55 WIN / 26 TIE / 4 LOSE / 3 N/A over 88 CELLS at 100k "
                   "docs, not 81 of 91 at 1M. The 91-cell denominator was explicitly named as "
                   "removed by the project's own 2026-07-28 correction; it survives in-tree only "
                   "as a dated archive."),
        "evidence": [RJ + ":24", LC + ":492", LC + ":616"],
        "rewrite": ("'55 wins, 26 ties, 4 losses and 3 not-applicable across 88 cells against a "
                    "live Elasticsearch 8.13.4 at 100,000 documents (`demo/playbooks/SCORECARD.md`) "
                    "- one of those wins is a recall draw, so read it as 54 W / 27 draws.'"),
    },
    {
        "id": "FC-BENCH-SQ8",
        "title": "SQ8 vector-memory savings (18 GB vs 92 GB, 5.1x, 10M SKUs on one node)",
        "intent": "any claim that scalar8 quantization reduces resident memory",
        "sev": ERROR,
        "kind": "pattern",
        "pattern": (r"(?:10\s*m(?:illion)?\s+skus|10\s*m\s*x\s*1\.5\s*k|"
                    r"\bsq8\b[^.\n]{0,80}(?:memory|ram|resident|footprint|gb)|"
                    r"(?:memory|ram|resident|footprint)[^.\n]{0,80}\bsq8\b|"
                    r"scalar8[^.\n]{0,80}(?:reduce|shrink|cut|save|less|lower|smaller)|"
                    r"(?:reduce|shrink|cut|save|4x less|less|lower|smaller)[^.\n]{0,60}scalar8|"
                    r"\bquantiz\w+[^.\n]{0,60}(?:reduce|cut|save|shrink)\w*[^.\n]{0,30}"
                    r"(?:memory|ram|resident|footprint))"),
        "exempt": [r"does not reduce resident memory|dims\s*(?:x|\*|×)\s*4 (?:bytes )?either way|"
                   r"#392|precision profile"],
        "reason": ("`scalar8` does NOT reduce resident memory: 'the kNN serving path does not hold "
                   "SQ8 codes resident, it quantizes each candidate's f32 vector per query' "
                   "(`quantizer.rs:186-190`), and the CHANGELOG says it 'still reads the "
                   "full-precision vector from `_source` on every query ... it buys the precision "
                   "profile of int8 and nothing else' (#392). The arithmetic is independently "
                   "wrong too: 10M x 1536 x 4 B = 61.44 GB, not 92 GB - and whatever overhead "
                   "convention you pick, the SQ8 column must carry the SAME number."),
        "evidence": [RJ + ":25", LC + ":497", LC + ":617"],
        "rewrite": ("'Plan `dims x 4` bytes per vector, with or without `scalar8` (#392). "
                    "`scalar8` buys an int8 precision profile, not a smaller resident footprint.' "
                    "This is already the published rule at `landing/docs/operations.html:313`."),
    },
    {
        "id": "FC-BENCH-TCO",
        "title": "~80% infrastructure cost / TCO reduction",
        "intent": "unsourced cost-savings percentages",
        "sev": ERROR,
        "kind": "pattern",
        "pattern": r"(?:\d{2}\s*%[^.\n]{0,60}(?:tco|cost|infrastructure|spend|bill)|"
                   r"(?:tco|cost|infrastructure|spend|bill)[^.\n]{0,60}\d{2}\s*%|"
                   r"\bcut(?:s)? (?:your )?(?:costs?|tco|spend) by\b)",
        "reason": "'~80% infrastructure cost reduction' is prose only - no model, no assumptions, "
                  "no source anywhere in the repo.",
        "evidence": [LC + ":498"],
        "rewrite": ("Give the inputs instead: binary size, idle RSS (~400 MB), node count (1), and "
                    "the licence (Apache-2.0, no Platinum tier). Let the reader do their own "
                    "arithmetic, or publish a model with its assumptions."),
    },
    {
        "id": "FC-BENCH-53X",
        "title": "'5.3x fewer tokens on 234 files / 170k LOC'",
        "intent": "the agent-gate headline with no results file",
        "sev": ERROR,
        "kind": "pattern",
        "pattern": r"(?:5\.3\s*(?:x|×)|\b234 files\b|\b170k? loc\b)",
        "reason": "Headlined in `docs/TOKEN_USAGE.md` and `demo/agent-gate/README.md` with NO "
                  "results file in `demo/agent-gate/`.",
        "evidence": [LC + ":503"],
        "rewrite": ("Use the agent-gate numbers that DO have a results file: retrieval regime "
                    "1.14x fewer tokens with 6/7 vs 3/7 correctness "
                    "(`demo/agent-gate/RESULTS_retrieval.txt`), and publish the analytics regime "
                    "alongside it, where XERJ uses 109.78x MORE tokens."),
    },
    {
        "id": "FC-BENCH-1515",
        "title": "'15/15 vs 1/15' reference-coding solve rate",
        "intent": "a denominator that appears in none of the data",
        "sev": ERROR,
        "kind": "pattern",
        "pattern": r"15\s*/\s*15|\b15 of 15\b|1\s*/\s*15\b",
        "reason": "Every artifact says 21/21 vs 1/21. The same marketing card also shows 16/16 and "
                  "11/16 - three denominators on one card - and 15 appears in none of the data.",
        "evidence": [LC + ":504", LC + ":620"],
        "rewrite": ("Cite the committed JSON: unrecallable-contract track 21/21 at \\$3.38 (xerj) vs "
                    "1/21 at \\$21.90 (bare); multilang track 16/16 at \\$1.58 vs 11/16 at \\$11.18. "
                    "Carry the caveat that the libraries were written for the study and that the "
                    "memorised-control track is a XERJ loss."),
    },
    {
        "id": "FC-BENCH-P99-FALSE",
        "title": "The withdrawn p99 framing ('ES 2-19 ms vs XERJ 60-150 ms')",
        "intent": "the retracted read-under-write latency range",
        "sev": ERROR,
        "kind": "pattern",
        "pattern": r"(?:60\s*(?:-|to)\s*150\s*ms|2\s*(?:-|to)\s*19\s*ms)",
        "reason": "Actual measured range is 3.45-20.13 ms (ES) vs 10.27-13.57 ms (XERJ). The "
                  "60-150 ms figure came from a saturation artifact - the open-loop harness offered "
                  "load faster than either engine could retire, so the mixed cells reported "
                  "QUEUEING (65-152 ms), not engine latency.",
        "evidence": [LC + ":496", RJ + ":24"],
        "rewrite": ("'XERJ loses four cells: read p99 under a sustained 40,000 docs/s open-loop "
                    "writer, 13.57/3.45, 13.45/6.76, 10.27/3.68 and 10.74/3.57 ms "
                    "(XERJ/ES).' Publish the losses - that habit is the project's credibility asset."),
    },
    {
        "id": "FC-BENCH-SOURCE",
        "title": "Citing a retracted or stale source file for a number",
        "intent": "product.html section 09, BENCHMARK_VS_ES.md, migrate-from-elasticsearch.md",
        "sev": ERROR,
        "kind": "pattern",
        "code": True,
        "pattern": (r"(?:product\.html[^\n]{0,20}(?:§|section\s*)?0?9|"
                    r"benchmark_vs_es\.md|"
                    r"migrate-from-elasticsearch\.md\s*:\s*21[0-9])"),
        "reason": ("Standing rule 7: never cite `landing/product.html` section 09, "
                   "`demo/playbooks/BENCHMARK_VS_ES.md`, or "
                   "`docs/recipes/migrate-from-elasticsearch.md:214-218` as number sources. All "
                   "three are stale or retracted; BENCHMARK_VS_ES.md still calls the synthetic "
                   "corpus 'real'."),
        "evidence": [LC + ":506", LC + ":653"],
        "rewrite": "Cite `demo/playbooks/SCORECARD.md`, `demo/usecases/*/results.json`, "
                   "`demo/agent-gate/RESULTS_*.txt`, `docs/case-studies/*/data/*.json` or "
                   "`docs/EXPERIMENTAL_ONNX.md`. If the number is in none of them, it is not "
                   "publishable.",
    },
    {
        "id": "FC-BENCH-REPRO",
        "title": "'Reproduce it in four commands' / 'an unexpected LOSE fails CI'",
        "intent": "the benchmark reproducibility promise",
        "sev": ERROR,
        "kind": "pattern",
        "pattern": r"(?:unexpected lose fails ci|fails? ci\b[^.\n]{0,40}(?:lose|regression benchmark)|"
                   r"four commands|reproduce (?:it |them )?(?:on )?your (?:own )?machine)",
        "reason": ("Nothing in `.github/workflows/` runs `bench-matrix.mjs`. Two of the four "
                   "commands reference `scratchpad/`, which is gitignored and absent from "
                   "`git ls-files`, and `bench-matrix.mjs:41` hardcodes a maintainer's output path. "
                   "Reproducibility is BROKEN as published."),
        "evidence": [LC + ":459", LC + ":622"],
        "rewrite": ("Say what is true: the harness `demo/playbooks/bench-matrix.mjs` and the "
                    "results file `SCORECARD.md` are committed; the wrapper scripts are not, and "
                    "no CI job runs the matrix."),
    },
    {
        "id": "FC-AGG-10X",
        "title": "'Aggregations often 10x+ faster'",
        "intent": "a family-wide performance generalisation",
        "sev": WARN,
        "kind": "pattern",
        "pattern": r"aggregat\w+[^.\n]{0,40}(?:often|typically|usually|up to)[^.\n]{0,20}\d+\s*(?:x|×)|"
                   r"(?:often|typically|usually)[^.\n]{0,30}\d+\s*(?:x|×)[^.\n]{0,30}aggregat",
        "reason": "Directionally supported - 9 aggregation rows land between 10x and 30x - but the "
                  "same family contains 2.46x and 2.57x TIES. Cite named rows, not a family average.",
        "evidence": [LC + ":631", LC + ":451"],
        "rewrite": ("'`percentile_ranks` 0.23 vs 6.98 ms (30.14x), `percentiles` 26.67x, "
                    "`median_absolute_deviation` 23.09x, `scripted_metric` 19.78x on the 100k-doc "
                    "board' - name the rows."),
    },

    # ---------------------------------------------------------------- compatibility
    {
        "id": "FC-DROPIN",
        "title": "'Drop-in replacement' for Elasticsearch",
        "intent": "the unqualified drop-in claim",
        "sev": ERROR,
        "kind": "pattern",
        "pattern": r"\bdrop-in\b|\bdrop in replacement\b",
        "requires": (r"(?:1,?366\s*/\s*1,?369|conformance suite|"
                     r"drop[- ]in for\b|for text|not (?:for|a drop)|"
                     r"float analytics|scripted writes|exact totals|anything distributed|"
                     r"es_compatibility\.md)"),
        "reason": ("The re-adjudication located the exact qualification, in-tree at "
                   "`demo/playbooks/ES_COMPATIBILITY.md:216`, a LIVE-BINARY audit: drop-in FOR "
                   "text/keyword search, retrieval, integer/long analytics, vector kNN ranking and "
                   "Kibana-shaped ops (8.13.0 handshake, 152/162 endpoints backed by live engine "
                   "state, ~40 query types, ~61 aggs); NOT for float analytics, scripted writes, "
                   "exact totals on scored full-text queries, or anything distributed. Unqualified, "
                   "the claim is contradicted by the roadmap's own Known partials: span queries "
                   "return 0 hits standalone, `type` degrades to `MatchAll`, scroll caps at 10k, "
                   "`post_filter` is silently ignored, `has_child`/`has_parent` 400."),
        # Removed 2026-08-22: this list used to end '`{query, knn}` does not fuse'.
        # PRs #395/#458 made the pair fold into one RRF-fused list (k=60) when the
        # request is `hybrid_safe` — none of aggs / aggregations / sort / collapse /
        # search_after / rescore / highlight / min_score / explain / profile present.
        # With any of those it keeps the lexical `bool.should` and takes nothing from
        # the kNN half; an ARRAY of `knn` beside a `query` is still a 400.
        #
        # The line fired on nothing (it is explanatory prose, not a `pattern`), so no
        # page was ever reworded around it — but a rule whose stated evidence is false
        # is how the next writer gets misled. If you are updating this rule, re-read
        # `xerj-api/src/es_compat.rs` rather than trusting this comment.
        "evidence": [RJ + ":26", LC + ":628"],
        "rewrite": ("'Drop-in for text and keyword search, retrieval, integer and long analytics, "
                    "and vector kNN ranking - 1,366 of 1,369 ES-YAML conformance cases on a curated "
                    "200-file subset. Not drop-in for float analytics, scripted writes, exact totals "
                    "on scored full-text queries, or anything distributed.'"),
    },
    {
        "id": "FC-KIBANA",
        "title": "'Kibana connects directly' without its qualification",
        "intent": "Kibana / dashboards / existing tooling connecting unchanged",
        "sev": ERROR,
        "kind": "pattern",
        "pattern": (r"(?:kibana|opensearch dashboards|\bosd\b)[^.\n]{0,60}"
                    r"(?:connect|point|work|plug|attach)\w*|"
                    r"(?:connect|point|plug)\w*[^.\n]{0,40}(?:kibana|opensearch dashboards)|"
                    r"(?:existing|your) (?:clients|dashboards|tooling)[^.\n]{0,40}unchanged"),
        "requires": (r"(?:x-elastic-product|8\.13\.0 handshake|version handshake|"
                     r"no kibana e2e test|no version matrix|not verified|unverified|"
                     r"alerting is 0/7|_watcher has no scheduler|no scheduler|"
                     r"verify (?:the specific behaviour|it against your own node)|"
                     r"against your own node|vary in depth)"),
        "reason": ("What IS verified: the node advertises version 8.13.0 / Lucene 9.10.0 / "
                   "build_flavor default / tagline 'You Know, for Search' and emits "
                   "`X-Elastic-Product: Elasticsearch` - the actual gates Kibana and the official "
                   "clients check - plus per-request User-Agent distribution auto-detection whose "
                   "`--help` says the OpenSearch version was 'confirmed against a real OSD "
                   "container'. What is NOT verified: no Kibana E2E test, no version matrix, no "
                   "Kibana container in `docker-compose.yml`. Genuine boundary: ALERTING IS 0/7 - "
                   "`_watcher` stores watches with no scheduler."),
        "evidence": [RJ + ":27", LC + ":629", LC + ":271"],
        "rewrite": ("'XERJ answers the version handshake Kibana checks - 8.13.0, Lucene 9.10.0, "
                    "`X-Elastic-Product: Elasticsearch` - and ships its own console at "
                    "`/_xerj-console`. There is no Kibana end-to-end test and no version matrix, "
                    "and alerting is 0/7 because `_watcher` has no scheduler. Verify the specific "
                    "behaviour you need against your own node.'"),
    },
    {
        "id": "FC-CLIENTS-TESTED",
        "title": "'Tested with the official Elasticsearch clients'",
        "intent": "claims of client-library testing",
        "sev": ERROR,
        "kind": "pattern",
        "pattern": (r"(?:\btested\b[^.\n]{0,25}\b(?:with|against)\b[^.\n]{0,45}\bclients?\b|"
                    r"(?:official )?client librar\w+[^.\n]{0,30}(?:tested|verified|certified)|"
                    r"(?:elasticsearch-py|opensearch-py|elasticsearch-js|go-elasticsearch|@elastic/elasticsearch)"
                    r"[^.\n]{0,30}(?:tested|verified|supported))"),
        "exempt": [_neg_near(r"clients?|client librar\w+", 120), r"no client library is tested"],
        "reason": ("No ES or OpenSearch client library is tested anywhere: no `requirements.txt`, "
                   "`package.json`, `go.mod` or `pom.xml` under `engine/` or `.github/`, and "
                   "`engine/docker-compose.yml` has exactly one service."),
        "evidence": [LC + ":270", LC + ":630"],
        "rewrite": "'Wire-compatible with Elasticsearch 8.13 clients - 1,366 of 1,369 ES-YAML "
                   "conformance cases on a curated 200-file subset.' Do not say 'tested with the "
                   "official clients'.",
    },
    {
        "id": "FC-CONFORMANCE-CAVEAT",
        "title": "1,366/1,369 conformance without the 'curated 200-file subset' caveat",
        "intent": "standing rule 5",
        "sev": WARN,
        "kind": "pattern",
        "scope": "window",
        "window": 400,
        "pattern": r"1,?366\s*/\s*1,?369|\b99\.8\s*%",
        "requires": r"curated|200[- ]file|subset|catch:? (?:expectations|assertions) are (?:not |un)verified",
        "reason": ("The suite is a CURATED 200-file subset, and its `catch:` expectations are not "
                   "verified - 266 `catch:` occurrences across 92 files are effectively unasserted, "
                   "because the runner records the response and returns Ok regardless of status."),
        "evidence": [LC + ":471", LC + ":230", LC + ":651"],
        "rewrite": "'1,366 of 1,369 ES-YAML conformance cases on a curated 200-file subset, gated "
                   "at zero failures on every commit. The suite does not verify error behaviour - "
                   "`catch:` expectations are unasserted.'",
    },
    {
        "id": "FC-SCROLL",
        "title": "Bare 'scroll is supported'",
        "intent": "the scroll claim without the 10,000-document snapshot cap",
        "sev": WARN,
        "kind": "pattern",
        "pattern": r"\bscroll\b[^.\n]{0,40}(?:supported|works|available|use (?:the )?scroll)|"
                   r"(?:support|supports)\s+(?:the\s+)?scroll\b",
        "requires": r"10,?000|10k\b|snapshot cap|search_after|capped",
        "reason": "The project's own rule: \"'Scroll is supported, with a 10,000-document snapshot "
                  "cap; use search_after beyond that' is a different compatibility claim from "
                  "'scroll is supported', and only the first one is true.\"",
        "evidence": [LC + ":633"],
        "rewrite": "'Scroll is supported with a 10,000-document snapshot cap; use `search_after` "
                   "beyond that.'",
    },
    {
        "id": "FC-PROFILE-EXPLAIN",
        "title": "`profile` / `explain` sold as a debugging tool",
        "intent": "query profiling or score explanation presented as accurate",
        "sev": WARN,
        "kind": "pattern",
        "pattern": r"(?:query profil\w+|\bprofile api\b|\b_explain\b|\bexplain api\b|"
                   r"\bexplain\b[^.\n]{0,30}(?:why|scor\w+|ranking))",
        "exempt": [r"synthesi[sz]ed|hardcoded|all[- ]zero|not es-accurate|es-shaped, not|placeholder"],
        "reason": ("Both are SYNTHESISED. `profile` emits a hardcoded `\"type\": \"MatchQuery\"`, a "
                   "`Debug`-formatted description truncated to 80 chars, and an ALL-ZERO breakdown. "
                   "`explain` emits a hand-formatted description with nested `details` of "
                   "`\"value\": 0.0`. ES-shaped, not ES-accurate."),
        "evidence": [LC + ":634"],
        "rewrite": "'`_search?profile=true` and `_explain` return ES-shaped envelopes with "
                   "synthesised contents - the timing breakdown is all zeros. They satisfy clients; "
                   "they do not diagnose scoring.'",
    },
    {
        "id": "FC-ALERTING",
        "title": "Alerting / watcher / notifications",
        "intent": "any claim that XERJ runs alerts",
        "sev": ERROR,
        "kind": "pattern",
        "pattern": r"(?:\balert(?:s|ing)\b|\b_watcher\b|\bwatches\b|\bnotif(?:y|ication)\w*\b\s*(?:rule|when|on)|"
                   r"\bpage(?:s|rduty)?\b[^.\n]{0,20}when|trigger\w*[^.\n]{0,25}(?:alert|notification))",
        "exempt": [_neg_near(r"alert\w*|_watcher|watches|notification\w*", 140),
                   r"no scheduler|0/7|stores watches"],
        "reason": "`_watcher` stores watches but NO SCHEDULER executes them. There is no rule "
                  "engine and no notification path. Kibana alerting scores 0/7 DELIVERED.",
        "evidence": [LC + ":264", LC + ":635", RJ + ":27"],
        "rewrite": "'`_watcher` accepts and stores watch definitions for client compatibility, but "
                   "nothing executes them - there is no scheduler and no notification path. Run "
                   "alerting outside XERJ.'",
    },
    {
        "id": "FC-ML",
        "title": "Anomaly detection / machine learning framed as Elastic ML",
        "intent": "ML feature parity claims",
        "sev": WARN,
        "kind": "pattern",
        "pattern": r"(?:anomaly detection|\bmachine learning\b|\bml jobs?\b|\bdata frame analytics\b)",
        "exempt": [_neg_near(r"anomaly detection|machine learning|ml", 140),
                   r"not elastic ml|scoring over live data|no scheduler"],
        "reason": "`_ml` scoring over live data is real, but it is NOT Elastic ML - no jobs, no "
                  "datafeeds, no model management. And `_watcher` has no scheduler, so nothing "
                  "acts on a score.",
        "evidence": [LC + ":635"],
        "rewrite": "'XERJ exposes `_ml` scoring over live data. It is not Elastic ML: there are no "
                   "ML jobs, datafeeds or model management, and nothing schedules or alerts on the "
                   "result.'",
    },
    {
        "id": "FC-KG",
        "title": "'Knowledge graph' / 'understands relationships' for xerj brain",
        "intent": "graph-database framing of the brain link detectors",
        "sev": WARN,
        "kind": "pattern",
        "pattern": r"(?:knowledge graph|graph database|understands? (?:the )?relationships|"
                   r"semantic graph|entity graph|infers? relationships)",
        "exempt": [r"not a graph database|graph-shaped index|structural|wikilink|unstemmed"],
        "reason": ("7 of the 8 link detectors are STRUCTURAL (wikilinks, markdown links, hrefs, "
                   "path citations, section order, directory chains). The one content-reading "
                   "detector compares UNSTEMMED strings. The product's own tool description says "
                   "it best: 'a search engine with a graph-shaped index over its own documents, "
                   "not a graph database'."),
        "evidence": [LC + ":636"],
        "rewrite": "'`xerj brain` is a search engine with a graph-shaped index over its own "
                   "documents, not a graph database. Seven of its eight edge detectors are "
                   "structural - wikilinks, markdown links, hrefs, path citations.'",
    },
    {
        "id": "FC-CALLGRAPH",
        "title": "Call graphs / dependency graphs / 'understands your codebase'",
        "intent": "code-intelligence claims beyond definition extraction",
        "sev": ERROR,
        "kind": "pattern",
        "pattern": r"(?:call graph|dependency graph|cross[- ]file reference|find (?:all )?(?:the )?callers|"
                   r"understands? your (?:code|codebase|repo(?:sitory)?)|call hierarch|import graph|"
                   r"who calls\b|reference(?:s)? resolution)",
        "exempt": [_neg_near(r"call graph|dependency graph|callers|import graph", 140),
                   r"definitions only|no imports|no call graph",
                   # derived through the graph API / the committed AST-audit scripts, not
                   # emitted by the extractor
                   r"\b_graph\b|graph api|taint|queryable facts|ast-vuln|rust-ast-audit|"
                   r"\bedges\b|derive[sd]?\b"],
        "reason": ("tree-sitter extraction is DEFINITIONS ONLY - no imports, no call graph, no "
                   "cross-file references (`code.rs:1-16`, `code.rs:397`). Marked a critical "
                   "caveat in the ledger."),
        "evidence": [LC + ":85"],
        "rewrite": "'`xerj autoindex` extracts symbol DEFINITIONS with tree-sitter across 34 "
                   "languages - functions, types, methods, with file and line. It does not resolve "
                   "imports, callers or cross-file references.'",
    },
    {
        "id": "FC-SPAN",
        "title": "Span queries listed as supported",
        "intent": "span_term / span_or / span_not without the zero-hits caveat",
        "sev": WARN,
        "kind": "pattern",
        "pattern": r"\bspan_(?:term|or|not|near|first|within)\b|\bspan quer(?:y|ies)\b",
        "requires": r"0 hits|zero hits|standalone|not (?:usable|supported)|known partial",
        "reason": "`span_term` / `span_or` / `span_not` RETURN 0 HITS STANDALONE. Do not claim "
                  "span query support without this caveat.",
        "evidence": [LC + ":171", LC + ":628"],
        "rewrite": "'Span queries parse but return zero hits standalone - a known partial. Use "
                   "`match_phrase` with a slop for proximity.'",
    },
    {
        "id": "FC-DECAY-SCORING",
        "title": "Decay / geo-proximity scoring functions",
        "intent": "gauss / linear / exp decay in function_score",
        "sev": WARN,
        "kind": "pattern",
        "pattern": r"(?:\bgauss\b|\bdecay function|geo[- ]proximity scor|distance decay|recency decay scor)",
        "exempt": [_neg_near(r"gauss|decay|decay function", 120)],
        "reason": "`function_score` has `field_value_factor`, `random_score`, `weight`, `filter`, "
                  "6 score modes, 6 boost modes and 10 modifiers - but NO `gauss` / `linear` / "
                  "`exp` decay functions.",
        "evidence": [LC + ":166"],
        "rewrite": "'`function_score` supports `field_value_factor`, `random_score`, `weight` and "
                   "`filter` with 6 score modes and 10 modifiers. Decay functions (`gauss`, "
                   "`linear`, `exp`) are not implemented.'",
    },

    # ---------------------------------------------------------------- dead code / roadmap
    {
        "id": "FC-COLUMNAR-LOGS",
        "title": "'Columnar logs' / xerj-logs as a shipped feature",
        "intent": "the dead xerj-logs module sold as a capability",
        "sev": ERROR,
        "kind": "pattern",
        "code": True,
        "pattern": r"(?:columnar log|log[- ]columnar|xerj-logs|xerj_logs|"
                   r"column(?:ar)?\s+(?:log|logging)\s+stor(?:e|age)\b)",
        "exempt": [r"zero call sites|dead code|not (?:wired|invoked)|1,?737 lines",
                   # ZBS2 columnar blocks in the segment write path are real and shipped
                   r"\bzbs2\b|segment write path|domain-aware encodings"],
        "reason": ("`crates/xerj-logs/src/*.rs` is 1,737 lines and NO `.rs` file outside "
                   "`crates/xerj-logs/` references `xerj_logs` at all - a dependency edge with no "
                   "call. `ROADMAP.md:108`: 'still not invoked from non-test engine/server code ... "
                   "Wire it or remove it.'"),
        "evidence": [RJ + ":28", LC + ":618"],
        "rewrite": ("Log analytics does work - via the ZBS2 codec and generic aggregations over a "
                    "`logs.rs`-extracted index. Say that. 'Columnar logs' names a module with zero "
                    "call sites."),
    },
    {
        "id": "FC-TB-SCALE",
        "title": "TB-scale / billions of documents / petabyte",
        "intent": "large-corpus claims",
        "sev": ERROR,
        "kind": "pattern",
        "pattern": r"(?:\btb[- ]scale\b|\bterabytes?\b|\bpetabytes?\b|\bbillions? of (?:documents|docs|records|rows)\b|"
                   r"\bhundreds of millions of (?:documents|docs|records)\b|\bweb[- ]scale\b|\bunlimited scale\b)",
        "exempt": [_neg_near(r"tb[- ]scale|terabytes?|petabytes?|billions?", 140),
                   r"a few million documents|rss[- ]runaway|20\.2 gb rss"],
        "reason": "`AGENTS.md:63`: 'do not claim TB-scale end-to-end.' There is an open "
                  "RSS-runaway defect with 20.2 GB RSS observed mid-corpus, and the project's own "
                  "guidance is 'do not plan corpora beyond a few million documents.'",
        "evidence": [LC + ":638", LC + ":573"],
        "rewrite": "'XERJ is sized for corpora up to a few million documents on one node. The "
                   "server retains heap per indexed document - an open RSS-runaway defect, 20.2 GB "
                   "observed mid-corpus.'",
    },
    {
        "id": "FC-TERRAFORM",
        "title": "Terraform module as an install or deploy method",
        "intent": "listing a Terraform module",
        "sev": ERROR,
        "kind": "pattern",
        "pattern": r"terraform",
        "exempt": [_neg_near(r"terraform", 120), r"does not exist|no terraform"],
        "reason": "No Terraform module exists - `find` for `*.tf` returns nothing.",
        "evidence": [LC + ":642", LC + ":614"],
        "rewrite": "Install methods that exist: `curl | sh` (`landing/get`), `get.ps1` on Windows, "
                   "the Docker image, and the Helm chart at `deploy/helm/xerj/` (StatefulSet, "
                   "`replicaCount: 1`). No brew, apt, deb, PKGBUILD, crates.io or Terraform.",
    },
    {
        "id": "FC-HOSTED",
        "title": "Cloud / managed / hosted offering",
        "intent": "any claim of a hosted service",
        "sev": ERROR,
        "kind": "pattern",
        "pattern": r"(?:\bmanaged service\b|\bhosted (?:offering|service|version|xerj|cloud)\b|"
                   r"\bxerj cloud\b|\bfully managed\b|\bserverless (?:tier|offering)\b|\bsaas (?:offering|tier)\b)",
        "exempt": [_neg_near(r"managed|hosted|cloud", 140), r"self[- ]hosted|no hosted harness|"
                   r"no cloud (?:offering|product)"],
        "reason": "No cloud, managed or hosted offering is claimed anywhere in the repo - "
                  "`landing/benchmarks/index.html:1083` says 'no hosted harness', the industries "
                  "pages say 'SELF-HOSTED', and pricing sells support tiers, not hosting. Keep it "
                  "that way.",
        "evidence": [LC + ":545"],
        "rewrite": "'XERJ is self-hosted. There is no managed or cloud offering; the paid tiers "
                   "sell support, not hosting.'",
    },
    {
        "id": "FC-MSRV",
        "title": "A declared minimum supported Rust version",
        "intent": "publishing an MSRV",
        "sev": WARN,
        "kind": "pattern",
        "pattern": r"(?:\bmsrv\b|minimum supported rust|requires rust \d|rust \d+\.\d+\+)",
        "exempt": [_neg_near(r"msrv|minimum supported rust", 120), r"none is declared|no msrv"],
        "reason": "No MSRV is declared anywhere - no `rust-version` in `engine/Cargo.toml`, no "
                  "`rust-toolchain.toml`. The only pinned toolchain is `Dockerfile:6` "
                  "(`rust:1.94-slim`).",
        "evidence": [LC + ":641", LC + ":543"],
        "rewrite": "Do not publish an MSRV. If you must mention a toolchain, say 'built with "
                   "`rust:1.94-slim` in the official Dockerfile; no MSRV is declared'.",
    },
    {
        "id": "FC-CLI-SUBCOMMAND",
        "title": "A `xerj` subcommand that does not exist",
        "intent": "xerj query / snapshot / cluster / migrate / serve-as-subcommand",
        "sev": ERROR,
        "kind": "pattern",
        "code": True,
        "pattern": r"\bxerj\s+(?:query|snapshot|cluster|migrate|restore|backup|admin|search)\b",
        "reason": "The only subcommands are `index`, `autoindex`, `brain` and `mcp`. There is no "
                  "`xerj query`, `xerj snapshot`, `xerj cluster` or `xerj migrate`. (`xerj mcp` "
                  "itself landed in rc.17 - on rc.16 it returns `unknown argument: mcp`.)",
        "evidence": [LC + ":56", LC + ":641"],
        "rewrite": "Use `xerj index`, `xerj autoindex`, `xerj brain` or `xerj mcp`. Everything else "
                   "is an HTTP call against the running node.",
    },
    {
        "id": "FC-MEMORY-DECAY",
        "title": "Memory decay / forgetting curves / importance weighting",
        "intent": "agent-memory lifecycle claims",
        "sev": ERROR,
        "kind": "pattern",
        "pattern": r"(?:forgetting curve|memory decay|decays? over time|\bttl\b[^.\n]{0,30}memor|"
                   r"memor\w+[^.\n]{0,30}\bttl\b|importance (?:weight|scor)\w*|"
                   r"(?:ages?|expire)\w*\s+out (?:of )?memor)",
        "exempt": [_neg_near(r"decay|ttl|importance|forgetting", 140),
                   r"permanent until|explicitly forgotten|stored_at|recency_weight"],
        "reason": "There is NO TTL, no decay and no importance field. Memories are permanent until "
                  "explicitly forgotten. The only time signal is `stored_at`, used by an optional "
                  "`recency_weight` re-rank.",
        "evidence": [LC + ":314"],
        "rewrite": "'Memories persist until you explicitly forget them - there is no TTL, decay or "
                   "importance weighting. The only time signal is `stored_at`, which an optional "
                   "`recency_weight` can re-rank on.'",
    },
    {
        "id": "FC-RECALL-HYBRID",
        "code": True,
        "title": "Hybrid or fused agent recall",
        "intent": "claiming RRF inside _recall",
        "sev": WARN,
        "kind": "pattern",
        "pattern": r"_recall\b[^.\n]{0,60}(?:hybrid|rrf|fus\w+)|(?:hybrid|rrf|fused)[^.\n]{0,40}\brecall\b",
        "exempt": [r"no rrf fusion inside|bm25 by default|no fusion in _recall"],
        "reason": "Recall is BM25 by default. Modes in strict order: `vector` supplied -> pure kNN; "
                  "`semantic: true` -> server-side embed plus a `semantic` clause; plain `query` -> "
                  "`{\"match\":{\"text\":q}}`. There is NO RRF fusion inside `_recall`.",
        "evidence": [LC + ":318"],
        "rewrite": "'`_recall` is BM25 by default; pass a `vector` for pure kNN or `semantic: true` "
                   "to embed server-side. It does not fuse the two - use the `hybrid` query type on "
                   "a normal index for RRF.'",
    },

    # ---------------------------------------------------------------- competitors
    {
        "id": "FC-ABS-SUPERLATIVE",
        "title": "Absolute / first-and-only claims",
        "intent": "'no other tool', 'the only engine', 'nobody else', 'first to'",
        "sev": ERROR,
        "kind": "pattern",
        "pattern": (r"(?:\bno other (?:engine|tool|database|product|search engine|system|vendor)\b|"
                    r"\bnobody else\b|\bno one else\b|\bthe only (?:engine|database|tool|product|"
                    r"search engine|system|binary|one)\b|"
                    r"\bonly (?:engine|database|search engine|product|tool) that\b|"
                    r"\bfirst (?:and only )?(?:engine|database|search engine|tool) to\b|"
                    r"\bunique(?:ly)? (?:in|among)\b|\bunmatched\b|\bnothing else (?:does|can)\b)"),
        "reason": ("The research had to RETRACT exactly this shape of claim. The honesty check "
                   "grades every competitor on built-in embeddings and native hybrid RRF, and "
                   "concludes: 'The claim \"one binary, no JVM, no external embedding service, "
                   "BM25 + vector fused with RRF in a single query, free\" is already true of "
                   "Manticore Search (GPL-3.0, single C++ daemon), and substantially true of "
                   "OpenSearch and Vespa at greater weight. XERJ is not the only engine in this "
                   "position and must never claim to be.'"),
        "evidence": [RC + ":161", RC + ":146"],
        "rewrite": ("Name the differentiator that survives the honesty check instead: "
                    "'Elasticsearch wire compatibility with a published, machine-checked "
                    "conformance suite (1,366/1,369)' - Manticore markets itself as a drop-in "
                    "replacement but publishes no conformance number. Or `xerj autoindex` "
                    "zero-config typed folder ingest, agent memory plus MCP in the same binary, "
                    "tree-sitter AST extraction, and Apache-2.0 vs GPL-3.0."),
    },
    {
        "id": "FC-ABS-MANTICORE",
        "title": "'Only engine with built-in embeddings + native RRF' - the retracted claim",
        "intent": "the specific superlative Manticore falsifies",
        "sev": ERROR,
        "kind": "pattern",
        "pattern": (r"(?:only|no other|nobody|first)[^.\n]{0,80}"
                    r"(?:built[- ]in embedd|no external embedding|without an embedding service|"
                    r"rrf|rank fusion|hybrid (?:search )?in (?:a |one )?single query)|"
                    r"(?:built[- ]in embedd|no external embedding service)[^.\n]{0,60}"
                    r"(?:only|no other|unique|nobody)"),
        "exempt": [r"manticore"],
        "reason": ("Manticore Search ships LOCAL ONNX embedding models (all-MiniLM-L6-v2, Sentence "
                   "Transformers, Qwen/Llama/Mistral/Gemma) with no API key, AND native "
                   "`OPTION fusion_method='rrf'` combining `MATCH()` and `KNN()` in one query - in a "
                   "single C++ daemon with no JVM. OpenSearch, Vespa, Qdrant, Milvus, Weaviate, "
                   "LanceDB and Bleve also ship rank fusion. XERJ additionally LOSES to Manticore "
                   "on neural breadth: Manticore ships Qwen/Llama/Mistral/Gemma-class models; "
                   "XERJ's neural path is MiniLM-class and its default is lexical."),
        "evidence": [RC + ":146", RC + ":161"],
        "rewrite": ("'Like Manticore Search, XERJ ships embeddings and RRF fusion in one binary "
                    "with no JVM and no external service. What XERJ adds is a published, "
                    "machine-checked ES conformance number, zero-config typed folder ingest, and "
                    "agent memory plus an MCP server in the same process.'"),
    },

    # ---------------------------------------------------------------- engine-implemented checks
    {
        "id": "FC-NUM-TIERC",
        "title": "A Tier C number (no backing evidence) appears in the article",
        "intent": "engine check - every numeric claim is looked up in the tiered citable list",
        "sev": ERROR,
        "kind": "engine",
        "reason": "Tier C numbers have NO backing evidence: no results file, no harness, no method, "
                  "no date - or they were explicitly retracted by the project on 2026-07-28 and are "
                  "not re-derivable, because their magnitudes were re-evaluated as saturation "
                  "artifacts.",
        "evidence": [LC + ":488", LC + ":493", RJ + ":24"],
        "rewrite": "Replace with the current Tier A value from `demo/playbooks/SCORECARD.md`, or "
                   "delete the number. See --explain FC-NUM-UNKNOWN for the accepted sources.",
    },
    {
        "id": "FC-NUM-UNKNOWN",
        "title": "A numeric claim with no recognised provenance",
        "intent": "engine check - the number is in no tier and in no evidence: entry",
        "sev": WARN,
        "kind": "engine",
        "reason": ("Standing rule 1: every performance number must name its file. If it is not in "
                   "`demo/playbooks/SCORECARD.md`, `demo/usecases/*/results.json`, "
                   "`demo/agent-gate/RESULTS_*.txt`, `docs/case-studies/*/data/*.json` or "
                   "`docs/EXPERIMENTAL_ONNX.md`, it does not get published."),
        "evidence": [LC + ":647", LC + ":436"],
        "rewrite": "Either add an `evidence:` entry to the frontmatter naming the file the number "
                   "came from, or remove the number.",
    },
    {
        "id": "FC-NUM-COMPANION",
        "title": "A Tier A number published without its mandatory companion fact",
        "intent": "engine check - losses and caveats must travel with the win they qualify",
        "sev": WARN,
        "kind": "engine",
        "reason": ("Standing rule 2: publish the losses. Every measured artifact in this repo "
                   "already does - the four mixed-write p99 losses, the 0.23x doc-index inversion, "
                   "the 109.78x agent-gate analytics loss, the 9+1 vs 10/10 agent sim, the kv "
                   "memorised-control loss. 'That habit is the project's most valuable credibility "
                   "asset - do not break it.' kNN 1.18x in particular is a TIE at 100% recall, not "
                   "a win."),
        "evidence": [LC + ":648", LC + ":449", LC + ":453", LC + ":467"],
        "rewrite": "Put the companion fact in the same paragraph, or within a few sentences.",
    },
    {
        "id": "FC-THING-RED",
        "title": "The article targets a format with no extractor (RED)",
        "intent": "engine check - THING coverage matrix gate",
        "sev": ERROR,
        "kind": "engine",
        "reason": "The THING coverage matrix is the publishing gate for the 'how do I scan X' "
                  "programme. 'Red rows must not be written until an extractor exists.'",
        "evidence": [RC + ":346", RC + ":374"],
        "rewrite": "Pick a GREEN row instead, or file the extractor as an issue first. The four "
                   "missing extractors with the clearest demand are XLSX/PPTX, mbox/email, "
                   "OCR/images and EPUB.",
    },
    {
        "id": "FC-THING-AMBER",
        "title": "The article targets a format that needs a verification run (AMBER)",
        "intent": "engine check - THING coverage matrix gate",
        "sev": WARN,
        "kind": "engine",
        "reason": "'Amber rows need one verification run first.' The mechanism is plausible - "
                  "usually a generic JSON/HTML/text path - but nobody has run it on a real corpus "
                  "of this shape.",
        "evidence": [RC + ":346"],
        "rewrite": "Run the extractor on a real corpus of this format, record the result, then "
                   "write the page and scope the copy to what you actually saw.",
    },
    {
        "id": "FC-COMP-EVIDENCE",
        "title": "A competitor comparison with no sourced evidence entry",
        "intent": "engine check - every 'vs <competitor>' needs an evidence entry with a URL",
        "sev": ERROR,
        "kind": "engine",
        "reason": "Competitor facts move - licences change, tiers change, projects get archived. "
                  "Every competitor claim in the research carries a URL for exactly this reason, "
                  "and several are tagged UNVERIFIED where one could not be found.",
        "evidence": [RC + ":97", RC + ":146"],
        "rewrite": "Add to the frontmatter: `evidence: [{claim: \"<competitor> gates RRF behind a "
                   "paid subscription\", source: \"https://...\"}]` - the competitor's own docs or "
                   "pricing page, not a third-party blog.",
    },
    {
        "id": "FC-COMP-ALTERNATIVE",
        "title": "A competitor comparison with no 'when to choose <competitor> instead' section",
        "intent": "engine check - comparison articles must name where the competitor wins",
        "sev": ERROR,
        "kind": "engine",
        "reason": ("The research states the rule for the hardest comparison: 'Against Manticore "
                   "specifically, XERJ loses on maturity (21 years vs pre-1.0) and on neural "
                   "embedding breadth ... A head-to-head with Manticore is the comparison most "
                   "likely to expose an overclaim - write it deliberately and honestly, or do not "
                   "write it at all.' The same standard applies to every competitor."),
        "evidence": [RC + ":161", RC + ":97"],
        "rewrite": "Add a section headed 'When to choose <competitor> instead' and put a real "
                   "reason in it. If you cannot name one, you do not understand the comparison well "
                   "enough to publish it.",
    },
    {
        "id": "FC-COMP-URL",
        "title": "Competitor evidence with no source URL",
        "intent": "engine check - competitor facts drift, so they need a link, not a repo path",
        "sev": WARN,
        "kind": "engine",
        "reason": "Licences, pricing tiers and feature gates move: Meilisearch put sharding behind "
                  "BUSL-1.1, ZincSearch was archived, Marqo's OSS was deprecated, Sourcegraph "
                  "closed its source and removed the free self-hosted tier. Every competitor row "
                  "in the research carries a URL, and the ones that could not get one are tagged "
                  "UNVERIFIED.",
        "evidence": [RC + ":97", RC + ":130"],
        "rewrite": "Point the evidence source at the competitor's own documentation or pricing "
                   "page: `source: \"https://manual.manticoresearch.com/Searching/Hybrid_search\"`.",
    },
    {
        "id": "FC-EV-INCOMPLETE",
        "title": "An `evidence:` entry is missing a claim or a source",
        "intent": "engine check - evidence block completeness (the block itself is optional)",
        "sev": ERROR,
        "kind": "engine",
        "reason": "An evidence entry with an empty claim or an empty source is worse than no entry "
                  "- it makes the article look sourced when it is not. An article with no "
                  "`evidence:` block at all is fine and renders no provenance section; this rule "
                  "only fires on an entry that exists and is half-written.",
        "evidence": [LC + ":647"],
        "rewrite": "Every entry needs both: `- claim: \"...\"` and `source: \"<repo path>|<url>|"
                   "Tier A: <file>\"`.",
    },
    {
        "id": "FC-EV-DANGLING",
        "title": "An `evidence:` source does not resolve",
        "intent": "engine check - the path does not exist, or it is not a URL or tier reference "
                  "(only fires on a source that is present; the block is optional)",
        "sev": ERROR,
        "kind": "engine",
        "reason": ("A source that does not resolve is the exact failure mode the re-adjudication "
                   "was commissioned to find: the previous agent cited a grep over "
                   "`xerj-engine/src/snapshot*.rs`, a path that does not exist in the tree. 'A grep "
                   "over a non-existent path returns zero hits and reads exactly like a proven "
                   "negative.'"),
        "evidence": [RJ + ":36"],
        "rewrite": "Use a path that exists in this repo (optionally with `:line`), an `https://` "
                   "URL, or `Tier A: <file>` / `Tier B: <file>`.",
    },
    {
        "id": "FC-EV-TIERC",
        "title": "An `evidence:` source cites Tier C or a retracted file",
        "intent": "engine check - the evidence block cannot launder a retracted number",
        "sev": ERROR,
        "kind": "engine",
        "reason": "Tier C is the 'NO backing evidence, do not cite' list. Naming it in an evidence "
                  "block does not create provenance.",
        "evidence": [LC + ":488", LC + ":653"],
        "rewrite": "Cite a Tier A or Tier B source, or drop the claim.",
    },
    {
        "id": "FC-SINGLE-NODE",
        "title": "Architecture / scale / production article without the single-node disclosure",
        "intent": "engine check - standing rule 3",
        "sev": WARN,
        "kind": "engine",
        "reason": "Standing rule 3: \"'Single-node' appears in any article that mentions "
                  "architecture, scale, HA, or production deployment.\"",
        "evidence": [LC + ":649"],
        "rewrite": "Add the words 'single-node' (or 'one node') where you describe the deployment.",
    },
]


# ======================================================================================
# 2. Tiered citable numbers
# ======================================================================================
#
# Key format: "<value><unit>" after normalisation - commas stripped, trailing zeros
# trimmed ("100.0%" -> "100%", "1.20x" -> "1.2x"), unit lowercased, "×" -> "x".
#
#   what      what the number actually measures
#   cite      where it lives in the ledger
#   context   optional regex; the tier only applies when it matches in the paragraph.
#             Used to stop a generic magnitude ("4x", "100 ms") from being classified
#             on sight. Without a context match the number falls through to
#             FC-NUM-UNKNOWN (WARN), never to a false ERROR.
#   exempt    optional regex; suppresses the finding entirely.
#   companion optional {"needs": regex, "window": chars, "why": text} - the loss or
#             caveat that must travel with the number.

_SCORECARD = "demo/playbooks/SCORECARD.md"

TIER_A = {
    "1.72x":    {"what": "bulk ingest, 191,286 vs 111,073 docs/s at 100k x 1 client", "cite": LC + ":446", "src": _SCORECARD,
                 "companion": {"needs": r"p99|lose|loss|slower|under (?:sustained )?write|13\.57|four losses",
                               "window": 900,
                               "why": "the benchmark board's four read-under-write p99 losses"}},
    "191286docs/s": {"what": "XERJ bulk ingest rate", "cite": LC + ":446", "src": _SCORECARD},
    "111073docs/s": {"what": "Elasticsearch 8.13.4 bulk ingest rate", "cite": LC + ":446", "src": _SCORECARD},
    "1.61x":    {"what": "index size on disk, 176.2 MB vs 283.0 MB at 100k docs", "cite": LC + ":447", "src": _SCORECARD,
                 "companion": {"needs": r"different measurement|not (?:a )?(?:better|like[- ]for[- ]like)|"
                                        r"176\.2|283|100k|not the (?:old|1\.20)",
                               "window": 600,
                               "why": "1.20x -> 1.61x is a DIFFERENT measurement, not a better result"}},
    "176.2mb":  {"what": "XERJ index size, 100k docs", "cite": LC + ":447", "src": _SCORECARD},
    "283mb":    {"what": "Elasticsearch index size, 100k docs", "cite": LC + ":447", "src": _SCORECARD},
    "1.18x":    {"what": "kNN k=10, 1.76 vs 2.08 ms p50 - scored a TIE", "cite": LC + ":449", "src": _SCORECARD,
                 "companion": {"needs": r"\btie\b|\bties\b|\bdraw\b|100(?:\.0)?\s*%\s*recall|recall@10|not a win",
                               "window": 400,
                               "why": "kNN 1.18x is a TIE at 100% recall on both engines, not a win"}},
    "1.76ms":   {"what": "XERJ kNN k=10 p50", "cite": LC + ":449", "src": _SCORECARD},
    "2.08ms":   {"what": "Elasticsearch kNN k=10 p50", "cite": LC + ":449", "src": _SCORECARD},
    "100%":     {"what": "kNN recall@10 - 100.0% on BOTH engines, a draw", "cite": LC + ":450", "src": _SCORECARD,
                 "context": r"recall",
                 "companion": {"needs": r"both|draw|\btie\b|vs 100|es (?:also|too)|equal",
                               "window": 300,
                               "why": "recall@10 is 100.0% on BOTH engines - it is a draw, and the "
                                      "board counts it as one of the 55 wins"}},
    "30.14x":   {"what": "percentile_ranks aggregation, 0.23 vs 6.98 ms", "cite": LC + ":451", "src": _SCORECARD},
    "26.67x":   {"what": "percentiles aggregation", "cite": LC + ":451", "src": _SCORECARD},
    "23.09x":   {"what": "median_absolute_deviation aggregation", "cite": LC + ":451", "src": _SCORECARD},
    "19.78x":   {"what": "scripted_metric aggregation", "cite": LC + ":451", "src": _SCORECARD},
    "6.98ms":   {"what": "ES percentile_ranks latency", "cite": LC + ":451", "src": _SCORECARD},
    "0.23ms":   {"what": "XERJ percentile_ranks latency", "cite": LC + ":451", "src": _SCORECARD},
    "1.32x":    {"what": "low end of the compound-query range", "cite": LC + ":452", "src": _SCORECARD},
    "3.16x":    {"what": "high end of the compound-query range", "cite": LC + ":452", "src": _SCORECARD},
    "2.41x":    {"what": "query_string (and range) - current value", "cite": LC + ":452", "src": _SCORECARD},
    "2.02x":    {"what": "multi_match", "cite": LC + ":452", "src": _SCORECARD},
    "1.93x":    {"what": "terms query", "cite": LC + ":452", "src": _SCORECARD},
    "2x":       {"what": "match_bool_prefix (2.00x)", "cite": LC + ":452", "src": _SCORECARD,
                 "context": r"match_bool_prefix|compound quer"},
    "1.64x":    {"what": "bool query - the CURRENT value that replaced the retracted 11.5x",
                 "cite": RJ + ":24", "src": _SCORECARD},
    "4.01x":    {"what": "wildcard query - current value (replaced 6.8x)", "cite": RJ + ":24", "src": _SCORECARD},
    "1.33x":    {"what": "terms aggregation - current value (replaced 1.15x)", "cite": LC + ":493", "src": _SCORECARD},
    "13.57ms":  {"what": "XERJ read p99 under sustained write - a LOSS", "cite": LC + ":453", "src": _SCORECARD},
    "3.45ms":   {"what": "ES read p99 in the same cell", "cite": LC + ":453", "src": _SCORECARD},
    "13.45ms":  {"what": "XERJ read p99 under sustained write - a LOSS", "cite": LC + ":453", "src": _SCORECARD},
    "6.76ms":   {"what": "ES read p99 in the same cell", "cite": LC + ":453", "src": _SCORECARD},
    "10.27ms":  {"what": "XERJ read p99 under sustained write - a LOSS", "cite": LC + ":453", "src": _SCORECARD},
    "3.68ms":   {"what": "ES read p99 in the same cell", "cite": LC + ":453", "src": _SCORECARD},
    "10.74ms":  {"what": "XERJ read p99 under sustained write - a LOSS", "cite": LC + ":453", "src": _SCORECARD},
    "3.57ms":   {"what": "ES read p99 in the same cell", "cite": LC + ":453", "src": _SCORECARD},
    "40000docs/s": {"what": "open-loop writer rate during the mixed cells", "cite": LC + ":453", "src": _SCORECARD},
    "39626docs/s": {"what": "achieved XERJ write rate in the mixed cells", "cite": LC + ":453", "src": _SCORECARD},
    "39688docs/s": {"what": "achieved ES write rate in the mixed cells", "cite": LC + ":453", "src": _SCORECARD},
    "17.24x":   {"what": "doc-index context ratio, large_literal regime", "cite": LC + ":467",
                 "src": "demo/usecases/doc-index/results.json",
                 "companion": {"needs": r"0\.23|inversion|literal inversion|loses|loss",
                               "window": 500,
                               "why": "always publish 17.24x and the 0.23x literal inversion together"}},
    "85.68x":   {"what": "doc-index context ratio, single-best-passage regime", "cite": LC + ":467",
                 "src": "demo/usecases/doc-index/results.json"},
    "0.23x":    {"what": "the literal inversion - a doc-index LOSS", "cite": LC + ":467",
                 "src": "demo/usecases/doc-index/results.json"},
    "95.5%":    {"what": "doc-index coverage, 21/22", "cite": LC + ":467", "src": "demo/usecases/doc-index/results.json"},
    "63.6%":    {"what": "grep baseline coverage, 14/22", "cite": LC + ":467", "src": "demo/usecases/doc-index/results.json"},
    "109.78x":  {"what": "agent-gate analytics regime - XERJ uses 109.78x MORE tokens (a loss)",
                 "cite": LC + ":468", "src": "demo/agent-gate/RESULTS_analytics.txt"},
    "1.14x":    {"what": "agent-gate retrieval regime - 1.14x fewer tokens", "cite": LC + ":469",
                 "src": "demo/agent-gate/RESULTS_retrieval.txt"},
    "442x":     {"what": "token byte profile, join materialized vs denormalized", "cite": LC + ":470",
                 "src": "docs/TOKEN_USAGE.md"},
    "6x":       {"what": "token byte profile, filter_path", "cite": LC + ":470", "src": "docs/TOKEN_USAGE.md",
                 "context": r"filter_path|token"},
    "99.8%":    {"what": "ES-YAML conformance, 1,366 / 1,369", "cite": LC + ":471",
                 "src": ".github/workflows/ci.yml:363-443",
                 "companion": {"needs": r"curated|200[- ]file|subset|catch:? |unverified",
                               "window": 400,
                               "why": "mandatory caveat: a curated 200-file subset, and catch: "
                                      "assertions are unverified"}},
    "12.9x":    {"what": "ONNX vs default embedder AT THE EMBEDDING LAYER, 116.671 vs 9.045 docs/s",
                 "cite": LC + ":472", "src": "docs/EXPERIMENTAL_ONNX.md:227-231",
                 "companion": {"needs": r"embedding layer|embedder only|not end[- ]to[- ]end",
                               "window": 400,
                               "why": "the 12.90x is at the embedding layer only, not end-to-end"}},
    "116.671docs/s": {"what": "ONNX embedding throughput", "cite": LC + ":472", "src": "docs/EXPERIMENTAL_ONNX.md"},
    "9.045docs/s": {"what": "default embedder throughput", "cite": LC + ":472", "src": "docs/EXPERIMENTAL_ONNX.md"},
    "36.06mib": {"what": "stripped binary size, Candle", "cite": LC + ":473", "src": "docs/EXPERIMENTAL_ONNX.md:244-248"},
    "54.81mib": {"what": "Candle + ONNX binary size", "cite": LC + ":473", "src": "docs/EXPERIMENTAL_ONNX.md:244-248"},
    "52.49mib": {"what": "ONNX-only binary size", "cite": LC + ":473", "src": "docs/EXPERIMENTAL_ONNX.md:244-248"},
    "36mb":     {"what": "full binary, rounded", "cite": LC + ":542", "src": "landing/llms.txt:176"},
    "23mb":     {"what": "slim binary (--no-default-features)", "cite": LC + ":542", "src": "landing/llms.txt:176"},
    "400mb":    {"what": "idle RSS baseline", "cite": LC + ":540", "src": "landing/docs/operations.html:218"},
}

TIER_B = {
    "199x":     {"what": "WordPress audit, ~26,000 vs ~5,200,000 tokens", "cite": LC + ":481",
                 "src": "docs/examples/ast-vuln-graph/",
                 "companion": {"needs": r"estimate|modell?ed|not (?:an )?executed|baseline is a model",
                               "window": 500,
                               "why": "the ~5.2M read-all baseline is a MODEL/ESTIMATE, not an executed run"}},
    "171x":     {"what": "XERJ self-audit, 10,533 vs 1,805,277 tokens", "cite": LC + ":482",
                 "src": "docs/examples/rust-ast-audit/",
                 "companion": {"needs": r"tiktoken|proxy|cl100k|in[- ]sample",
                               "window": 500,
                               "why": "token counting is tiktoken cl100k_base - a proxy, not real LLM "
                                      "tokens - and the 6/6 detection score is explicitly in-sample"}},
    "32x":      {"what": "XERJ self-audit triage, 84,122 vs 2,715,003 tokens", "cite": LC + ":482",
                 "src": "docs/examples/rust-ast-audit/"},
    "19.6x":    {"what": "grep dedup-union floor in the self-audit", "cite": LC + ":482",
                 "src": "docs/examples/rust-ast-audit/"},
    "3.6s":     {"what": "WordPress index build", "cite": LC + ":481", "src": "docs/examples/ast-vuln-graph/"},
    "3.2s":     {"what": "self-audit index build", "cite": LC + ":482", "src": "docs/examples/rust-ast-audit/"},
    "1.9s":     {"what": "self-audit graph build", "cite": LC + ":482", "src": "docs/examples/rust-ast-audit/"},
    "518mb":    {"what": "autoindex corpus A", "cite": LC + ":483", "src": "demo/usecases/autoindex/AGENT_SIM_SCORECARD.md"},
    "38.1s":    {"what": "autoindex corpus A wall time", "cite": LC + ":483", "src": "demo/usecases/autoindex/AGENT_SIM_SCORECARD.md"},
    "923mb":    {"what": "autoindex corpus B", "cite": LC + ":483", "src": "demo/usecases/autoindex/AGENT_SIM_SCORECARD.md"},
    "77.3s":    {"what": "autoindex corpus B wall time", "cite": LC + ":483", "src": "demo/usecases/autoindex/AGENT_SIM_SCORECARD.md"},
    "33.7krec/s": {"what": "autoindex throughput on corpus B", "cite": LC + ":483",
                   "src": "demo/usecases/autoindex/AGENT_SIM_SCORECARD.md"},
    "245mb":    {"what": "autoindex client peak RSS on 4.61 GB", "cite": LC + ":483", "src": "demo/usecases/autoindex/AGENT_SIM_SCORECARD.md"},
    "257mb":    {"what": "autoindex client peak RSS on 4.61 GB", "cite": LC + ":483", "src": "demo/usecases/autoindex/AGENT_SIM_SCORECARD.md"},
    "160mb":    {"what": "autoindex client peak RSS on 923 MB", "cite": LC + ":483", "src": "demo/usecases/autoindex/AGENT_SIM_SCORECARD.md"},
    "168mb":    {"what": "autoindex client peak RSS on 923 MB", "cite": LC + ":483", "src": "demo/usecases/autoindex/AGENT_SIM_SCORECARD.md"},
    "4.61gb":   {"what": "autoindex corpus C", "cite": LC + ":483", "src": "demo/usecases/autoindex/AGENT_SIM_SCORECARD.md"},
    "18x":      {"what": "April 2026 head-to-head, index creation", "cite": LC + ":485",
                 "src": "engine/reports/2026-04-25*",
                 "companion": {"needs": r"april|2026-04|superseded|cold start|older board",
                               "window": 500,
                               "why": "the April 2026 board is superseded by the July board - cite it "
                                      "only for cold start / RSS, and always with the date"}},
    "19x":      {"what": "April 2026 head-to-head, PUT p50", "cite": LC + ":485", "src": "engine/reports/2026-04-25*"},
    "2.4x":     {"what": "April 2026 head-to-head, GET", "cite": LC + ":485", "src": "engine/reports/2026-04-25*"},
    "2519mb":   {"what": "ES RSS, April 2026 board", "cite": LC + ":485", "src": "engine/reports/2026-04-25*"},
    "191mb":    {"what": "XERJ RSS, April 2026 board", "cite": LC + ":485", "src": "engine/reports/2026-04-25*"},
    "7.04s":    {"what": "ES cold start", "cite": LC + ":485", "src": "engine/reports/2026-04-25*"},
    "0.4s":     {"what": "XERJ cold start", "cite": LC + ":485", "src": "engine/reports/2026-04-25*"},
    "17.6x":    {"what": "cold-start ratio, April 2026", "cite": LC + ":485", "src": "engine/reports/2026-04-25*"},
    "3.25s":    {"what": "ES SIGTERM shutdown", "cite": LC + ":485", "src": "engine/reports/2026-04-25*"},
    "0.24s":    {"what": "XERJ SIGTERM shutdown", "cite": LC + ":485", "src": "engine/reports/2026-04-25*"},
    "1.88x":    {"what": "bulk 100K - ES is 1.88x FASTER (a published loss)", "cite": LC + ":485",
                 "src": "engine/reports/2026-04-25*"},
    "6008939docs/s": {"what": "burst ingest, WAL-durable and in-memtable, 2026-04-27 capture",
                      "cite": LC + ":486", "src": "landing/demo/index.html",
                      "companion": {"needs": r"burst|in[- ]memtable|2026-04-27|v0\.9",
                                    "window": 400,
                                    "why": "only citable with the date and the 'burst, in-memtable' qualifier"}},
    "409809docs/s": {"what": "segment-durable ingest on 655,147 docs", "cite": LC + ":486", "src": "landing/demo/index.html"},
    "50494docs/s": {"what": "sustained ingest over 60,928,671 docs", "cite": LC + ":486", "src": "landing/demo/index.html"},
    "6.7x":     {"what": "12 GiB -> 1.8 GB compression, v0.9-era, NOT re-validated", "cite": LC + ":486",
                 "src": "landing/demo/index.html"},
    "1206.65s": {"what": "sustained-ingest wall time", "cite": LC + ":486", "src": "landing/demo/index.html"},
    "12gib":    {"what": "sustained-ingest corpus size", "cite": LC + ":486", "src": "landing/demo/index.html"},
    "1.8gb":    {"what": "sustained-ingest index size", "cite": LC + ":486", "src": "landing/demo/index.html"},
    "95405docs/s": {"what": "XERJ bulk 100K, April 2026 - the losing side", "cite": LC + ":485", "src": "engine/reports/2026-04-25*"},
    "179574docs/s": {"what": "ES bulk 100K, April 2026 - the winning side", "cite": LC + ":485", "src": "engine/reports/2026-04-25*"},
}

TIER_C = {
    "11.5x":  {"what": "bool query - RETRACTED 2026-07-28", "now": "1.64x", "cite": LC + ":493"},
    "6.9x":   {"what": "query_string - RETRACTED", "now": "2.41x", "cite": LC + ":493", "context": r"query_string|query string"},
    "6.8x":   {"what": "wildcard - RETRACTED", "now": "4.01x", "cite": LC + ":493", "context": r"wildcard"},
    "3.5x":   {"what": "range query - RETRACTED", "now": "2.41x", "cite": LC + ":493", "context": r"\brange\b"},
    "1.15x":  {"what": "terms aggregation - RETRACTED", "now": "1.33x", "cite": LC + ":493", "context": r"terms agg|aggregation"},
    "3.4x":   {"what": "kNN - RETRACTED", "now": "1.18x, a TIE", "cite": LC + ":494",
               "context": r"\bknn\b|vector|ann\b|nearest neighb"},
    "2.64ms": {"what": "ES kNN latency in the retracted board", "cite": LC + ":494", "context": r"\bknn\b|vector"},
    "0.78ms": {"what": "XERJ kNN latency in the retracted board", "cite": LC + ":494", "context": r"\bknn\b|vector"},
    "1.2x":   {"what": "disk ratio - RETRACTED; 1.61x is a DIFFERENT measurement, not a better result",
               "now": "1.61x", "cite": LC + ":495",
               "context": r"disk|index size|storage|smaller|footprint|on[- ]disk"},
    "672.5mb": {"what": "retracted disk figure", "cite": LC + ":495"},
    "806.7mb": {"what": "retracted disk figure", "cite": LC + ":495"},
    "18gb":   {"what": "SQ8 vector memory - no results file, no harness, no method, no date",
               "cite": LC + ":497", "context": r"vector|memory|sq8|scalar8|quantiz|sku|float32"},
    "92gb":   {"what": "float32 vector memory - arithmetic does not reconcile (10M x 1536 x 4 B = 61.44 GB)",
               "cite": LC + ":497", "context": r"vector|memory|sq8|scalar8|quantiz|sku|float32"},
    "5.1x":   {"what": "SQ8 memory ratio - both sides of the ratio are wrong", "cite": LC + ":497",
               "context": r"vector|memory|sq8|scalar8|quantiz|sku|float32"},
    "80%":    {"what": "'~80% infrastructure cost reduction' - prose only, no model", "cite": LC + ":498",
               "context": r"cost|tco|infrastructure|spend|saving|cheaper|bill"},
    "15ms":   {"what": "retail latency ladder - no source anywhere in the repo", "cite": LC + ":499",
               "context": r"retail|ladder|storefront|sku|catalog"},
    "25ms":   {"what": "retail latency ladder - no source", "cite": LC + ":499", "context": r"retail|ladder|storefront|sku|catalog"},
    "40ms":   {"what": "retail latency ladder - no source", "cite": LC + ":499", "context": r"retail|ladder|storefront|sku|catalog"},
    "60ms":   {"what": "retail latency ladder - no source", "cite": LC + ":499", "context": r"retail|ladder|storefront|sku|catalog"},
    "100ms":  {"what": "retail latency ladder - no source", "cite": LC + ":499", "context": r"retail|ladder|storefront|sku|catalog"},
    "3.8x":   {"what": "retail latency ratio - no source", "cite": LC + ":499", "context": r"retail|ladder|storefront|sku|catalog"},
    "620mb":  {"what": "'Elasticsearch package ~620 MB' - unsourced", "cite": LC + ":500",
               "context": r"elasticsearch|package|download|tarball|distribution"},
    "800mb":  {"what": "'Elasticsearch Docker image ~800 MB' - unsourced, and inconsistent with the 620 MB figure",
               "cite": LC + ":500", "context": r"elasticsearch|docker|image|container"},
    "2.53x":  {"what": "migration-recipe match ratio - cited to a file that contains none of them",
               "cite": LC + ":501", "context": r"match|migrat|recipe|ingest"},
    "1.94x":  {"what": "migration-recipe match_all ratio - unsourced", "cite": LC + ":501",
               "context": r"match_all|migrat|recipe|ingest"},
    "6.92x":  {"what": "migration-recipe query_string ratio - unsourced", "cite": LC + ":501",
               "context": r"query_string|migrat|recipe|ingest"},
    "5x":     {"what": "'USERS REPORT ~5x FEWER TOKENS' - field testimony, not measurement. "
                       "No n, no method, no transcripts. Never upgrade it to 'measured'",
               "cite": LC + ":502", "context": r"token",
               "exempt": r"users report|user report|field testimony|testimony|not a controlled run|anecdot"},
    "5.3x":   {"what": "'5.3x fewer tokens on 234 files' - headlined in two places with no results file",
               "cite": LC + ":503", "context": r"token|file|loc\b"},
    "99.95%": {"what": "availability SLA - forward-looking commercial target on unshipped replication",
               "cite": LC + ":505"},
    "99.99%": {"what": "availability SLA - forward-looking commercial target on unshipped replication",
               "cite": LC + ":505"},
    "61.6mb": {"what": "binary size in docs/WHY_XERJ.md - ~1.7x off; measured is 36.06 MiB",
               "cite": LC + ":619"},
    "58.2mb": {"what": "slim binary size in docs/WHY_XERJ.md - measured is ~23 MB", "cite": LC + ":619"},
}

# Structural / methodological constants that are not performance claims.
# Matching one of these suppresses FC-NUM-UNKNOWN.
NUMBER_ALLOW = {
    "4gb":   r"heap|elasticsearch|harness|methodolog",
    "8080":  r"port",
    "9200":  r"port|localhost|127\.0\.0\.1",
    "9201":  r"port|localhost|elasticsearch",
    "100%":  r"",          # also Tier A above; harmless either way
    "0%":    r"",
    "50%":   r"",
    "10%":   r"",
    "20%":   r"",
    "1gb":   r"heap|ram|memory|example",
    "2gb":   r"heap|ram|memory|example",
    "8gb":   r"heap|ram|memory|example",
    "16gb":  r"heap|ram|memory|example",
    "1ms":   r"",
    "1s":    r"",
    "2s":    r"",
    "5s":    r"",
    "10s":   r"",
    "30s":   r"",
    "60s":   r"",
}


# ======================================================================================
# 3. THING coverage matrix
# ======================================================================================
#
# Baked from RESEARCH-competitors-longtail.md:346-381 so that CI is deterministic.
# `factcheck.py --check-matrix` re-parses that file and reports any drift.

GREEN, AMBER, RED = "GREEN", "AMBER", "RED"

THING_MATRIX = [
    {"thing": "Source code / monorepo", "status": GREEN, "mech": "code.rs - tree-sitter, 34 languages",
     "cite": RC + ":352", "gate": "Write. Do not claim language parity with 158-language competitors.",
     "aliases": [r"source code", r"monorepo", r"codebase", r"\brust code\b", r"\bpython code\b"]},
    {"thing": "CSV / TSV", "status": GREEN, "mech": "csv_x.rs + type inference", "cite": RC + ":353",
     "gate": "Write. Pair with aggregations - that is the differentiator vs grep.",
     "aliases": [r"\bcsv\b", r"\btsv\b"]},
    {"thing": "JSON / JSONL", "status": GREEN, "mech": "json.rs, jsonl.rs", "cite": RC + ":354",
     "gate": "Write.", "aliases": [r"\bjsonl?\b", r"\bndjson\b"]},
    {"thing": "YAML / XML config", "status": GREEN, "mech": "yaml_x.rs, xml_x.rs", "cite": RC + ":355",
     "gate": "Write.", "aliases": [r"\byaml\b", r"\bxml\b"]},
    {"thing": "SQLite database", "status": GREEN, "mech": "sqlite_x.rs", "cite": RC + ":356",
     "gate": "Write - genuinely rare capability, near-zero competition.", "aliases": [r"sqlite"]},
    {"thing": "SQL dump", "status": GREEN, "mech": "sqldump.rs", "cite": RC + ":357",
     "gate": "Write - essentially uncontested.", "aliases": [r"sql dump", r"\.sql dump", r"mysqldump", r"pg_dump"]},
    {"thing": "PDF library", "status": GREEN, "mech": "pdf.rs", "cite": RC + ":358",
     "gate": "Write. Highest-ratio page in the whole report.", "aliases": [r"\bpdfs?\b"]},
    {"thing": "Word documents", "status": GREEN, "mech": "docx.rs", "cite": RC + ":359",
     "gate": "Write.", "aliases": [r"\bdocx\b", r"word document"]},
    {"thing": "HTML export / site dump", "status": GREEN, "mech": "html.rs", "cite": RC + ":360",
     "gate": "Write. Note: no crawler - the files must already be on disk.",
     "aliases": [r"\bhtml export\b", r"site dump", r"\bhtml files\b"]},
    {"thing": "Log files", "status": GREEN, "mech": "logs.rs", "cite": RC + ":361",
     "gate": "Write, SCOPED TO SINGLE-NODE AND MODEST VOLUME. xerj-logs is not wired into the serving path.",
     "aliases": [r"\blog files?\b", r"\blogfiles?\b"]},
    {"thing": "Plain text / Markdown", "status": GREEN, "mech": "txt.rs", "cite": RC + ":362",
     "gate": "Write.", "aliases": [r"\bmarkdown\b", r"plain text", r"\btext files?\b", r"\bnotes\b"]},
    {"thing": "Unity project", "status": GREEN, "mech": "unity.rs", "cite": RC + ":363",
     "gate": "Write - unique, uncontested niche nobody else serves.", "aliases": [r"\bunity\b"]},
    {"thing": "Motion-capture data", "status": GREEN, "mech": "bvh.rs", "cite": RC + ":364",
     "gate": "Curiosity value; negligible volume. Low priority.", "aliases": [r"\bbvh\b", r"motion[- ]capture", r"\bmocap\b"]},

    {"thing": "Obsidian vault / wikilinks", "status": AMBER, "mech": "txt.rs + detect/wikilink.rs, detect/mdlink.rs",
     "cite": RC + ":365", "gate": "Verify wikilink graph extraction on a real vault before publishing.",
     "aliases": [r"obsidian", r"wikilink", r"\bvault\b", r"zettelkasten"]},
    {"thing": "Jupyter notebooks", "status": AMBER, "mech": ".ipynb is JSON -> json.rs", "cite": RC + ":366",
     "gate": "Verify cell-level extraction. Do not claim notebook-aware handling.",
     "aliases": [r"jupyter", r"\bipynb\b", r"\bnotebooks?\b"]},
    {"thing": "Slack export", "status": AMBER, "mech": "Slack ships JSON -> json.rs", "cite": RC + ":367",
     "gate": "Verify on a real export. Scope copy to 'the JSON files Slack gives you', not 'Slack integration'.",
     "aliases": [r"\bslack\b"]},
    {"thing": "Confluence / Notion export", "status": AMBER, "mech": "HTML or Markdown -> html.rs / txt.rs",
     "cite": RC + ":368", "gate": "Verify on a real export first.", "aliases": [r"confluence", r"notion"]},
    {"thing": "OpenAPI / API spec", "status": AMBER, "mech": "JSON/YAML -> json.rs / yaml_x.rs", "cite": RC + ":369",
     "gate": "Works as structured data; no spec-aware handling. Do not oversell.",
     "aliases": [r"openapi", r"swagger", r"api spec"]},
    {"thing": "Research papers / arXiv", "status": AMBER, "mech": "pdf.rs + detect/pathcite.rs, cratecite.rs",
     "cite": RC + ":370", "gate": "PDF path is solid; verify the citation detectors behave on papers.",
     "aliases": [r"arxiv", r"research papers?", r"academic papers?"]},
    {"thing": "Chat transcripts / meeting notes", "status": AMBER, "mech": "txt.rs / json.rs", "cite": RC + ":371",
     "gate": "No speaker- or turn-aware schema. Say so.",
     "aliases": [r"chat transcripts?", r"meeting notes", r"transcripts?"]},
    {"thing": "Git history / commits", "status": AMBER, "mech": "no extractor - git log piped to JSONL",
     "cite": RC + ":372", "gate": "Only publishable as an explicit RECIPE, never as native support.",
     "aliases": [r"git history", r"git log", r"commit history", r"\bcommits\b"]},
    {"thing": "Container / Docker logs", "status": AMBER, "mech": "logs.rs after redirection to disk",
     "cite": RC + ":373", "gate": "No log shipper, no collector. Scope carefully.",
     "aliases": [r"docker logs", r"container logs", r"kubernetes logs", r"\bk8s logs\b"]},
    {"thing": "Browser history", "status": AMBER, "mech": "SQLite file -> sqlite_x.rs", "cite": RC + ":381",
     "gate": "Plausible and cheap to verify - potentially a fun, uncontested page.",
     "aliases": [r"browser history", r"chrome history", r"firefox history"]},

    {"thing": "S3 bucket", "status": RED, "mech": "filesystem walk only", "cite": RC + ":374",
     "gate": "Do not imply native S3 ingest. Quickwit owns object-storage indexing.",
     "aliases": [r"\bs3 bucket\b", r"s3://", r"object stor(?:e|age)",
                 r"\bs3\b[^\n]{0,20}\b(?:ingest|index|indexing|search|scan|crawl)\b",
                 r"\b(?:ingest|index|indexing|search|scan|crawl)\w*\b[^\n]{0,20}\bs3\b"]},
    {"thing": "Email archive (mbox / PST)", "status": RED, "mech": "no extractor", "cite": RC + ":375",
     "gate": "Roadmap item. Real demand - worth an issue, not a page.",
     "aliases": [r"\bmbox\b", r"\bpst\b", r"email archive", r"\bemails?\b", r"mailbox"]},
    {"thing": "Screenshots / scanned docs / OCR", "status": RED, "mech": "no extractor", "cite": RC + ":376",
     "gate": "Paperless-ngx owns this.", "aliases": [r"\bocr\b", r"scanned docs?", r"screenshots?", r"scanned documents?"]},
    {"thing": "Ebooks (EPUB)", "status": RED, "mech": "no extractor", "cite": RC + ":377", "gate": "-",
     "aliases": [r"\bepub\b", r"\bebooks?\b"]},
    {"thing": "Excel / PowerPoint", "status": RED, "mech": "no extractor", "cite": RC + ":378",
     "gate": "Notable gap - DOCX is covered but XLSX/PPTX are not. Likely the highest-value missing extractor.",
     "aliases": [r"\bxlsx?\b", r"\bexcel\b", r"\bpptx?\b", r"powerpoint", r"spreadsheets?"]},
    {"thing": "Parquet", "status": RED, "mech": "no extractor", "cite": RC + ":379", "gate": "-",
     "aliases": [r"parquet"]},
    {"thing": "Audio / video transcripts", "status": RED, "mech": "no extractor", "cite": RC + ":380", "gate": "-",
     "aliases": [r"audio", r"\bvideo\b", r"podcasts?", r"\bmp3\b", r"\bmp4\b"]},
]


# ======================================================================================
# 4. Competitors
# ======================================================================================

COMPETITORS = [
    ("Elasticsearch",   [r"elasticsearch", r"\bes\s*8\.\d", r"\belastic\b"]),
    ("OpenSearch",      [r"opensearch"]),
    ("Manticore Search", [r"manticore"]),
    ("Meilisearch",     [r"meilisearch", r"meili"]),
    ("Typesense",       [r"typesense"]),
    ("Qdrant",          [r"qdrant"]),
    ("Weaviate",        [r"weaviate"]),
    ("Milvus",          [r"milvus", r"zilliz"]),
    ("Vespa",           [r"\bvespa\b"]),
    ("Pinecone",        [r"pinecone"]),
    ("Chroma",          [r"\bchroma(?:db)?\b"]),
    ("LanceDB",         [r"lancedb"]),
    ("Bleve",           [r"\bbleve\b"]),
    ("Quickwit",        [r"quickwit"]),
    ("ZincSearch",      [r"zincsearch", r"\bzinc\b"]),
    ("Sourcegraph",     [r"sourcegraph"]),
    ("Zoekt",           [r"\bzoekt\b"]),
    # Code- and shell-side alternatives.  These are the tools a coding agent
    # actually reaches for, and /compare/xerj-vs-ripgrep-for-code-agents.md
    # was published against ripgrep while ripgrep was absent from this list,
    # so FC-COMP-EVIDENCE and FC-COMP-ALTERNATIVE never ran on it.
    ("ripgrep",         [r"\bripgrep\b", r"\brg\b"]),
    ("ast-grep",        [r"ast-grep", r"astgrep"]),
    ("grep",            [r"\bgrep\b", r"\begrep\b", r"\bfgrep\b", r"\bzgrep\b"]),
    ("Sourcebot",       [r"sourcebot"]),
    ("Algolia",         [r"algolia"]),
    ("Apache Solr",     [r"\bsolr\b"]),
    ("ClickHouse",      [r"clickhouse"]),
    ("pgvector",        [r"pgvector"]),
    ("ParadeDB",        [r"paradedb", r"pg_search"]),
    ("Postgres FTS",    [r"tsvector", r"postgres full[- ]text"]),
    ("SQLite FTS5",     [r"\bfts5\b", r"sqlite full[- ]text", r"sqlite fts"]),
    ("Sonic",           [r"\bsonic\b"]),
    ("Tantivy",         [r"tantivy", r"\btoshi\b"]),
    ("Marqo",           [r"\bmarqo\b"]),
    ("turbopuffer",     [r"turbopuffer"]),
    ("Redis",           [r"\bredis\b", r"redisearch"]),
    ("Splunk",          [r"\bsplunk\b"]),
    ("Datadog",         [r"datadog"]),
    ("Grafana Loki",    [r"\bloki\b"]),
    ("VictoriaLogs",    [r"victorialogs", r"victoriametrics"]),
    ("FAISS",           [r"\bfaiss\b"]),
    ("sqlite-vec",      [r"sqlite-vec"]),
    ("Vald",            [r"\bvald\b"]),
    ("S3 Vectors",      [r"s3 vectors"]),
]

# Facts about competitors that the research already had to correct. Any article that
# contradicts one of these is wrong even if it carries a source URL.
COMPETITOR_FACTS = {
    "Manticore Search": [
        "Ships LOCAL ONNX embedding models with no API key (all-MiniLM-L6-v2, Sentence "
        "Transformers, Qwen/Llama/Mistral/Gemma) - RC:146",
        "Native RRF: OPTION fusion_method='rrf' with MATCH() + KNN() in one query - RC:146",
        "Single C++ daemon, no JVM, no external deps. GPL-3.0. 21 years of lineage - RC:97",
        "XERJ LOSES to Manticore on maturity and on neural embedding breadth - RC:161",
    ],
    "Elasticsearch": [
        "RRF and the Inference API are behind PAID subscriptions; free/Basic has neither - RC:131",
    ],
    "Meilisearch": [
        "Sharding moved behind BUSL-1.1 on 2025-08-27 - RC:132",
        "No RRF and no BM25 - semanticRatio interpolation only - RC:146",
    ],
    "ZincSearch": ["Archived 2026-08-18 - RC:130"],
    "Marqo": ["OSS deprecated - RC:130"],
    "Typesense": ["Hybrid is 0.7*keyword + 0.3*semantic, NOT canonical RRF - RC:146"],
    "Qdrant": ["Built-in inference is CLOUD ONLY; native RRF and DBSF are real - RC:146"],
    "Chroma": ["Hybrid is CLOUD ONLY; single-node support is documented as 'planned' - RC:146"],
    "Vespa": ["$20,000/month enterprise minimum, on their own calculator - RC:137"],
    "Sourcegraph": ["Closed-sourced mid-2023; free self-hosted tier is gone - RC:97"],
    "Quickwit": ["Acquired into Datadog, January 2025 - RC (Category B)"],
    "Pinecone": ["$50/mo minimum; turbopuffer cut to $16/mo - RC:133"],
}


def rule_by_id(rid):
    for r in RULES:
        if r["id"] == rid.upper():
            return r
    return None
