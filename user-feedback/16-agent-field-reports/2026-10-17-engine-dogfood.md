# Dogfood of autoindex and def on sled and the engine tree (2026-10-17)

**Agent:** Cursor Grok 4.7  ·  **XERJ:** xerj v1.0.0-rc.96  ·  **Platform:** Linux x86_64

**Pointed at:** `/tmp/sled` (59 files) and `/workspace/engine` (964 files, 34 MB, `target` gitignored), prefixes `sled` and `eng`.

**Used it for:** `xerj autoindex`, `xerj autoindex map`, and `xerj def`, to see dataset count and definition rank before changing them.

**Verdict:** Autoindex finished and the map was real, but one repo became 161 indices and a map too large to hand an agent. `xerj def Db` ranked a type alias and test files above `pub struct Db`. The phrase "flush the tree to disk" did not land on `Tree::flush`. A mistyped `--prefix` printed "no match" and exited 0. I would autoindex again; I would not trust def rank or an empty search until those are fixed.

**Numbers:** engine before `xerj-done` wall=39.1s files=954 records=57758 datasets=161, map 376110 bytes. After the small-config fold: wall=33.8s records=57270 datasets=9, map 34232 bytes. sled stayed 1 dataset, 832 records (wall 1.2s then 1.1s). `xerj def Db -k 8`: struct moved from 7th to 1st (56.78 vs type aliases 32.78). `xerj def "flush the tree to disk"`: `src/tree.rs` `pub fn flush` was 10th, tied at 29.00.

**Filed alongside:** #1610 #1611 #1614 #1615 #1616 #1617 #1618
