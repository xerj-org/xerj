#!/usr/bin/env python3
"""Measurement + evaluation core of the autoindex release gate.

  gate.py run  --bin X --corpus DIR --queries Q.json --work DIR [...] --out R.json
  gate.py eval --budgets budgets.toml --tier 1gb --candidate R.json
               [--baseline B.json] [--engine-baseline ES.json] --md OUT.md --json OUT.json

`run` measures ONE binary end to end on a private port with a throwaway data
dir and state dir; `eval` turns one or two result files into a verdict
table. Only the Python 3.11+ stdlib is used. Never submits xerj feedback
(XERJ_DISABLE_FEEDBACK=true everywhere).
"""
import argparse, json, os, re, signal, statistics, subprocess, sys, threading, time, tomllib, urllib.request

MiB = 1024 * 1024
FATAL_PATTERNS = {
    "eagain": re.compile(r"os error 11|Resource temporarily unavailable|EAGAIN", re.I),
    "oom": re.compile(r"out of memory|memory allocation of \d+ bytes failed|Cannot allocate memory|OOM", re.I),
    "abort": re.compile(r"SIGABRT|panicked at|FATAL:", re.I),
    "emfile": re.compile(r"Too many open files|os error 24", re.I),
}
BREAKER_RE = re.compile(r"engaging the parent memory circuit breaker", re.I)
RETRY_RE = re.compile(r"\bretry(ing)?\b|backing off|\b429\b|too many requests", re.I)


def log(*a):
    print("[gate]", *a, file=sys.stderr, flush=True)


def tree_bytes(path):
    total = 0
    for root, _, files in os.walk(path):
        for f in files:
            try:
                total += os.lstat(os.path.join(root, f)).st_size
            except OSError:
                pass
    return total


def proc_status(pid):
    out = {}
    try:
        with open("/proc/%d/status" % pid) as f:
            for line in f:
                k, _, v = line.partition(":")
                if k in ("VmRSS", "VmHWM"):
                    out[k] = int(v.split()[0]) // 1024  # MB
                elif k == "Threads":
                    out[k] = int(v)
    except (OSError, ValueError):
        pass
    return out


def children(pid):
    try:
        with open("/proc/%d/task/%d/children" % (pid, pid)) as f:
            return [int(x) for x in f.read().split()]
    except OSError:
        return []


def tree_rss(pid):
    rss, stack = 0, [pid]
    while stack:
        p = stack.pop()
        rss += proc_status(p).get("VmRSS", 0)
        stack.extend(children(p))
    return rss


class Sampler(threading.Thread):
    """Peak RSS / thread count of the node and the autoindex client tree."""

    def __init__(self):
        super().__init__(daemon=True)
        self.node_pid = None
        self.client_pid = None
        self.node_peak = 0
        self.client_peak = 0
        self.client_threads_peak = 0
        self.stop = False

    def run(self):
        while not self.stop:
            if self.node_pid:
                s = proc_status(self.node_pid)
                self.node_peak = max(self.node_peak, s.get("VmHWM", 0), s.get("VmRSS", 0))
            if self.client_pid:
                s = proc_status(self.client_pid)
                self.client_peak = max(self.client_peak, tree_rss(self.client_pid), s.get("VmHWM", 0))
                self.client_threads_peak = max(self.client_threads_peak, s.get("Threads", 0))
            time.sleep(0.25)


def http(url, body=None, timeout=60):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, headers={"Content-Type": "application/json"},
                                 method="POST" if data else "GET")
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read() or b"null")


class Node:
    def __init__(self, a, env, sampler):
        self.a, self.env, self.sampler = a, env, sampler
        self.url = "http://127.0.0.1:%d" % a.port
        self.p = None
        self.logs = []

    def start(self, tag):
        logp = os.path.join(self.a.work, "node-%s.log" % tag)
        self.logs.append(logp)
        t = time.time()
        self.p = subprocess.Popen([self.a.bin, "--insecure", "--data-dir", os.path.join(self.a.work, "data"),
                                   "--port", str(self.a.port), "--disable-feedback"],
                                  stdout=open(logp, "w"), stderr=subprocess.STDOUT, env=self.env)
        self.sampler.node_pid = self.p.pid
        while time.time() - t < 300:
            if self.p.poll() is not None:
                raise RuntimeError("node exited during boot (rc=%s), see %s" % (self.p.returncode, logp))
            try:
                http(self.url + "/_cluster/health", timeout=2)
                return time.time() - t
            except Exception:
                time.sleep(0.25)
        raise RuntimeError("node never became healthy")

    def alive(self):
        return self.p is not None and self.p.poll() is None

    def stop(self):
        if not self.alive():
            return
        self.p.send_signal(signal.SIGINT)
        try:
            self.p.wait(120)
        except subprocess.TimeoutExpired:
            self.p.kill(); self.p.wait()


