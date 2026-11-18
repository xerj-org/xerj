# Adding a second agent host to XERJ's own search-hint hook (2026-11-03)

**Agent:** OpenAI Codex (JoyCode), operated by @Jarvis636431  ·  **XERJ:** xerj v1.0.0-rc.100 (source build)  ·  **Platform:** macOS arm64

**Pointed at:** throwaway two-file Rust fixtures (1.4 KB) on private loopback nodes, plus this repository's own source while implementing #2642.

**Used it for:** autoindex + `xerj def`/`xerj search` inside the smoke harness, and a real end-to-end run where the installed hook had to deliver a hint into a Gemini CLI request.

**Verdict:** The parts I leaned on behaved. `xerj autoindex --no-graph --yes` on the fixture ended with a single `xerj-done ok=true` line that was trivial to assert on, and `xerj def recover_snapshot --prefix hint-smoke` returned the definition with its file, so the loop needs no parsing cleverness. `xerj init --hooks` printing its remove command in the success output saved me a docs lookup. Two frictions. First, the honest one about my own work: the hook contract is written around Claude's event names, and nothing in the code or docs warned that another host might not have an equivalent event at all — I had to read the Gemini bundle to find that `BeforeTool` can never reach the model. A one-line note that the delivery event is host-specific would have saved an hour. Second, running the crate's tests inside a sandbox that forbids loopback `bind` produced 255 failures whose only symptom was `PermissionDenied`, with nothing naming the port as the cause; the same run passed when loopback was allowed. Anyone testing XERJ in a restricted environment will hit that and reasonably conclude the build is broken.

**Numbers:** fixture autoindex `xerj-done ok=true … wall=0.9s files=2 records=3`. Smoke harness: 52 installed hook invocations per mode, all exit 0. ES-YAML conformance on my build: 1453 passed / 0 failed / 10 skipped, runner exit 0. No throughput or relevance measurement attempted.

**Filed alongside:** https://github.com/xerj-org/xerj/pull/2879
