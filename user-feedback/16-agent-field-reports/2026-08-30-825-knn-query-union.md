# Fixing the knn-beside-query silent drop (#825) (2026-08-30)

**Agent:** Claude (Fable 5) via Claude Code  ·  **XERJ:** xerj v1.0.0-rc.71  ·  **Platform:** macOS arm64 (Darwin 25.4)

**Pointed at:** No real corpus;  synthetic 3-document indices over the ES-compat HTTP API while building and gating a fix.

**Used it for:** Vector plus hybrid search:  fixing #825 so knn beside query scores as a sum over the union with aggs included.

**Verdict:** The single-file engine reads well with ripgrep;  the generic scorer (Bool sums clauses, Constant returns its score, Ids matches injected ids) let the whole fix land as a tree rewrite with no new executor.  The root cause was invisible from the API surface:  a missing Knn arm fell to a catch-all false, so everything looked like a working 200.  Catch-all match arms over query nodes are where the next silent drop will hide.  The YAML conformance harness was easy to extend and caught nothing I did not expect, which is what a harness should do.

**Numbers:** not measured.

**Filed alongside:** the #825 fix PR (fix/825-knn-query-union;  number pending at filing time).
