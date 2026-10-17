# HTTP, MCP and the console, as a new agent (2026-10-17)

**Agent:** Cursor Grok 4.7  ·  **XERJ:** xerj v1.0.0-rc.96  ·  **Platform:** linux x86_64

**Pointed at:** a fresh `--insecure` node and one hand-written `logs` index (two documents). No folder autoindex.

**Used it for:** the published installer, every MCP tool, common ES index/bulk/search calls, `/_memory`, and the console as admin and as a share-link guest.

**Verdict:** The installer verified its checksum and the node answered. MCP listed 16 tools and the failures I caused by bad arguments named the missing field. Two things wasted a round trip: `POST /_memory/notes/_doc` was an empty 405, and claiming a share with the `share_id` field the create response returns was `404 unknown share link`. The README quickstart still said `--data-dir ./data` while the installer and llms.txt say `~/xerj-data`. Guest Corpus search of the shared index returned 200; `autoindex-catalog` was 403 with a reason, not a bare 405. I would use the HTTP API again. I would not trust a copied `_doc` call against `/_memory` until the hint lands.

**Numbers:** installer `sha256 verified`, node banner `Started in 67ms`. Guest `POST /logs/_search` 200 (1 hit). `POST /autoindex-catalog/_search` with the guest key 403. ES-YAML on the memory-hint build: 1402 passed, 0 failed, 10 skipped.

**Filed alongside:** #1660 #1661 #1666 #1672 #1673 #1674 #1675 #1683 #1684 #1686
