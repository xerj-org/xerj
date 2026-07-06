# Contributing to XERJ

Thanks for your interest in XERJ — an Elasticsearch-wire-compatible search, vector, and log engine written in Rust. Contributions of all kinds are welcome: bug reports, documentation, tests, and code.

By contributing, you agree that your contributions will be licensed under the project's [Apache License 2.0](./LICENSE).

## Prerequisites

- **Rust** (stable). Install via [rustup](https://rustup.rs/). The workspace uses the Rust 2021 edition.
- **Node.js** (v20+) — only needed to run the benchmark tooling under `demo/playbooks`.
- A POSIX-ish environment (Linux/macOS). XERJ builds to a single native binary.

The Cargo workspace lives in the [`engine/`](./engine) directory — run all `cargo` commands from there.

## Building

```bash
cd engine
cargo build --release -p xerj-server      # the server binary
cargo build --release                     # everything
```

Run the server locally (insecure = no TLS, no auth; listens on `http://0.0.0.0:9200`):

```bash
./target/release/xerj --data-dir ./data --insecure
```

## Testing

```bash
cd engine
cargo test                                # unit + integration tests
cargo test -p xerj-engine --test integration
```

### ES-YAML conformance suite (required)

XERJ's compatibility contract is the Elasticsearch REST API YAML test suite (1,329 cases extracted from ES 8.13). **100% pass is the target — there is no "known failures" list.** If a test expects one response and XERJ returns another, XERJ is wrong; fix the engine, not the test.

```bash
cd engine
# Start XERJ in a scratch data dir first:
target/release/xerj --insecure --data-dir /tmp/xerj-test &

# Run the full suite:
cargo run -p es-yaml-runner -- --dir tests/es-compat-yaml/yaml

# Or a single suite / file:
cargo run -p es-yaml-runner -- --dir tests/es-compat-yaml/yaml/search
cargo run -p es-yaml-runner -- --file tests/es-compat-yaml/yaml/bulk/10_basic.yml --verbose
```

Run the full suite before opening a PR. A test that was passing yesterday and fails today is a P0 regression.

## Code style

- Format with `cargo fmt` and keep `cargo clippy` clean (`cargo clippy --all-targets -- -D warnings`). CI enforces both.
- Match the style, naming, and comment density of the surrounding code.
- Comments should explain constraints and non-obvious *why*, not narrate the code.

## Git workflow

Every non-trivial change lands on a task-named branch, gets a commit with a body explaining the motivation (and before/after benchmark numbers for perf work), and is fast-forwarded into `main`. The git history is the project's engineering log.

```bash
git checkout main && git pull
git checkout -b <type>/<short-slug>        # e.g. perf/shard-wal, fix/merge-hang
# ...edit, build, test...
git commit                                 # detailed body: motivation, what changed, trade-offs
git push origin <type>/<short-slug>        # open a PR against main
```

**Commit bodies should include:** motivation, what changed, before/after benchmark numbers (for perf), known trade-offs, and pointers to the files a future contributor needs. Do not force-push to `main`, and don't `git commit --amend` a commit that's already been pushed.

## Pull requests

Before opening a PR, please make sure:

- [ ] `cargo build --release` succeeds.
- [ ] `cargo test`, `cargo fmt --check`, and `cargo clippy -D warnings` pass.
- [ ] The ES-YAML conformance suite still passes (no new failures).
- [ ] New ES-compatible behavior has a matching YAML test (add one under `engine/tests/es-compat-yaml/yaml/` if none exists).
- [ ] Docs are updated if you changed user-facing behavior.

The PR template will prompt you for this checklist.

## Benchmarks

Performance claims must be reproducible. The head-to-head-vs-Elasticsearch harness lives under [`demo/playbooks`](./demo/playbooks):

```bash
node demo/playbooks/bench-matrix.mjs --docs 100k,1m --clients 1,8 --knn --mixed
```

Read latencies are measured with a lean keep-alive HTTP client applied identically to both engines (see the methodology note the harness emits). If your change touches a hot path, include before/after numbers in the commit body.

## Reporting bugs and requesting features

Use the GitHub issue templates:

- **Bug report** — include the version, OS, reproduction steps, and (importantly) whether the behavior is an Elasticsearch wire-compatibility divergence.
- **Feature request** — describe the use case and, where relevant, the corresponding Elasticsearch behavior.

Please do **not** open public issues for security vulnerabilities — see [SECURITY.md](./SECURITY.md) for private disclosure.

## Code of Conduct

This project follows the [Contributor Covenant](./CODE_OF_CONDUCT.md). By participating, you are expected to uphold it.
