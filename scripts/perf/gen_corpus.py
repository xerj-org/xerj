#!/usr/bin/env python3
"""Deterministic synthetic corpus for the autoindex release gate.

Same --seed + --size-mb => byte-identical tree on every machine, so a gate
number from CI and one from a laptop are about the same input. The mix is
modelled on the real 5 GB folder behind #2421-#2428 (Rust/Go dependency
sources, docs, HTML, JSONL audit data, logs, ADS-B flight traces):

  share  family                         why it is here
  -----  -----------------------------  ------------------------------------
  24%    code (.rs .go .py .ts)          the bulk of real repos; docs index
  12%    markdown                        docs index, headings, links
   8%    HTML pages with <table>s        #2422: one index per page table
   9%    CSV (a few big + many small)    tabular datasets; small distinct
                                         schemas tempt one-index-per-table
  12%    JSONL records                   record data
  10%    logs                            line records
  15%    big JSON, numeric arrays        flight traces: few huge files
   5%    multi-array JSON + YAML          #2422: grouped arrays skipped caps
   5%    tiny files (< 1 KiB)            per-file overhead, many inodes
         deep/long paths                 #2424: 329-char index names

It also writes <out>/../queries.json (or --queries): 60 distinct queries, 20
per search surface, each built from planted phrases so every one has hits.
Only the stdlib is used.
"""
import argparse, json, os, random, sys

WORDS = ("alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike "
         "november oscar papa quebec romeo sierra tango uniform victor whiskey xray yankee zulu "
         "buffer cache segment shard replica merge flush commit journal snapshot cursor token "
         "parser lexer schema mapping field record batch stream socket packet header payload "
         "retry backoff timeout deadline latency throughput budget quota limit window bucket "
         "kernel driver module plugin handler router service client server worker thread pool "
         "vector matrix tensor gradient kernel filter reducer mapper sorter index search query").split()

# Planted topics: every query is drawn from these; each phrase is written into
# several families so all three surfaces can find it.
TOPICS = [
    "fsync wal segment rotation", "tokio spawn blocking task", "http client retry exponential backoff",
    "parse toml config file", "tls certificate verification error", "use after free netfilter",
    "disk budget flood stage watermark", "json schema validation failure", "aircraft altitude ground speed",
    "goroutine leak context cancel", "memtable flush threshold", "circuit breaker memory pressure",
    "bloom filter false positive rate", "raft leader election timeout", "btree page split",
    "gzip decompression bomb", "oauth refresh token rotation", "dns resolver cache poisoning",
    "postgres vacuum autovacuum", "kafka consumer lag offset", "rate limiter token bucket",
    "mmap page fault storm", "unicode normalization nfc", "csv quoting escape delimiter",
    "sql injection prepared statement", "cron schedule daylight saving", "s3 multipart upload abort",
    "grpc deadline exceeded status", "websocket ping pong keepalive", "lru eviction policy",
    "simd utf8 validation", "heap profile allocation hotspot", "deadlock mutex ordering",
    "zstd dictionary compression level", "hnsw graph recall ef", "bm25 term frequency saturation",
    "semver caret range resolution", "crate feature unification", "go module proxy checksum",
    "flight squawk transponder code", "runway heading crosswind", "audit log retention policy",
    "invoice line item total", "sensor temperature humidity reading", "inventory warehouse sku",
    "payment refund chargeback", "kubernetes pod eviction", "terraform state lock",
    "docker layer cache miss", "prometheus histogram bucket", "opentelemetry span context",
    "redis pipeline transaction", "elastic shard allocation", "lucene segment merge policy",
    "regex catastrophic backtracking", "integer overflow checked arithmetic", "async cancellation safety",
    "file descriptor exhaustion", "thread spawn resource unavailable", "index name too long",
]

