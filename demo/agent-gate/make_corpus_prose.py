#!/usr/bin/env python3
"""Deterministic PROSE/CODE-heavy corpus for the Agent Gate — the regime where
XERJ is expected to WIN on tokens.

The mixed corpus (make_corpus.py) is 99.99% structured records, where grep on a
one-integer question is unbeatable. This corpus is the opposite: many small
documents of natural-language and source text, no large record streams. It is
the shape of a real code/knowledge repository, and it is where ranked retrieval
beats unranked grep on tokens — grep must return every candidate line, ranked
search returns the one passage.

Seeded, so runs are byte-identical and comparable across versions.
Usage: make_corpus_prose.py <dir>
"""
import os
import random
import sys
import textwrap

OUT = sys.argv[1] if len(sys.argv) > 1 else "/tmp/gate-prose"
os.makedirs(OUT, exist_ok=True)
os.chdir(OUT)
random.seed(31337)

for d in ("docs", "runbooks", "adr", "src", "src/api", "src/store", "tests"):
    os.makedirs(d, exist_ok=True)

# ── Ground-truth documents: each answers exactly one gate question, phrased so
#    the answer's wording differs from the question's (no lexical gift). ──────

open("docs/postmortem-checkout.md", "w").write(textwrap.dedent("""\
    # Postmortem: checkout unavailable, 14 June 2026

    ## Impact
    Checkout returned errors for 51 minutes across all regions.

    ## Root cause
    The payment gateway's TLS certificate expired. Renewal was automated but
    the service never reloaded it, so every handshake was rejected.

    ## Fix
    Reloaded the service and added a certificate-expiry alert firing 14 days out.
    """))

open("docs/postmortem-search.md", "w").write(textwrap.dedent("""\
    # Postmortem: search quality regression, 3 May 2026

    ## Impact
    Click-through on search fell by more than a third for two days.

    ## Root cause
    A retuned analyser stopped applying stemming to German queries, so plural
    forms no longer matched their singular index terms.

    ## Fix
    Reverted the analyser and added a relevance regression test to the pipeline.
    """))

open("runbooks/database-failover.md", "w").write(textwrap.dedent("""\
    # Runbook: database failover

    Promote the hot standby with `pg_ctl promote`. Writes are unavailable for
    roughly forty seconds while the promotion settles. Point the application at
    the new primary only after replication lag on the old node reaches zero.
    """))

open("runbooks/pool-exhaustion.md", "w").write(textwrap.dedent("""\
    # Runbook: connection pool exhaustion

    Symptoms: p99 latency climbs and `pool exhausted` appears in the logs. The
    usual trigger is a deploy that raised per-request fan-out, saturating the
    fixed pool. Mitigate by raising the pool ceiling and rolling back the deploy
    that increased fan-out.
    """))

open("adr/0007-idempotent-consumers.md", "w").write(textwrap.dedent("""\
    # ADR 0007: all queue consumers must be idempotent

    The async queue guarantees at-least-once delivery, not exactly-once. A
    consumer can therefore see the same message twice. Every consumer must key
    its side effects on a stable identifier so a redelivery is a no-op rather
    than a double charge or a duplicate email.
    """))

open("docs/security-secrets.md", "w").write(textwrap.dedent("""\
    # Handling secrets

    Secrets live in the vault and are injected into the process environment at
    boot. They are never written to a file that is committed to version control.
    Production access additionally requires a hardware security key.
    """))

# Source that answers "how does the payment client avoid double billing".
open("src/api/payments.py", "w").write(textwrap.dedent('''
    """Payment gateway client: bounded retries with a stable idempotency key."""
    import hashlib
    import time

    MAX_RETRIES = 3

    def idempotency_key(order_id: str, attempt: int) -> str:
        """Deterministic key so a retried charge is collapsed by the gateway
        into the original, never billed twice."""
        return hashlib.sha256(f"{order_id}:{attempt}".encode()).hexdigest()

    def charge(client, order_id: str, amount_cents: int):
        for attempt in range(MAX_RETRIES):
            try:
                return client.post("/charge", json={
                    "amount": amount_cents,
                    "idempotency_key": idempotency_key(order_id, attempt),
                })
            except TimeoutError:
                time.sleep(2 ** attempt)  # exponential backoff
        raise RuntimeError("payment gateway unreachable after retries")
'''))

open("src/store/pool.rs", "w").write(textwrap.dedent('''
    //! Fixed-ceiling connection pool. Exhaustion is the most common outage
    //! trigger: a deploy raises per-request fan-out and the pool saturates,
    //! p99 climbs, and the service starts shedding load with 503s.
    pub struct Pool {
        max: usize,
        in_use: usize,
    }

    impl Pool {
        pub fn acquire(&mut self) -> Result<Conn, PoolError> {
            if self.in_use >= self.max {
                return Err(PoolError::Exhausted);
            }
            self.in_use += 1;
            Ok(Conn::new())
        }
    }
'''))

# Filler: many plausible-but-irrelevant documents, so retrieval has to actually
# rank rather than return the only file. This is what makes grep expensive —
# its candidate set is large; ranked search still returns one passage.
TOPICS = ["caching", "logging", "metrics", "tracing", "rate limiting",
          "feature flags", "backups", "cron scheduling", "webhooks",
          "pagination", "serialization", "config loading", "health checks",
          "graceful shutdown", "circuit breaking", "load shedding",
          "request validation", "content negotiation", "compression",
          "connection keepalive", "dns resolution", "retry budgets"]
BODY = ("This module handles {t}. It exposes a small, dependency-free surface "
        "and is covered by unit tests. Configuration is read once at startup "
        "and validated eagerly so a bad value fails fast rather than at first "
        "use. The design favours explicitness over cleverness; see the module "
        "comment for the rationale and the trade-offs that were considered.")
for i, t in enumerate(TOPICS):
    slug = t.replace(" ", "-")
    open(f"src/{slug}.md", "w").write(f"# {t.title()}\n\n{BODY.format(t=t)}\n")
    # a source file too, so the corpus is code + prose
    open(f"src/{slug}.py", "w").write(
        f'"""{t.title()} implementation."""\n\n'
        f"# {BODY.format(t=t)}\n"
        f"def configure_{slug.replace('-', '_')}(cfg):\n"
        f"    return cfg.get({t.split()[0]!r}, {{}})\n")

# A few longer design docs for bulk.
for i in range(6):
    paras = "\n\n".join(
        f"Section {j}: " + BODY.format(t=random.choice(TOPICS))
        for j in range(random.randint(8, 16)))
    open(f"adr/design-note-{i:02d}.md", "w").write(f"# Design note {i}\n\n{paras}\n")

# tiny binary junk, to exercise skip-not-fatal
open("logo.bin", "wb").write(os.urandom(4096))

n = sum(len(files) for _, _, files in os.walk("."))
sz = sum(os.path.getsize(os.path.join(r, f))
         for r, _, fs in os.walk(".") for f in fs)
print(f"prose corpus built: {n} files, {sz/1e3:.1f} KB")
