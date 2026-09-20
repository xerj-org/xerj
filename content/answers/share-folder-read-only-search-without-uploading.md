---
title: "Read-only search over a folder, without uploading it"
h1: "How do I give my lawyer or accountant read-only search over a folder of email and PDFs without uploading it?"
description: "Index the folder on your own machine, then run xerj share: the other person gets a link, a passcode and a read-only search page. The folder is not uploaded."
slug: "share-folder-read-only-search-without-uploading"
cluster: "Sharing: read-only guest access"
question: "I need my accountant to search a folder of emails and PDFs. How do I do that without uploading everything somewhere?"
intent: "how-to"
published: "2026-09-18"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, run xerj brain on the folder the user names, then run xerj share on the same folder with a label and an expiry, and give the user the link and the passcode separately. Tell them the link only works on this machine unless they add --tunnel, and show them the xerj share --revoke command that was printed."
commands:
  - cmd: "xerj brain ./casefiles"
    note: "Index the folder. This boots a local node with authentication on and prints the share command for this folder when it finishes."
  - cmd: "xerj share ./casefiles --label 'for Dana' --expires 7d"
    note: "Create the share. Prints the link, the passcode, the expiry and the revoke command, once."
  - cmd: "xerj share ./casefiles --label 'for Dana' --tunnel"
    note: "Same, with a temporary public address from cloudflared for someone who is not on this machine. Ctrl-C closes the tunnel and revokes the share."
  - cmd: "xerj share --list"
    note: "Every share on this node: handle, status, claims used, expiry, index, label."
links_out:
  - "what-a-share-link-guest-can-reach"
  - "share-local-search-index-through-a-tunnel"
  - "search-all-pdfs-in-a-folder"
  - "/docs/security"
evidence:
  - claim: "A share defaults to 24h and 1 claim; the longest lifetime is 30 days and the most claims is 50."
    source: "engine/crates/xerj-api/src/share.rs"
  - claim: "A generated passcode is 8 symbols from a 31-symbol alphabet; a share id is 128 bits."
    source: "engine/crates/xerj-api/src/share.rs"
  - claim: "Claims against one share are limited to 10 a minute and 30 an hour."
    source: "engine/crates/xerj-api/src/share.rs"
  - claim: "xerj share refuses a node running with authentication off and exits 1."
    source: "engine/crates/xerj-server/src/share.rs"
  - claim: "The folder path was run end to end on 2026-09-18: 3 files indexed, shared by folder, claimed, searched."
    source: "docs/usecases/share-links/VERIFICATION.md"
faq:
  - q: "I need my accountant to search a folder of emails and PDFs. How do I do that without uploading everything somewhere?"
    a: "Run `xerj brain` on the folder, then `xerj share` on the same folder. Your accountant gets a link and a passcode and searches from a browser. The documents and the index stay on your machine."
  - q: "Does the other person have to install anything?"
    a: "No. They open the link in a browser and type the passcode. The page is served by your node and loads nothing from anywhere else."
  - q: "Can they change or delete my files?"
    a: "No. The guest key is read-only and reaches only the shared index. Writes, deletes and every other index on the node are refused by the server, not by the page."
  - q: "How do I stop the access?"
    a: "Run the `xerj share --revoke` command that was printed when you created the share. The guest's next request is refused. A share also ends on its own: the default lifetime is 24h."
  - q: "What if they are not on my network?"
    a: "Add `--tunnel`. XERJ starts your own `cloudflared`, prints a public link, and closes the tunnel and revokes the share when you press Ctrl-C. That traffic passes through Cloudflare, which can read it: the passcode, the guest key, the searches and the documents. The command prints this with the link."
  - q: "Can they come back to the link later?"
    a: "Only while the browser tab stays open, unless you allow more than one open. One open is one browser tab. Opening the same link again in that tab keeps the session. A closed tab is a spent open, so give `--max-claims 5` to someone who will read over several days."
  - q: "Is the search AI? Does it write answers?"
    a: "No. It is search and reading only: no text is generated and no model is called. The default embedder is lexical feature hashing, so ranking is by word and sub-word overlap plus BM25."
