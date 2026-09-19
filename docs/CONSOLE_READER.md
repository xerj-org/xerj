# The console: corpus home, reader, and guest mode

`xerj brain <folder>` indexes a folder and opens the bundled console
(`/_xerj-console/`). This page documents the three console surfaces that show
**your own documents** — what was indexed, any one record, and a read-only view
for someone you shared an index with — and the security design behind them,
because a reader for email displays text that strangers wrote.

Source: `xerj-ux/src/` (a buildless SPA, embedded into the binary by
`engine/crates/xerj-console-api/build.rs`). A recorded end-to-end run against a
real node is in [`docs/usecases/console-reader/`](./usecases/console-reader/README.md).

## Corpus home

The console's landing route (an empty hash, or `#/corpus`) answers "what is
indexed on this engine?".

- One card per dataset recorded in the `autoindex-catalog` index — the
  `doc_kind: "dataset"` documents `xerj autoindex` / `xerj brain` write
  (`engine/crates/xerj-autoindex/src/catalog.rs`). A card shows the index name,
  record / file / byte counts, formats, the time span, the `semantic_text`
  field if there is one, the best-covered fields with their types, and the
  sample queries the catalog entry carries.
- **A card describes the last run, not the whole index.** The catalog entry is
  rewritten by each `xerj brain` / `xerj autoindex` run over the dataset. The
  record count is the index's total at the end of that run; the file count,
  bytes, formats, `semantic_text` field and field types are that run's own.
  After a second `xerj brain` over another folder that lands in the same
  index, the card shows the second folder's files and formats beside the
  combined record count. The card labels those facts `last run`. A guest's
  card is counted from the index itself and does not have this limit: records,
  emails (one per message: a `msg-s0` or `m<offset>-msg-s0` record, so an
  email without a `Message-ID` and a long email each count once), attachment
  records and formats.
- A sample query opens the Reader with the sample's **text** in the search box
  and run over the Reader's search fields (below). The field the sample was
  written for is always among them, so the Reader finds what the catalog's own
  request finds; because it searches more fields than the sample did, it can
  find more. It is not a byte-for-byte replay of the catalog's request.
- Any other user index the engine holds is listed by name and document count,
  so an engine filled some other way does not read as empty.
- **Empty engine:** the page shows one command, `xerj brain <folder>`, and
  nothing else.
- **Catalog unreadable:** the page shows the error and says nothing is shown in
  its place.

There is no sample corpus. Corpus, Discover and the Reader are listed in
`NEVER_MOCK` (`xerj-ux/src/data/query.js`): they get the engine's answer or an
explicit error. The console's in-memory mock data still backs the telemetry
dashboards that have no live source, and those are labelled `SAMPLE DATA`.

## Reader

Route: `#/reader?index=<index>&id=<id>[&brain=<brain>]`. A search box and
result list on the left, the open record on the right, and the records the
brain links it to underneath. Every hit in Discover links here by id.

A record is rendered by **shape**, detected from its own fields (the autoindex
extractors are the contract — `engine/crates/xerj-autoindex/src/extract/`):

| Shape | Detected by | Shown |
| --- | --- | --- |
| email | `email_subject` / `email_from` / `email_message_id` | headers, the text body, the attachment list |
| attachment | `attachment_name` | name, type, size, the extracted text (per page for a PDF), a link back to the parent email |
| pdf | `page` + `body` | title, page number, that page's extracted text |
| code symbol / file | `name` + `language` + `code`, or `language` + `defs` | signature, file and line, the code |
| note | `body` or `text` | title and text — markdown, docx, html, plain text |
| file | `ax_locator: "file"` and no text | the one record autoindex writes per *file*: path, format, and the records that came out of it (the message first, then pages in order) |
| anything else | — | the `_source` as JSON; vector and passage plumbing fields are counted, not dumped |

