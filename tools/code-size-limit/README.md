# Source-code size skips

Source files handled by the tree-sitter extractor, including Vue/Svelte components, have
a fixed **2 MiB (2,097,152 byte)** input cap. A file exactly at the cap can be
parsed; one byte over is skipped. This is independent of the whole-file flags
`--max-file-gb`, `--max-file-mb` and `--max-file-bytes`. Setting
`--max-file-mb 6` does not make a source file above 2 MiB eligible for AST
extraction. File size alone does not establish whether source is generated.

The skip reason names both the exact limit and the reported file size, says
that no records were extracted and explains the independent limit. For example,
the synthetic boundary case reports **2,097,153 bytes** against **2,097,152**;
rounding both to `2.0 MiB` no longer hides the difference. The catalog retains
`status: skipped`, `records: 0` and the source byte count. Other files can
continue. Exit 3 is completed-with-junk, not a runtime error, but that success
does not make the skipped file searchable.

This is the diagnostic follow-up to merged
[#2666](https://github.com/xerj-org/xerj/pull/2666), which introduced explicit
size refusal reasons. It addresses the misleading reason/help aspects of
[#2745](https://github.com/xerj-org/xerj/issues/2745). It does **not** raise or
make the cap configurable, chunk large code files, or change parser behavior.
Those remain separate work under the issue. SQL source, notebook cells and
PDFs have their own extraction paths; they are not changed.
The indexed-document size limit and semantic mapping behavior are also unchanged.

To exclude an unwanted large file from future plans, add its path to the
project's `.xerjignore`; this excludes content, not indexes it. An existing
committed plan/catalog may retain the diagnostic recorded by its older build.
No migration of stored reasons is added. Use a new prefix/state
directory if you need an isolated fresh-plan check; keep old data until the
replacement has been verified, and use normal ownership-aware removal afterward.

## Reproduce with synthetic Perl

From the repository root, using a built binary and Python 3:

```sh
python3 tools/code-size-limit/smoke.py --binary engine/target/release/xerj --estimate-only
python3 tools/code-size-limit/smoke.py --binary engine/target/release/xerj
```

The script creates three ASCII Perl fixtures at cap minus one, cap and cap
plus one byte, totaling 6 MiB, outside any existing dataset. It snapshots the
executable, uses a private temporary node/data/state directory and never calls
an existing node. Each invocation dry-runs first. The full invocation checks
the catalog, `def` retrieval, completed-with-junk exit, unchanged rerun and
graceful restart. Its node is lexical with `--no-semantic` indexing. It sends
no transcript or real repository content and loads no model. It stops its own
node and removes its own temporary files when done.

Reference retrieved before writing this follow-up: implementation and rationale
at `af721040` (#2666), `engine/crates/xerj-autoindex/src/extract/code.rs:33–48`
and `384–389`; the bounded reader at baseline `e5675dec`,
`engine/crates/xerj-autoindex/src/extract/mod.rs:163–171`; and refusal-to-catalog
classification in `engine/crates/xerj-autoindex/src/lib.rs:4293–4312`.
No external implementation was copied, no cap algorithm was replaced and no
performance improvement is claimed. See [verification receipts](VERIFICATION.md).
