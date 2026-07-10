#!/usr/bin/env python3
"""Multi-GB mixed corpus generator for the xerj autoindex scale proof.

Layout (~7 GB, ~35M records):
  logs/app-NN.jsonl      12 x ~320MB JSON server logs
  events/events-NN.csv    8 x ~220MB retail event CSVs
  syslog/sys-NN.log       4 x ~240MB plain syslog lines
  metrics/metrics-NN.ndjson 2 x ~200MB nested metric docs
  tail/                   sql dump, sqlite db, pdf, html, xml, yaml, junk

Writes a manifest.json with exact per-file line/record counts.
"""
import json, os, random, sqlite3, string, sys, zlib
from multiprocessing import Pool

ROOT = sys.argv[1] if len(sys.argv) > 1 else "/home/claude/xerj-autoindex-scale/corpus"

SERVICES = ["checkout", "auth", "catalog", "payments", "shipping", "search", "gateway", "notify"]
LEVELS = ["INFO", "INFO", "INFO", "INFO", "WARN", "ERROR", "DEBUG"]
MSGS = [
    "request completed", "cache miss for key", "retrying upstream call",
    "connection pool exhausted", "slow query detected", "token refreshed",
    "payment captured", "inventory reserved", "circuit breaker half-open",
    "GC pause exceeded budget", "TLS handshake renegotiated", "queue depth high",
]
COUNTRIES = ["US", "DE", "JP", "BR", "IN", "GB", "FR", "AU", "CA", "MX", "PL", "KR"]
EVENT_TYPES = ["view", "view", "view", "add_to_cart", "purchase", "refund", "wishlist"]
HOSTS = ["web-%02d" % i for i in range(1, 25)]
PROCS = ["kernel", "sshd", "nginx", "systemd", "cron", "postfix", "dockerd", "kubelet"]
SYSMSG = [
    "Accepted publickey for deploy from port",
    "Out of memory: killed process",
    "eth0: link becomes ready",
    "Started Session of user",
    "upstream timed out while reading response header",
    "oom_reaper: reaped process",
    "segfault at 0 ip 00007f sp 00007f error 4",
    "renewed lease on interface",
]

