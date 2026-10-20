"""Boundary-crossing email corpus for the #1108 wrong-and-confident gate.

The `systemone-classify-email` benchmark already measured the local `/_decide`
vote on a hand-authored "hard" tier of 8 boundary-crossing emails and found it
wrong AND confident on the two `invoice` cases (0.836-1.000). Eight examples
cannot resolve a 5 % rate: one miss is 12.5 %. #1108's gate is "wrong-and-confident
<= 5 % at p >= 0.8" on the classify-email boundary tier, so the tier has to be
big enough to put that 5 % line somewhere a measurement can land on either side.

This script scales the SAME four boundary patterns the hand-authored tier used
(see `systemone-classify-email/gen_corpus.py`'s HARD list) up to a deterministic
set of ORIGINAL, template-generated emails — no real mailbox, no third-party
dataset, no licence or privacy question. The labels are ground truth BY
CONSTRUCTION (each generator emits exactly one class), which is what lets a fire
be scored right-or-wrong. The four patterns, each borrowing another class's
surface vocabulary on purpose:

  invoice     -> money/billing phrased casually, the word "invoice" never used
                 ("did the transfer land", "wrapping up the offsite numbers")
  personal    -> a casual note that happens to name a dollar figure
                 ("owe you for gas, send $40 whenever")
  newsletter  -> a digest that ends by asking the reader to reply / vote
  needs-reply -> a required confirmation buried inside a status-update rundown

History (the labelled vote corpus the watch decides against) is the 200-email
train split of `systemone-classify-email/corpus.json`, reused verbatim so the
two benchmarks share one vote corpus. This script writes ONLY the boundary tier.

Deterministic (seed 1108): rerunning reproduces boundary.json byte for byte.

Usage: python3 gen_boundary.py   ->  writes boundary.json
"""
import json
import random

random.seed(1108)

PER_LABEL = 30  # 4 labels -> 120 boundary emails

FIRST = ["Alex", "Priya", "Jonas", "Mei", "Sara", "Tomás", "Nina", "Ola",
         "Derek", "Fatima", "Luca", "Hana", "Owen", "Ingrid", "Kwame", "Yuki",
         "Rafe", "Noor", "Pavel", "Esme"]
COMPANIES = ["Northwind Supply", "Bluepeak Logistics", "Harborline Freight",
             "Cedarline Studio", "Voss & Rane", "Marlowe Analytics",
             "Fenwick Cloud", "Atlas Facilities", "Ridgeway Consulting",
             "Greenfield Robotics"]


def amount():
    return f"${random.randint(35, 4800):,}"


def when():
    return random.choice(["today", "by Friday", "this week", "before month end",
                          "by end of day", "tomorrow", "by the 15th", "this afternoon"])


# ── invoice, with no invoice vocabulary ──────────────────────────────────
def invoice(i):
    name = random.choice(FIRST)
    a = amount()
    variants = [
        (f"did that ever go through?",
         f"Hey {name}, quick check — did the transfer for last month's freight "
         f"land yet? Should have been around {a}, the reference was on the packing "
         f"slip. No rush, just reconciling the books {when()}."),
        (f"for the offsite",
         f"Wrapping up the offsite numbers. Catering came to {a}, venue was a bit "
         f"more. Same split as always — tell accounting {when()} if your card "
         f"wasn't the one charged so we can true it up."),
        (f"the transfer",
         f"Hi {name} — can you confirm the {a} went out {when()}? The account on "
         f"file changed last quarter and I want to be sure it landed in the right "
         f"one before I close out the month."),
        (f"reconciling",
         f"Going through the books {when()}. I have {a} against the pallet job but "
         f"no matching record on your side — did we ever settle that, or is it "
         f"still open? Reference should be on the slip."),
    ]
    return random.choice(variants)


