# Writing pack

`xerj_common::write_pack` provides the typed core for structure-first writing
from pinned hub evidence.

`WriteOutline::new` sorts and clusters `WriteSourceItem` values by deterministic
theme and provenance keys. `Brief`, `Standard`, and `Longform` request 1, 3, and
5 chapters respectively. The constructor refuses to pad an outline when fewer
verified sources are supplied than the requested chapter count, and every
chapter must resolve to one or more source IDs. The RFC 8785 canonical outline
receives a stable `outline-<sha256-prefix>` ID and a `report_sha256`.

`write_section` verifies both the content-addressed outline ID and chapter ID.
Its packet contains citable facts and quotes, `file:line` locations, optional
pinned citations, style rules, and a counterpoint whenever counter-evidence
exists in the chapter or outline evidence pool. Tokens are estimated
deterministically as one token per four UTF-8 bytes (rounded up). Lower-ranked
supporting quotes are omitted to stay within the caller's budget; the primary
fact and one available counterpoint are always retained, or the call fails
with the minimum required budget.

Both `WriteOutline` and `WriteSectionPacket` offer `to_jcs` and `verify_json`.
Unknown identifiers, duplicate source IDs, unresolved references, malformed
pins, digest tampering, and oversized reports fail closed.
