#!/usr/bin/env python3
"""The folder behind review-repro.json: the edge cases the correctness review of
PR #945 found in the console Reader. Each file exists to make one defect
visible on a REAL node:

  inbox/06-bigpdf.eml    a 300-page PDF, then a trailing text attachment
                         (the attachment join read 200 records and stopped)
  inbox/04-samename.eml  two attachments both named scan.pdf
                         (the list was keyed by file name)
  inbox/02-nomid.eml     no Message-ID header (the join key was the message id)
  inbox/03-dup.eml,
  archive/03-dup.eml     two files, one Message-ID (each listed the other's)
  inbox/01..08           ordinary subjects: autoindex types email_subject as
                         `keyword` here, so `match` needs the whole subject
  cfg/Makefile, *.ini    line files: their text lands in `text`, not `body`
"""
import os, shutil, sys
from email.message import EmailMessage
from email.utils import format_datetime
from datetime import datetime, timezone, timedelta

out = sys.argv[1]
shutil.rmtree(out, ignore_errors=True)
os.makedirs(f"{out}/inbox"); os.makedirs(f"{out}/archive"); os.makedirs(f"{out}/cfg")
t0 = datetime(2026, 8, 3, 9, 0, tzinfo=timezone.utc)

def pdf(pages):
    """Minimal text PDF, one line of text per page."""
    objs = []
    def add(b): objs.append(b); return len(objs)
    font = add(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
    pages_id = len(objs) + 1 + 2 * len(pages)  # reserve: after all page+content objs
    kids = []
    for text in pages:
        stream = f"BT /F1 12 Tf 72 720 Td ({text}) Tj ET".encode()
        c = add(b"<< /Length %d >>\nstream\n" % len(stream) + stream + b"\nendstream")
        p = add(f"<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 612 792] /Contents {c} 0 R /Resources << /Font << /F1 {font} 0 R >> >> >>".encode())
        kids.append(p)
    pid = add(f"<< /Type /Pages /Kids [{' '.join(f'{k} 0 R' for k in kids)}] /Count {len(kids)} >>".encode())
    assert pid == pages_id
    cat = add(f"<< /Type /Catalog /Pages {pid} 0 R >>".encode())
    body = b"%PDF-1.4\n"; offs = []
    for i, o in enumerate(objs, 1):
        offs.append(len(body)); body += b"%d 0 obj\n" % i + o + b"\nendobj\n"
    xref = len(body)
    body += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objs) + 1)
    for o in offs: body += b"%010d 00000 n \n" % o
    body += f"trailer\n<< /Size {len(objs)+1} /Root {cat} 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode()
    return body

def mail(path, subj, body, mid="auto", atts=(), n=0):
    m = EmailMessage()
    m["Subject"] = subj; m["From"] = "Dana Reyes <dana@acme.example>"; m["To"] = "board@acme.example"
    m["Date"] = format_datetime(t0 + timedelta(days=n))
    if mid == "auto": m["Message-ID"] = f"<{os.path.basename(path)}@acme.example>"
    elif mid: m["Message-ID"] = mid
    m.set_content(body)
    for name, data in atts:
        if name.endswith(".pdf"): m.add_attachment(data, maintype="application", subtype="pdf", filename=name)
        else: m.add_attachment(data.decode(), subtype="plain", filename=name)
    with open(path, "wb") as f: f.write(bytes(m))

mail(f"{out}/inbox/01-plain.eml", "Lunch on Friday?", "Tacos. Noon. No agenda.", n=1)
mail(f"{out}/inbox/02-nomid.eml", "No message id here", "This mail has no Message-ID header.", mid=None, n=2,
     atts=[("nomid-notes.txt", b"nomidnotes plain text attachment"), ("nomid.pdf", pdf(["nomidpdf page one", "nomidpdf page two"]))])
mail(f"{out}/inbox/03-dup.eml", "Duplicate id (inbox copy)", "inbox copy body", mid="<dup@acme.example>", n=3,
     atts=[("inbox-only.txt", b"inboxonly attachment text")])
mail(f"{out}/archive/03-dup.eml", "Duplicate id (archive copy)", "archive copy body", mid="<dup@acme.example>", n=3,
     atts=[("archive-only.txt", b"archiveonly attachment text")])
mail(f"{out}/inbox/04-samename.eml", "Two scans with the same file name", "Both attachments are called scan.pdf.", n=4,
     atts=[("scan.pdf", pdf(["firstscan alpha"])), ("scan.pdf", pdf(["secondscan beta"]))])
mail(f"{out}/inbox/05-brief.eml", "Baseline term sheet", "The baseline term sheet is attached.", n=5,
     atts=[("brief.pdf", pdf([f"brief page {i} valuation cap" for i in range(1, 15)]))])
mail(f"{out}/inbox/06-bigpdf.eml", "Big PDF then a trailing text file", "See the two attachments.", n=6,
     atts=[("aaa-big.pdf", pdf([f"bigpdf page {i} filler words here" for i in range(1, 301)])), ("zzz-last.txt", b"zzzlast trailing attachment text")])
mail(f"{out}/inbox/07-verylong.eml", "verylongsubjectword in a subject only", "Nothing about that word in the body.", n=7)
mail(f"{out}/inbox/08-retention.eml", "Security review of the retention policy", "The retention policy needs a clause on backup deletion.", n=8)
open(f"{out}/cfg/Makefile", "w").write("all: build test\n\nbuild:\n\tcargo build --release\n\ntest:\n\tcargo test\n\nverylongsubjectword:\n\techo makefiletargetword\n\nclean:\n\trm -rf out\n\ninstall:\n\tcp out/bin /usr/local/bin\n")
open(f"{out}/cfg/settings.ini", "w").write("[Unit]\nDescription=demo inifileword\nAfter=network.target\n\n[Service]\nExecStart=/bin/true\nRestart=always\nUser=verylongsubjectword\n\n[Install]\nWantedBy=multi-user.target\n")
open(f"{out}/cfg/.env", "w").write("DATABASE_URL=postgres://localhost/envfileword\nAPI_TOKEN=verylongsubjectword\n")
print("repro corpus at", out, "files:", sum(len(f) for _, _, f in os.walk(out)))