# ── personal, with a dollar figure ───────────────────────────────────────
def personal(i):
    name = random.choice(FIRST)
    a = amount()
    variants = [
        (f"owe you for gas",
         f"Hey! Can you send me {a} whenever for the gas we split on the drive "
         f"back? Venmo's fine, or just get me {when()} next time you're over. No "
         f"rush at all."),
        (f"hey, free this weekend?",
         f"Hey {name}, free this weekend? Also — still owe you for the concert "
         f"tickets, want to say {a}? Let me know what works, cash or Venmo when I "
         f"see you."),
        (f"this weekend",
         f"Was thinking we could grab dinner this weekend. Oh and I'll bring the "
         f"{a} I owe you from the trip — completely forgot until now. No pressure "
         f"either way, just wanted to catch up."),
        (f"quick one",
         f"{name}! Sorting out the group thing — your share works out to about "
         f"{a}. Pay me back {when()} or just cover the next round, whatever's "
         f"easier. Miss you, let's actually hang soon."),
    ]
    return random.choice(variants)


# ── newsletter, ending in a direct question ──────────────────────────────
def newsletter(i):
    co = random.choice(COMPANIES)
    variants = [
        (f"before Friday's launch",
         f"Quick note from the team at {co} before Friday: we could use your take "
         f"— reply and tell us which tagline you'd run with, A or B. We publish "
         f"the results in next week's issue either way, so no wrong answer."),
        (f"this week at {co}",
         f"This week's digest: three things we shipped and a customer story worth "
         f"reading. One ask before you go — which feature should we cover next "
         f"issue? Just reply with a word. We read every response."),
        (f"a customer story worth reading",
         f"Our weekly from {co}: the usual roundup, plus a short piece on how a "
         f"customer cut their costs. Curious — would a deep-dive version be useful "
         f"to you? Hit reply and let us know, it shapes what we write next."),
        (f"what changed in the release",
         f"Here's what changed this week at {co}. Reading time about four minutes. "
         f"Before you unsubscribe or archive — one question: did the new layout "
         f"work for you on mobile? Reply yes or no, it genuinely helps."),
    ]
    return random.choice(variants)


# ── needs-reply, buried in a status-update rundown ───────────────────────
def needs_reply(i):
    co = random.choice(COMPANIES)
    variants = [
        (f"This week at {co}: two quick things",
         f"Hi — this week's rundown from the studio: the mockups are attached and "
         f"we shipped the new brand kit. One thing before Friday though: I do need "
         f"you to confirm the print run number, otherwise the press slot slips."),
        (f"one thing before I book this",
         f"Before I book the freight slot I need a yes or no on the pallet count — "
         f"24 or 32? The vendor holds the rate until 5pm {when()}, after that it "
         f"resets and we lose the discount, so whenever you get a sec."),
        (f"status + one ask",
         f"Everything's on track at {co} — design signed off, copy is in review. "
         f"The only blocker is you: I need the final headcount {when()} or the "
         f"caterer won't hold our slot. Can you send it over?"),
        (f"weekly update",
         f"Rundown: shipping is on schedule, the invoice cleared, support queue is "
         f"quiet. The one thing I can't move without you — do we go with vendor A "
         f"or B? I need that call {when()} to lock the rate."),
    ]
    return random.choice(variants)


GEN = {"invoice": invoice, "personal": personal,
       "newsletter": newsletter, "needs-reply": needs_reply}

rows = []
for label, gen in GEN.items():
    seen = set()
    n = 0
    i = 0
    while n < PER_LABEL:
        i += 1
        subject, body = gen(i)
        key = subject + body
        if key in seen:
            continue
        seen.add(key)
        rows.append({"label": label, "subject": subject, "text": body,
                     "split": "boundary"})
        n += 1

random.Random(1108).shuffle(rows)
json.dump(rows, open("boundary.json", "w"), indent=1)
import collections
print(f"boundary.json: {len(rows)} emails, "
      f"{dict(collections.Counter(r['label'] for r in rows))}")
