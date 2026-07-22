#!/usr/bin/env python3
"""Deterministic heterogeneous corpus for the Agent Gate.

Seeded, so every run produces byte-identical input and results are
comparable across engine versions. Usage: make_corpus.py <dir>
"""
import sys, os
OUT = sys.argv[1] if len(sys.argv) > 1 else "/tmp/gate-corpus"
os.makedirs(OUT, exist_ok=True)
os.chdir(OUT)

import json, random, gzip, sqlite3, os, datetime, textwrap
random.seed(2026)
for d in ("logs", "docs", "code", "config", "web"):
    os.makedirs(d, exist_ok=True)

# 1. nginx-style access logs (plain text; one gzipped)
IPS = [f"10.0.{random.randint(0,9)}.{random.randint(1,254)}" for _ in range(400)]
PATHS = ["/api/checkout", "/api/login", "/health", "/api/search?q=shoes",
         "/static/app.js", "/api/payments/refund"]
def nginx(n):
    out = []
    for _ in range(n):
        t = datetime.datetime(2026, 6, random.randint(1, 28), random.randint(0, 23), random.randint(0, 59))
        code = random.choices([200, 200, 301, 404, 500, 502], [70, 10, 5, 8, 5, 2])[0]
        out.append(f'{random.choice(IPS)} - - [{t.strftime("%d/%b/%Y:%H:%M:%S")} +0000] '
                   f'"GET {random.choice(PATHS)} HTTP/1.1" {code} {random.randint(200,90000)} "-" "Mozilla/5.0"')
    return "\n".join(out)
open("logs/access-2026-06-01.log", "w").write(nginx(60000))
open("logs/access-2026-06-02.log", "w").write(nginx(60000))
with gzip.open("logs/access-2026-05-31.log.gz", "wt") as f:
    f.write(nginx(40000))

# 2. syslog-style — same family, different shape
SVCS = ["sshd", "kernel", "cron", "postfix", "systemd"]
MSG = ["Accepted publickey for deploy from 10.0.3.4 port 55212",
       "Out of memory: Killed process 8123 (java)",
       "session opened for user root by (uid=0)",
       "connection timed out while talking to upstream",
       "Started Daily apt download activities."]
open("logs/syslog", "w").write("\n".join(
    f'Jun {random.randint(1,28):2d} {random.randint(0,23):02d}:{random.randint(0,59):02d}:'
    f'{random.randint(0,59):02d} host1 {random.choice(SVCS)}[{random.randint(100,9999)}]: {random.choice(MSG)}'
    for _ in range(40000)))

# 3. structured app logs (JSONL)
SERV = ["checkout", "auth", "payments", "search", "inventory"]
with open("logs/app.jsonl", "w") as f:
    for _ in range(120000):
        t = datetime.datetime(2026, 6, random.randint(1, 28), random.randint(0, 23), random.randint(0, 59))
        f.write(json.dumps({"ts": t.isoformat() + "Z", "svc": random.choice(SERV),
                            "level": random.choices(["INFO", "WARN", "ERROR"], [80, 15, 5])[0],
                            "msg": random.choice(["request ok", "upstream timeout", "token expired",
                                                  "disk pressure detected", "payment declined"]),
                            "dur_ms": round(random.expovariate(1 / 60), 2),
                            "uid": f"u{random.randint(1,20000)}"}) + "\n")

# 4. CSVs — one European dialect, one plain
with open("orders_eu.csv", "w") as f:
    f.write("order_id;customer_id;amount_eur;country;status;placed_at\n")
    for i in range(80000):
        f.write(f"o{i};u{random.randint(1,20000)};{random.randint(5,900)},{random.randint(10,99)};"
                f"{random.choice(['DE','FR','ES','IT'])};"
                f"{random.choices(['paid','refunded','failed'],[92,5,3])[0]};"
                f"2026-06-{random.randint(1,28):02d}\n")
with open("inventory.csv", "w") as f:
    f.write("sku,warehouse,qty,reorder_level,updated\n")
    for i in range(30000):
        f.write(f"SKU-{i:06d},WH{random.randint(1,6)},{random.randint(0,900)},"
                f"{random.randint(5,60)},2026-06-{random.randint(1,28):02d}\n")

# 5. SQLite, two tables
con = sqlite3.connect("crm.db"); c = con.cursor()
c.execute("create table customers(user_id text primary key, plan text, mrr real, country text, csat int)")
for i in range(1, 20001):
    c.execute("insert into customers values(?,?,?,?,?)",
              (f"u{i}", random.choices(["free", "pro", "enterprise"], [70, 25, 5])[0],
               round(random.uniform(0, 4000), 2), random.choice(['DE', 'FR', 'ES', 'IT']), random.randint(1, 5)))
