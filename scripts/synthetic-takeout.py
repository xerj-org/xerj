#!/usr/bin/env python3
"""synthetic-takeout.py — a deterministic, seeded Google Takeout tree.

Writes the shape of a real Takeout export so `xerj autoindex` / `xerj brain`
can be tested and MEASURED on mail without anyone's actual mail:

    <out>/Takeout/archive_browser.html                       export index (noise)
    <out>/Takeout/Mail/All mail Including Spam and Trash.mbox
    <out>/Takeout/Keep/<note>.json + <note>.html              every note twice
    <out>/Takeout/Keep/Labels.txt
    <out>/Takeout/Drive/*.md, *.txt                           plain documents
    <out>/takeout-…-002.zip, takeout-…-003.tgz                with --with-archive
    <truth file>                                              NOT inside <out>

The mbox is the interesting part. It contains, on purpose, everything that
breaks naive mbox readers:

  * reply threads (In-Reply-To + References), replies whose parent is absent
    from the mailbox, and replies that only resolve through References
  * attachments: real parseable PDFs with a text layer, text/CSV files, and
    opaque binary blobs; some with non-ASCII filenames
  * non-ASCII subjects and sender names as RFC 2047 encoded-words
  * bodies with lines that begin "From " (written mboxrd-quoted, ">From ")
    and already-quoted ">From " lines (written ">>From ")
  * 8-bit bodies: declared ISO-8859-1, and undeclared Windows-1252
  * malformed entries: header-less binary garbage, an unterminated multipart
    with truncated base64, a broken encoded-word with invalid UTF-8, one
    300 KB line with no newline, a separator with nothing after it, an
    UNQUOTED prose line "From what I understand…" (legal in mboxcl2; must not
    split the message), and a duplicated Message-ID
  * Gmail's X-GM-THRID / X-Gmail-Labels headers (Spam and Trash included)
  * a final message with NO trailing newline

Same arguments => byte-identical output: all randomness comes from one
`random.Random(seed)`, dates are derived from a fixed epoch, and nothing reads
the clock. The truth file records what was written (counts, the sha256 of the
mbox, and a set of unique "needle" tokens with where each was planted) so a
test or a benchmark can check what the indexer found against what exists.

Line endings are CRLF by default; `--eol lf` writes a Unix-style mailbox. That
default is this generator's choice — it is not a claim about what any mail
provider writes.

Usage:
    scripts/synthetic-takeout.py --out /tmp/t --messages 200
    scripts/synthetic-takeout.py --out /data/t --target-bytes 1G --profile mixed
"""

from __future__ import annotations

import argparse
import base64
import gzip
import hashlib
import io
import json
import os
import quopri
import random
import re
import sys
import tarfile
import zipfile

MBOX_NAME = "All mail Including Spam and Trash.mbox"
BASE_EPOCH = 1609750800  # 2021-01-04T09:00:00Z — fixed; the clock is never read
WEEKDAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]

PEOPLE = [
    ("Dana Klein", "dana.klein"), ("Alex Rivard", "alex"), ("Priya Natarajan", "priya.n"),
    ("Zoë Müller", "zoe.mueller"), ("山田 太郎", "yamada"), ("محمد علي", "m.ali"),
    ("Søren Østergård", "soren"), ("Chloé Lefèvre", "chloe"), ("Bob Tanaka", "bob.t"),
    ("Ивана Петрова", "ivana"), ("Grace Okafor", "grace.o"), ("Liam O'Connor", "liam"),
    ("Mei-Ling Wu", "meiling"), ("Tomás Ruiz", "tomas.ruiz"), ("Hannah Schmidt", "hannah"),
    ("Omar Haddad", "omar.h"), ("Nina Kowalska", "nina.k"), ("Victor Hugo Lima", "vhl"),
]
DOMAINS = ["example.org", "example.com", "corp.example", "mail.example.net"]
NONASCII_SUBJECTS = [
    "Überweisung — Rechnung Nr.", "設計書のレビュー依頼", "مرحبا بالعالم — اجتماع",
    "Réunion décalée à", "Счёт на оплату №", "Año nuevo, presupuesto", "🚀 Launch checklist v",
]
ASCII_SUBJECTS = [
    "Quarterly forecast", "Term sheet draft", "Lunch on", "Invoice", "Re-org notes",
    "Contract redlines", "Deployment window", "Offsite agenda", "Board deck", "Hiring loop",
    "Vendor renewal", "Security review", "Roadmap sync", "Customer escalation", "Travel plans",
]
LABEL_SETS = [
    ["Inbox"], ["Inbox", "Important"], ["Sent"], ["Inbox", "Category Updates"],
    ["Archived"], ["Inbox", "Starred"], ["Spam"], ["Trash"], ["Inbox", "Ärger"],
]
LABEL_WEIGHTS = [30, 14, 22, 12, 12, 4, 3, 2, 1]
FROM_LINES = [
    "From the desk of the managing partner:",
    "From what I can tell the numbers are fine.",
    "From now on, please copy legal on these.",
]
PROSE_FROM = "From what I understand, the deal closes Monday and not before."