def run_autoindex(a, env, node, sampler, res):
    out = os.path.join(a.work, "autoindex.json")
    err = os.path.join(a.work, "autoindex.stderr")
    cmd = [a.bin, "autoindex", a.corpus, "--url", node.url, "--prefix", a.prefix,
           "--state-dir", os.path.join(a.work, "state"), "--approve", "proceed",
           "--json", "--progress", "plain", "--disable-feedback"] + a.autoindex_args
    log("autoindex:", " ".join(cmd))
    t = time.time()
    p = subprocess.Popen(cmd, stdout=open(out, "w"), stderr=open(err, "w"), env=env)
    sampler.client_pid = p.pid
    timed_out = False
    try:
        p.wait(a.timeout_min * 60)
    except subprocess.TimeoutExpired:
        timed_out = True
        p.send_signal(signal.SIGINT)
        try:
            p.wait(30)
        except subprocess.TimeoutExpired:
            p.kill(); p.wait()
    wall = time.time() - t
    sampler.client_pid = None
    res["autoindex"] = {"exit_code": p.returncode, "wall_s": round(wall, 1), "timed_out": timed_out, "cmd": cmd}
    try:
        txt = open(out).read().strip()
        j = json.loads(txt[txt.index("{"):]) if txt else {}
    except Exception:
        j = {}
    keep = ["files_total", "files_indexed", "records_total", "docs_per_sec", "workers", "bulk_congestion_events",
            "bulk_concurrency_final", "files_junk", "wall_seconds"]
    res["autoindex"]["summary"] = {k: j.get(k) for k in keep if k in j}
    stderr_txt = open(err, errors="replace").read()
    res["autoindex"]["stderr_tail"] = stderr_txt[-2000:]
    prog = re.findall(r"xerj-progress phase=index basis=bytes pct=[0-9.]+ items=\S+ bytes=(\d+)/(\d+)", stderr_txt)
    if prog:
        res["autoindex"]["progress_bytes_done"] = int(prog[-1][0])
    return stderr_txt


def q_cli(a, env, url, q):
    t = time.time()
    try:
        r = subprocess.run([a.bin, "search", "--url", url, "--prefix", a.prefix, "-k", "5", "--json", q],
                           env=env, capture_output=True, text=True, timeout=a.query_timeout)
        ms = (time.time() - t) * 1000
        j = json.loads(r.stdout)
        hits = len(j.get("hits", {}).get("hits", [])) if isinstance(j, dict) else 0
        return ms, hits, None if r.returncode == 0 else "rc=%d" % r.returncode
    except subprocess.TimeoutExpired:
        return a.query_timeout * 1000.0, 0, "timeout"
    except Exception as e:
        return (time.time() - t) * 1000, 0, "parse: %s" % str(e)[:80]


def q_raw(a, env, url, q):
    body = {"size": 5, "query": {"simple_query_string": {"query": q, "fields": ["*"], "lenient": True}}}
    t = time.time()
    try:
        j = http("%s/%s-*/_search?request_cache=false" % (url, a.prefix), body, timeout=a.query_timeout)
        ms = (time.time() - t) * 1000
        return ms, len(j.get("hits", {}).get("hits", [])), None
    except Exception as e:
        return (time.time() - t) * 1000, 0, str(e)[:80]