An email's attachments and an attachment's parent are joined on **`ax_file`**,
the id autoindex stamps on every record that came out of one file, narrowed to
the record's **message** by its `ax_locator`. The attachment list is the file's
records whose locator starts with the message part followed by `att`; an
attachment's parent is the file's record whose locator starts with the message
part followed by `msg-`, its first section. For an `.eml` the message part is
empty (one file, one message: `msg-s0`, `att0-p3-s0`). A locator can also carry
a message part `m<offset>-` (`m812-msg-s0`, `m812-att0-p3-s0`), which is the
shape of the mailbox (mbox) ingest in
[#949](https://github.com/xerj-org/xerj/pull/949): one file holds many
messages, and joined on the file alone one email would list the attachments of
every message in the mailbox. A record whose locator has neither shape joins on
`ax_file` alone.

The join does not use `email_message_id`. The extractor stamps that field only
when the message has a `Message-ID` header, and two files can carry the same id
(a copy in `inbox/` and one in `archive/`). Joined on it, the first email
showed no attachments at all and the second pair pooled each other's.
`email_message_id` is the fallback only for records that carry no `ax_file`
(written by something other than autoindex).

A PDF attachment contributes one record per page section. The attachment list
shows each attachment once, at its lowest page, in the order the email carries
them. Attachments are told apart by the ordinal the extractor puts in the
record's locator (`att0-p3-s0`, `att1-s0`), not by file name, so two attachments
both named `scan.pdf` are two entries. To build the list the Reader reads the
email's attachment records a page of 1,000 at a time — seven small fields each,
never the page text — up to 5,000 records. If an email holds more, the list is
headed `ATTACHMENTS · N+` and says how many records exist and how many were
read. If the join fails, the email says its attachments could not be read; it
does not show an empty list. Every record also links up to its file's record
(`FROM FILE`), joined on `ax_file`; a file record lists the first 200 of its
records and says `200 OF 303 SHOWN` when there are more.

#### What the search box searches

`MATCH`, `PHRASE` and `PREFIX` (and the lexical leg of `HYBRID`) run over
**every text-typed field in the mapping** — `body` first, then `message`,
`content`, `text`, then the rest in mapping order, at most 12 — plus
`email_subject`, `title` and `attachment_name` when the mapping has them. One
index can hold its text under more than one name: autoindex's prose extractors
write `body`, its line extractor (Makefile, `.ini`, logs) writes `text`.

autoindex usually types an email's subject, a title and an attachment's file
name as **`keyword`**, not `text`: every attachment page record copies its
parent's subject, the field's cardinality ratio collapses, and the type
inferrer picks `keyword`. On a keyword field the engine's `match` needs the
whole value — measured on a live node, `{"match":{"email_subject":"Lunch"}}`
returns 0 hits with the subject `Lunch on Friday?` indexed
([`review-repro.json`](./usecases/console-reader/review-repro/review-repro.json),
recorded 2026-09-19). So for a
keyword-typed search field the Reader sends a case-insensitive `wildcard`
instead: `MATCH` is one `*word*` clause per word, OR-ed; `PHRASE` is
`*the words as typed*`; `PREFIX` is `typed*`. That is a **substring** test, not
token matching: `on` also finds `Duplication`. `*`, `?` and `\` typed into the
box are matched as "any one character", never as a pattern. The subject clause
matches the message's own records only; an attachment record carries a copy of
its parent's subject and is found by its own name and text instead.

`TERM` needs `field=value` and `RANGE` needs `field>=value` (or `>`, `<=`,
`<`). Anything else is not sent: the list says `NOT SEARCHED` and shows the
syntax. The list shows 25 results; `SHOW 25 MORE` extends it up to 200, and past
that it says so.

What the Reader does **not** do: it does not render a PDF's pages or an
email's HTML. It shows the text the extractor indexed. The engine stores
extracted text, not attachment bytes.

### The graph panel

The panel is a `GET /_graph/{brain}/ego?node=<id>&hops=1&direction=both` call
([SECOND_BRAIN.md](./SECOND_BRAIN.md#get-_graphbrainego)). Neighbours are
grouped by edge type, each one a link that opens in the same Reader.

autoindex's file-level detectors (`same_dir`, `mdlink`, `pathcite`, `href`)
link **file records**, not the records inside a file — in the recorded run the
email message record has no edge of its own, while its file has two. So for a
record that is not itself a file record the panel makes a second `ego` call for
the record's file and merges the two; links that came through the file are
marked `FILE`, and the panel's last line says how many did and why. A brain
that is missing or refused is asked once, not twice.

The panel states, in words, which of these is true: no brain for this index, a
brain with no links for this record, links clipped by the limit, links to ids
with no document behind them, or a refusal.

**On an auth-enabled engine (the default) the graph panel is refused for a
signed-in operator.** The console holds a passkey session, not an engine API
key, and the console's search proxy deliberately cannot read the reserved
`.xerj-memory-*` namespace
([SECURITY_MODEL.md](./SECURITY_MODEL.md)). Search, records and attachments go
through the session-authenticated proxy and work; the graph panel says "the
graph API refused this console session (HTTP 401)" rather than claiming there
are no links. On `--insecure` it works. A guest's graph panel works on an
auth-enabled engine, because a guest *has* a key (below). Tracked as
[#936](https://github.com/xerj-org/xerj/issues/936).

### Discover

Discover derives its index list and its query fields from the engine — through
the console session API (`…/data-sources/connections/built-in/indices` and
`…/indices/{index}/fields`), not `GET /_mapping`, which an auth-enabled engine
refuses a console session. The REQUEST panel shows the body that was sent; the
preview and the executed body are built by one function
(`xerj-ux/src/data/search-body.js`). `*` runs against one index at a time — the
search proxy takes a single exact index name — and the results header names
which. While a search is in flight the table says so; when one fails it shows
the error and zero rows.

`SEMANTIC` and `HYBRID` use the index's `semantic_text` field. With the default
embedder that is lexical feature hashing, not a neural model. Two things the
console does because of how the engine treats `hybrid`, both measured on a live
node: a HYBRID search carries no aggregations (the engine answers `400` to
`aggs` beside a fusion query), so Discover shows no facets or histogram under
HYBRID and says why; and a facet filter is put inside each leg of the hybrid,
because `bool{must: hybrid, filter}` returns 0 hits and `post_filter` is
ignored, both without an error
([#943](https://github.com/xerj-org/xerj/issues/943)).

## Guest mode

A share link's guest page (built separately; `POST /_share/{id}/claim`) hands
the browser to the console by writing **one** `sessionStorage` record:

```js
sessionStorage['xerj.share'] = JSON.stringify({
  api_key:    '<base64 id:secret — a read-only key minted for this claim>',
  index:      'ax-inbox',              // or 'a,b' — comma-joined
  brain:      'inbox',                 // or null: no graph in this share
  expires_at: '2026-09-25T18:00:00Z',  // RFC 3339
  label:      'Q3 board pack',         // or null
})
```

When that record is present, `xerj-ux/src/boot.js` starts the guest shell
(`guest-app.js`) **instead of** the operator console — before any console API
call, including the `/me` auth probe.

- **What a guest sees:** a banner — `Guest · read-only · <label> · expires
  <UTC time> (<time left>)` — and two views, CORPUS (what the shared indices
  hold, counted from the indices themselves) and READER. No dashboards, no
  Discover, Data, Users, Alerts or Settings, no edit mode. Operator routes typed
  into the address bar land on the guest's corpus view.
- **What a guest's tab can request:** `POST /{shared}/_search`,
  `POST /{shared}/_count`, `GET /{shared}/_mapping`,
  `GET /_graph/{shared brain}/ego`. URLs are built from the share's own
  validated names; there is no function in guest mode that takes a path. A
  fetch guard additionally refuses every other request — other indices,
  `/_xerj-console/api/…`, `/_cat`, `/_cluster`, any other origin — before it
  reaches the network. The key is attached per request, never with a cookie.
- **It fails closed.** A record with no key, no index, a pattern / `_all` /
  dot-prefixed index, a malformed brain, or a missing, unparseable or past
  `expires_at` is not a share: the record is cleared and the tab shows "this
  share link is not valid". No request is made with it.
- **Refusals do not leak.** A `403` is shown as "not permitted"; the engine's
  error body — which names resources and the grant that would fix it — is never
  read into the page. A refused graph shows "This share does not include the
  knowledge graph."
- **How it ends:** `expires_at` passing (checked on a timer, on every request,
  and when the tab becomes visible again), a `401` (the key was revoked), or
  LEAVE. Each removes the key from `sessionStorage`, removes the documents from
  the page, and stops all requests.

The UI is not the security boundary — the key is. A guest key is `read` on the
shared indices and nothing else, so the engine refuses anything the UI would
have refused. The guard exists so the UI never even asks, and so a regression
shows up as a failing test instead of as a `403` in someone's audit log.

## Security design: documents are attacker-controlled

Anyone can mail you `<img src=x onerror=…>` as a subject, name an attachment
`"><script>…`, or put a `javascript:` URL in a body. In guest mode the viewer's
API key sits in `sessionStorage` on the same origin, so one script execution is
key theft. The design removes the HTML parser from the path instead of relying
on escaping at every interpolation:

1. **No markup from data.** The corpus home, the reader and the whole guest
   shell render through `xerj-ux/src/ux/safe-dom.js`: `createElement`,
   `createTextNode`, `setAttribute`, `replaceChildren` — nothing else. A
   document string can only become a text node or an inert attribute value.
   Tags and attribute names come from closed allow-lists with no `script`,
   `img`, `svg`, `iframe`, `style`, `form`, no `src` / `style` / `on*`; an
   unknown one throws.
2. **No links out.** The only `href` that survives is an in-app hash route
   (`#/…`). A URL in a document is displayed as text. `javascript:`, `data:`,
   `vbscript:`, protocol-relative and absolute URLs are dropped.
3. **Email HTML is never rendered.** An HTML body that reached the index is
   shown as its source.
4. **Highlights without `<em>`.** The engine splices highlight tags into raw,
   unescaped document text. The reader asks for two private-use code points
   (U+E000 / U+E001) as delimiters, splits on them, and emits text and `<mark>`
   elements. A document that forges the delimiters gets at worst a stray
   `<mark>`.
5. **A Content-Security-Policy on the console page**
   (`engine/crates/xerj-console-api/src/spa.rs`, `CONSOLE_CSP`):
   `script-src 'self'` (no inline script, no `eval`), `connect-src 'self'`,
   `object-src 'none'`, `base-uri 'none'`, `frame-ancestors 'none'`. The page
   has no inline script. This is the second wall: a missed escape in some other
   view cannot run script or send a key to another origin.
   `style-src` allows `'unsafe-inline'` because the operator shell sets `style`
   attributes. `login` and `setup` carry an inline module script, show no
   document data, and are served without the policy.
6. **The rest of the operator console** (Discover's table, facets, charts)
   still builds HTML strings and escapes at each interpolation. It is covered
   by the same hostile-fixture browser test and by the policy, not by
   construction.

### Tests

```sh
node --test xerj-ux/test/*.test.mjs           # pure: render trees, guest contract, import graph
node --test xerj-ux/test/browser/*.test.mjs   # real headless Chrome (Node >= 22, no npm install)
```

The browser suite drives Chrome over the DevTools protocol against a fake node
that serves the real SPA under the policy string parsed out of `spa.rs`, with
hostile documents in every position an outsider controls: subject, sender,
recipients, dates, message ids, attachment filenames, bodies, highlight
fragments, graph node titles and edge types, catalog sample queries, field
names, record ids. A control page first proves the same fixtures *do* execute
through `innerHTML` and that the harness sees the key leave for another origin.
The suite then asserts no payload ran, no dialog opened, no request left the
origin, the policy reported no violation, and the document-derived DOM holds no
forbidden element, no `on*` / `src` / `style` attribute and no `href` other
than `#/…`. The guest suite asserts the request log: only the four operations,
never a console endpoint, the key only on those requests, the operator bundle
never downloaded.

Set `XERJ_REQUIRE_BROWSER=1` to make a missing Chrome a failure instead of a
skip (CI does).

## What is not covered here, and known limits

- The share link itself — minting, passcodes, claim limits, revocation — is a
  separate surface; this page covers only what the console does with the
  record it is handed.
- The operator's graph panel needs `--insecure` today (above,
  [#936](https://github.com/xerj-org/xerj/issues/936)).
- The console's passkey sign-in only works when the console is reached at
  `http://localhost:9200`: the WebAuthn relying-party origin is fixed
  (`xerj-console-api/src/state.rs`, `RpConfig::default`), so a node on another
  port refuses enrolment
  ([#935](https://github.com/xerj-org/xerj/issues/935)). That predates this
  work and affects the whole operator console, not only these views; guest mode
  does not sign in and is unaffected.
- `xerj brain` still opens the Second Brain dashboard
  (`#/second-brain?brain=<name>`); the Corpus home is where a bare
  `/_xerj-console/` lands.
- `*` in Discover searches one index at a time.
- An operator's Corpus card describes the last `xerj brain` / `xerj autoindex`
  run over the dataset, not the whole index (above).
- Subject, title and file-name search is a substring test when autoindex typed
  the field `keyword`, which it usually does (above). A text + keyword
  multi-field for those fields would make it token search; that is an autoindex
  change and is not part of this work.
- The Reader's result list stops at 200. An email's attachment list reads at
  most 5,000 attachment records and says so when there are more.
- A reader deep link opened without a session returns to the same record after
  sign-in (the route is kept in `sessionStorage` across the `/login` redirect;
  only an in-console `#/…` route is ever kept). First-boot `/setup` links use
  their own `&next=`.
- After a share ends in a tab — expiry, revocation or LEAVE — that tab keeps
  showing the ended screen on reload, through a `sessionStorage` marker that
  holds the reason and nothing else. The screen has an `OPERATOR SIGN-IN`
  button that clears it.
- PDF pages and email HTML are shown as extracted text, never rendered.
- The console loads its fonts from Google Fonts, in guest mode too. The page is
  served with `Referrer-Policy: no-referrer`; an air-gapped deployment sees
  fallback fonts.
- A browser extension, or anyone with access to the guest's unlocked browser
  profile, can read `sessionStorage`. The key's lifetime and its read-only,
  index-scoped grant are what bound that.
