# `xerj trace` fixture

`synthetic-session.jsonl` is a made-up agent session for `xerj trace` (#1789).
No real transcript went into it. The corpus (`demo-codec`), its pin, the file
`src/frame.rs` and every question are invented.

`synthetic_session()` in `src/trace.rs` generates it, and the tests fail if the
file drifts from that output. To regenerate it:

```sh
XERJ_REGEN_TRACE_FIXTURE=1 cargo test --profile ci-test -p xerj-hub --lib trace:: -- --test-threads=1
```

It has five tool calls:

1. `xerj_hub_def` with an evidence receipt that has no pinned bytes. This is the receipt packs return today.
2. `xerj_hub_ask` with a receipt that pins two byte-exact citations.
3. `xerj ask --json` run through a shell tool. Its one hit names the wrong line, which `--repo` catches.
4. `xerj_hub_ask` with a receipt that was edited after it was issued, so its digest no longer matches.
5. A shell call with no hub answer in its result.

After the five calls come one line that is cut short and a closing assistant message.