# Coverage must not depend on luck: at small N a 1% feature simply never fires,
# and a fixture that lacks the hard cases tests nothing. These message numbers
# ALWAYS carry the named feature; the probabilistic draws add more on top.
FORCED = {
    2: "body-needle", 3: "latin1", 4: "cp1252", 5: "from-line", 6: "quoted-from",
    9: "attach-pdf", 12: "attach-text", 15: "attach-binary", 18: "nonascii-subject",
}
FORCED_REPLY = {11: "via-references", 14: "dangling", 17: "direct"}


def make_vocab(rng: random.Random, n: int = 24000):
    """A Zipf-distributed synthetic vocabulary, so postings lists have the skew
    real text has (a uniform draw gives every term the same frequency, which no
    inverted index ever sees)."""
    onsets = ["b", "br", "c", "ch", "d", "dr", "f", "fl", "g", "gr", "h", "j", "k", "l", "m",
              "n", "p", "pl", "pr", "qu", "r", "s", "sh", "st", "t", "tr", "v", "w", "z", ""]
    nuclei = ["a", "e", "i", "o", "u", "ai", "ea", "io", "ou", "y"]
    codas = ["", "", "n", "r", "s", "t", "l", "m", "nd", "st", "ck", "ng"]
    seen, vocab = set(), []
    while len(vocab) < n:
        w = "".join(rng.choice(onsets) + rng.choice(nuclei) + rng.choice(codas)
                    for _ in range(rng.choice((1, 2, 2, 3, 3, 4))))
        if len(w) > 2 and w not in seen:
            seen.add(w)
            vocab.append(w)
    cum, total = [], 0.0
    for rank in range(1, n + 1):
        total += 1.0 / rank
        cum.append(total)
    return vocab, cum


class Text:
    def __init__(self, rng: random.Random):
        self.rng = rng
        self.vocab, self.cum = make_vocab(rng)

    def words(self, n: int):
        return self.rng.choices(self.vocab, cum_weights=self.cum, k=n)

    def paragraphs(self, n_words: int) -> str:
        out, left = [], n_words
        while left > 0:
            k = min(left, self.rng.randint(25, 90))
            ws = self.words(k)
            sentences, i = [], 0
            while i < len(ws):
                j = min(len(ws), i + self.rng.randint(5, 16))
                sentences.append(" ".join(ws[i:j]).capitalize() + ".")
                i = j
            out.append(self._wrap(" ".join(sentences)))
            left -= k
        return "\n\n".join(out)

    @staticmethod
    def _wrap(text: str, width: int = 74) -> str:
        lines, cur, n = [], [], 0
        for w in text.split(" "):
            if n + len(w) + (1 if cur else 0) > width and cur:
                lines.append(" ".join(cur))
                cur, n = [], 0
            cur.append(w)
            n += len(w) + (1 if len(cur) > 1 else 0)
        if cur:
            lines.append(" ".join(cur))
        return "\n".join(lines)


def needle(rng: random.Random, used: set) -> str:
    """A token that exists nowhere else in the corpus: 'xq' never occurs in the
    syllable vocabulary, so a hit for it is a hit for exactly one planted spot."""
    while True:
        t = "xq" + "".join(rng.choice("bcdfghjklmnprstvwz") + rng.choice("aeiou") for _ in range(5))
        if t not in used:
            used.add(t)
            return t


def encoded_word(text: str) -> str:
    """RFC 2047 B-encoding; split on CHARACTER boundaries into <=75-col words."""
    if all(ord(c) < 128 for c in text):
        return text
    words, cur = [], ""
    for ch in text:
        if len((cur + ch).encode("utf-8")) > 39:
            words.append(cur)
            cur = ""
        cur += ch
    if cur:
        words.append(cur)
    return "\r\n ".join("=?UTF-8?B?%s?=" % base64.b64encode(w.encode("utf-8")).decode() for w in words)


def b64_lines(data: bytes) -> str:
    return base64.encodebytes(data).decode("ascii").rstrip("\n")


