# Reading an indexed folder of email and PDFs — a recorded live run

This is the end-to-end check behind [`docs/CONSOLE_READER.md`](../../CONSOLE_READER.md):
the real `xerj` binary with **auth on** (the default), a real folder indexed by
`xerj brain`, and a real headless Chrome driving the console — once as a
signed-in operator, once as a share-link guest holding a read-only key.

```sh
./run.sh /path/to/xerj [port]     # default 9550 (+1 REST, +2 gRPC)
```

Needs Node >= 22, Chrome (or `CHROME_BIN`) and `python3`. Everything it writes
goes under a temp dir (`$WORK`); the node it boots is stopped at the end.
[`live-e2e.json`](./live-e2e.json) is the output of the run recorded for this
change; the numbers quoted in the docs and the article come from it.

## The folder

`mkcorpus.py` writes 10 files: six `.eml` messages (two with a PDF attached —
this repository's own briefs from `landing/resources/`), two standalone PDFs and
two markdown notes that link to each other and to one of the PDFs.

One message is hostile in every header an outsider controls: a `<script>` and an
`<img onerror>` in the subject, an `<svg onload>` in the sender's display name,
an HTML body with `<body onload>`, `<script>` and a `javascript:` iframe, and a
PDF attachment **named** `"><img src=x onerror=…>.pdf`. Each payload, if it ran,
would set a flag and send `sessionStorage['xerj.share']` to another origin.

## What the run does and asserts

1. `xerj brain <folder> --url http://localhost:<port> --data-dir … --no-open`
   boots the node, indexes, detects links, and prints the one-time passkey
   setup link.
2. **Operator.** Chrome enrols a passkey with a DevTools virtual authenticator,
   then opens Corpus, the Reader on the hostile email (and searches it, so the
   engine's real highlight fragments come back), and Discover.
3. **Guest.** The script mints an API key with `read` on the email index and on
   the brain's edges index, asks the engine what that key can and cannot do,
   writes `{api_key, index, brain, expires_at, label}` to `sessionStorage`, and
   opens the console: Reader on the hostile email, a click on the
   hostile-named attachment, then Corpus.
4. In both: no payload executed, no dialog opened, no request left the origin,
   the Content-Security-Policy reported no violation, and the document-derived
   DOM contains no forbidden element, no `on*` / `src` / `style` attribute and
   no `href` other than an in-app `#/` route. For the guest, additionally: the
   tab made no request to any `/_xerj-console/api/…` endpoint.

## Why the operator half maps `localhost:9200`

The console's WebAuthn relying-party origin is fixed at
`http://localhost:9200`, so a node on any other port refuses passkey enrolment
([#935](https://github.com/xerj-org/xerj/issues/935)). To run the operator half
at all on a test port, that half uses a second Chrome started with
`--host-resolver-rules=MAP localhost:9200 127.0.0.1:<port>`. It first fetches a
file only this branch's bundle serves and stops if the answer is not ours, so
nothing is ever sent to a node that really listens on `:9200`. The guest half
needs no sign-in and uses the real origin.

## What the recorded run shows

See [`live-e2e.json`](./live-e2e.json) for the full output (2026-09-18, the
`feat/console-corpus-reader` branch, Chrome 152); screenshots of the same run
are in [`shots/`](./shots/). In short:

- `xerj brain` wrote one dataset, `ax-docs` — 81 records from the 10 files —
  and a brain, `casefile`, with 33 links. The hostile message's file alone
  produced a file record, a message record and one record per section of its
  attached PDF's pages.
- The operator's Corpus home shows that dataset's card from the catalog; the
  Reader shows the hostile email as text with its attachment listed once;
  Discover's index list, facets and date histogram come from the index's own
  fields. The operator's graph panel reports the refusal
  ([#936](https://github.com/xerj-org/xerj/issues/936)) instead of "no links".
- The guest key gets `200` on its own index's `_search` / `_mapping` and on the
  brain's `ego`, and `403` on another index, on `autoindex-catalog` and on a
  write; the console API answers it `401`.
- The guest's tab requested only `_search`, `_mapping` and `ego` (plus the
  browser's own `/favicon.ico` for the seed page, without a key), and made 0
  requests to the console API. Its graph panel shows 2 linked records for the
  hostile email — both `same_dir` links of the email's *file* (`04.eml`,
  `06.eml`); the message record itself has none.
- The guest's Corpus view counts the shared index from the index itself: 81
  records, 6 emails, 43 attachment records.

![The guest reader showing the hostile email as text](./shots/live-guest-reader.png)
