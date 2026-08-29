#!/usr/bin/env python3
"""Per-page SEO metadata for landing/ — hand-written, reviewed, versioned.

`scripts/seo/fix_heads.py` reads this table and nothing else decides what a
page's `<title>`, `<meta name="description">`, structured-data type or
breadcrumb label is.  Keeping it in one file means a title change is a
one-line diff instead of a hunt through 80 HTML files.

Fields
    label        the page's own breadcrumb name (last ListItem).  Required.
    kind         which JSON-LD body the page gets:
                   "software"    -> SoftwareApplication  (the product itself)
                   "collection"  -> CollectionPage       (a hub that lists pages)
                   "techarticle" -> TechArticle          (reference documentation)
                   "article"     -> Article              (a case study)
                   "webpage"     -> WebPage              (everything else)
    description  the meta description AND og:description/twitter:description.
                 Target 110-165 characters: Google states no hard limit but
                 truncates "to fit the device width".  Omit to keep whatever
                 the file already has (only done where the existing one is
                 already unique and correctly sized).
    title        overrides the file's <title>.  Omit to keep the existing one.
                 Target <= 60 characters.

Every description below was written against that page's actual body copy.
None of them describe content the page does not contain — Google treats a
description that misrepresents the page as a reason to ignore it and generate
its own snippet.
"""

from __future__ import annotations

import pathlib

# Site-name suffix conventions already in use on the site are preserved:
# marketing pages read "XERJ.ai — Topic", docs read "XERJ.ai · Docs · Topic".
# Only the nine over-length titles and the six duplicated ones are rewritten.

