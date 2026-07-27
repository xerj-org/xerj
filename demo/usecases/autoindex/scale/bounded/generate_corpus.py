#!/usr/bin/env python3
"""Generate a deterministic, compact corpus for shipped-path memory diagnostics."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

SCHEMA = "xerj.bounded_corpus.v1"
WORDS = (
    "ledger invoice quarter revenue margin audit payable receivable "
    "forecast covenant depreciation treasury subsidiary"
).split()


def document(number: int) -> dict:
    """Return a deterministic document whose heap shape is stable across runs."""
    words = [WORDS[(number * 7 + offset * 3) % len(WORDS)] for offset in range(48)]
    return {
        "doc_key": f"doc-{number:08d}",
        "company": f"company-{number % 31:02d}",
        "year": 2018 + number % 9,
        "quarter": f"Q{1 + number % 4}",
        "amount": round(((number * 104729) % 10_000_000) / 100.0, 2),
        "body": " ".join(words),
        "labels": [f"bucket-{number % 17:02d}", f"region-{number % 7:02d}"],
    }


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def generate(output: Path, count: int) -> dict:
    if output.exists() and any(output.iterdir()):
        raise ValueError(f"refusing nonempty output directory: {output}")
    output.mkdir(parents=True, exist_ok=True, mode=0o700)
    output.chmod(0o700)
    docs_path = output / "documents.ndjson"
    with docs_path.open("w", encoding="utf-8", newline="\n") as target:
        for number in range(count):
            target.write(json.dumps(document(number), sort_keys=True, separators=(",", ":")))
            target.write("\n")
    manifest = {
        "schema": SCHEMA,
        "generator": Path(__file__).name,
        "documents": count,
        "file": docs_path.name,
        "bytes": docs_path.stat().st_size,
        "sha256": sha256(docs_path),
        "first_id": "doc-00000000",
        "middle_id": f"doc-{count // 2:08d}",
        "last_id": f"doc-{count - 1:08d}",
    }
    (output / "corpus_manifest.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    docs_path.chmod(0o600)
    (output / "corpus_manifest.json").chmod(0o600)
    return manifest


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--documents", type=int, default=4096)
    args = parser.parse_args()
    if args.documents < 1:
        parser.error("--documents must be positive")
    manifest = generate(args.output, args.documents)
    print(json.dumps(manifest, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