class MCP:
    def __init__(self, a, env, url):
        self.a = a
        self.p = subprocess.Popen([a.bin, "mcp", "--url", url, "--disable-feedback"], stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, env=env, bufsize=1)
        self.i = 0
        self.call("initialize", {"protocolVersion": "2024-11-05", "capabilities": {},
                                 "clientInfo": {"name": "perf-gate", "version": "1"}})
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
        self.p.stdin.flush()

    def call(self, m, params):
        self.i += 1
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.i, "method": m, "params": params}) + "\n")
        self.p.stdin.flush()
        while True:
            line = self.p.stdout.readline()
            if not line:
                raise RuntimeError("mcp eof")
            try:
                j = json.loads(line)
            except ValueError:
                continue
            if j.get("id") == self.i:
                return j

    def query(self, q):
        t = time.time()
        try:
            j = self.call("tools/call", {"name": "xerj_search", "arguments": {"prefix": self.a.prefix, "query": q, "size": 5}})
        except Exception as e:
            return (time.time() - t) * 1000, 0, str(e)[:80]
        ms = (time.time() - t) * 1000
        r = j.get("result", {})
        txt = "".join(c.get("text", "") for c in r.get("content", []))
        hits = 0
        try:
            jj = json.loads(txt)
            hits = len(jj.get("hits", {}).get("hits", jj.get("results", []))) if isinstance(jj, dict) else len(jj)
        except Exception:
            hits = 1 if txt.strip() and "no results" not in txt.lower() else 0
        if r.get("isError") or "error" in j:
            msg = txt or json.dumps(j.get("error"))
            return ms, 0, ("mcp error: " + msg.replace("\n", " "))[:200]
        return ms, hits, None

    def close(self):
        try:
            self.p.stdin.close(); self.p.wait(10)
        except Exception:
            self.p.kill()


def pct(v, p):
    v = sorted(v)
    if not v:
        return None
    k = (len(v) - 1) * p / 100.0
    lo = int(k); hi = min(lo + 1, len(v) - 1)
    return v[lo] + (v[hi] - v[lo]) * (k - lo)


def cmd_run(a):
    os.makedirs(a.work, exist_ok=True)
    env = dict(os.environ, XERJ_DISABLE_FEEDBACK="true", XERJ_MAX_PROCESS_MEMORY_MB=str(a.cap_mb),
               XERJ_URL="http://127.0.0.1:%d" % a.port)
    env.pop("XERJ_API_KEY", None)
    queries = json.load(open(a.queries))
    res = {"label": a.label, "bin": a.bin, "corpus": a.corpus, "cap_mb": a.cap_mb, "started": time.strftime("%Y-%m-%dT%H:%M:%S%z")}
    try:
        res["version"] = subprocess.run([a.bin, "--version"], capture_output=True, text=True, env=env, timeout=30).stdout.strip()
    except Exception:
        res["version"] = "?"
    res["source_bytes"] = tree_bytes(a.corpus)
    sampler = Sampler(); sampler.start()
    node = Node(a, env, sampler)
    fatal = {}
    try:
        res["boot_s"] = round(node.start("ingest"), 2)
        stderr_txt = run_autoindex(a, env, node, sampler, res)
        res["node_alive_after_autoindex"] = node.alive()
        try:
            cat = http("%s/_cat/indices/%s-*?format=json&bytes=b&h=index,docs.count,store.size" % (node.url, a.prefix))
        except Exception:
            cat = []
        res["indices"] = len(cat)
        res["docs"] = sum(int(c.get("docs.count") or 0) for c in cat)
        res["index_store_bytes"] = sum(int(c.get("store.size") or 0) for c in cat)
        res["node_peak_rss_mb_ingest"] = sampler.node_peak
        node.stop()
        node_txt = "".join(open(p, errors="replace").read() for p in node.logs)
        res["breaker_trips"] = len(BREAKER_RE.findall(node_txt))
        res["bulk_retries"] = int(res["autoindex"]["summary"].get("bulk_congestion_events") or 0) + len(RETRY_RE.findall(stderr_txt))
        for k, rx in FATAL_PATTERNS.items():
            n = len(rx.findall(stderr_txt)) + len(rx.findall(node_txt))
            if n:
                fatal[k] = n
        res["fatal"] = fatal
        res["state_bytes"] = tree_bytes(os.path.join(a.work, "state"))
        res["data_dir_bytes"] = tree_bytes(os.path.join(a.work, "data"))
        res["client_peak_rss_mb"] = sampler.client_peak
        res["client_threads_peak"] = sampler.client_threads_peak
        if a.drop_caches:
            subprocess.run("sync; echo 3 | sudo -n tee /proc/sys/vm/drop_caches >/dev/null", shell=True)
        # Restart so every query below is served by a cold process (no
        # request/query/segment caches), the way an agent's first call is.
        res["restart_s"] = round(node.start("search"), 2)
        lat = {}
        for surface in ("cli", "raw", "mcp"):
            rows = []
            m = MCP(a, env, node.url) if surface == "mcp" else None
            for q in queries[surface]:
                if surface == "cli":
                    ms, hits, e = q_cli(a, env, node.url, q)
                elif surface == "raw":
                    ms, hits, e = q_raw(a, env, node.url, q)
                else:
                    ms, hits, e = m.query(q)
                rows.append({"q": q, "ms": round(ms, 1), "hits": hits, "err": e})
            if m:
                m.close()
            lat[surface] = rows
            ms = [r["ms"] for r in rows]
            log("%s p50=%.0f p95=%.0f max=%.0f ms" % (surface, pct(ms, 50), pct(ms, 95), max(ms)))
        res["queries"] = lat
        allms = [r["ms"] for rows in lat.values() for r in rows]
        res["search"] = {"n": len(allms), "p50_ms": round(pct(allms, 50), 1), "p95_ms": round(pct(allms, 95), 1),
                         "max_ms": round(max(allms), 1),
                         "with_hits_pct": round(100.0 * sum(1 for rows in lat.values() for r in rows if r["hits"] and not r["err"]) / len(allms), 1),
                         "errors": sum(1 for rows in lat.values() for r in rows if r["err"]),
                         "first_error": next(("%s: %s" % (k, r["err"]) for k, rows in lat.items() for r in rows if r["err"]), None)}
        for s, rows in lat.items():
            ms = [r["ms"] for r in rows]
            res["search"][s] = {"p50_ms": round(pct(ms, 50), 1), "p95_ms": round(pct(ms, 95), 1), "max_ms": round(max(ms), 1)}
    except Exception as e:
        res["error"] = str(e)
        log("ERROR", e)
    finally:
        res["node_peak_rss_mb"] = sampler.node_peak
        node.stop()
        sampler.stop = True
    ai = res.get("autoindex", {})
    if ai.get("wall_s"):
        done = res["source_bytes"]
        if ai.get("exit_code") not in (0, 3) or ai.get("timed_out"):
            # incomplete run: count only bytes autoindex reported as indexed
            done = min(done, ai.get("progress_bytes_done") or 0)
        res["indexed_bytes"] = done
        res["throughput_mb_s"] = round(done / MiB / ai["wall_s"], 2)
    res["state_to_source_ratio"] = round(res.get("state_bytes", 0) / max(1, res["source_bytes"]), 3)
    res["index_bytes"] = res.get("data_dir_bytes", 0)
    json.dump(res, open(a.out, "w"), indent=1)
    log("wrote", a.out)