def jsonl_file(args):
    path, seed, target_mb = args
    rng = random.Random(seed)
    t = 1751000000000 + seed * 977  # epoch ms, advances per line
    n, size, buf = 0, 0, []
    target = target_mb * 1024 * 1024
    with open(path, "w") as f:
        while size < target:
            t += rng.randint(1, 40)
            iso = "2026-06-%02dT%02d:%02d:%02d.%03dZ" % (
                1 + (t // 86400000) % 28, (t // 3600000) % 24,
                (t // 60000) % 60, (t // 1000) % 60, t % 1000)
            line = ('{"ts":"%s","level":"%s","service":"%s","msg":"%s upstream=%s attempt=%d deadline_ms=%d",'
                    '"latency_ms":%.2f,'
                    '"status":%d,"client_ip":"10.%d.%d.%d","user_id":"u%07d",'
                    '"trace_id":"%016x","span_id":"%08x","bytes_out":%d,'
                    '"http":{"method":"%s","path":"/api/v2/%s/%d/items?page=%d&limit=50","user_agent":"Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.%d.%d Safari/537.36"},'
                    '"k8s":{"pod":"%s-%08x-x%d","node":"ip-10-%d-%d-%d.ec2.internal","namespace":"prod-%s"},'
                    '"region":"%s","cache":"%s","retries":%d}\n') % (
                iso, rng.choice(LEVELS), rng.choice(SERVICES), rng.choice(MSGS),
                rng.choice(SERVICES), rng.randint(1, 3), rng.choice((250, 500, 1000, 5000)),
                rng.random() * 900, rng.choice((200, 200, 200, 201, 204, 301, 404, 500, 503)),
                rng.randint(0, 255), rng.randint(0, 255), rng.randint(1, 254),
                rng.randint(1, 4_000_000), rng.getrandbits(64), rng.getrandbits(32),
                rng.randint(64, 250_000),
                rng.choice(("GET", "GET", "GET", "POST", "PUT", "DELETE")), rng.choice(SERVICES),
                rng.randint(1, 900_000), rng.randint(1, 400),
                rng.randint(1000, 9999), rng.randint(10, 200),
                rng.choice(SERVICES), rng.getrandbits(32), rng.randint(0, 9),
                rng.randint(0, 255), rng.randint(0, 255), rng.randint(1, 254), rng.choice(SERVICES),
                rng.choice(("us-east-1", "eu-west-1", "ap-south-1")),
                rng.choice(("hit", "miss", "bypass")), rng.randint(0, 4))
            buf.append(line); size += len(line); n += 1
            if len(buf) >= 20000:
                f.write("".join(buf)); buf.clear()
        f.write("".join(buf))
    return (path, n, size)

def csv_file(args):
    path, seed, target_mb = args
    rng = random.Random(seed)
    t = 1750000000000 + seed * 3163
    n, size, buf = 0, 0, []
    target = target_mb * 1024 * 1024
    with open(path, "w") as f:
        hdr = ("event_time,event_type,product_id,product_name,category_path,price,qty,country,"
               "session_id,revenue,referrer_url,user_agent,coupon\n")
        f.write(hdr); size += len(hdr)
        while size < target:
            t += rng.randint(2, 90)
            qty = rng.randint(1, 8)
            price = rng.random() * 400 + 1
            line = ('%d,%s,P%06d,%s %s %d-pack,home>%s>%s,%.2f,%d,%s,s%012x,%.2f,'
                    'https://www.example-shop.com/%s/%s?utm_source=%s&utm_campaign=summer%d,'
                    '"Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/1%02d.0 Safari/537.36",'
                    '%s\n') % (
                t, rng.choice(EVENT_TYPES), rng.randint(1, 300_000),
                rng.choice(("Acme", "Globex", "Initech", "Umbrella", "Stark")),
                rng.choice(("widget", "gadget", "sprocket", "flange", "gizmo")), rng.randint(1, 24),
                rng.choice(("kitchen", "garden", "office", "sports")),
                rng.choice(("storage", "lighting", "tools", "decor")),
                price, qty,
                rng.choice(COUNTRIES), rng.getrandbits(48), price * qty,
                rng.choice(("kitchen", "garden", "office")), rng.choice(("sale", "new", "clearance")),
                rng.choice(("google", "newsletter", "affiliate")), rng.randint(20, 26),
                rng.randint(10, 40),
                rng.choice(("", "SAVE10", "FREESHIP", "VIP20")))
            buf.append(line); size += len(line); n += 1
            if len(buf) >= 20000:
                f.write("".join(buf)); buf.clear()
        f.write("".join(buf))
    return (path, n, size)

def syslog_file(args):
    path, seed, target_mb = args
    rng = random.Random(seed)
    n, size, buf = 0, 0, []
    target = target_mb * 1024 * 1024
    sec = seed * 40000
    with open(path, "w") as f:
        while size < target:
            sec += rng.randint(1, 5)
            line = ("Jun %2d %02d:%02d:%02d %s %s[%d]: %s %d client=10.%d.%d.%d:%d sess=%016x "
                    "duration=%dms unit=%s.service cgroup=/system.slice/%s.service result=%s\n") % (
                1 + (sec // 86400) % 28, (sec // 3600) % 24, (sec // 60) % 60, sec % 60,
                rng.choice(HOSTS), rng.choice(PROCS), rng.randint(100, 65000),
                rng.choice(SYSMSG), rng.randint(1, 99999),
                rng.randint(0, 255), rng.randint(0, 255), rng.randint(1, 254), rng.randint(1024, 65000),
                rng.getrandbits(64), rng.randint(1, 30000),
                rng.choice(PROCS), rng.choice(PROCS),
                rng.choice(("done", "failed", "timeout", "deferred")))
            buf.append(line); size += len(line); n += 1
            if len(buf) >= 20000:
                f.write("".join(buf)); buf.clear()
        f.write("".join(buf))
    return (path, n, size)

def metrics_file(args):
    path, seed, target_mb = args
    rng = random.Random(seed)
    t = 1751500000
    n, size, buf = 0, 0, []
    target = target_mb * 1024 * 1024
    with open(path, "w") as f:
        while size < target:
            t += rng.randint(5, 15)
            doc = ('{"@timestamp":%d,"host":{"name":"%s","dc":"%s","os":"Ubuntu 24.04.2 LTS","kernel":"6.8.0-%d-generic"},'
                   '"cpu":{"user":%.3f,"system":%.3f,"iowait":%.3f,"steal":%.4f,"cores":[%.2f,%.2f,%.2f,%.2f,%.2f,%.2f,%.2f,%.2f]},'
                   '"mem":{"used_pct":%.2f,"swap_mb":%d,"page_faults":%d,"slab_mb":%d},'
                   '"disk":[{"dev":"nvme0n1","util":%.2f,"read_iops":%d,"write_iops":%d},{"dev":"sda","util":%.2f,"read_iops":%d,"write_iops":%d}],'
                   '"net":{"rx_bytes":%d,"tx_bytes":%d,"tcp_retrans":%d},'
                   '"top_proc":{"name":"%s","rss_mb":%d,"cpu_pct":%.1f},'
                   '"tags":["prod","%s","tier-%d"]}\n') % (
                t * 1000, rng.choice(HOSTS), rng.choice(("us-east", "eu-west", "ap-south")),
                rng.randint(30, 60),
                rng.random(), rng.random() * 0.3, rng.random() * 0.1, rng.random() * 0.01,
                *(rng.random() * 100 for _ in range(8)),
                rng.random() * 100, rng.randint(0, 2048), rng.randint(0, 500000), rng.randint(100, 3000),
                rng.random() * 100, rng.randint(0, 90000), rng.randint(0, 60000),
                rng.random() * 100, rng.randint(0, 20000), rng.randint(0, 9000),
                rng.getrandbits(32), rng.getrandbits(32), rng.randint(0, 500),
                rng.choice(PROCS), rng.randint(20, 8000), rng.random() * 800,
                rng.choice(SERVICES), rng.randint(0, 3))
            buf.append(doc); size += len(doc); n += 1
            if len(buf) >= 10000:
                f.write("".join(buf)); buf.clear()
        f.write("".join(buf))
    return (path, n, size)

MINIMAL_PDF = """%%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >> endobj
4 0 obj << /Length %d >> stream
%s
endstream endobj
5 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
trailer << /Root 1 0 R >>
"""

def make_pdf(path, title, lines):
    parts = ["BT /F1 12 Tf 50 740 Td (%s) Tj ET" % title]
    y = 720
    for ln in lines:
        parts.append("BT /F1 10 Tf 50 %d Td (%s) Tj ET" % (y, ln.replace("(", "").replace(")", "")))
        y -= 14
    stream = "\n".join(parts)
    with open(path, "w") as f:
        f.write(MINIMAL_PDF % (len(stream), stream))

def tail_files(root):
    tail = os.path.join(root, "tail")
    os.makedirs(tail, exist_ok=True)
    counts = {}
    rng = random.Random(42)
    # SQL dump ~40MB
    sqlp = os.path.join(tail, "crm_backup.sql")
    n = 0
    with open(sqlp, "w") as f:
        f.write("-- pg_dump style backup\nCREATE TABLE customers (id INT PRIMARY KEY, name TEXT, email TEXT, ltv NUMERIC, signup DATE, tier TEXT);\n")
        while f.tell() < 40 * 1024 * 1024:
            rows = ",".join("(%d,'%s %s','%s%d@example.com',%.2f,'2024-%02d-%02d','%s')" % (
                n * 50 + i, rng.choice(("Ada", "Grace", "Alan", "Edsger", "Barbara", "Donald")),
                rng.choice(("Lovelace", "Hopper", "Turing", "Dijkstra", "Liskov", "Knuth")),
                "user", n * 50 + i, rng.random() * 9000, rng.randint(1, 12), rng.randint(1, 28),
                rng.choice(("free", "pro", "enterprise"))) for i in range(50))
            f.write("INSERT INTO customers VALUES %s;\n" % rows)
            n += 1
    counts[sqlp] = n * 50
    # SQLite ~30MB
    dbp = os.path.join(tail, "inventory.sqlite")
    con = sqlite3.connect(dbp)
    con.execute("CREATE TABLE parts (sku TEXT PRIMARY KEY, name TEXT, warehouse TEXT, qty INT, unit_cost REAL, updated_at TEXT)")
    rows = [("SKU%07d" % i, "part-%s-%d" % (rng.choice(("bolt", "gear", "cam", "rotor", "seal")), i),
             rng.choice(("ATL", "FRA", "NRT", "SYD")), rng.randint(0, 9000), rng.random() * 120,
             "2026-0%d-%02dT12:00:00Z" % (rng.randint(1, 6), rng.randint(1, 28))) for i in range(220_000)]
    con.executemany("INSERT INTO parts VALUES (?,?,?,?,?,?)", rows)
    con.commit(); con.close()
    counts[dbp] = 220_000
    # PDFs
    for i in range(5):
        p = os.path.join(tail, "report-q%d.pdf" % (i + 1))
        make_pdf(p, "Quarterly Ops Report %d" % (i + 1),
                 ["Uptime was 99.9%d percent across region %s" % (i, r) for r in ("us-east", "eu-west")] +
                 ["Postmortem: queue saturation in checkout service", "Action items: shard the payments topic"])
        counts[p] = 1
    # HTML / XML / YAML
    hp = os.path.join(tail, "runbook.html")
    with open(hp, "w") as f:
        f.write("<html><head><title>Oncall runbook</title></head><body>" +
                "".join("<h2>Alert %d</h2><p>If %s fires, restart %s and drain the queue.</p>" % (i, rng.choice(SYSMSG), rng.choice(SERVICES)) for i in range(200)) +
                "</body></html>")
    counts[hp] = 1
    xp = os.path.join(tail, "feed.xml")
    with open(xp, "w") as f:
        f.write("<?xml version='1.0'?><rss><channel>" +
                "".join("<item><title>Release note %d</title><description>Fixes %s in %s</description><pubDate>2026-06-%02d</pubDate></item>" % (i, rng.choice(MSGS), rng.choice(SERVICES), 1 + i % 28) for i in range(500)) +
                "</channel></rss>")
    counts[xp] = 500
    yp = os.path.join(tail, "deploy-config.yaml")
    with open(yp, "w") as f:
        for s in SERVICES:
            f.write("%s:\n  replicas: %d\n  image: registry/%s:v1.%d\n  limits:\n    cpu: %dm\n    memory: %dMi\n" % (
                s, rng.randint(2, 12), s, rng.randint(0, 30), rng.randint(100, 2000), rng.randint(128, 4096)))
    counts[yp] = 1
    # junk: random binary with .csv extension (extension lies), true .bin, empty, truncated json
    with open(os.path.join(tail, "not-really.csv"), "wb") as f:
        f.write(os.urandom(2 * 1024 * 1024))
    with open(os.path.join(tail, "blob.bin"), "wb") as f:
        f.write(zlib.compress(os.urandom(1024 * 1024)))
    open(os.path.join(tail, "empty.log"), "w").close()
    with open(os.path.join(tail, "truncated.json"), "w") as f:
        f.write('{"a": [1,2,3, {"deep": "value", "unclosed": ')
    return counts

def count_existing(args):
    kind, (path, seed, target_mb) = args
    n = 0
    with open(path, "rb") as f:
        while True:
            chunk = f.read(1 << 22)
            if not chunk:
                break
            n += chunk.count(b"\n")
    if kind == "csv":
        n -= 1  # header
    return (path, n, os.path.getsize(path))

def main():
    os.makedirs(ROOT, exist_ok=True)
    for d in ("logs", "events", "syslog", "metrics"):
        os.makedirs(os.path.join(ROOT, d), exist_ok=True)
    jobs = []
    for i in range(12):
        jobs.append(("jsonl", (os.path.join(ROOT, "logs", "app-%02d.jsonl" % i), i, 320)))
    for i in range(8):
        jobs.append(("csv", (os.path.join(ROOT, "events", "events-%02d.csv" % i), 100 + i, 220)))
    for i in range(4):
        jobs.append(("syslog", (os.path.join(ROOT, "syslog", "sys-%02d.log" % i), 200 + i, 240)))
    for i in range(2):
        jobs.append(("metrics", (os.path.join(ROOT, "metrics", "metrics-%02d.ndjson" % i), 300 + i, 200)))
    fns = {"jsonl": jsonl_file, "csv": csv_file, "syslog": syslog_file, "metrics": metrics_file}
    manifest = {}
    todo, done = [], []
    for k, a in jobs:
        path, seed, target_mb = a
        if os.path.exists(path) and os.path.getsize(path) >= target_mb * 1024 * 1024:
            done.append((k, a))
        else:
            todo.append((k, a))
    with Pool(16) as pool:
        results = [pool.apply_async(fns[k], (a,)) for k, a in todo] + \
                  [pool.apply_async(count_existing, ((k, a),)) for k, a in done]
        for r in results:
            path, n, size = r.get()
            manifest[path] = {"records": n, "bytes": size}
    for p, n in tail_files(ROOT).items():
        manifest[p] = {"records": n, "bytes": os.path.getsize(p)}
    total_b = sum(v["bytes"] for v in manifest.values())
    total_r = sum(v["records"] for v in manifest.values())
    with open(os.path.join(ROOT, "..", "manifest.json"), "w") as f:
        json.dump({"files": manifest, "total_bytes": total_b, "total_records": total_r}, f, indent=1)
    print("corpus: %.2f GB, %d records, %d files" % (total_b / 2**30, total_r, len(manifest)))

if __name__ == "__main__":
    main()