c.execute("create table tickets(id integer primary key, user_id text, subject text, status text)")
for i in range(1, 8001):
    c.execute("insert into tickets values(?,?,?,?)",
              (i, f"u{random.randint(1,20000)}",
               random.choice(["cannot log in", "refund not received", "API returns 500",
                              "how do I export data", "billing question"]),
               random.choice(["open", "closed", "pending"])))
con.commit(); con.close()

DOCS = {
"docs/postmortem-2026-06-14.md": "# Postmortem: checkout outage, 14 June 2026\n\n## Impact\nCheckout was unavailable for 51 minutes.\n\n## Root cause\nThe payment gateway TLS certificate expired and our client rejected the handshake. Certificate renewal was automated but the reload hook never fired after renewal.\n\n## Resolution\nWe reloaded the service manually and added a certificate-expiry alert at 14 days.\n",
"docs/postmortem-2026-05-03.md": "# Postmortem: search relevance drop\n\n## Impact\nSearch click-through fell 38% for two days.\n\n## Root cause\nA mis-tuned analyser dropped stemming for German queries, so plural forms stopped matching.\n\n## Resolution\nReverted the analyser and added a relevance regression test to CI.\n",
"docs/runbook-oncall.md": "# On-call runbook\n\nEscalation is primary, then secondary, then the engineering manager. Quiet hours never apply to sev1. Always attach the trace identifier to the incident channel.\n",
"docs/runbook-database.md": "# Database runbook\n\n## Failover\nPromote the standby with `pg_ctl promote`. Expect about 40 seconds of write unavailability.\n\n## Connection pool exhaustion\nSymptoms are rising p99 and `connection pool exhausted` in the logs. Raise pool limits and check for a deploy that increased per-request fan-out.\n",
"docs/architecture.md": "# Architecture overview\n\nThe platform is a set of stateless services behind an nginx ingress, backed by Postgres with read replicas and a Redis cache. Asynchronous work runs through a durable queue with at-least-once delivery, so all consumers must be idempotent.\n",
"docs/security-policy.md": "# Security policy\n\nSecrets are stored in the vault and injected at runtime; never in environment files committed to git. All production access requires hardware-backed multi-factor authentication. Access reviews happen quarterly.\n",
"docs/hr-leave.md": "# Leave policy\n\nStaff accrue paid time off at two days per month and may carry over up to ten days into the following year.\n",
}
for k, v in DOCS.items():
    open(k, "w").write(v)

open("code/payments.py", "w").write(textwrap.dedent('''
    """Payment gateway client with retry and idempotency."""
    import time, hashlib

    MAX_RETRIES = 3

    def idempotency_key(order_id: str, attempt: int) -> str:
        """Stable key so a retried charge is never double-billed."""
        return hashlib.sha256(f"{order_id}:{attempt}".encode()).hexdigest()

    def charge(client, order_id, amount_cents):
        for attempt in range(MAX_RETRIES):
            try:
                return client.post("/charge", json={"amount": amount_cents,
                    "idempotency_key": idempotency_key(order_id, attempt)})
            except TimeoutError:
                time.sleep(2 ** attempt)
        raise RuntimeError("payment gateway unreachable after retries")
'''))
open("code/pool.rs", "w").write(textwrap.dedent('''
    //! Connection pool with a hard ceiling.
    //! Exhaustion is the most common production failure: a deploy raises
    //! per-request fan-out and the pool saturates, so p99 climbs and the
    //! service starts returning 503.
    pub struct Pool { max: usize, in_use: usize }
    impl Pool {
        pub fn acquire(&mut self) -> Result<Conn, PoolError> {
            if self.in_use >= self.max { return Err(PoolError::Exhausted); }
            self.in_use += 1; Ok(Conn::new())
        }
    }
'''))
open("code/search.js", "w").write(textwrap.dedent('''
    // Client-side search with debounce and German stemming support.
    const DEBOUNCE_MS = 250;
    export function buildQuery(text, {language = "en"} = {}) {
      // Stemming must stay enabled for German or plural forms stop matching.
      return {query: {match: {body: {query: text, analyzer: `${language}_stem`}}}};
    }
'''))

open("web/status.html", "w").write("<html><head><title>Status</title></head><body><h1>Service status</h1>"
    "<p>All systems operational. Last incident: checkout outage on 14 June 2026 caused by an expired TLS certificate.</p></body></html>")
open("config/deploy.yaml", "w").write("replicas: 6\nresources:\n  limits:\n    memory: 2Gi\n    cpu: '2'\n"
    "readinessProbe:\n  httpGet:\n    path: /health\n    port: 8080\nrollingUpdate:\n  maxSurge: 2\n  maxUnavailable: 0\n")
open("config/app.toml", "w").write("[server]\nport = 8080\nworkers = 16\n\n[database]\npool_max = 128\n"
    "statement_timeout_ms = 5000\n\n[cache]\nttl_seconds = 300\n")

open("blob.bin", "wb").write(os.urandom(8192))
print("corpus built")