def asctime_utc(epoch: int) -> str:
    days, rem = divmod(epoch, 86400)
    h, rem = divmod(rem, 3600)
    mi, s = divmod(rem, 60)
    y, m, d = civil(days)
    wd = WEEKDAYS[(days + 3) % 7]  # 1970-01-01 was a Thursday
    return "%s %s %2d %02d:%02d:%02d +0000 %d" % (wd, MONTHS[m - 1], d, h, mi, s, y)


def rfc5322_date(epoch: int) -> str:
    days, rem = divmod(epoch, 86400)
    h, rem = divmod(rem, 3600)
    mi, s = divmod(rem, 60)
    y, m, d = civil(days)
    wd = WEEKDAYS[(days + 3) % 7]
    return "%s, %d %s %d %02d:%02d:%02d +0000" % (wd, d, MONTHS[m - 1], y, h, mi, s)


def civil(z: int):
    """days since 1970-01-01 -> (y, m, d). Howard Hinnant's civil_from_days."""
    z += 719468
    era = z // 146097
    doe = z - era * 146097
    yoe = (doe - doe // 1460 + doe // 36524 - doe // 146096) // 365
    y = yoe + era * 400
    doy = doe - (365 * yoe + yoe // 4 - yoe // 100)
    mp = (5 * doy + 2) // 153
    d = doy - (153 * mp + 2) // 5 + 1
    m = mp + 3 if mp < 10 else mp - 9
    return (y + 1 if m <= 2 else y), m, d


def make_pdf(pages: list[list[str]]) -> bytes:
    """A minimal valid PDF 1.4 with a real text layer (Helvetica, WinAnsi).
    `pages` is a list of pages, each a list of ASCII lines."""
    objs: list[bytes] = []
    n_pages = len(pages)
    font_obj = 3 + 2 * n_pages
    kids = " ".join("%d 0 R" % (3 + 2 * i) for i in range(n_pages))
    objs.append(b"<< /Type /Catalog /Pages 2 0 R >>")
    objs.append(("<< /Type /Pages /Kids [%s] /Count %d >>" % (kids, n_pages)).encode())
    for i, lines in enumerate(pages):
        ops = ["BT", "/F1 11 Tf", "72 740 Td", "14 TL"]
        for ln in lines:
            esc = ln.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)")
            ops.append("(%s) Tj T*" % esc)
        ops.append("ET")
        stream = "\n".join(ops).encode("latin-1")
        objs.append(("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents %d 0 R "
                     "/Resources << /Font << /F1 %d 0 R >> >> >>" % (4 + 2 * i, font_obj)).encode())
        objs.append(b"<< /Length %d >>\nstream\n" % len(stream) + stream + b"\nendstream")
    objs.append(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>")
    out = io.BytesIO()
    out.write(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")
    offsets = []
    for n, body in enumerate(objs, start=1):
        offsets.append(out.tell())
        out.write(b"%d 0 obj\n" % n + body + b"\nendobj\n")
    xref = out.tell()
    out.write(b"xref\n0 %d\n0000000000 65535 f \n" % (len(objs) + 1))
    for off in offsets:
        out.write(b"%010d 00000 n \n" % off)
    out.write(b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(objs) + 1, xref))
    return out.getvalue()


def lognorm(rng: random.Random, mu: float, sigma: float, lo: int, hi: int) -> int:
    return max(lo, min(hi, int(rng.lognormvariate(mu, sigma))))


QUOTE_RE = re.compile(rb"(?m)^(>*From )")


class Mailbox:
    def __init__(self, rng: random.Random, text: Text, profile: str, truth: dict,
                 blob_max: int = 3 << 20, long_line: int = 300 << 10):
        self.rng, self.text, self.profile, self.truth = rng, text, profile, truth
        self.blob_max, self.long_line = blob_max, long_line
        self.used_needles: set = set()
        self.n = 0
        self.epoch = BASE_EPOCH
        # open threads: (thread_id, [message ids oldest->newest], subject)
        self.threads: list = []
        self.present_ids: set = set()
        self.attach_rate = {"mixed": 0.22, "text": 0.04}[profile]

    # ── pieces ──────────────────────────────────────────────────────────
    def person(self):
        name, local = self.rng.choice(PEOPLE)
        return name, "%s@%s" % (local, self.rng.choice(DOMAINS))

    def addr(self, person) -> str:
        name, email = person
        return "%s <%s>" % (encoded_word(name), email)

    def plant(self, where: str, message_id: str) -> str:
        t = needle(self.rng, self.used_needles)
        self.truth["needles"].append({"token": t, "where": where, "message_id": message_id})
        return t

    def attachment(self, message_id: str, want_needle: bool, kind: str | None = None):
        """-> (mime part text, kind). `kind` forces pdf/text/binary."""
        rng = self.rng
        roll = rng.random()
        if self.profile == "text":
            roll = min(roll, 0.59)  # text profile: PDFs and text files only
        roll = {"pdf": 0.0, "text": 0.45, "binary": 0.9}.get(kind, roll)
        if roll < 0.30:
            kind = "pdf"
            pages = []
            for p in range(rng.randint(1, 4)):
                lines = Text._wrap(" ".join(self.text.words(rng.randint(120, 380))), 80).split("\n")
                if want_needle and p == 0:
                    lines.insert(1, "Reference code %s appears on this page only." %
                                 self.plant("pdf-attachment", message_id))
                pages.append(lines)
            data, ctype = make_pdf(pages), "application/pdf"
            name = rng.choice(["term-sheet", "invoice", "contract", "report", "statement"]) + \
                "-%d.pdf" % rng.randint(100, 9999)
        elif roll < 0.60:
            kind = "text"
            if rng.random() < 0.5:
                rows = ["date,account,amount,memo"] + [
                    "2023-%02d-%02d,%s,%d.%02d,%s" % (rng.randint(1, 12), rng.randint(1, 28),
                                                      rng.choice(self.text.vocab[:400]),
                                                      rng.randint(1, 99999), rng.randint(0, 99),
                                                      " ".join(self.text.words(3)))
                    for _ in range(rng.randint(10, 400))]
                body, ctype, ext = "\n".join(rows), "text/csv", "csv"
            else:
                body, ctype, ext = self.text.paragraphs(lognorm(rng, 6.0, 0.8, 40, 6000)), "text/plain", "txt"
            if want_needle:
                body += "\n%s\n" % self.plant("text-attachment", message_id)
            data = body.encode("utf-8")
            name = rng.choice(["notes", "export", "minutes", "ledger"]) + "-%d.%s" % (rng.randint(1, 999), ext)
        else:
            kind = "binary"
            size = lognorm(rng, 11.0, 1.1, min(2048, self.blob_max), self.blob_max)
            if rng.random() < 0.75:
                data = b"\x89PNG\r\n\x1a\n" + rng.randbytes(size)
                ctype, name = "image/png", "IMG_%04d.png" % rng.randint(1, 9999)
            else:
                data = b"PK\x03\x04" + rng.randbytes(size)
                ctype, name = "application/zip", "archive-%d.zip" % rng.randint(1, 999)
        if rng.random() < 0.10:
            name = rng.choice(["設計書", "Überblick", "تقرير", "résumé"]) + "-" + name
        self.truth["attachments"][kind] += 1
        fname = encoded_word(name)
        part = ("Content-Type: %s; name=\"%s\"\nContent-Transfer-Encoding: base64\n"
                "Content-Disposition: attachment; filename=\"%s\"\n\n%s\n" % (ctype, fname, fname, b64_lines(data)))
        return part, kind

    # ── one ordinary message ────────────────────────────────────────────
    def message(self) -> bytes:
        rng = self.rng
        self.n += 1
        self.epoch += rng.randint(40, 5400)
        sender, rcpt = self.person(), self.person()
        mid = "%08x.%06d@%s" % (rng.getrandbits(32), self.n, sender[1].split("@")[1])

        forced = FORCED.get(self.n)
        forced_reply = FORCED_REPLY.get(self.n)
        in_reply_to, refs, thread_id, subject = None, [], None, None
        reply_roll = rng.random()
        if self.threads and (reply_roll < 0.55 or forced_reply):
            ti = rng.randrange(max(0, len(self.threads) - 200), len(self.threads))
            thread_id, chain, subject = self.threads[ti]
            parent = rng.choice(chain[-3:])
            refs = chain[:chain.index(parent) + 1]
            in_reply_to = parent
            fate = rng.random()
            fate = {"dangling": 0.0, "via-references": 0.04, "direct": 0.5}.get(forced_reply, fate)
            if fate < 0.03:      # parent AND ancestors are not in this mailbox
                in_reply_to = "gone-%06d@elsewhere.example" % self.n
                refs = ["older-%06d@elsewhere.example" % self.n, in_reply_to]
                self.truth["replies_dangling"] += 1
            elif fate < 0.06:    # parent missing, an ancestor in References is present
                in_reply_to = "lost-%06d@elsewhere.example" % self.n
                refs = refs + [in_reply_to]
                self.truth["replies_resolvable"] += 1
                self.truth["replies_via_references"] += 1
            else:
                self.truth["replies_resolvable"] += 1
            chain.append(mid)
            if not subject.lower().startswith("re:"):
                subject = "Re: " + subject
        else:
            if rng.random() < 0.15 or forced == "nonascii-subject":
                subject = "%s %d" % (rng.choice(NONASCII_SUBJECTS), rng.randint(1, 9999))
            else:
                subject = "%s %s" % (rng.choice(ASCII_SUBJECTS), " ".join(self.text.words(rng.randint(1, 4))))
            thread_id = rng.getrandbits(62) | (1 << 60)
            self.threads.append((thread_id, [mid], subject))
            self.truth["threads"] += 1
        self.present_ids.add(mid)

        # body
        body = self.text.paragraphs(lognorm(rng, 4.6, 0.9, 8, 5000))
        if self.n % 97 == 0 or forced == "body-needle":
            body += "\n\nTracking token: %s\n" % self.plant("body", mid)
        if rng.random() < 0.05 or forced == "from-line":
            line = rng.choice(FROM_LINES)
            body = body + "\n\n" + line + "\n" + self.text.paragraphs(20)
            self.truth["from_lines_in_bodies"] += 1
        if rng.random() < 0.01 or forced == "quoted-from":
            body += "\n\nOn Monday Bob wrote:\n>From the archive, as requested.\n> second quoted line\n"
            self.truth["quoted_from_lines_in_bodies"] += 1

        labels = rng.choices(LABEL_SETS, weights=LABEL_WEIGHTS, k=1)[0]
        head = [
            "X-GM-THRID: %d" % thread_id,
            "X-Gmail-Labels: %s" % ",".join(encoded_word(l) for l in labels),
            "MIME-Version: 1.0",
            "Date: %s" % rfc5322_date(self.epoch),
            "Message-ID: <%s>" % mid,
            "Subject: %s" % encoded_word(subject),
            "From: %s" % self.addr(sender),
            "To: %s" % self.addr(rcpt),
        ]
        if in_reply_to:
            head.append("In-Reply-To: <%s>" % in_reply_to)
            head.append("References: %s" % "\n ".join("<%s>" % r for r in refs[-12:]))

        style = rng.random()
        style = {"latin1": 0.0, "cp1252": 0.035}.get(forced, style)
        bare = False
        if style < 0.03:
            # declared ISO-8859-1, sent 8-bit
            tok = self.plant("latin1-8bit-body", mid)
            latin = (body + "\n\nGrüße aus Köln, café, naïve, señor %s\n" % tok).encode("latin-1")
            text_part = b"Content-Type: text/plain; charset=iso-8859-1\nContent-Transfer-Encoding: 8bit\n\n" + latin
            self.truth["bodies_latin1_8bit"] += 1
        elif style < 0.04:
            # UNDECLARED Windows-1252: no MIME-Version, no Content-Type, raw 8-bit
            # bytes (curly quotes 0x93/0x94, euro 0x80, e-acute 0xe9) — what an old
            # desktop client wrote. Not valid UTF-8, and nothing says what it is.
            tok = self.plant("cp1252-undeclared-body", mid)
            text_part = b"\n" + (body + "\n\n\u201cQuoted\u201d price: \u20ac420, caf\u00e9 %s\n" % tok).encode("cp1252")
            bare = True
            self.truth["bodies_cp1252_undeclared"] += 1
        elif style < 0.13:
            b = "b%012x" % rng.getrandbits(48)
            html = "<html><body>%s</body></html>" % "".join("<p>%s</p>" % p for p in body.split("\n\n"))
            text_part = ("Content-Type: multipart/alternative; boundary=\"%s\"\n\n--%s\n"
                         "Content-Type: text/plain; charset=utf-8\n\n%s\n--%s\n"
                         "Content-Type: text/html; charset=utf-8\n\n%s\n--%s--\n" % (b, b, body, b, html, b)).encode()
        elif style < 0.18:
            html = "<html><body>%s</body></html>" % "".join("<p>%s</p>" % p for p in body.split("\n\n"))
            text_part = ("Content-Type: text/html; charset=utf-8\n\n%s\n" % html).encode()
        elif style < 0.30:
            uni = body + "\n\n— Zoë (設計 · مرحبا)\n"
            text_part = (b"Content-Type: text/plain; charset=utf-8\nContent-Transfer-Encoding: quoted-printable\n\n"
                         + quopri.encodestring(uni.encode("utf-8")))
        else:
            text_part = ("Content-Type: text/plain; charset=utf-8\n\n%s\n" % body).encode()

        if bare:
            head = [h for h in head if not h.startswith("MIME-Version")]
        forced_kind = {"attach-pdf": "pdf", "attach-text": "text", "attach-binary": "binary"}.get(forced)
        if not bare and (rng.random() < self.attach_rate or forced_kind):
            b = "m%012x" % rng.getrandbits(48)
            parts = [b"--" + b.encode() + b"\n" + text_part]
            for k in range(rng.choice((1, 1, 1, 2, 3))):
                part, _ = self.attachment(mid, want_needle=((self.n % 11 == 0 or bool(forced_kind)) and k == 0),
                                          kind=forced_kind if k == 0 else None)
                parts.append(("\n--%s\n" % b).encode() + part.encode())
            parts.append(("\n--%s--\n" % b).encode())
            payload = ("Content-Type: multipart/mixed; boundary=\"%s\"\n\n" % b).encode() + b"".join(parts)
            self.truth["messages_with_attachments"] += 1
        else:
            payload = text_part
        self.truth["messages_regular"] += 1
        return self.entry("\n".join(head).encode() + b"\n" + payload, thread_id)

    # ── malformed and adversarial entries ───────────────────────────────
    def malformed(self) -> list[bytes]:
        rng, out = self.rng, []
        tid = lambda: rng.getrandbits(62) | (1 << 60)  # noqa: E731
        self.epoch += 60
        # Bytes that are not UTF-8, not a BOM (FF FE / FE FF would make this VALID
        # UTF-16 and the "garbage" a legitimate document), and include the five
        # code points Windows-1252 leaves undefined (81 8D 8F 90 9D).
        out.append(self.entry(b"\x81\x8d\x8f\x90\x9d\x00\x01garbage with no headers\n\x80\x81\x82", tid()))
        out.append(self.entry(
            b"From: a@example.org\nSubject: broken multipart\nMIME-Version: 1.0\n"
            b"Content-Type: multipart/mixed; boundary=\"zz\"\n\n--zz\n"
            b"Content-Type: application/pdf; name=\"x.pdf\"\nContent-Transfer-Encoding: base64\n\nJVBERi0xLjQKJ", tid()))
        out.append(self.entry(
            b"From: b@example.org\nSubject: =?UTF-8?B?4pyTIGJyb2tlbg\xff\xfe?= tail\n"
            b"Message-ID: <badword-%d@example.org>\n\nbody after a broken encoded-word \xe8\xa8\n" % self.n, tid()))
        out.append(self.entry(
            b"From: c@example.org\nSubject: one very long line\nMessage-ID: <longline-%d@example.org>\n\n" % self.n
            + b"L" * self.long_line, tid()))
        out.append(self.entry(b"", tid()))
        prose_id = "prosefrom-%d@example.org" % self.n
        out.append(self.entry((
            "From: d@example.org\nSubject: unquoted prose From\nMessage-ID: <%s>\n\nFirst paragraph.\n\n%s\n"
            "This sentence must stay in the same message: %s\n" % (
                prose_id, PROSE_FROM, self.plant("after-unquoted-prose-from", prose_id))).encode(), tid(), quote=False))
        dup = b"From: e@example.org\nSubject: duplicate id\nMessage-ID: <duplicate-%d@example.org>\n\nsame id twice\n" % self.n
        out.append(self.entry(dup, tid()))
        out.append(self.entry(dup, tid()))
        self.truth["entries_malformed_block"] += len(out)
        self.truth["entries_empty"] += 1                 # the separator-only entry
        self.truth["attachments_malformed"]["pdf"] += 1  # the truncated-base64 x.pdf
        return out

    def entry(self, message: bytes, thread_id: int, quote: bool = True) -> bytes:
        if quote:
            message = QUOTE_RE.sub(rb">\1", message)
        sep = ("From %d@xxx %s\n" % (thread_id, asctime_utc(self.epoch))).encode()
        self.truth["entries"] += 1
        return sep + message


def write_mbox(path: str, rng, text, args, truth) -> None:
    box = Mailbox(rng, text, args.profile, truth, args.blob_max, args.long_line_bytes)
    sha, written = hashlib.sha256(), 0
    eol = b"\r\n" if args.eol == "crlf" else b"\n"

    def conv(b: bytes) -> bytes:
        return b if eol == b"\n" else b.replace(b"\r\n", b"\n").replace(b"\n", b"\r\n")

    with open(path, "wb") as f:
        pending = None
        count = 0

        def flush(last: bool):
            nonlocal written, pending
            if pending is None:
                return
            data = conv(pending if last else pending + b"\n" if pending.endswith(b"\n") else pending + b"\n\n")
            if last:
                data = data.rstrip(b"\r\n")  # the final message has NO trailing newline
            f.write(data)
            sha.update(data)
            written += len(data)
            pending = None

        def push(entry: bytes):
            nonlocal pending
            flush(False)
            pending = entry

        malformed_every = 1000
        while True:
            if args.messages is not None and count >= args.messages:
                break
            if args.target_bytes is not None and written >= args.target_bytes:
                break
            if count == min(7, (args.messages or 8) - 1) or (count and count % malformed_every == 0):
                for e in box.malformed():
                    push(e)
            push(box.message())
            count += 1
            if not args.quiet and count % 20000 == 0:
                print("  … %d messages, %.1f MB" % (count, written / 1e6), file=sys.stderr)
        flush(True)
    truth["mbox"] = {"path": os.path.relpath(path, args.out), "bytes": written, "sha256": sha.hexdigest(),
                     "eol": args.eol, "trailing_newline": False}


def write_keep(keep_dir: str, rng, text, n: int, truth, box_needles: set, ascii_names: bool) -> None:
    os.makedirs(keep_dir, exist_ok=True)
    titles = ["Groceries", "Deal follow-ups", "設計メモ", "قائمة المهام", "Reading list", "Gift ideas"]
    for i in range(n):
        title = "%s %d" % (titles[i % len(titles)], i + 1) if i % 5 else ""
        created = (BASE_EPOCH + i * 86400) * 1_000_000
        note = {"color": "DEFAULT", "isTrashed": i % 7 == 6, "isPinned": i % 4 == 0, "isArchived": i % 5 == 4,
                "title": title, "userEditedTimestampUsec": created + 3_600_000_000,
                "createdTimestampUsec": created}
        tok = needle(rng, box_needles)
        truth["needles"].append({"token": tok, "where": "keep-note", "message_id": None})
        if i % 3 == 0:
            note["listContent"] = [{"text": " ".join(text.words(3)), "isChecked": bool(j % 2)} for j in range(4)]
            note["listContent"].append({"text": tok, "isChecked": False})
        else:
            note["textContent"] = text.paragraphs(40) + "\n" + tok
        if i % 2 == 0:
            note["labels"] = [{"name": "work"}] + ([{"name": "Ärger"}] if i % 4 == 0 else [])
        stem = "note-%03d" % i if (i % 6 or ascii_names) else "メモ-%03d" % i
        with open(os.path.join(keep_dir, stem + ".json"), "w", encoding="utf-8") as f:
            json.dump(note, f, ensure_ascii=False, sort_keys=True)
        with open(os.path.join(keep_dir, stem + ".html"), "w", encoding="utf-8") as f:
            body = note.get("textContent") or " ".join(x["text"] for x in note["listContent"])
            f.write("<html><head><title>%s</title></head><body><div class=\"note\">%s</div></body></html>\n"
                    % (title, body.replace("\n", "<br>")))
    with open(os.path.join(keep_dir, "Labels.txt"), "w", encoding="utf-8") as f:
        f.write("work\nÄrger\n")
    truth["keep_notes"] = n


def write_archives(out: str) -> list[str]:
    """Deterministic stand-ins for the OTHER parts of a multi-part download that
    were never extracted. autoindex must answer 'extract it first', not index."""
    names = []
    zpath = os.path.join(out, "takeout-20240101T000000Z-002.zip")
    with zipfile.ZipFile(zpath, "w", zipfile.ZIP_DEFLATED) as z:
        info = zipfile.ZipInfo("Takeout/Mail/part-two.mbox", date_time=(2024, 1, 1, 0, 0, 0))
        z.writestr(info, "From x@xxx Mon Jan  1 00:00:00 +0000 2024\nSubject: inside a zip\n\nnot indexed\n")
    names.append(os.path.basename(zpath))
    tpath = os.path.join(out, "takeout-20240101T000000Z-003.tgz")
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w", format=tarfile.USTAR_FORMAT) as t:
        data = b"inside a tarball; not indexed\n"
        ti = tarfile.TarInfo("Takeout/Drive/part-three.txt")
        ti.size, ti.mtime = len(data), 0
        t.addfile(ti, io.BytesIO(data))
    with open(tpath, "wb") as f, gzip.GzipFile(fileobj=f, mode="wb", mtime=0, filename="") as g:
        g.write(raw.getvalue())
    names.append(os.path.basename(tpath))
    return names


def parse_size(s: str) -> int:
    m = re.fullmatch(r"(\d+(?:\.\d+)?)\s*([KMG]?)B?", s.strip(), re.I)
    if not m:
        raise argparse.ArgumentTypeError("size like 250M or 1G, got %r" % s)
    return int(float(m.group(1)) * {"": 1, "K": 1 << 10, "M": 1 << 20, "G": 1 << 30}[m.group(2).upper()])


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--out", required=True, help="directory to create (must not exist or be empty)")
    ap.add_argument("--seed", type=int, default=42)
    size = ap.add_mutually_exclusive_group()
    size.add_argument("--messages", type=int, help="ordinary messages to write (default 200)")
    size.add_argument("--target-bytes", type=parse_size, help="write until the mbox is this large, e.g. 1G")
    ap.add_argument("--profile", choices=["mixed", "text"], default="mixed",
                    help="mixed: 22%% of messages carry attachments, most bytes are base64 blobs (what a real "
                         "mailbox weighs). text: almost all bytes are indexable text — the indexer's worst case.")
    ap.add_argument("--eol", choices=["crlf", "lf"], default="crlf")
    ap.add_argument("--keep-notes", type=int, default=12)
    ap.add_argument("--with-archive", action="store_true", help="also write an unextracted .zip and .tgz")
    ap.add_argument("--blob-max", type=parse_size, default=3 << 20,
                    help="largest binary attachment (default 3M); shrink it for a small committed fixture")
    ap.add_argument("--long-line-bytes", type=parse_size, default=300 << 10,
                    help="length of the malformed block's single newline-free line (default 300K)")
    ap.add_argument("--ascii-names", action="store_true",
                    help="ASCII file names only (for a tree that is committed to git and checked out on "
                         "every OS); message CONTENT stays non-ASCII either way")
    ap.add_argument("--truth", help="where to write the ground truth (default: <out>.truth.json, OUTSIDE the tree)")
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args()
    if args.messages is None and args.target_bytes is None:
        args.messages = 200
    if os.path.isdir(args.out) and os.listdir(args.out):
        print("refusing to write into non-empty %s" % args.out, file=sys.stderr)
        return 2

    rng = random.Random(args.seed)
    text = Text(rng)
    truth = {
        "generator": "synthetic-takeout.py", "seed": args.seed, "profile": args.profile,
        "entries": 0, "messages_regular": 0, "entries_malformed_block": 0, "messages_with_attachments": 0,
        "threads": 0, "replies_resolvable": 0, "replies_via_references": 0, "replies_dangling": 0,
        "from_lines_in_bodies": 0, "quoted_from_lines_in_bodies": 0, "bodies_latin1_8bit": 0,
        "bodies_cp1252_undeclared": 0, "entries_empty": 0, "attachments_malformed": {"pdf": 0},
        "attachments": {"pdf": 0, "text": 0, "binary": 0}, "needles": [],
    }
    takeout = os.path.join(args.out, "Takeout")
    os.makedirs(os.path.join(takeout, "Mail"), exist_ok=True)
    os.makedirs(os.path.join(takeout, "Drive"), exist_ok=True)
    write_mbox(os.path.join(takeout, "Mail", MBOX_NAME), rng, text, args, truth)
    used = {n["token"] for n in truth["needles"]}
    write_keep(os.path.join(takeout, "Keep"), rng, text, args.keep_notes, truth, used, args.ascii_names)
    drive_tok = needle(rng, used)
    truth["needles"].append({"token": drive_tok, "where": "drive-markdown", "message_id": None})
    with open(os.path.join(takeout, "Drive", "meeting-notes.md"), "w", encoding="utf-8") as f:
        f.write("# Meeting notes\n\n%s\n\nAction item code: %s\n" % (text.paragraphs(120), drive_tok))
    with open(os.path.join(takeout, "Drive", "todo.txt"), "w", encoding="utf-8") as f:
        f.write(text.paragraphs(60) + "\n")
    with open(os.path.join(takeout, "archive_browser.html"), "w", encoding="utf-8") as f:
        f.write("<html><head><title>Archive browser</title></head><body><h1>Your export</h1><ul>"
                "<li>Mail/%s</li><li>Keep/ (%d notes)</li><li>Drive/meeting-notes.md</li></ul></body></html>\n"
                % (MBOX_NAME, args.keep_notes))
    truth["archives"] = write_archives(args.out) if args.with_archive else []
    truth["expected_skips"] = {"default:takeout-index": 1, "default:takeout-keep-html-twin": args.keep_notes}

    truth_path = args.truth or (args.out.rstrip("/\\") + ".truth.json")
    with open(truth_path, "w", encoding="utf-8") as f:
        json.dump(truth, f, ensure_ascii=False, indent=1, sort_keys=True)
        f.write("\n")
    if not args.quiet:
        m = truth["mbox"]
        print("wrote %s\n  mbox: %.1f MB, %d entries (%d regular, %d in malformed blocks), %d threads\n"
              "  attachments: %s\n  truth: %s" % (args.out, m["bytes"] / 1e6, truth["entries"],
                                                  truth["messages_regular"], truth["entries_malformed_block"],
                                                  truth["threads"], truth["attachments"], truth_path),
              file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
