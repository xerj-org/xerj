#!/usr/bin/env python3
"""Every [A07]-style check id in the proposal documents must exist in factcheck-results.json and be confirmed.

Ranges such as [C096–C105] and lists such as [A08, C062] are expanded. Exit 1 on an unknown id,
on an id whose claim was not confirmed, or on the one deliberate exception being cited as support.
"""
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
results = {r["id"]: r for r in json.loads((ROOT / "proposals/data/factcheck-results.json").read_text())}
FILES = ["proposals/report.md", "proposals/proposed-llms.txt", "proposals/proposed-llms-install.md", "proposals/install-prompt-library.md"]
ID = r"[A-D]\d{2,3}"
bad = used = 0
for f in FILES:
    text = (ROOT / f).read_text()
    for group in re.findall(r"\[((?:%s)(?:[–,\s]+(?:%s))*)[^\]]*\]" % (ID, ID), text):
        ids = []
        for part in re.split(r",\s*", group):
            m = re.match(r"(%s)–(%s)" % (ID, ID), part.strip())
            if m:
                a, b = m.groups()
                w = len(a) - 1
                ids += [f"{a[0]}{k:0{w}d}" for k in range(int(a[1:]), int(b[1:]) + 1)]
            else:
                ids += re.findall(ID, part)
        for i in ids:
            used += 1
            r = results.get(i)
            if r is None:
                print(f"{f}: unknown id {i}")
                bad += 1
            elif not r["verdict"].startswith("confirmed"):
                # the single unconfirmed claim may be cited only where the report says it failed
                ctx = text[max(0, text.find(i) - 200): text.find(i) + 50]
                if "not on" not in ctx and "failed" not in ctx and "removed" not in ctx:
                    print(f"{f}: {i} is cited as support but is {r['verdict']}")
                    bad += 1
print(f"{used} id citations checked, {bad} problems")
sys.exit(1 if bad else 0)