PAGES: dict[str, dict[str, str]] = {

    # ── root / product ──────────────────────────────────────────────────────
    "index.html": dict(
        label="Home", kind="software",
        description="The unified search engine for AI: connect it, run xerj autoindex, and it works. One Rust binary replaces search, vector DB, embeddings and agent memory.",
    ),
    # `404.html` is what makes Cloudflare Pages return a real HTTP 404.  With
    # no top-level 404.html the project is treated as a single-page app and
    # every unmatched path is answered with `index.html` and a 200 (see
    # urlmap.NOINDEX and seo_lint rule 67).  It is registered here like any
    # other hand-written page so `fix_heads.py` owns its <head>; `noindex`
    # comes from urlmap.NOINDEX, and it is kept out of sitemap.xml.
    "404.html": dict(
        label="Page not found", kind="webpage",
        description="This URL has no page behind it on xerj.org. The response is a real HTTP 404, not a copy of the homepage — the docs, the answers hub and the site map are here.",
    ),
    "product.html": dict(
        label="Product", kind="software",
        description="One Rust binary holding logs, vectors and agent memory instead of three glued systems — with live dashboard visualizations and benchmarks against ES 8.13.",
    ),
    "for-agents.html": dict(
        label="For AI agents", kind="software",
        title="XERJ.ai — For AI agents: search, data & memory",
        description="The search, data and memory layer for AI agents. One binary: zero-config folder indexing, keyword, semantic, vector and hybrid search, per-agent memory.",
    ),
    "playground.html": dict(
        label="Playground", kind="software",
        description="Live XERJ dashboards running on seeded data — click filters, drill down, read real queries. Leave a work email to unlock the interactive playground.",
    ),
    "playground/index.html": dict(
        label="Dashboards", kind="webpage",
        description="The XERJ observability dashboards application. Deliberately not indexed — open it from the playground gate at xerj.org/playground instead.",
    ),
    "benchmarks/index.html": dict(
        label="Benchmarks", kind="software",
        title="XERJ.ai — Benchmarks: performance, retrieval, token savings, ZTA",
        description="Every number traces to a run: the 88-cell Elasticsearch board, code retrieval measured against grep and a trigram engine, 6.5x cheaper agent runs, and the zero-token architecture behind them.",
    ),
    "benchmarks/elasticsearch.html": dict(
        label="ES scorecard", kind="software",
        title="XERJ vs Elasticsearch — reproducible benchmarks",
        description="XERJ v1.0.0-rc.6 against Elasticsearch 8.13.4 across 88 measured cells — 55 win, 26 tie, 4 lose, 3 n/a — plus the four commands that reproduce every one.",
    ),
    "demo/index.html": dict(
        label="Real-data demo", kind="software",
        title="XERJ.ai — Demo: 60M SSH events vs Elasticsearch",
        description="A sales-engineer walkthrough on 60M real loghub OpenSSH events: real curl, real console clicks, each step shown against the Elasticsearch equivalent.",
    ),
    "agent-search/index.html": dict(
        label="Agentic search demo", kind="software",
    ),
    "aise-demo.html": dict(
        label="Product tour", kind="software",
        description="A 90-second single-take product tour of XERJ — one 22 MB binary with no JVM, the Elasticsearch-compatible API, and the live AI-telemetry dashboards.",
    ),

    # ── enterprise funnel ───────────────────────────────────────────────────
    "pricing/index.html": dict(
        label="Pricing", kind="software",
        description="XERJ editions — Community, Professional and Enterprise. Every tier ships the same binary; deployment footprint, support and compliance are what differ.",
    ),
    "security/index.html": dict(
        label="Security", kind="software",
        description="XERJ's security posture — mutual TLS, RBAC, audit logging, BYOK encryption and certification status for a zero-trust-ready, self-hosted data plane.",
    ),
    "architecture/index.html": dict(
        label="Architecture", kind="software",
        description="XERJ deployment topologies — single node, HA cluster, multi-region and air-gapped — all from the same ~36 MB binary, with RTO and RPO targets each.",
    ),
    "solutions/index.html": dict(
        label="Solutions", kind="collection",
        description="XERJ by solution — SIEM, RAG, observability, Elasticsearch replacement, operational intelligence, AI security review and semantic analytics.",
    ),
    "resources/index.html": dict(
        label="Resources", kind="collection",
    ),
    "industries/index.html": dict(
        label="Industries", kind="collection",
    ),
    "industries/finserv.html": dict(label="Financial services", kind="webpage"),
    "industries/healthcare.html": dict(label="Healthcare", kind="webpage"),
    "industries/public-sector.html": dict(label="Public sector", kind="webpage"),
    "industries/retail.html": dict(label="Retail", kind="webpage"),

    # ── brand ───────────────────────────────────────────────────────────────
    "brand.html": dict(label="Brand", kind="webpage"),
    "brandbook/index.html": dict(label="Brand book", kind="webpage"),

    # ── use cases ───────────────────────────────────────────────────────────
    # All six shared one title and one description before this pass.
    "use-cases.html": dict(
        label="Use cases", kind="collection",
        description="Seven enterprise retrieval workloads on one binary — no Logstash, no Beats, no separate vector database — each with a reproducible ES 8.13 benchmark.",
    ),
    "use-cases/ai-search-retrieval.html": dict(
        label="AI search & retrieval", kind="webpage",
        title="XERJ.ai — Use case: AI-native search & retrieval",
        description="XERJ runs BM25 and exact vector kNN in one query tree and one execution pass, so hybrid retrieval for RAG is a feature rather than an integration project.",
    ),
    "use-cases/elasticsearch-replacement.html": dict(
        label="Elasticsearch replacement", kind="webpage",
        title="XERJ.ai — Use case: Elasticsearch replacement",
        description="A drop-in Elasticsearch replacement — same :9200 API, same query DSL, same client libraries — on one binary instead of a JVM cluster. Measured vs ES 8.13.",
    ),
    "use-cases/operational-intelligence.html": dict(
        label="Operational intelligence", kind="webpage",
        title="XERJ.ai — Use case: operational intelligence",
        description="Replace Beats, Logstash, Elasticsearch and cold storage with one ingest endpoint, one storage engine and one retention number. Measured against ES 8.13.",
    ),
    "use-cases/security-analytics.html": dict(
        label="Security analytics", kind="webpage",
        title="XERJ.ai — Use case: security analytics at scale",
        description="SOC search-and-aggregate workloads — filters, term lookups, aggregations — on a single XERJ node instead of a four-node Elasticsearch 8.13 cluster.",
    ),
    "use-cases/unified-observability.html": dict(
        label="Unified observability", kind="webpage",
        title="XERJ.ai — Use case: unified observability",
        description="Logs, metrics and traces in one engine with one query language and one bill, instead of Loki or Splunk plus Prometheus plus Tempo. Measured vs ES 8.13.",
    ),
    "use-cases/code-security-audit.html": dict(
        label="AI code-security audit", kind="webpage",
        title="XERJ.ai — Use case: AI code-security audit",
        description="Index a codebase as security facts, compile the rule into a queryable invariant, then read only the violators — ~26,000 tokens to audit WordPress core.",
    ),
    "use-cases/second-brain.html": dict(
        label="Second brain", kind="webpage",
        title="XERJ.ai — Use case: a second brain from a folder",
        description="xerj brain <folder> turns notes into a bi-temporal, evidence-carrying knowledge graph. Every link shows the text that taught it; 121 files to 364 links.",
    ),
    "use-cases/semantic-analytics.html": dict(
        label="Semantic analytics", kind="webpage",
        title="XERJ.ai — Use case: semantic analytics in one call",
        description="Retrieval and aggregation in one POST /_search: XERJ runs aggregations over the retrieved top-k neighbour set, so text and vectors are queried together.",
    ),
    # Added by main after this branch was cut; its own <title> and
    # <meta name="description"> are unique and correctly sized, so both are
    # kept verbatim and only the breadcrumb label and JSON-LD kind are set.
    "use-cases/zero-token-ai-search.html": dict(
        label="Zero-token AI search", kind="webpage",
    ),

    # ── case studies ────────────────────────────────────────────────────────
    "case-studies.html": dict(
        label="Case studies", kind="collection",
        description="Three engineering questions answered end to end: a measured WordPress-core security audit, semantic analytics in one request, and Postgres CDC search.",
    ),
    "case-studies/calltree-analytics.html": dict(
        label="Semantic analytics in one request", kind="article",
        description="kNN and aggregations in a single POST /_search: a deep-research question over 130 synthetic support conversations. A correctness demo, not a benchmark.",
    ),
    "case-studies/daily-dev-postgres-cdc.html": dict(
        label="Postgres CDC + hybrid search", kind="article",
        description="Logical-replication CDC keeps XERJ in sync with Postgres, and one hybrid RRF query replaces the tsvector plus pgvector two-query merge in your app.",
    ),
    "case-studies/reference-coding.html": dict(
        label="Reference-coding for Claude Code", kind="article",
        title="XERJ.ai — Case study: reference-coding for agents",
        description="Across 13 purpose-built libraries in 5 languages, XERJ retrieval made the same Claude Code model correct where its memory failed — 21/21 against 1/21.",
    ),
    "case-studies/wordpress-security-audit.html": dict(
        label="WordPress security audit", kind="article",
        description="An AI agent audited WordPress core — 1,492 files, roughly 619k lines — for about 26,000 tokens instead of 5,200,000, with per-phase costs measured.",
    ),
    "case-studies/xerj-self-audit.html": dict(
        label="XERJ audits its own Rust", kind="article",
        description="XERJ indexed its own Rust engine as a call graph and found a real unauthenticated stack-overflow DoS for 10,533 tokens instead of 1.8 million. Fix shipped.",
    ),

    # ── docs · hubs ─────────────────────────────────────────────────────────
    "docs/index.html": dict(
        label="Docs", kind="collection",
        description="XERJ documentation — install, configure and operate the engine. Every key, flag and default here comes out of the Rust crates under engine/crates/.",
    ),
    "docs/recipes/index.html": dict(
        label="Recipes", kind="collection",
        description="Task-oriented XERJ recipes, each verified end to end against a live node — zero-config indexing, retrieval by meaning, agent memory, hybrid ranking.",
    ),
    "docs/agents/index.html": dict(
        label="For AI", kind="collection",
        description="Build with XERJ: the seven canonical agent operations — autoindex a folder, then search, semantic, vector kNN, hybrid and agent memory on the :9200 API.",
    ),

    # ── docs · start ────────────────────────────────────────────────────────
    "docs/quickstart.html": dict(
        label="Quickstart", kind="techarticle",
        description="Operator quickstart for XERJ's native REST API on port 8080 — create indices with explicit field mappings and bulk-ingest through the turbo-ingest path.",
    ),
    "docs/install.html": dict(
        label="Install", kind="techarticle",
        description="Install XERJ from its single static binary — system requirements, checksum verification, directory layout and a systemd unit, on Linux, macOS or Windows.",
    ),
    "docs/migration-from-es.html": dict(
        label="Migrate from Elasticsearch", kind="techarticle",
        description="Point an existing Elasticsearch client at XERJ's port 9200 listener. Exactly what the compatibility layer covers on day one, and what it does not.",
    ),

    # ── docs · reference ────────────────────────────────────────────────────
    "docs/cli.html": dict(
        label="CLI reference", kind="techarticle",
        description="The xerj command line — server flags, config and data-dir overrides, and the autoindex, index, brain and mcp subcommands that drive a running node.",
    ),
    "docs/config.html": dict(
        label="Config TOML", kind="techarticle",
        description="The complete xerj.toml surface: every section, key and production-ready default, with runnable examples and the config-file precedence order.",
    ),
    "docs/env.html": dict(
        label="Environment variables", kind="techarticle",
        description="XERJ reads only two environment variables — XERJ_CONFIG for the config path and XERJ_LOG for the tracing filter. Every other behaviour lives in the TOML.",
    ),
    "docs/api-native.html": dict(
        label="Native REST API", kind="techarticle",
        description="XERJ's native REST API on port 8080 — health, Prometheus metrics, index management, turbo-ingest, explain plans and the log-shaped endpoints.",
    ),
    "docs/api-es-compat.html": dict(
        label="ES-compatible API", kind="techarticle",
        description="The Elasticsearch-compatible API on port 9200 — supported index, document, bulk and search operations, and the structured error for anything unsupported.",
    ),

    # ── docs · data model ───────────────────────────────────────────────────
    "docs/queries.html": dict(
        label="Query types", kind="techarticle",
        description="Every query type XERJ's parser dispatches, generated from the engine source — match, term, range, bool, knn, semantic, hybrid and the rest of the set.",
    ),
    "docs/analyzers.html": dict(
        label="Analyzers", kind="techarticle",
        description="How XERJ turns raw text into a token stream: the four built-in analyzers, what each one is correct for, and how default_analyzer picks between them.",
    ),
    "docs/aggregations.html": dict(
        label="Aggregations", kind="techarticle",
        description="Bucket and metric aggregations in XERJ, with exact counts in the bucket path and no HyperLogLog approximation — plus the deliberate sampling exceptions.",
    ),
    "docs/vectors.html": dict(
        label="Vectors & kNN", kind="techarticle",
        description="Dense vectors and kNN in XERJ — full-precision storage, a persisted HNSW graph, exact rescoring of candidates, and measured recall against ES 8.13.4.",
    ),
    "docs/ingest.html": dict(
        label="Ingest pipelines", kind="techarticle",
        description="Four ingest paths into XERJ — turbo-ingest NDJSON bulk, the ES bulk API, autoindex and streaming — with parsers running in-process with the writer.",
    ),

    # ── docs · engine ───────────────────────────────────────────────────────
    "docs/storage.html": dict(
        label="Storage & WAL", kind="techarticle",
        description="XERJ's on-disk layout: one append-only WAL per index and immutable three-file segments, all mmap'd so reads come straight out of the OS page cache.",
    ),
    "docs/compression.html": dict(
        label="Compression & encodings", kind="techarticle",
        description="XERJ's two compression layers — per-field column encodings chosen at write time and Zstandard block compression — measured 1.61x smaller on disk than ES.",
    ),
    "docs/clustering.html": dict(
        label="Clustering", kind="techarticle",
        description="XERJ ships an embedded Raft implementation — no etcd, no ZooKeeper. Leader election, metadata replication, and per-shard data replication in one binary.",
    ),

    # ── docs · operate ──────────────────────────────────────────────────────
    "docs/security.html": dict(
        label="Auth & TLS", kind="techarticle",
        description="Auth and TLS in XERJ: API-key authentication enabled by default under [auth], the accepted header schemes, and certificate configuration under [tls].",
    ),
    "docs/backup-restore.html": dict(
        label="Backup & restore", kind="techarticle",
        description="Segments are immutable and the WAL append-only, so a XERJ backup is a filesystem copy — hot backup with rsync, the restore procedure, and the caveats.",
    ),
    "docs/operations.html": dict(
        label="Running in production", kind="techarticle",
        description="Running XERJ in production — the systemd unit, non-root user setup, readiness, config reload, log levels, health and metrics, and capacity planning.",
    ),
    "docs/metrics.html": dict(
        label="Metrics", kind="techarticle",
        description="GET /v1/metrics returns Prometheus text format. Every counter, histogram and gauge XERJ emits, with per-index labels wherever they make sense.",
    ),
    "docs/troubleshooting.html": dict(
        label="Troubleshooting", kind="techarticle",
        description="The short list of things that go wrong in a XERJ deployment — the symptom, the metric and log line that confirm it, and the config key you turn.",
    ),
    "docs/upgrades.html": dict(
        label="Upgrades", kind="techarticle",
        description="XERJ follows SemVer: patch releases are drop-in, minor releases add fields, major releases may change the on-disk format. Single-node and cluster steps.",
    ),

    # ── docs · playbooks ────────────────────────────────────────────────────
    "docs/playbooks/full-text.html": dict(
        label="Playbook · Full-text search", kind="techarticle",
        description="Full-text search on XERJ — BM25 scoring, analyzers, highlighters and query-time boosting, with the schema, the ingest command and the queries to copy.",
    ),
    "docs/playbooks/log-analytics.html": dict(
        label="Playbook · Log analytics", kind="techarticle",
        description="Ingest Nginx, JSON, syslog or OTLP logs at line rate and query them in milliseconds — schema, ingest command, queries, and retention without ILM.",
    ),
    "docs/playbooks/observability.html": dict(
        label="Playbook · Observability", kind="techarticle",
        description="Metrics, traces and logs as one queryable store — OTLP in, Prometheus text out, traces as a connected graph. Replaces Splunk plus Prometheus plus Tempo.",
    ),
    "docs/playbooks/siem.html": dict(
        label="Playbook · SIEM", kind="techarticle",
        description="Security analytics on an event stream — auth logs, firewall drops, process executions, DNS. Schema, ingest command, and five core detection queries.",
    ),
    "docs/playbooks/vector-search.html": dict(
        label="Playbook · Vector & RAG", kind="techarticle",
        description="Vector and RAG retrieval on one box — the dense_vector schema, hybrid BM25 plus kNN fusion, and dashboards for recall, latency and cache hit rate.",
    ),

    # ── docs · agents ───────────────────────────────────────────────────────
    "docs/agents/quickstart.html": dict(label="Agent quickstart", kind="techarticle"),
    "docs/agents/endpoints.html": dict(label="Endpoint contract", kind="techarticle"),

    # ── docs · recipes ──────────────────────────────────────────────────────
    # Added by main after this branch was cut; title and description kept
    # verbatim (both unique and correctly sized) — label and kind only.
    "docs/recipes/air-gapped-deployment.html": dict(
        label="Air-gapped deployment", kind="techarticle",
    ),
    "docs/recipes/zero-config-autoindex.html": dict(
        label="Zero-config autoindex", kind="techarticle",
        title="XERJ · Docs · Recipe · Zero-config autoindex",
        description="One command — xerj autoindex <folder> — turns JSONL, CSV, SQLite, PDF, DOCX, HTML and log files into typed, queryable indices with zero configuration.",
    ),
    "docs/recipes/document-folder-index.html": dict(
        label="Index a folder of documents", kind="techarticle",
        title="XERJ · Docs · Recipe · Index a document folder",
        description="Point an agent at a recursive folder of PDF, DOCX, HTML, Markdown and TXT files: one xerj-index pass extracts, chunks and auto-embeds every one of them.",
    ),
    "docs/recipes/semantic-search-rag.html": dict(
        label="Semantic search & RAG", kind="techarticle",
        description="Semantic search and RAG retrieval with XERJ — map a field as semantic_text, auto-embed on ingest, retrieve by meaning, with no separate vector database.",
    ),
    "docs/recipes/hybrid-search.html": dict(label="Hybrid search", kind="techarticle"),
    "docs/recipes/vector-search-knn.html": dict(
        label="Vector search · kNN", kind="techarticle",
        description="Vector similarity search in XERJ — dense_vector fields, Elasticsearch-8 knn queries served by HNSW when unfiltered, and pre-filtering with bool filter.",
    ),
    "docs/recipes/vector-quantization.html": dict(
        label="Vector quantization", kind="techarticle",
        description="Opt a dense_vector field into scalar8 quantization: kNN scored from 1-byte-per-dimension codes at recall@10 = 0.998, originals still returned in _source.",
    ),
    "docs/recipes/passage-retrieval.html": dict(
        label="Passage retrieval", kind="techarticle",
        description="Map a field as semantic_text so XERJ embeds every passage on ingest, and a long, multi-topic document competes on its single best-matching section.",
    ),
    "docs/recipes/agentic-memory.html": dict(
        label="Agent memory", kind="techarticle",
        description="Give an AI agent long-term memory with XERJ's namespaced offline memory API — store, recall by vector or text, filter by metadata, forget, stay isolated.",
    ),
    "docs/recipes/log-analytics.html": dict(
        label="Log analytics", kind="techarticle",
        description="Bulk-ingest application logs into XERJ and answer error-rate, latency-percentile and top-service questions with Elasticsearch-compatible aggregations.",
    ),
    "docs/recipes/anomaly-detection.html": dict(
        label="Anomaly detection", kind="techarticle",
        description="Detect metric spikes on demand with XERJ's Elasticsearch-shaped anomaly detector: a moving-window z-score that flags the bucket where a metric broke out.",
    ),
    "docs/recipes/continuous-anomaly-datafeeds.html": dict(
        label="Continuous anomaly datafeeds", kind="techarticle",
        description="Run an Elasticsearch _ml datafeed on XERJ to score a live metric index continuously — one pass now, then a timer that appends only newly flagged buckets.",
    ),
    "docs/recipes/migrate-from-elasticsearch.html": dict(
        label="Migrate from Elasticsearch", kind="techarticle",
        description="Migrate to XERJ by pointing your client's base URL at it — XERJ speaks the Elasticsearch REST wire protocol on 9200, so queries and bulk loads survive.",
    ),

    # ── generated article hubs ─────────────────────────────────────────────
    "answers/index.html": dict(
        label="Answers", kind="collection",
        title="XERJ answers — direct technical guidance",
        description="Direct answers about XERJ search, indexing and agent workflows, with evidence links and practical guidance for AI builders.",
    ),
    "compare/index.html": dict(
        label="Comparisons", kind="collection",
        title="XERJ comparisons — retrieval choices",
        description="Focused comparisons for teams choosing a search, vector and agent-memory architecture, with evidence links and clear trade-offs.",
    ),
}