class Gen:
    def __init__(self, out, size_mb, seed):
        self.out = out
        self.target = int(size_mb * 1024 * 1024)
        self.r = random.Random(seed)
        self.written = 0
        self.files = 0
        self.by_family = {}
        # Pre-built pools make generation I/O-bound, not RNG-bound.
        r = self.r
        self.sentences = [" ".join(r.choice(WORDS) for _ in range(r.randint(6, 18))).capitalize() + "."
                          for _ in range(4000)]
        self.idents = ["_".join(r.choice(WORDS) for _ in range(r.randint(1, 3))) for _ in range(3000)]

    # ---------- helpers ----------
    def topic(self):
        return self.r.choice(TOPICS)

    def para(self, n):
        s = self.r.choices(self.sentences, k=n)
        if self.r.random() < 0.5:
            s.insert(self.r.randrange(len(s) + 1), "Notes on %s." % self.topic())
        return " ".join(s)

    def write(self, fam, rel, data):
        p = os.path.join(self.out, rel)
        os.makedirs(os.path.dirname(p), exist_ok=True)
        b = data.encode() if isinstance(data, str) else data
        with open(p, "wb") as f:
            f.write(b)
        self.written += len(b)
        self.files += 1
        st = self.by_family.setdefault(fam, [0, 0])
        st[0] += 1
        st[1] += len(b)
        return len(b)

    # ---------- families ----------
    def code_file(self, i, nbytes=8192):
        r = self.r
        lang = r.choice(["rs", "go", "py", "ts"])
        repo = "repo-%02d" % (i % 24)
        mod = "/".join(r.choice(self.idents) for _ in range(r.randint(1, 4)))
        lines = []; size = 0; f = 0
        while size < nbytes:
            f += 1
            name = r.choice(self.idents) + "_%d" % f
            t = self.topic()
            if lang == "rs":
                l = "/// %s\npub fn %s(input: &[u8], limit: usize) -> Result<usize, Error> {\n    // %s\n    let mut n = 0usize;\n    for b in input.iter().take(limit) { n = n.wrapping_add(*b as usize); }\n    Ok(n)\n}\n" % (r.choice(self.sentences), name, t)
            elif lang == "go":
                l = "// %s handles %s.\nfunc %s(ctx context.Context, in []byte) (int, error) {\n\tif ctx.Err() != nil { return 0, ctx.Err() }\n\treturn len(in), nil\n}\n" % (name, t, name)
            elif lang == "py":
                l = 'def %s(data, limit=%d):\n    """%s -- %s"""\n    return sum(data[:limit])\n' % (name, r.randint(1, 999), r.choice(self.sentences), t)
            else:
                l = "// %s\nexport function %s(xs: number[]): number {\n  return xs.reduce((a, b) => a + b, %d);\n}\n" % (t, name, r.randint(0, 9))
            lines.append(l); size += len(l) + 1
        self.write("code", "%s/src/%s/mod_%d.%s" % (repo, mod, i, lang), "\n".join(lines))

    def markdown(self, i, nbytes=4096):
        r = self.r
        body = ["# %s\n" % self.topic().title()]; size = 0
        while size < nbytes:
            l = "## %s\n\n%s\n" % (r.choice(self.idents).replace("_", " ").title(), self.para(r.randint(3, 10)))
            if r.random() < 0.3:
                l += "```\n%s\n```\n" % r.choice(self.sentences)
            body.append(l); size += len(l)
        self.write("markdown", "docs/%s/%s/note-%d.md" % (r.choice(WORDS), r.choice(WORDS), i), "\n".join(body))

    def html(self, i, nbytes=4096):
        r = self.r
        # A distinct column set per page: the #2422 trigger (202 of 454 b5
        # indices were HTML page tables, 197 of them under 16 rows).
        cols = r.sample(WORDS, r.randint(3, 6))
        rows = r.randint(3, 40)
        trs = "".join("<tr>%s</tr>" % "".join("<td>%s</td>" % (r.randint(0, 99999) if c in WORDS[:20] else r.choice(WORDS)) for c in cols) for _ in range(rows))
        paras = []; size = len(trs)
        while size < nbytes:
            p = "<h2>%s</h2><p>%s</p>" % (self.topic(), self.para(8)); paras.append(p); size += len(p)
        doc = ("<!doctype html><html><head><title>%s</title></head><body><h1>%s</h1><p>%s</p>"
               "<table><thead><tr>%s</tr></thead><tbody>%s</tbody></table>%s</body></html>") % (
            self.topic(), self.topic(), self.para(6), "".join("<th>%s_%d</th>" % (c, i % 97) for c in cols), trs, "".join(paras))
        self.write("html", "site/%s/page-%d.html" % (r.choice(WORDS), i), doc)

    def csv_small(self, i):
        r = self.r
        cols = ["%s_%d" % (c, i) for c in r.sample(WORDS, r.randint(3, 6))]
        lines = [",".join(cols)]
        for k in range(r.randint(4, 30)):
            lines.append(",".join(str(r.randint(0, 9999)) if j % 2 else r.choice(WORDS) for j in range(len(cols))))
        self.write("csv", "tables/t%03d/%s.csv" % (i % 400, r.choice(WORDS)), "\n".join(lines) + "\n")

    def csv_big(self, i, nbytes):
        r = self.r
        out = ["ts,sensor_id,site,temperature,humidity,status,note"]
        n = len(out[0])
        while n < nbytes:
            l = "%d,%d,%s,%.2f,%.1f,%s,%s" % (1700000000 + r.randint(0, 10**7), r.randint(1, 5000), r.choice(WORDS),
                                            r.uniform(-20, 45), r.uniform(0, 100), r.choice(["ok", "warn", "fail"]),
                                            r.choice(TOPICS))
            out.append(l); n += len(l) + 1
        self.write("csv", "warehouse/sensors/readings-%d.csv" % i, "\n".join(out) + "\n")

    def jsonl(self, i, nbytes):
        r = self.r
        out = []; n = 0
        while n < nbytes:
            rec = {"id": r.randint(1, 10**9), "ts": 1700000000 + r.randint(0, 10**7), "actor": r.choice(self.idents),
                   "action": r.choice(["create", "update", "delete", "login", "export"]), "topic": r.choice(TOPICS),
                   "message": r.choice(self.sentences), "amount": round(r.uniform(0, 5000), 2)}
            l = json.dumps(rec, separators=(",", ":")); out.append(l); n += len(l) + 1
        self.write("jsonl", "audit/%d/events-%d.jsonl" % (i % 12, i), "\n".join(out) + "\n")

    def log(self, i, nbytes):
        r = self.r
        out = []; n = 0
        lv = ["INFO", "WARN", "ERROR", "DEBUG"]
        while n < nbytes:
            l = "2026-10-%02dT%02d:%02d:%02d.%03dZ %s %s: %s (%s)" % (r.randint(1, 28), r.randint(0, 23), r.randint(0, 59), r.randint(0, 59),
                                                                 r.randint(0, 999), r.choice(lv), r.choice(self.idents), r.choice(self.sentences), r.choice(TOPICS))
            out.append(l); n += len(l) + 1
        self.write("logs", "var/log/svc-%d/app-%d.log" % (i % 8, i), "\n".join(out) + "\n")

    def flight_json(self, i, nbytes):
        # ADS-B style: one object with huge parallel numeric arrays.
        r = self.r
        n = max(1000, nbytes // 60)
        t0 = 1729000000
        obj = {"icao": "%06x" % r.randint(0, 0xFFFFFF), "callsign": "XRJ%04d" % i, "note": r.choice(TOPICS),
               "trace": [[t0 + k, round(r.uniform(-90, 90), 5), round(r.uniform(-180, 180), 5), r.randint(0, 45000),
                          round(r.uniform(0, 600), 1), r.randint(0, 359)] for k in range(n)]}
        self.write("flight_json", "adsb/traces/%02d/trace_full_%06x.json" % (i % 16, i), json.dumps(obj, separators=(",", ":")))

    def multi_array_json(self, i):
        r = self.r
        obj = {}
        for a in range(r.randint(2, 6)):
            keys = r.sample(WORDS, 3)
            obj["%s_list" % r.choice(WORDS)] = [{k: r.randint(0, 999) if j else r.choice(WORDS) for j, k in enumerate(keys)} for _ in range(r.randint(2, 20))]
        if r.random() < 0.3:
            body = "\n".join("%s: %s" % (r.choice(self.idents), r.choice(WORDS)) for _ in range(r.randint(5, 40)))
            self.write("multi_json", "config/%s/settings-%d.yaml" % (r.choice(WORDS), i), body + "\n")
        else:
            self.write("multi_json", "grammars/%s/spec-%d.json" % (r.choice(WORDS), i), json.dumps(obj, indent=1))

    def tiny(self, i):
        r = self.r
        self.write("tiny", "small/%02d/%02d/f%d.txt" % (i % 50, (i // 50) % 50, i), r.choice(self.sentences) + " " + self.topic() + "\n")

    def deep(self, i):
        # Go-module-like deep paths: the slug that produced a 329-char index name (#2424).
        r = self.r
        segs = ["golang.org", "x", "github.com-%s-%s" % (r.choice(WORDS), r.choice(WORDS))]
        segs += ["%s-%s-%s-%s-v%d" % (r.choice(WORDS), r.choice(WORDS), r.choice(WORDS), r.choice(WORDS), r.randint(1, 9)) for _ in range(r.randint(8, 14))]
        p = "gomod/" + "/".join(segs)
        cols = ["%s_%d" % (c, i) for c in r.sample(WORDS, 3)]
        self.write("deep_paths", p + "/data-%d.csv" % i, ",".join(cols) + "\n" + "\n".join("%d,%s,%d" % (k, r.choice(WORDS), r.randint(0, 99)) for k in range(5)) + "\n")
        self.write("deep_paths", p + "/README-%d.md" % i, "# %s\n\n%s\n" % (self.topic(), self.para(4)))

    def run(self):
        T = self.target
        gb = T / float(1 << 30)
        shares = [("code", .24), ("markdown", .12), ("html", .08), ("csv", .09), ("jsonl", .12), ("logs", .10),
                  ("flight_json", .15), ("multi_json", .05), ("tiny", .05)]
        budget = {k: T * v for k, v in shares}
        # File COUNTS per GB, tuned to the real folder's ~30k files/GB: the
        # count drives per-file and per-dataset overhead, the byte share
        # drives ingest volume. Sizes grow with --size-mb; counts scale too
        # but never drop below a floor, so a 50 MB smoke run keeps the shape.
        def cnt(per_gb, floor):
            return max(floor, int(per_gb * gb))
        for i in range(cnt(120, 20)):
            self.deep(i)
        # Big-file families: a fixed number of large files whose size scales.
        for i in range(4):
            self.flight_json(i, int(budget["flight_json"] / 4))
        for i in range(24):
            self.jsonl(i, int(budget["jsonl"] / 24))
        for i in range(16):
            self.log(i, int(budget["logs"] / 16))
        for i in range(3):
            self.csv_big(i, int(budget["csv"] * 0.8 / 3))
        n_small_csv = cnt(800, 60)
        for i in range(n_small_csv):
            self.csv_small(i)
        for i in range(cnt(1500, 80)):
            self.multi_array_json(i)
        for i in range(cnt(8000, 400)):
            self.tiny(i)
        n_html = cnt(3000, 150)
        for i in range(n_html):
            self.html(i, int(budget["html"] / n_html))
        n_md = cnt(6000, 300)
        for i in range(n_md):
            self.markdown(i, int(budget["markdown"] / n_md))
        n_code = cnt(16000, 800)
        for i in range(n_code):
            self.code_file(i, int(budget["code"] / n_code))
        # Top up with more code files so the total lands on --size-mb.
        i = n_code
        while self.written < T:
            self.code_file(i, int(budget["code"] / n_code)); i += 1
        return self


def queries(seed):
    r = random.Random(seed + 1)
    t = TOPICS[:]
    r.shuffle(t)
    # 60 distinct strings, 20 per surface; no string repeats across surfaces,
    # so a cache warmed by one surface cannot serve another.
    return {"cli": t[0:20], "raw": t[20:40], "mcp": t[40:60]}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--size-mb", type=float, default=1024)
    ap.add_argument("--seed", type=int, default=2421)
    ap.add_argument("--queries", help="write the query set here (default <out>.queries.json)")
    a = ap.parse_args()
    if os.path.exists(a.out) and os.listdir(a.out):
        sys.exit("refusing to write into non-empty %s" % a.out)
    os.makedirs(a.out, exist_ok=True)
    g = Gen(a.out, a.size_mb, a.seed).run()
    qpath = a.queries or a.out.rstrip("/") + ".queries.json"
    with open(qpath, "w") as f:
        json.dump(queries(a.seed), f, indent=1)
    manifest = {"seed": a.seed, "size_mb_requested": a.size_mb, "bytes": g.written, "files": g.files,
                "by_family": {k: {"files": v[0], "bytes": v[1]} for k, v in sorted(g.by_family.items())},
                "queries": qpath}
    with open(a.out.rstrip("/") + ".manifest.json", "w") as f:
        json.dump(manifest, f, indent=1)
    print(json.dumps(manifest))


if __name__ == "__main__":
    main()
