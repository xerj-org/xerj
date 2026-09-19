"""Shared helpers for the local-judge BEIR harness. Standard library only."""
import collections, json, math, os, urllib.request, urllib.error

# No default on purpose: `load.py` deletes and recreates the index it is given, so
# a harness that silently fell back to a well-known port could wipe someone's node.
URL = os.environ.get("XERJ_URL", "")
KEY = os.environ.get("XERJ_API_KEY", "")


def call(method, path, body=None, ctype="application/json", timeout=900):
    if not URL:
        raise SystemExit("set XERJ_URL to the node this harness may write to, e.g. http://localhost:12600")
    data = body if isinstance(body, (bytes, type(None))) else json.dumps(body).encode()
    headers = {"content-type": ctype}
    if KEY:
        headers["authorization"] = "ApiKey " + KEY
    req = urllib.request.Request(URL + path, data=data, method=method, headers=headers)
    try:
        return json.loads(urllib.request.urlopen(req, timeout=timeout).read())
    except urllib.error.HTTPError as e:
        return {"_err": e.code, "body": e.read().decode()[:600]}


def queries(ds):
    out = {}
    for line in open(os.path.join(ds, "queries.jsonl")):
        d = json.loads(line)
        out[d["_id"]] = d["text"]
    return out


def qrels(ds, split):
    out = collections.defaultdict(dict)
    for i, line in enumerate(open(os.path.join(ds, "qrels", split + ".tsv"))):
        if i == 0:
            continue
        q, d, s = line.rstrip("\n").split("\t")
        out[q][d] = int(s)
    return out


def corpus(ds):
    out = {}
    for line in open(os.path.join(ds, "corpus.jsonl")):
        d = json.loads(line)
        out[d["_id"]] = (d.get("title") or "", d.get("text") or "")
    return out


def ndcg10(ranked, rel):
    dcg = sum(rel.get(d, 0) / math.log2(i + 2) for i, d in enumerate(ranked[:10]))
    ideal = sorted(rel.values(), reverse=True)[:10]
    idcg = sum(g / math.log2(i + 2) for i, g in enumerate(ideal))
    return dcg / idcg if idcg else 0.0


def sigmoid(z):
    if z >= 0:
        return 1.0 / (1.0 + math.exp(-z))
    e = math.exp(z)
    return e / (1.0 + e)


def ece(conf_label, bins=10):
    """Expected calibration error, equal-width bins, for binary (p, y) pairs:
    the gap between mean predicted probability and observed positive rate."""
    n = len(conf_label)
    if not n:
        return 0.0
    total = 0.0
    for b in range(bins):
        lo, hi = b / bins, (b + 1) / bins
        xs = [(p, y) for p, y in conf_label if (lo < p <= hi) or (b == 0 and p == 0.0)]
        if xs:
            total += len(xs) / n * abs(sum(y for _, y in xs) / len(xs) - sum(p for p, _ in xs) / len(xs))
    return total


def reliability(conf_label, bins=10):
    rows = []
    for b in range(bins):
        lo, hi = b / bins, (b + 1) / bins
        xs = [(p, y) for p, y in conf_label if (lo < p <= hi) or (b == 0 and p == 0.0)]
        if xs:
            rows.append({"bin": f"{lo:.1f}-{hi:.1f}", "n": len(xs),
                         "mean_p": sum(p for p, _ in xs) / len(xs),
                         "observed": sum(y for _, y in xs) / len(xs)})
    return rows


def platt_fit(logit_label, iters=100):
    """Maximum-likelihood fit of p = sigmoid(a*logit + b) by Newton's method.
    Returns (a, b). Pure Python so the fit is reproducible anywhere."""
    a, b = 1.0, 0.0
    for _ in range(iters):
        g_a = g_b = h_aa = h_ab = h_bb = 0.0
        for x, y in logit_label:
            p = sigmoid(a * x + b)
            d = p - y
            w = max(p * (1.0 - p), 1e-12)
            g_a += d * x; g_b += d
            h_aa += w * x * x; h_ab += w * x; h_bb += w
        h_aa += 1e-9; h_bb += 1e-9
        det = h_aa * h_bb - h_ab * h_ab
        if abs(det) < 1e-18:
            break
        da = (h_bb * g_a - h_ab * g_b) / det
        db = (h_aa * g_b - h_ab * g_a) / det
        a -= da; b -= db
        if abs(da) < 1e-9 and abs(db) < 1e-9:
            break
    return a, b


def nll(logit_label, a, b):
    eps = 1e-12
    return -sum(y * math.log(max(sigmoid(a * x + b), eps)) + (1 - y) * math.log(max(1 - sigmoid(a * x + b), eps))
                for x, y in logit_label) / max(len(logit_label), 1)


def temperature_fit(logit_label, iters=100):
    """Maximum-likelihood fit of p = sigmoid(logit / T), i.e. one slope a = 1/T and no
    intercept, by Newton's method. Returns a. Temperature scaling cannot move the
    point where p = 0.5; Platt (`platt_fit`) can, which is why both are reported."""
    a = 1.0
    for _ in range(iters):
        g = h = 0.0
        for x, y in logit_label:
            p = sigmoid(a * x)
            g += (p - y) * x
            h += max(p * (1.0 - p), 1e-12) * x * x
        if h < 1e-18:
            break
        step = g / h
        a -= step
        if abs(step) < 1e-9:
            break
    return a
