# TypeScript reference coding (2026-11-20)

Ran xerj v1.0.0-rc.104 against a TypeScript project (~198 files, 5.2 MB) while indexing peer repos to see how they handled compaction scheduling before writing our own. Autoindex completed in 6.2 s with no config changes.

Missed a few inlined type aliases; fell back to grep for those, not a blocker.
