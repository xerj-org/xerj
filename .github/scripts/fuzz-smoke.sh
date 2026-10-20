#!/usr/bin/env bash
# Build and exercise every libFuzzer harness in engine/fuzz.
#
# This is the script CI runs, so a contributor can reproduce the gate exactly:
#
#     rustup toolchain install nightly
#     cargo install cargo-fuzz --locked
#     bash .github/scripts/fuzz-smoke.sh
#
# It does two things, and the second is the one that matters on a pull request:
#
#   1. builds every target with AddressSanitizer + libFuzzer coverage, so a
#      harness cannot silently rot into "does not compile";
#   2. runs every target for FUZZ_SECONDS over its checked-in seeds, which both
#      replays known-interesting inputs as a regression suite and spends the
#      remaining budget on new mutations.
#
# Seeds live in engine/fuzz/seeds/<target>/ and are tracked. The working corpus
# in engine/fuzz/corpus/<target>/ is NOT: libFuzzer writes every input it finds
# interesting there, and committing a few hundred opaque blobs after each run is
# how a seed corpus stops being reviewable. Seeds are copied in before each run;
# promote a discovered input by copying it into seeds/ under a name that says
# what it covers.
#
# A crash, a sanitizer report, an OOM, or a single input slower than
# FUZZ_TIMEOUT fails the job, and the offending input is written to
# engine/fuzz/artifacts/<target>/ for CI to upload.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
FUZZ_DIR="$REPO_ROOT/engine/fuzz"

# Per-target wall-clock budget. Short on a pull request (the point there is the
# seed replay plus a little exploration); override for a real campaign:
#   FUZZ_SECONDS=3600 bash .github/scripts/fuzz-smoke.sh
FUZZ_SECONDS="${FUZZ_SECONDS:-20}"
# A single input allowed to take this long. Anything slower is a
# denial-of-service bug in a parser reachable from an unauthenticated request,
# which is exactly what these harnesses exist to find.
FUZZ_TIMEOUT="${FUZZ_TIMEOUT:-25}"
FUZZ_RSS_MB="${FUZZ_RSS_MB:-4096}"

if ! cargo +nightly fuzz --version >/dev/null 2>&1; then
    echo "fuzz-smoke: needs a nightly toolchain and cargo-fuzz." >&2
    echo "  rustup toolchain install nightly" >&2
    echo "  cargo install cargo-fuzz --locked" >&2
    exit 1
fi

cd "$REPO_ROOT/engine"

# The fuzz crate is its own workspace with no committed lockfile, so every
# run used to resolve its dependency graph fresh from crates.io. When upstream
# yanked every `zip 8.x` release (2026-10-16), that fresh resolve failed with
# "failed to select a version for the requirement `zip = "^8"`" on every
# branch, while the engine itself still built from its locked `zip 8.6.0`.
# Start the fuzz workspace from the engine's lockfile instead: the fuzz build
# then uses exactly the versions the engine ships and tests (a locked yanked
# version stays usable), and cargo only adds the fuzz-only crates on top.
# A contributor's own fuzz/Cargo.lock is left alone.
if [ ! -f fuzz/Cargo.lock ]; then
    cp Cargo.lock fuzz/Cargo.lock
fi

echo "==> building fuzz targets (ASan + libFuzzer)"
cargo +nightly fuzz build

# The target list comes from the crate, never from a list pasted into this
# script: a target that is added and not wired up here would otherwise be
# claimed-but-not-run, which is the whole reason this file exists.
mapfile -t TARGETS < <(cargo +nightly fuzz list)
if [ "${#TARGETS[@]}" -eq 0 ]; then
    echo "fuzz-smoke: no fuzz targets found in engine/fuzz — refusing to pass." >&2
    exit 1
fi

echo "==> ${#TARGETS[@]} targets, ${FUZZ_SECONDS}s each"
for target in "${TARGETS[@]}"; do
    seeds="$FUZZ_DIR/seeds/$target"
    if [ ! -d "$seeds" ] || [ -z "$(ls -A "$seeds" 2>/dev/null)" ]; then
        echo "fuzz-smoke: $target has no seeds at $seeds — refusing to pass." >&2
        echo "  A target with no seeds explores from the empty input and proves" >&2
        echo "  almost nothing; add real inputs under that directory." >&2
        exit 1
    fi

    corpus="$FUZZ_DIR/corpus/$target"
    mkdir -p "$corpus"
    cp -f "$seeds"/* "$corpus"/

    echo "--- $target ($(ls -1 "$seeds" | wc -l) seeds)"
    cargo +nightly fuzz run "$target" -- \
        -max_total_time="$FUZZ_SECONDS" \
        -timeout="$FUZZ_TIMEOUT" \
        -rss_limit_mb="$FUZZ_RSS_MB" \
        -print_final_stats=1
done

echo "==> all ${#TARGETS[@]} fuzz targets survived ${FUZZ_SECONDS}s each"
