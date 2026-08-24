#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
factcheck.py - the fact-check gate for xerj.org marketing articles.

Dependency-free. Python 3.8+, standard library only.

Reads Markdown article sources (frontmatter + body) and fails the build on claims
the product cannot back. The rules, their evidence citations and their compliant
rewrites live in `scripts/seo/claims_rules.py`; this file is only the engine.

    python3 scripts/seo/factcheck.py                      # content/answers + content/compare
    python3 scripts/seo/factcheck.py content/answers/x.md
    python3 scripts/seo/factcheck.py --json --fail-on warn
    python3 scripts/seo/factcheck.py --only content/compare/xerj-vs-manticore.md
    python3 scripts/seo/factcheck.py --explain FC-S3-BACKUP
    python3 scripts/seo/factcheck.py --list-rules
    python3 scripts/seo/factcheck.py --check-matrix        # THING matrix drift vs the research doc
    python3 scripts/seo/factcheck.py --self-test

Severity
    ERROR   the claim is not supportable; the build fails
    WARN    heuristic, or a missing qualification a human must judge

Exit codes
    0  no finding at or above --fail-on
    1  findings at or above --fail-on
    2  usage or I/O error

Frontmatter schema this gate reads (produced by scripts/seo/build_articles.py):

    ---
    title: "How do I search a PDF library?"
    target_format: pdf                # or: formats: [pdf, csv]
    competitors: [manticore]          # optional, advisory
    evidence:
      - claim: "XERJ extracts text from PDFs with pdf.rs"
        source: "engine/crates/xerj-autoindex/src/extract/pdf.rs"
      - claim: "kNN k=10 is a tie at 1.18x"
        source: "Tier A: demo/playbooks/SCORECARD.md"
    ---