# ---------------------------------------------------------------- eval ----
def cmd_eval(a):
    B = tomllib.load(open(a.budgets, "rb"))
    t = B["tier"][a.tier]
    ratio = B.get("ratio", {}).get(a.tier, B.get("ratio", {}).get("default", {}))
    c = json.load(open(a.candidate))
    base = json.load(open(a.baseline)) if a.baseline else None
    eng = json.load(open(a.engine_baseline)) if a.engine_baseline else None
    rows = []  # (check, value, budget, ok, note)

    def add(name, val, budget, ok, note=""):
        rows.append({"check": name, "value": val, "budget": budget, "pass": bool(ok), "note": note})

    ai = c.get("autoindex", {})
    fatal = c.get("fatal", {})
    completed = (not c.get("error") and not ai.get("timed_out") and ai.get("exit_code") in (0, 3)
                 and c.get("node_alive_after_autoindex", False) and not fatal)
    note = []
    if ai.get("timed_out"): note.append("timed out after %s s" % ai.get("wall_s"))
    if ai.get("exit_code") not in (0, 3): note.append("exit %s" % ai.get("exit_code"))
    if not c.get("node_alive_after_autoindex", False): note.append("node died")
    if fatal: note.append(", ".join("%s x%d" % kv for kv in fatal.items()))
    if c.get("error"): note.append(c["error"][:80])
    add("autoindex completes (no OOM/abort/EAGAIN/timeout)", "yes" if completed else "NO", "exit 0/3, node alive", completed, "; ".join(note))
    tp = c.get("throughput_mb_s")
    add("autoindex throughput (MB/s)", tp, ">= %s" % t["min_throughput_mb_s"], completed and tp is not None and tp >= t["min_throughput_mb_s"],
        "%.0f of %.0f MB indexed in %s s" % (c.get("indexed_bytes", c.get("source_bytes", 0)) / MiB, c.get("source_bytes", 0) / MiB, ai.get("wall_s")))
    add("node peak RSS (MB), cap %d" % t["node_memory_cap_mb"], c.get("node_peak_rss_mb"), "<= %d" % t["max_node_peak_rss_mb"],
        (c.get("node_peak_rss_mb") or 0) <= t["max_node_peak_rss_mb"],
        "XERJ_MAX_PROCESS_MEMORY_MB=%s" % c.get("cap_mb"))
    add("client peak RSS (MB)", c.get("client_peak_rss_mb"), "<= %d" % t["max_client_peak_rss_mb"],
        (c.get("client_peak_rss_mb") or 0) <= t["max_client_peak_rss_mb"], "peak threads %s" % c.get("client_threads_peak"))
    sr = c.get("state_to_source_ratio", 0)
    add("state dir / source", sr, "<= %s" % t["max_state_to_source_ratio"], sr <= t["max_state_to_source_ratio"],
        "%.0f MB state" % (c.get("state_bytes", 0) / MiB))
    if "max_index_plus_state_mb" in t:
        tot = (c.get("index_bytes") or 0) + (c.get("state_bytes") or 0)
        add("index + state on disk (MB)", round(tot / MiB), "<= %d" % t["max_index_plus_state_mb"],
            tot / MiB <= t["max_index_plus_state_mb"], "%.2fx source" % (tot / max(1, c.get("source_bytes", 1))))
    add("indices per prefix", c.get("indices"), "<= %d" % t["max_indices_per_prefix"],
        0 < (c.get("indices") or 0) <= t["max_indices_per_prefix"], "%s docs" % c.get("docs"))
    s = c.get("search", {})
    add("uncached search p95 (ms)", s.get("p95_ms"), "<= %d" % t["max_search_p95_ms"],
        s.get("p95_ms") is not None and s["p95_ms"] <= t["max_search_p95_ms"],
        " / ".join("%s %s" % (k, s[k]["p95_ms"]) for k in ("cli", "raw", "mcp") if k in s))
    add("uncached search max (ms)", s.get("max_ms"), "<= %d" % t["max_search_max_ms"],
        s.get("max_ms") is not None and s["max_ms"] <= t["max_search_max_ms"], "n=%s, errors=%s" % (s.get("n"), s.get("errors")))
    add("queries with hits (%)", s.get("with_hits_pct"), ">= %d" % t["min_queries_with_hits_pct"],
        (s.get("with_hits_pct") or 0) >= t["min_queries_with_hits_pct"])
    add("search errors", s.get("errors"), "0", s.get("errors") == 0, (s.get("first_error") or "")[:120])
    add("circuit-breaker trips", c.get("breaker_trips"), "<= %d" % t["max_breaker_trips"],
        c.get("breaker_trips") is not None and c["breaker_trips"] <= t["max_breaker_trips"])
    add("bulk retries / 429s", c.get("bulk_retries"), "<= %d" % t["max_bulk_retries"],
        c.get("bulk_retries") is not None and c["bulk_retries"] <= t["max_bulk_retries"])
    add("node restart (s)", c.get("restart_s"), "<= %d" % t["max_restart_s"],
        c.get("restart_s") is not None and c["restart_s"] <= t["max_restart_s"])

    lim = t["max_regression_pct"] / 100.0
    if base:
        def reg(name, key_fn, higher_better):
            cv, bv = key_fn(c), key_fn(base)
            if cv is None or bv is None or bv == 0:
                add("vs baseline: " + name, cv, "baseline %s" % bv, True, "skipped (missing)")
                return
            delta = (cv - bv) / bv
            worse = -delta if higher_better else delta
            add("vs baseline: " + name, cv, "%s %s (<= %d%% worse)" % ("baseline", bv, t["max_regression_pct"]),
                worse <= lim, "%+.0f%%" % (delta * 100))
        reg("throughput MB/s", lambda r: r.get("throughput_mb_s"), True)
        reg("search p95 ms", lambda r: r.get("search", {}).get("p95_ms"), False)
        reg("node peak RSS MB", lambda r: r.get("node_peak_rss_mb"), False)
        reg("client peak RSS MB", lambda r: r.get("client_peak_rss_mb"), False)
        reg("state/source", lambda r: r.get("state_to_source_ratio"), False)
        reg("index bytes", lambda r: r.get("index_bytes"), False)
    if eng and ratio:
        en = eng.get("engine", "baseline engine")
        eb = eng.get("source_bytes") or (eng.get("corpus_mb", 0) * MiB)
        if eb and abs(eb - c.get("source_bytes", 0)) > 0.05 * eb:
            add("engine baseline is for this corpus", "%.0f MB" % (eb / MiB), "%.0f MB" % (c.get("source_bytes", 0) / MiB), False,
                "re-measure the baseline on the same corpus")
        if "max_index_bytes_vs_baseline" in ratio and eng.get("index_bytes"):
            r_ = c.get("index_bytes", 0) / eng["index_bytes"]
            add("index bytes vs %s" % en, round(r_, 2), "<= %sx" % ratio["max_index_bytes_vs_baseline"],
                r_ <= ratio["max_index_bytes_vs_baseline"], "%.0f MB vs %.0f MB" % (c.get("index_bytes", 0) / MiB, eng["index_bytes"] / MiB))
        if "min_ingest_mb_s_vs_baseline" in ratio and eng.get("ingest_mb_s"):
            r_ = (c.get("throughput_mb_s") or 0) / eng["ingest_mb_s"]
            add("ingest MB/s vs %s" % en, round(r_, 2), ">= %sx" % ratio["min_ingest_mb_s_vs_baseline"],
                r_ >= ratio["min_ingest_mb_s_vs_baseline"], "%s vs %s MB/s" % (c.get("throughput_mb_s"), eng["ingest_mb_s"]))
        if "max_search_p95_vs_baseline" in ratio and eng.get("search_p95_ms") and s.get("p95_ms"):
            r_ = s["p95_ms"] / eng["search_p95_ms"]
            add("search p95 vs %s" % en, round(r_, 2), "<= %sx" % ratio["max_search_p95_vs_baseline"],
                r_ <= ratio["max_search_p95_vs_baseline"], "%s vs %s ms" % (s["p95_ms"], eng["search_p95_ms"]))

    ok = all(r["pass"] for r in rows)
    md = ["### autoindex release gate: %s (%s, tier %s)" % ("PASS" if ok else "FAIL", c.get("version"), a.tier), "",
          "| check | value | budget | result | note |", "|---|---|---|---|---|"]
    for r in rows:
        md.append("| %s | %s | %s | %s | %s |" % (r["check"], r["value"], r["budget"], "pass" if r["pass"] else "**FAIL**", r["note"].replace("|", "/")))
    md.append("")
    md.append("corpus %.0f MB, %s; %d indices, %s docs, index %.0f MB on disk" % (
        c.get("source_bytes", 0) / MiB, c.get("corpus"), c.get("indices") or 0, c.get("docs"), c.get("index_bytes", 0) / MiB))
    text = "\n".join(md)
    print(text)
    if a.md:
        open(a.md, "w").write(text + "\n")
    if a.json:
        json.dump({"pass": ok, "tier": a.tier, "budgets": t, "ratio_budgets": ratio if eng else None, "checks": rows,
                   "candidate": c, "baseline": base, "engine_baseline": eng}, open(a.json, "w"), indent=1)
    sys.exit(0 if ok else 1)


