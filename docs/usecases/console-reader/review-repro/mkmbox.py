#!/usr/bin/env python3
"""A folder holding ONE mailbox file (mbox), for mbox-repro.mjs: several
messages share one file, so they share one `ax_file` once autoindex's mbox
ingest (PR #949) splits the file. Each message exists to make one join visible:

  "Quarterly report 2026"         report.pdf (3 pages) + notes.txt
  "Lunch on Friday?"              no attachments
  "Two scans, same name"          two attachments both named scan.pdf
  "No message id in this one"     no Message-ID header; nomid.txt
  "Invoice 9120 attached"         invoice.pdf (2 pages)

truth.json (written next to the folder) lists what each message carries, from
this generator, not from the engine.
"""
import json, mailbox, os, shutil, sys
from email.message import EmailMessage
from email.utils import format_datetime
from datetime import datetime, timezone, timedelta

out = sys.argv[1]
shutil.rmtree(out, ignore_errors=True)
os.makedirs(f"{out}/Mail")
t0 = datetime(2026, 8, 3, 9, 0, tzinfo=timezone.utc)

def pdf(pages):
    """Minimal text PDF, one line of text per page (same as mkrepro.py)."""
    objs = []
    def add(b): objs.append(b); return len(objs)
    font = add(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
    pages_id = len(objs) + 1 + 2 * len(pages)
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

MSGS = [
    ("Quarterly report 2026", "<q1@acme.example>", "The quarterly report and my notes.",
     [("report.pdf", pdf([f"quarterlypdf page {i}" for i in (1, 2, 3)])), ("notes.txt", b"quarterlynotes plain text")]),
    ("Lunch on Friday?", "<lunch@acme.example>", "Tacos. Noon. No agenda.", []),
    ("Two scans, same name", "<scans@acme.example>", "Both attachments are called scan.pdf.",
     [("scan.pdf", pdf(["firstmboxscan alpha"])), ("scan.pdf", pdf(["secondmboxscan beta"]))]),
    ("No message id in this one", None, "This message has no Message-ID header.",
     [("nomid.txt", b"mboxnomid plain text attachment")]),
    ("Invoice 9120 attached", "<inv@acme.example>", "Invoice attached.",
     [("invoice.pdf", pdf(["invoicepdf page 1", "invoicepdf page 2"]))]),
]
box = mailbox.mbox(f"{out}/Mail/All mail.mbox", create=True)
for n, (subj, mid, body, atts) in enumerate(MSGS):
    m = EmailMessage()
    m["Subject"] = subj; m["From"] = "Dana Reyes <dana@acme.example>"; m["To"] = "board@acme.example"
    m["Date"] = format_datetime(t0 + timedelta(days=n))
    if mid: m["Message-ID"] = mid
    m.set_content(body)
    for name, data in atts:
        if name.endswith(".pdf"): m.add_attachment(data, maintype="application", subtype="pdf", filename=name)
        else: m.add_attachment(data.decode(), subtype="plain", filename=name)
    box.add(m)
box.flush(); box.close()
with open(f"{out}.truth.json", "w") as f:
    json.dump({subj: [name for name, _ in atts] for subj, _, _, atts in MSGS}, f, indent=2)
print("mailbox at", f"{out}/Mail/All mail.mbox", "messages:", len(MSGS))
