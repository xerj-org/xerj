# Data directory startup diagnostics

The server prints `Data dir` (canonical absolute path when available),
`Data source` and `Data status` in the existing startup banner. Structured
tracing also records `data_dir`, `data_dir_source` and `data_dir_state` before
admin-key/TLS state is created. Logging filters still apply to tracing; the
banner remains the existing human-readable startup output.

Precedence is unchanged: `--data-dir` wins over the selected TOML file;
`--config` wins over `XERJ_CONFIG`; no explicit `server.data_dir` uses `./data`
relative to the process working directory. A selected config that omits the
key is identified as a default, not incorrectly attributed to that file.
There is no new `XERJ_DATA_DIR` variable. Relative config-file values remain
relative to the process working directory, not the config file's directory.

`created this launch` means this process successfully created the final directory
with one `create_dir` call. A directory supplied by another process before that
call is `existing directory (may be empty)`. Missing parents are still created.
Existing symlink directories are reused; display resolves their target when
possible. Display-resolution failure falls back to an absolute/requested path
and does not prevent an otherwise valid startup. Control characters are escaped.
If opt-in auto-port selection relocates a locked data directory to an available
sibling, the final path and that relocation are identified together.

A new directory is not evidence that old data was lost. If existing indices or
the owner account were expected, stop this node and use the previous explicit
`--data-dir` or `--config`. No directory is searched, selected, moved or deleted
by these diagnostics. Existing files remain authoritative. There is no record
of previously used directories or account/index inspection to guess a match.
Console empty-state changes and automatic comparison of other known data
directories remain outside this scoped part of #2409.

## Reproduction and verification

The real CLI regression starts private nodes with temporary directories and
tests config/`XERJ_CONFIG`/flag/default selection, omitted config keys, first
creation and an existing-directory restart. An operator sentinel must survive.
It failed on the unchanged base because `Data source` was absent.

```sh
cd engine
cargo +1.97.1 test --locked --offline -p xerj-server \
  --bin xerj --test data_directory_startup -- --test-threads=1
cargo +1.97.1 clippy --locked --offline --no-deps -p xerj-server \
  --bin xerj --test data_directory_startup -- -D warnings
cargo +1.97.1 build --locked --offline --release -j 32 -p xerj-server -p es-yaml-runner
```

The implementation preserves the prior loader's validation through
`Config::from_toml_str` and derives key presence from the same file bytes.
The added direct TOML dependency already exists in the workspace/lockfile; no
dependency version changes. Existing CLI in-process indexing still uses the
same config loader and never opts into directory relocation.

Reference before implementation: existing `Config::load` at baseline
`9cf3f29c`, `engine/crates/xerj-common/src/config.rs:138`, and startup's
`engine/crates/xerj-server/src/main.rs:4341`. Rust's
[`create_dir`](https://doc.rust-lang.org/std/fs/fn.create_dir.html) documents
leaf creation/failure, while [`path::absolute`](https://doc.rust-lang.org/std/path/fn.absolute.html)
distinguishes absolute display paths from symlink resolution. No external
implementation was copied. This is startup observability, not an engine
storage-format or data-recovery change.