def main():
    ap = argparse.ArgumentParser()
    sp = ap.add_subparsers(dest="cmd", required=True)
    r = sp.add_parser("run")
    r.add_argument("--bin", required=True); r.add_argument("--corpus", required=True)
    r.add_argument("--queries", required=True); r.add_argument("--work", required=True)
    r.add_argument("--out", required=True); r.add_argument("--label", default="candidate")
    r.add_argument("--port", type=int, default=9400); r.add_argument("--prefix", default="pg")
    r.add_argument("--cap-mb", type=int, default=3072); r.add_argument("--timeout-min", type=float, default=15)
    r.add_argument("--query-timeout", type=float, default=60)
    r.add_argument("--drop-caches", action="store_true")
    r.add_argument("--autoindex-args", default="", help="extra args, space separated")
    e = sp.add_parser("eval")
    e.add_argument("--budgets", required=True); e.add_argument("--tier", default="1gb")
    e.add_argument("--candidate", required=True); e.add_argument("--baseline")
    e.add_argument("--engine-baseline"); e.add_argument("--md"); e.add_argument("--json")
    a = ap.parse_args()
    if a.cmd == "run":
        a.bin = os.path.abspath(a.bin); a.corpus = os.path.abspath(a.corpus); a.work = os.path.abspath(a.work)
        a.autoindex_args = a.autoindex_args.split()
        cmd_run(a)
    else:
        cmd_eval(a)


if __name__ == "__main__":
    main()