"""

from __future__ import annotations

import argparse
import io
import json
import os
import re
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))
if _HERE not in sys.path:
    sys.path.insert(0, _HERE)

import claims_rules as R  # noqa: E402

ERROR, WARN = R.ERROR, R.WARN
SEV_ORDER = {ERROR: 2, WARN: 1}

REPO_ROOT = os.environ.get("XERJ_REPO_ROOT") or os.path.dirname(os.path.dirname(_HERE))
DEFAULT_GLOBS = ("content/answers/*.md", "content/compare/*.md")


# ======================================================================================
# Finding
# ======================================================================================

class Finding:
    __slots__ = ("path", "line", "col", "sev", "rule", "msg", "excerpt", "extra")

    def __init__(self, path, line, col, sev, rule, msg, excerpt="", extra=None):
        self.path, self.line, self.col = path, line, col
        self.sev, self.rule, self.msg = sev, rule, msg
        self.excerpt = excerpt
        self.extra = extra or {}

    def as_dict(self):
        d = {"path": self.path, "line": self.line, "col": self.col, "severity": self.sev,
             "rule": self.rule, "message": self.msg}
        if self.excerpt:
            d["excerpt"] = self.excerpt
        rule = R.rule_by_id(self.rule)
        if rule:
            d["evidence"] = rule["evidence"]
            d["rewrite"] = rule["rewrite"]
        d.update(self.extra)
        return d

    def text(self):
        return "%s:%d:%d: %-5s %-22s %s" % (self.path, self.line, self.col, self.sev,
                                            self.rule, self.msg)


# ======================================================================================
# Normalisation - every substitution below is 1:1 in length, so match offsets in the
# normalised string map exactly onto the raw string.
# ======================================================================================

_TRANS = {
    0x2018: "'", 0x2019: "'", 0x201A: "'", 0x201B: "'",
    0x201C: '"', 0x201D: '"', 0x201E: '"',
    0x2010: "-", 0x2011: "-", 0x2012: "-", 0x2013: "-", 0x2014: "-", 0x2015: "-",
    0x00A0: " ", 0x2007: " ", 0x202F: " ", 0x2009: " ", 0x200A: " ",
    0x00B7: ".", 0x2022: "*",
}


def normalise(s):
    return s.translate(_TRANS).lower()


_INLINE_CODE = re.compile(r"`[^`\n]*`")
_LINK_URL = re.compile(r"\]\([^)\n]*\)")
_LINK_TEXT = re.compile(r"\[([^\]\n]*)\]\(")


def _mask(text, rx):
    """Blank out a span, preserving length (and therefore offsets)."""
    out = list(text)
    for m in rx.finditer(text):
        for i in range(m.start(), m.end()):
            if out[i] != "\n":
                out[i] = " "
    return "".join(out)


# ======================================================================================
# Frontmatter
# ======================================================================================

def _unquote(v):
    v = v.strip()
    if len(v) >= 2 and v[0] == v[-1] and v[0] in "\"'":
        return v[1:-1]
    return v


def parse_frontmatter(lines):
    """Minimal YAML subset: scalars, inline lists, and a block list of mappings.

    Returns (meta, evidence, body_start_index, meta_lineno).
    `evidence` is a list of {"claim":..., "source":..., "line": n}.
    """
    meta, evidence = {}, []
    if not lines or lines[0].strip() != "---":
        return meta, evidence, 0, {}
    end = None
    for i in range(1, len(lines)):
        if lines[i].strip() in ("---", "..."):
            end = i
            break
    if end is None:
        return meta, evidence, 0, {}

    meta_lineno = {}
    key = None
    cur = None
    for i in range(1, end):
        raw = lines[i].rstrip("\n")
        if not raw.strip() or raw.lstrip().startswith("#"):
            continue
        indent = len(raw) - len(raw.lstrip())
        s = raw.strip()

        if indent == 0 and ":" in s and not s.startswith("-"):
            k, _, v = s.partition(":")
            key = k.strip().lower()
            v = v.strip()
            meta_lineno[key] = i + 1
            if key == "evidence":
                cur = None
                if v and v != "|":
                    meta[key] = v
                continue
            if v.startswith("[") and v.endswith("]"):
                meta[key] = [_unquote(x) for x in v[1:-1].split(",") if x.strip()]
            else:
                meta[key] = _unquote(v)
            continue

        if key == "evidence":
            if s.startswith("-"):
                item = s[1:].strip()
                cur = {"claim": "", "source": "", "line": i + 1}
                evidence.append(cur)
                if item.startswith("{") and item.endswith("}"):
                    for part in re.split(r",(?=\s*\w+\s*:)", item[1:-1]):
                        kk, _, vv = part.partition(":")
                        cur[kk.strip().lower()] = _unquote(vv)
                elif item:
                    kk, _, vv = item.partition(":")
                    if kk.strip().lower() in ("claim", "source"):
                        cur[kk.strip().lower()] = _unquote(vv)
                    else:
                        cur["claim"] = _unquote(item)
            elif cur is not None and ":" in s:
                kk, _, vv = s.partition(":")
                kk = kk.strip().lower()
                if kk in ("claim", "source", "note", "tier"):
                    cur[kk] = _unquote(vv)
        elif key and indent > 0 and s.startswith("-"):
            meta.setdefault(key, [])
            if isinstance(meta[key], list):
                meta[key].append(_unquote(s[1:]))

    return meta, evidence, end + 1, meta_lineno


# ======================================================================================
# Body segmentation
# ======================================================================================

class Para:
    __slots__ = ("start_line", "raw", "norm", "nocode", "is_code", "is_heading", "_links")

    def __init__(self, start_line, raw, is_code, is_heading):
        self.start_line = start_line
        self.raw = raw
        self.is_code = is_code
        self.is_heading = is_heading
        # Newlines become spaces so a hard-wrapped sentence still matches a multi-word
        # pattern. The substitution is 1:1, so offsets still map onto `raw`, and
        # `pos_to_linecol` counts newlines in `raw`, not here.
        self.norm = normalise(raw).replace("\n", " ")
        self.nocode = _mask(_mask(self.norm, _INLINE_CODE), _LINK_URL)
        self._links = None

    def link_spans(self):
        if self._links is None:
            self._links = [(m.start(1), m.end(1)) for m in _LINK_TEXT.finditer(self.norm)]
        return self._links

    def pos_to_linecol(self, pos):
        pre = self.raw[:pos]
        nl = pre.count("\n")
        col = pos - (pre.rfind("\n") + 1) + 1
        return self.start_line + nl, col


_FENCE = re.compile(r"^\s{0,3}(`{3,}|~{3,})")


def segment(lines, offset):
    """Split the body into paragraphs, tracking fenced code blocks and headings."""
    paras, buf, buf_start, in_code, fence = [], [], None, False, None

    def flush(is_code=False, is_heading=False):
        nonlocal buf, buf_start
        if buf:
            paras.append(Para(buf_start, "\n".join(buf), is_code, is_heading))
        buf, buf_start = [], None

    for idx, raw in enumerate(lines):
        lineno = offset + idx + 1
        line = raw.rstrip("\n")
        m = _FENCE.match(line)
        if m:
            if not in_code:
                flush()
                in_code, fence = True, m.group(1)[0]
                buf_start, buf = lineno, [line]
            elif line.lstrip()[:1] == fence:
                buf.append(line)
                flush(is_code=True)
                in_code = False
            else:
                buf.append(line)
            continue
        if in_code:
            if buf_start is None:
                buf_start = lineno
            buf.append(line)
            continue
        if not line.strip():
            flush()
            continue
        if line.lstrip().startswith("#"):
            flush()
            paras.append(Para(lineno, line, False, True))
            continue
        if buf_start is None:
            buf_start = lineno
        buf.append(line)
    flush(is_code=in_code)
    return paras


# ======================================================================================
# Rule evaluation
# ======================================================================================

_CACHE = {}


def _rx(pat):
    r = _CACHE.get(pat)
    if r is None:
        r = _CACHE[pat] = re.compile(pat, re.I)
    return r


def _scope_text(rule, art, pi, pos):
    scope = rule.get("scope", "paragraph")
    if scope == "document":
        return art.doc_norm
    if scope == "window":
        w = rule.get("window", 400)
        c = art.offsets[pi] + pos
        return art.doc_norm[max(0, c - w):c + w]
    return art.para_scope[pi]


class Article:
    def __init__(self, path, text):
        self.path = path
        self.lines = text.splitlines()
        self.meta, self.evidence, self.body_start, self.meta_lineno = parse_frontmatter(self.lines)
        self.paras = segment(self.lines[self.body_start:], self.body_start)
        self.prose = [p for p in self.paras if not p.is_code]
        # A flat normalised document with paragraph offsets, for window/document scopes.
        chunks, self.offsets = [], {}
        cur = 0
        for i, p in enumerate(self.paras):
            self.offsets[i] = cur
            chunks.append(p.norm)
            cur += len(p.norm) + 2
        self.doc_norm = "\n\n".join(chunks)
        self.doc_nocode = "\n\n".join(p.nocode if not p.is_code else " " * len(p.norm)
                                     for p in self.paras)
        # A heading is not a claim on its own: give it the following section paragraph so a
        # qualification stated in the body is in scope for an H1 that restates the question.
        self.para_scope = []
        for i, p in enumerate(self.paras):
            sc = p.norm
            if p.is_heading:
                for q in self.paras[i + 1:]:
                    if not q.is_heading:
                        sc = sc + "\n\n" + q.norm
                        break
            self.para_scope.append(sc)
        self.title = str(self.meta.get("title", ""))
        h1 = ""
        for p in self.paras:
            if p.is_heading and p.raw.lstrip().startswith("# "):
                h1 = p.raw.lstrip()[2:].strip()
                break
        self.h1 = h1
        self.ev_numbers = set()
        self.ev_text = ""
        for e in self.evidence:
            blob = "%s %s" % (e.get("claim", ""), e.get("source", ""))
            self.ev_text += " " + normalise(blob)
            for key, _, _ in iter_numbers(normalise(blob)):
                self.ev_numbers.add(key)


# --------------------------------------------------------------------------------------
# Numbers
# --------------------------------------------------------------------------------------

NUM_RE = re.compile(
    r"(?<![\w.])"
    r"(\d{1,3}(?:,\d{3})+|\d+(?:\.\d+)?)"
    r"\s?([kkmM])?"
    r"\s*"
    r"(docs/s|rec/s|tokens/s|mib|gib|gb|mb|kb|tb|ms|%|×|x|s)"
    r"(?![\w/])",
    re.I)


def _trim(v):
    v = v.replace(",", "")
    if "." in v:
        v = v.rstrip("0").rstrip(".")
    return v or "0"


def iter_numbers(norm_text):
    """Yield (key, match, raw) over a normalised, code-masked string."""
    for m in NUM_RE.finditer(norm_text):
        val, mult, unit = m.group(1), (m.group(2) or ""), m.group(3)
        if unit == "s" and not m.group(0)[:-1].endswith((" ", "\t")):
            continue  # "the call 400s" / "HTTP 500s" - a verb, not a duration
        unit = "x" if unit == "×" else unit.lower()
        key = "%s%s%s" % (_trim(val), mult.lower(), unit)
        yield key, m, m.group(0)


def _ctx_ok(entry, scope_text):
    ctx = entry.get("context")
    return (not ctx) or bool(_rx(ctx).search(scope_text))


# --------------------------------------------------------------------------------------
# The checker
# --------------------------------------------------------------------------------------

class Checker:
    def __init__(self, art, opts):
        self.a = art
        self.opts = opts
        self.f = []

    def add(self, para, pos, sev, rid, msg, excerpt="", extra=None):
        line, col = para.pos_to_linecol(pos) if para else (1, 1)
        self.f.append(Finding(self.a.path, line, col, sev, rid, msg, excerpt, extra))

    def add_line(self, lineno, sev, rid, msg, excerpt="", extra=None):
        self.f.append(Finding(self.a.path, lineno, 1, sev, rid, msg, excerpt, extra))

    # ---------------------------------------------------------------- 1. claim rules
    def check_patterns(self):
        a = self.a
        seen = set()
        for rule in R.RULES:
            if rule.get("kind") != "pattern":
                continue
            if self.opts.skip and rule["id"] in self.opts.skip:
                continue
            rx = _rx(rule["pattern"])
            scan_code = rule.get("code", False)
            fired_doc = False
            for pi, para in enumerate(a.paras):
                if para.is_code and not scan_code:
                    continue
                hay = para.norm if scan_code else para.nocode
                for m in rx.finditer(hay):
                    scope = _scope_text(rule, a, pi, m.start())
                    if rule.get("context") and not _rx(rule["context"]).search(scope):
                        continue
                    if any(_rx(e).search(scope) for e in rule.get("exempt", [])):
                        continue
                    if rule.get("requires"):
                        if _rx(rule["requires"]).search(scope):
                            continue
                        if any(a <= m.start() and m.end() <= b for a, b in para.link_spans()):
                            continue  # link text names another page; that page has its own gate
                    if rule.get("scope") == "document":
                        if fired_doc:
                            break
                        fired_doc = True
                    k = (rule["id"], pi)
                    if k in seen:
                        break
                    seen.add(k)
                    quiet = " (no qualifying evidence in this %s)" % rule.get("scope", "paragraph")
                    msg = rule["title"]
                    if rule.get("requires"):
                        msg += " - required qualification is missing%s" % quiet
                    self.add(para, m.start(), rule["sev"], rule["id"],
                             "%s: '%s'" % (msg, m.group(0).strip()[:70]),
                             m.group(0).strip()[:120],
                             {"heading": True} if para.is_heading else None)
                    break
                if fired_doc:
                    break

    # ---------------------------------------------------------------- 2. numbers
    def check_numbers(self):
        a = self.a
        seen = set()
        for pi, para in enumerate(a.paras):
            if para.is_code:
                continue
            for key, m, raw in iter_numbers(para.nocode):
                scope = a.para_scope[pi]
                wide = a.doc_norm[max(0, a.offsets[pi] + m.start() - 700):
                                  a.offsets[pi] + m.start() + 700]

                ent = R.TIER_C.get(key)
                if ent and _ctx_ok(ent, scope):
                    if ent.get("exempt") and _rx(ent["exempt"]).search(scope):
                        continue
                    now = ent.get("now")
                    self.add(para, m.start(), ERROR, "FC-NUM-TIERC",
                             "Tier C number '%s' - %s%s [%s]"
                             % (raw.strip(), ent["what"],
                                ("; the current value is %s" % now) if now else "",
                                ent["cite"]),
                             raw.strip(), {"number": key, "tier": "C"})
                    continue

                ent = R.TIER_A.get(key)
                tier = "A"
                if not (ent and _ctx_ok(ent, scope)):
                    ent, tier = R.TIER_B.get(key), "B"
                    if not (ent and _ctx_ok(ent, scope)):
                        ent = None

                if ent:
                    comp = ent.get("companion")
                    if comp:
                        w = comp.get("window", 500)
                        near = a.doc_norm[max(0, a.offsets[pi] + m.start() - w):
                                          a.offsets[pi] + m.start() + w]
                        if not _rx(comp["needs"]).search(near) and (key, "comp") not in seen:
                            seen.add((key, "comp"))
                            self.add(para, m.start(), WARN, "FC-NUM-COMPANION",
                                     "Tier %s number '%s' is published without its companion "
                                     "fact: %s [%s]" % (tier, raw.strip(), comp["why"], ent["cite"]),
                                     raw.strip(), {"number": key, "tier": tier})
                    continue

                if key in a.ev_numbers:
                    continue
                allow = R.NUMBER_ALLOW.get(key)
                if allow is not None and (not allow or _rx(allow).search(scope)):
                    continue
                if (key, "unk") in seen:
                    continue
                seen.add((key, "unk"))
                self.add(para, m.start(), WARN, "FC-NUM-UNKNOWN",
                         "number '%s' has no provenance: it is not in the Tier A or Tier B "
                         "citable list and no evidence: entry mentions it. Name the file it "
                         "came from, or delete it." % raw.strip(),
                         raw.strip(), {"number": key})

    # ---------------------------------------------------------------- 3. THING gate
    def check_thing(self):
        a = self.a
        declared = []
        for k in ("target_format", "target_formats", "format", "formats", "thing", "things"):
            v = a.meta.get(k)
            if isinstance(v, list):
                declared += v
            elif v:
                declared.append(v)
        hay = normalise(" ".join([a.title, a.h1] + [str(x) for x in declared]))
        line = a.meta_lineno.get("target_format") or a.meta_lineno.get("formats") or 1
        for row in R.THING_MATRIX:
            if row["status"] == R.GREEN:
                continue
            if not any(_rx(al).search(hay) for al in row["aliases"]):
                continue
            if row["status"] == R.RED:
                self.add_line(line, ERROR, "FC-THING-RED",
                              "target format '%s' is RED in the THING coverage matrix (%s): %s [%s]"
                              % (row["thing"], row["mech"], row["gate"], row["cite"]),
                              row["thing"], {"thing": row["thing"], "status": "RED"})
            else:
                self.add_line(line, WARN, "FC-THING-AMBER",
                              "target format '%s' is AMBER (%s) - needs a verification run first: "
                              "%s [%s]" % (row["thing"], row["mech"], row["gate"], row["cite"]),
                              row["thing"], {"thing": row["thing"], "status": "AMBER"})

    # ---------------------------------------------------------------- 4. competitors
    _SUPERLATIVE = (r"(?:fastest|best|superior|beats?|outperform\w*|leaves? .{0,20} behind|"
                    r"blows? .{0,20} away|crush\w*|smoke\w*|destroys?|wipes the floor|"
                    r"(?:faster|cheaper|smaller|simpler|better|leaner) than)")

    def check_competitors(self):
        a = self.a
        doc = a.doc_nocode
        # A page under `content/compare/` is a comparison by construction, so
        # naming the competitor in its slug or title is itself the trigger.
        # `/compare/xerj-vs-ripgrep-for-code-agents` never said "vs ripgrep"
        # in body prose and stayed scrupulously non-superlative, so no body
        # trigger fired and the page shipped without the gate ever running.
        compare_id = ""
        if os.path.basename(os.path.dirname(str(a.path))) == "compare":
            slug = os.path.splitext(os.path.basename(str(a.path)))[0]
            compare_id = normalise("%s %s %s" % (slug, a.title or "", a.h1 or ""))
        for name, aliases in R.COMPETITORS:
            alt = "(?:%s)" % "|".join(aliases)
            triggers = []
            if compare_id and _rx(alt).search(compare_id):
                m = _rx(alt).search(doc)
                if m is None:
                    m = _rx(r"^").search(doc)
                triggers.append((m, "a /compare/ page about %s" % name))
            for pat, why in (
                (r"\bvs\.?\s+" + alt, "'vs %s'" % name),
                (alt + r"\s+vs\.?\b", "'%s vs'" % name),
                (r"\balternatives?\s+to\s+" + alt, "'alternative to %s'" % name),
                (r"\bcompared\s+(?:to|with)\s+" + alt, "'compared to %s'" % name),
                (r"\bmigrat\w+\s+(?:from|off(?:\s+of)?)\s+" + alt, "'migrate from %s'" % name),
                (self._SUPERLATIVE + r"[^.\n]{0,60}" + alt, "a superlative about %s" % name),
                (alt + r"[^.\n]{0,60}" + self._SUPERLATIVE, "a superlative about %s" % name),
            ):
                m = _rx(pat).search(doc)
                if m:
                    triggers.append((m, why))
            if not triggers:
                continue
            m0, why0 = triggers[0]
            line = self._line_of(m0.start())

            named, ok_ev = False, False
            for e in a.evidence:
                src = e.get("source", "")
                blob = normalise("%s %s" % (e.get("claim", ""), src))
                if _rx(alt).search(blob):
                    named = True
                    if re.match(r"https?://", src.strip(), re.I):
                        ok_ev = True
                        break
            if named and not ok_ev:
                self.add_line(line, WARN, "FC-COMP-URL",
                              "the evidence entry for %s has no http(s) source URL. Competitor "
                              "licences, tiers and features drift - link the vendor's own docs or "
                              "pricing page." % name, name, {"competitor": name})
            if not named:
                facts = R.COMPETITOR_FACTS.get(name)
                extra = (" Facts the research already pinned down: %s" % "; ".join(facts)) if facts else ""
                self.add_line(line, ERROR, "FC-COMP-EVIDENCE",
                              "the article makes %s but no evidence: entry names %s with an "
                              "http(s) source URL.%s" % (why0, name, extra),
                              name, {"competitor": name})

            sect = (r"when to (?:choose|use|pick|prefer)[^.\n]{0,60}" + alt,
                    alt + r"[^.\n]{0,60}(?:is (?:the )?better|instead|wins? (?:here|when|if))",
                    r"(?:choose|use|pick|prefer)\s+" + alt + r"\s+(?:instead|when|if)")
            if not any(_rx(s).search(doc) for s in sect):
                self.add_line(line, ERROR, "FC-COMP-ALTERNATIVE",
                              "the article makes %s but has no 'when to choose %s instead' "
                              "section. Name where the competitor wins, or do not publish the "
                              "comparison." % (why0, name),
                              name, {"competitor": name})

    def _line_of(self, doc_pos):
        best = 1
        for i, p in enumerate(self.a.paras):
            off = self.a.offsets[i]
            if off <= doc_pos < off + len(p.norm):
                return p.pos_to_linecol(doc_pos - off)[0]
            if off <= doc_pos:
                best = p.start_line
        return best

    # ---------------------------------------------------------------- 5. evidence block
    _URL = re.compile(r"^https?://\S+$", re.I)
    _TIER = re.compile(r"^tier\s*[ab]\b", re.I)
    _TIERC = re.compile(r"\btier\s*c\b", re.I)
    _BAD_SRC = re.compile(r"benchmark_vs_es\.md|product\.html[^\n]{0,12}(?:§|section\s*)?0?9\b|"
                          r"migrate-from-elasticsearch\.md\s*:\s*21[0-9]", re.I)

    def check_evidence(self):
        """Validate the ``evidence:`` block when the article has one.

        The block is optional.  It existed to point at capture files that are
        no longer part of the repository, so an article with no block at all is
        normal and produces no finding here - the loop simply has nothing to
        iterate.  What is NOT relaxed is a source that IS present: every entry
        still has to carry both halves (FC-EV-INCOMPLETE), still may not name a
        Tier C or retracted file (FC-EV-TIERC), and its path still has to
        resolve in this repository (FC-EV-DANGLING).  A typo in a future
        citation fails exactly as it did before.
        """
        a = self.a
        for e in a.evidence:
            ln = e.get("line", 1)
            claim, src = e.get("claim", "").strip(), e.get("source", "").strip()
            if not claim or not src:
                self.add_line(ln, ERROR, "FC-EV-INCOMPLETE",
                              "evidence entry is incomplete: %s"
                              % ("missing claim" if not claim else "missing source"),
                              claim or src)
                continue
            if self._TIERC.search(src) or self._BAD_SRC.search(src):
                self.add_line(ln, ERROR, "FC-EV-TIERC",
                              "evidence source is a Tier C or retracted source: '%s'" % src, src)
                continue
            if self._URL.match(src) or self._TIER.match(src):
                continue
            path = src.split("#", 1)[0].split("?", 1)[0].strip()
            path = re.sub(r":\d+(?:-\d+)?$", "", path)
            path = path.strip("`'\" ")
            if _resolve_source(path):
                continue
            self.add_line(ln, ERROR, "FC-EV-DANGLING",
                          "evidence source does not resolve: '%s' is not a path in this repo, "
                          "not an http(s) URL, and not a 'Tier A/B: ...' reference" % src, src)

    # ---------------------------------------------------------------- 6. standing rules
    _ARCH = re.compile(r"\b(architecture|at scale|scal(?:e|es|ing)|production|deploy\w*|"
                       r"topology|capacity|high availability|\bha\b|cluster)\b", re.I)
    _SINGLE = re.compile(r"single[- ]node|\bone node\b|single node", re.I)

    def check_standing(self):
        if self._ARCH.search(self.a.doc_nocode) and not self._SINGLE.search(self.a.doc_norm):
            m = self._ARCH.search(self.a.doc_nocode)
            self.add_line(self._line_of(m.start()), WARN, "FC-SINGLE-NODE",
                          "the article discusses architecture, scale, production or deployment "
                          "but never says 'single-node' (standing rule 3)", m.group(0))

    def run(self):
        self.check_patterns()
        self.check_numbers()
        self.check_thing()
        self.check_competitors()
        self.check_evidence()
        self.check_standing()
        self.f = self._dedupe_headings(self.f)
        self.f.sort(key=lambda x: (x.line, x.col, x.rule))
        return self.f

    @staticmethod
    def _dedupe_headings(findings):
        """Drop a heading-line finding when the same rule also fires in its section body."""
        body = {(f.rule, f.line) for f in findings if not f.extra.get("heading")}
        out = []
        for f in findings:
            if f.extra.get("heading") and any(
                    r == f.rule and f.line < ln <= f.line + 8 for r, ln in body):
                continue
            f.extra.pop("heading", None)
            out.append(f)
        return out


def _glob_any(pat):
    import glob
    return bool(glob.glob(pat))


def _resolve_source(path):
    """A source resolves as a repo path, a glob, or a site-relative xerj.org page.

    Site-relative forms ('/docs/recipes/hybrid-search') are how an article cites a
    published docs page; they resolve against `landing/`, so a link to a page that does
    not exist is still caught.
    """
    if not path:
        return False
    if path.startswith("/"):
        rel = path.strip("/")
        for cand in (os.path.join(REPO_ROOT, "landing", rel + ".html"),
                     os.path.join(REPO_ROOT, "landing", rel, "index.html"),
                     os.path.join(REPO_ROOT, "landing", rel)):
            if os.path.exists(cand):
                return True
        return os.path.exists(path)
    cand = os.path.join(REPO_ROOT, path)
    return os.path.exists(cand) or ("*" in path and _glob_any(cand))


# ======================================================================================
# THING matrix drift check
# ======================================================================================

# Optional developer input for --check-matrix.  The competitor research is part
# of the session working record and is not committed, so this is a path that may
# or may not exist in a given checkout - never a reference a reader is expected
# to follow.  claims_rules.R.THING is the committed, authoritative copy of the
# matrix and is what every gate reads; this flag only re-parses the source
# document to catch drift while that document is still on disk.
MATRIX_SOURCE = os.path.join(".dogfood", "seo", "RESEARCH-competitors-longtail.md")


def check_matrix_if_available():
    """Re-parse the research matrix when the working record is present."""
    research = os.path.join(REPO_ROOT, MATRIX_SOURCE)
    if not os.path.exists(research):
        print("skip: --check-matrix needs the competitor research from the session "
              "working record, which is not committed. The baked THING matrix in "
              "claims_rules.py is authoritative and is already covered by --self-test.")
        return 0
    return check_matrix(research)


def check_matrix(research_path):
    """Re-parse the THING coverage matrix and report drift against the baked table."""
    if not os.path.exists(research_path):
        print("cannot read %s" % research_path, file=sys.stderr)
        return 2
    with io.open(research_path, encoding="utf-8", errors="replace") as fh:
        lines = fh.readlines()
    live = {}
    in_matrix = False
    for i, raw in enumerate(lines):
        if "THING coverage matrix" in raw:
            in_matrix = True
            continue
        if in_matrix:
            if raw.startswith("#") or raw.startswith(">"):
                if live:
                    break
                continue
            if not raw.startswith("|"):
                continue
            cells = [c.strip() for c in raw.strip().strip("|").split("|")]
            if len(cells) < 3 or cells[0].lower() in ("thing", "") or set(cells[0]) <= set("-: "):
                continue
            status = (R.GREEN if "\U0001F7E2" in cells[2] else
                      R.AMBER if "\U0001F7E1" in cells[2] else
                      R.RED if "\U0001F534" in cells[2] else None)
            if status:
                live[cells[0]] = (status, i + 1)
    baked = {r["thing"]: r["status"] for r in R.THING_MATRIX}
    counts = {R.GREEN: 0, R.AMBER: 0, R.RED: 0}
    for s, _ in live.values():
        counts[s] += 1
    print("parsed %d rows from %s: %d GREEN / %d AMBER / %d RED"
          % (len(live), os.path.relpath(research_path, REPO_ROOT),
             counts[R.GREEN], counts[R.AMBER], counts[R.RED]))
    print("baked table in claims_rules.THING_MATRIX: %d rows (%d GREEN / %d AMBER / %d RED)"
          % (len(baked), sum(1 for v in baked.values() if v == R.GREEN),
             sum(1 for v in baked.values() if v == R.AMBER),
             sum(1 for v in baked.values() if v == R.RED)))
    drift = 0
    bs = sorted(baked.values())
    ls = sorted(s for s, _ in live.values())
    if bs != ls:
        print("DRIFT: status distribution differs between the doc and the baked table")
        drift = 1
    for thing, (status, ln) in sorted(live.items()):
        if thing not in baked:
            hit = [k for k in baked if k.split(" /")[0].lower()[:6] in thing.lower()]
            if not hit:
                print("  doc row not in baked table: %-38s %-5s (%s:%d)"
                      % (thing, status, os.path.basename(research_path), ln))
                drift = 1
    return 1 if drift else 0


# ======================================================================================
# CLI
# ======================================================================================

def collect_paths(args):
    import glob
    paths = []
    if args.paths:
        for p in args.paths:
            paths += sorted(glob.glob(p)) if any(c in p for c in "*?[") else [p]
    else:
        for g in DEFAULT_GLOBS:
            paths += sorted(glob.glob(os.path.join(REPO_ROOT, g)))
    if args.only:
        want = os.path.normpath(args.only)
        paths = [p for p in paths
                 if os.path.normpath(p) == want or os.path.normpath(p).endswith(os.sep + want)
                 or os.path.basename(p) == os.path.basename(want)]
    return paths


def explain(rid):
    rule = R.rule_by_id(rid)
    if not rule:
        print("no such rule: %s (try --list-rules)" % rid, file=sys.stderr)
        return 2
    w = 78

    def wrap(text, indent="    "):
        out, line = [], indent
        for word in str(text).split():
            if len(line) + len(word) + 1 > w:
                out.append(line)
                line = indent
            line += ("" if line == indent else " ") + word
        out.append(line)
        return "\n".join(out)

    print("=" * w)
    print("%s  [%s]" % (rule["id"], rule["sev"]))
    print("=" * w)
    print(rule["title"])
    print()
    print("INTENT")
    print(wrap(rule["intent"]))
    print()
    print("KIND")
    print(wrap("%s%s" % (rule.get("kind", "pattern"),
                         "" if rule.get("kind") == "engine" else
                         " (scope: %s)" % rule.get("scope", "paragraph"))))
    if rule.get("pattern"):
        print()
        print("PATTERN")
        print(wrap(rule["pattern"]))
    if rule.get("context"):
        print()
        print("ONLY WHEN THIS IS ALSO PRESENT")
        print(wrap(rule["context"]))
    if rule.get("requires"):
        print()
        print("REQUIRED QUALIFICATION (absence is what fires the rule)")
        print(wrap(rule["requires"]))
    if rule.get("exempt"):
        print()
        print("PERMITTED PHRASINGS (any of these in scope suppresses the rule)")
        for e in rule["exempt"]:
            print(wrap(e))
    print()
    print("WHY THE CLAIM IS NOT SUPPORTABLE")
    print(wrap(rule["reason"]))
    print()
    print("EVIDENCE")
    for e in rule["evidence"]:
        print("    %s" % e)
    print()
    print("COMPLIANT REWRITE")
    print(wrap(rule["rewrite"]))
    print("=" * w)
    return 0


def list_rules():
    print("%-24s %-6s %s" % ("RULE", "SEV", "TITLE"))
    print("-" * 100)
    for r in R.RULES:
        print("%-24s %-6s %s" % (r["id"], r["sev"], r["title"]))
    print("-" * 100)
    print("%d rules | %d ERROR | %d WARN | tiers: %d A / %d B / %d C | THING rows: %d"
          % (len(R.RULES),
             sum(1 for r in R.RULES if r["sev"] == ERROR),
             sum(1 for r in R.RULES if r["sev"] == WARN),
             len(R.TIER_A), len(R.TIER_B), len(R.TIER_C), len(R.THING_MATRIX)))
    return 0


def self_test():
    fails = []
    ids = set()
    for r in R.RULES:
        if not r.get("evidence"):
            fails.append("%s has no evidence citation" % r["id"])
        for c in r.get("evidence", []):
            if not re.search(r"\.md:\d+", c):
                fails.append("%s evidence '%s' is not a file:line citation" % (r["id"], c))
        if not r.get("rewrite"):
            fails.append("%s has no compliant rewrite" % r["id"])
        if r["id"] in ids:
            fails.append("duplicate rule id %s" % r["id"])
        ids.add(r["id"])
        if r["sev"] not in (ERROR, WARN):
            fails.append("%s has severity %s" % (r["id"], r["sev"]))
        try:
            for k in ("pattern", "context", "requires"):
                if r.get(k):
                    re.compile(r[k])
            for e in r.get("exempt", []):
                re.compile(e)
        except re.error as exc:
            fails.append("%s regex: %s" % (r["id"], exc))
    for name, tbl in (("TIER_A", R.TIER_A), ("TIER_B", R.TIER_B), ("TIER_C", R.TIER_C)):
        for k, v in tbl.items():
            if not v.get("cite"):
                fails.append("%s[%s] has no citation" % (name, k))
            if k != k.lower():
                fails.append("%s[%s] key must be lowercase" % (name, k))
    overlap = set(R.TIER_A) & set(R.TIER_C)
    if overlap:
        fails.append("keys in both Tier A and Tier C: %s" % sorted(overlap))

    cases = [
        ("11.5x", "the bool query is 11.5× faster", "FC-NUM-TIERC"),
        ("1.18x-tie", "kNN is 1.18× at k=10, a tie at 100% recall on both engines", None),
        ("num", "we index 3.7× faster", "FC-NUM-UNKNOWN"),
    ]
    for name, body, want in cases:
        art = Article("<t>", "---\ntitle: t\n---\n\n%s\n" % body)
        got = {f.rule for f in Checker(art, Opts()).run()}
        if want and want not in got:
            fails.append("self-test %s: expected %s, got %s" % (name, want, sorted(got)))
        if not want and "FC-NUM-TIERC" in got:
            fails.append("self-test %s: unexpected Tier C error" % name)

    for k in ("1.20x", "100.0%", "2.00x"):
        pass
    if _trim("1.20") != "1.2" or _trim("100.0") != "100" or _trim("2,500") != "2500":
        fails.append("number normalisation is wrong")

    if fails:
        for f in fails:
            print("FAIL %s" % f)
        print("\n%d self-test failure(s)" % len(fails))
        return 1
    print("self-test OK: %d rules, %d/%d/%d tiered numbers, %d THING rows"
          % (len(R.RULES), len(R.TIER_A), len(R.TIER_B), len(R.TIER_C), len(R.THING_MATRIX)))
    return 0


class Opts:
    def __init__(self, skip=()):
        self.skip = set(skip)


FIXTURE_DIR = os.path.join(_HERE, "testdata", "factcheck")


def fixture_check(verbose=False):
    """Run every fixture and print the confusion matrix.

    A `bad_*.md` fixture declares `expect: [RULE, ...]` in its frontmatter: the rules it
    MUST trip. A `good_*.md` fixture must produce zero ERRORs; any ERROR on a good fixture
    is a false positive and the RULE gets fixed, never the fixture.
    """
    import glob
    paths = sorted(glob.glob(os.path.join(FIXTURE_DIR, "*.md")))
    if not paths:
        print("no fixtures under %s" % FIXTURE_DIR, file=sys.stderr)
        return 2
    tp = {}
    fn = {}
    fp = {}
    rows = []
    for p in paths:
        base = os.path.basename(p)
        with io.open(p, encoding="utf-8", errors="replace") as fh:
            art = Article(base, fh.read())
        found = Checker(art, Opts()).run()
        fired = {f.rule for f in found}
        errs = {f.rule for f in found if f.sev == ERROR}
        exp = art.meta.get("expect") or []
        if isinstance(exp, str):
            exp = [x.strip() for x in exp.strip("[]").split(",") if x.strip()]
        exp = [e.strip().upper() for e in exp]
        miss = [e for e in exp if e not in fired]
        for e in exp:
            (tp if e in fired else fn).setdefault(e, []).append(base)
        bad = base.startswith("bad_")
        false_pos = [] if bad else sorted(errs)
        for r in false_pos:
            fp.setdefault(r, []).append(base)
        rows.append((base, bad, len(exp), len(exp) - len(miss), miss,
                     sum(1 for f in found if f.sev == ERROR),
                     sum(1 for f in found if f.sev == WARN),
                     sorted(fired - set(exp)), false_pos))
        if verbose:
            for f in found:
                print(f.text())

    w = "%-26s %-5s %8s %8s %7s %6s  %s"
    print(w % ("FIXTURE", "KIND", "EXPECTED", "CAUGHT", "ERRORS", "WARNS", "MISSED / FALSE POSITIVE"))
    print("-" * 118)
    for base, bad, nexp, ncaught, miss, ne, nw, extra, false_pos in rows:
        note = ""
        if miss:
            note = "MISS: " + ",".join(miss)
        if false_pos:
            note = (note + "  " if note else "") + "FALSE POSITIVE: " + ",".join(false_pos)
        print(w % (base, "bad" if bad else "good", nexp or "-", ncaught or "-",
                   ne, nw, note or "ok"))
    print("-" * 118)
    n_tp = sum(len(v) for v in tp.values())
    n_fn = sum(len(v) for v in fn.values())
    n_fp = sum(len(v) for v in fp.values())
    n_tn = sum(1 for r in rows if not r[1] and r[5] == 0)
    n_good = sum(1 for r in rows if not r[1])
    print("rule-instance confusion matrix over %d fixtures (%d bad / %d good)"
          % (len(rows), len(rows) - n_good, n_good))
    print("    true  positives : %3d   (expected rule fired)" % n_tp)
    print("    false negatives : %3d   %s" % (n_fn, sorted(fn) if fn else ""))
    print("    false positives : %3d   %s   (ERROR on a good_ fixture)"
          % (n_fp, sorted(fp) if fp else ""))
    print("    true  negatives : %3d   (good_ fixtures with zero ERRORs, of %d)" % (n_tn, n_good))
    cov = sorted({r["id"] for r in R.RULES} - set(tp) - set(fn))
    print("    rules with no fixture: %d  %s" % (len(cov), cov if cov else ""))
    return 1 if (n_fn or n_fp) else 0


def main(argv=None):
    ap = argparse.ArgumentParser(
        prog="factcheck.py",
        description="Fact-check gate for xerj.org articles. Rules live in claims_rules.py.")
    ap.add_argument("paths", nargs="*", help="markdown files or globs "
                                             "(default: content/answers/*.md content/compare/*.md)")
    ap.add_argument("--json", action="store_true", help="emit JSON")
    ap.add_argument("--fail-on", default="error", choices=["error", "warn"],
                    help="minimum severity that sets a non-zero exit code (default: error)")
    ap.add_argument("--only", default="", metavar="FILE",
                    help="restrict the run to one file")
    ap.add_argument("--explain", default="", metavar="RULE-ID",
                    help="print a rule's evidence and its compliant rewrite")
    ap.add_argument("--list-rules", action="store_true")
    ap.add_argument("--check-matrix", action="store_true",
                    help="re-parse the THING coverage matrix and report drift")
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--fixture-check", action="store_true",
                    help="run scripts/seo/testdata/factcheck/*.md and print the confusion matrix")
    ap.add_argument("--skip", default="", help="comma-separated rule IDs to suppress")
    ap.add_argument("--stats", action="store_true", help="print a per-rule count")
    args = ap.parse_args(argv)

    if args.explain:
        return explain(args.explain)
    if args.list_rules:
        return list_rules()
    if args.self_test:
        return self_test()
    if args.fixture_check:
        return fixture_check()
    if args.check_matrix:
        return check_matrix_if_available()

    paths = collect_paths(args)
    if not paths:
        print("no input files (looked for %s under %s)" % (", ".join(DEFAULT_GLOBS), REPO_ROOT),
              file=sys.stderr)
        return 2

    opts = Opts(skip=[s.strip().upper() for s in args.skip.split(",") if s.strip()])
    findings, nfiles = [], 0
    for p in paths:
        try:
            with io.open(p, encoding="utf-8", errors="replace") as fh:
                text = fh.read()
        except OSError as exc:
            print("cannot read %s: %s" % (p, exc), file=sys.stderr)
            return 2
        nfiles += 1
        rel = os.path.relpath(p, REPO_ROOT) if p.startswith(REPO_ROOT) else p
        findings += Checker(Article(rel, text), opts).run()

    n_err = sum(1 for f in findings if f.sev == ERROR)
    n_warn = sum(1 for f in findings if f.sev == WARN)
    threshold = SEV_ORDER[ERROR] if args.fail_on == "error" else SEV_ORDER[WARN]
    gated = sum(1 for f in findings if SEV_ORDER[f.sev] >= threshold)

    if args.json:
        print(json.dumps({
            "files": nfiles,
            "errors": n_err,
            "warnings": n_warn,
            "fail_on": args.fail_on,
            "gated": gated,
            "findings": [f.as_dict() for f in findings],
        }, indent=2, sort_keys=False))
    else:
        for f in findings:
            print(f.text())
        if args.stats:
            per = {}
            for f in findings:
                per[f.rule] = per.get(f.rule, 0) + 1
            print("")
            for k in sorted(per, key=lambda x: (-per[x], x)):
                print("%6d  %s" % (per[k], k))
        print("\n%d file(s), %d ERROR, %d WARN%s"
              % (nfiles, n_err, n_warn,
                 "" if not findings else "  (--explain <rule-id> for the evidence and a rewrite)"))
    return 1 if gated else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