#: Directory -> (breadcrumb label, deployed URL of the hub page that covers it).
#:
#: Every URL here must be a real page in the repo (seo_lint enforces it).
#: `docs/playbooks` is deliberately absent: there is no playbooks index page,
#: so that level is skipped rather than pointed at a URL that 404s.
SECTION_HUBS: dict[str, tuple[str, str]] = {
    "docs": ("Docs", "/docs/"),
    "docs/recipes": ("Recipes", "/docs/recipes/"),
    "docs/agents": ("For AI", "/docs/agents/"),
    "use-cases": ("Use cases", "/use-cases"),
    "case-studies": ("Case studies", "/case-studies"),
    "industries": ("Industries", "/industries/"),
    "answers": ("Answers", "/answers/"),
    "compare": ("Comparisons", "/compare/"),
}

#: JSON-LD @type per `kind`.
KIND_TYPE = {
    "software": "SoftwareApplication",
    "collection": "CollectionPage",
    "techarticle": "TechArticle",
    "article": "Article",
    "webpage": "WebPage",
}

#: og:type per `kind`.  ogp.me only defines a small vocabulary; "article" for
#: anything article-shaped, "website" for everything else.
KIND_OG_TYPE = {
    "software": "website",
    "collection": "website",
    "techarticle": "article",
    "article": "article",
    "webpage": "website",
}

#: kinds whose JSON-LD must carry headline/datePublished/dateModified.
ARTICLE_KINDS = frozenset({"techarticle", "article"})


def entry(rel: str) -> dict[str, str]:
    try:
        return PAGES[rel]
    except KeyError:
        # Generated article pages are described by their Markdown frontmatter.
        # Import lazily so the hand-written page table remains cheap and the
        # parser does not create an import cycle during tool startup.
        from article_data import load_for_rel

        article = load_for_rel(pathlib.Path(__file__).resolve().parents[2], rel)
        if article is not None:
            return article.head_meta()
        raise KeyError(
            f"{rel} has no entry in scripts/seo/pagedata.py or content/ — add "
            f"a page entry or an article source"
        ) from None