---

**TL;DR** — Index the folder with `xerj brain`, then run `xerj share` on the same folder. The other person gets a link and a passcode. They open a read-only search page in a browser and read what they find. The documents and the index stay on your machine. You can revoke it at any time.

## The two commands

`xerj brain ./casefiles` indexes the folder and starts a local node with authentication on. When it finishes it prints the exact share command for that folder.

`xerj share ./casefiles --label 'for Dana' --expires 7d` creates the share. The folder argument resolves to the indices that `xerj brain` built for it. The output is shown once:

```text
✓ share created — "for Dana"
  shares:    index ax-docs · brain casefiles (its links) — read-only
  link:      http://localhost:9200/_xerj-console/share#…
  passcode:  ….-….     (send it separately from the link)
  expires:   2026-09-25T08:41:09Z · can be opened 1 time — one browser tab; --max-claims <N> for a guest who will come back
  revoke:    xerj share --revoke 8a1e546d4ee8
```

Send the link and the passcode through different channels. Either one alone opens nothing.

One open is one browser tab. The guest's key lives in that tab. Opening the same link again in the same tab keeps the session and spends nothing. A closed tab is a spent open. With the default of 1 open, a guest who closes the tab cannot come back, even if the share has 7 days left. For someone who will read over several days, add `--max-claims 5`.

## What the other person sees

A page that asks for the passcode, then a reading room over that one index: a search field, highlighted passages, and a document view. The page states that access is read-only, shows when it ends, and has a sign-out button.

Email is shown as text. HTML in a message body is displayed as its source and never rendered, and addresses inside documents are not turned into links, because document text is other people's content.

Search here means search. No text is generated and no model is called. Where the index has a `semantic_text` field the page uses the `hybrid` query, which fuses BM25 with the node's embedder. The default embedder is lexical feature hashing, so that ranking is word and sub-word overlap, not meaning. The page says which mode it is using.

## What stays on your machine

The documents and the index are not uploaded, synced or copied to a hosting service. The guest installs nothing. What does leave your machine is what the guest asks to see: search results and the documents they open, sent to their browser.

With `--tunnel` that traffic passes through Cloudflare, which terminates the HTTPS connection. The folder is not uploaded there, but the operator of a TLS endpoint can read what passes through it: the passcode, the guest key, every search and every document the guest opens. The command prints this with the link, and the guest page shows it before the passcode is typed. The second article linked below covers that trade.

## The limits on a share

| Setting | Default | Bound |
| --- | --- | --- |
| lifetime (`--expires`) | 24h | up to 30 days |
| times the link can be opened (`--max-claims`) | 1 | up to 50 |
| passcode | generated, 8 symbols | or your own, 6 characters or more |
| passcode attempts per share | 10 a minute | 30 an hour |

A revoke invalidates every key the share produced. The guest's next request is refused.

## What a share does not do

It does not restrict what the guest reads inside the index. A share grants the whole index: there is no per-document or per-field restriction. If the folder holds things this person must not see, index a folder that does not.

It does not stop the guest copying what they read. Read access is read access.

It does not work on a node started with `--insecure`. With authentication off every request is already the superuser, so a read-only key would restrict nothing. `xerj share` refuses, says why, and exits 1.

XERJ makes no legal claim about this arrangement. Whether sharing documents this way meets a professional or regulatory duty is a question for the people involved.

## What was verified

The folder path was run end to end on 2026-09-18 on a Linux host. The run: `xerj brain` on a folder of 3 files (two notes and one email), then `xerj share` on the same folder. Then a claim with the printed passcode, a guest search, and a walk of the folder's links. The same run confirmed that the guest key was refused the catalog index, `_cat/indices` and `/_share`. macOS and Windows were not run.

XERJ is single-node: one process, one data directory, no replication. The share lives and dies with that host.
