#!/usr/bin/env python3
"""Detector born from an AI read, not a pattern. Reading wp_http_validate_url
showed its private-IP blocklist omits 169.254.0.0/16 (link-local / cloud
metadata 169.254.169.254) -> SSRF. That's a SEMANTIC COMPLETENESS gap: a
security validator whose allow/deny list is missing a dangerous case. Structural
detectors can't see it. This encodes the class: find IP-range SSRF validators and
report which dangerous ranges they FAIL to cover."""
import json, urllib.request, re
X="http://127.0.0.1:9200"
def q(body):
    r=urllib.request.Request(f"{X}/wpaudit/_search",data=json.dumps(body).encode(),
        headers={"Content-Type":"application/json"})
    return [h["_source"] for h in json.loads(urllib.request.urlopen(r).read())["hits"]["hits"]]

# dangerous ranges every SSRF validator SHOULD block, with a cheap presence test
# over the function source (does the code mention the range's signature at all?)
DANGEROUS = {
 "127.0.0.0/8 loopback":            [r"\b127\b"],
 "10.0.0.0/8 private":              [r"\b10\b"],
 "172.16.0.0/12 private":           [r"\b172\b"],
 "192.168.0.0/16 private":          [r"\b192\b", r"\b168\b"],
 "169.254.0.0/16 link-local/METADATA": [r"\b169\b", r"169\.254", r"\b254\b"],
 "100.64.0.0/10 CGNAT":             [r"\b100\b.*\b64\b", r"100\.64"],
 "IPv6 ::1 / ULA fc00::/7":         [r"::1", r"fc00", r"fe80", r"inet_pton|FILTER_FLAG_IPV6|filter_var"],
}
def covers(code, sigs): return any(re.search(s, code) for s in sigs)

# find candidate SSRF validators: functions that resolve a host AND compare IP octets
S=q({"query":{"match":{"code":"gethostbyname"}},"size":200,"_source":["func","file","code"]})
IPGUARD=re.compile(r"(gethostbyname|parse_url).*?(127|192\.168|private|local)", re.S)
OCTET=re.compile(r"===\s*\$?\w*\[?0?\]?|explode\(\s*['\"]\.['\"]|\$parts\b")
validators=[d for d in S if re.search(r"\b127\b", d["code"]) and re.search(r"\b192\b", d["code"])
            and ("gethostbyname" in d["code"] or "$parts" in d["code"] or "explode( '.'" in d["code"])]
print(f"SSRF IP-range validators found: {len(validators)}\n")
for d in validators:
    print(f"### {d['func']}  ({d['file']})")
    missing=[name for name,sigs in DANGEROUS.items() if not covers(d["code"], sigs)]
    for name in DANGEROUS:
        ok = name not in missing
        flag = "" if ok else "   <<< MISSING"
        print(f"    [{'x' if ok else ' '}] {name}{flag}")
    if any("METADATA" in m for m in missing):
        print("    !! SSRF TO CLOUD METADATA POSSIBLE — validator does not block 169.254.0.0/16")
    print()
