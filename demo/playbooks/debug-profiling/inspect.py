#!/usr/bin/env python3
"""Validate a XERJ profiling bundle and print an agent-readable summary."""

import argparse
import gzip
import hashlib
import json
import pathlib
import sys


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser(
        description="Verify profile hashes/formats and summarize evidence gaps."
    )
    parser.add_argument("bundle", type=pathlib.Path)
    args = parser.parse_args()
    bundle = args.bundle.resolve()
    manifest = json.loads((bundle / "manifest.json").read_text())
    errors = []
    for name, metadata in manifest.get("artifacts", {}).items():
        path = bundle / name
        if not path.is_file():
            errors.append(f"missing artifact: {name}")
        elif sha256(path) != metadata["sha256"]:
            errors.append(f"hash mismatch: {name}")
    for label, metadata in manifest.get("attachments", {}).items():
        path = bundle / metadata["file"]
        if not path.is_file() or sha256(path) != metadata["sha256"]:
            errors.append(f"invalid attachment: {label}")
    heap = bundle / "heap.pb.gz"
    if heap.exists():
        try:
            with gzip.open(heap, "rb") as source:
                if not source.read(1):
                    errors.append("empty heap protobuf")
        except OSError:
            errors.append("heap.pb.gz is not valid gzip")
    cpu = bundle / "cpu.pb"
    if cpu.exists() and cpu.stat().st_size == 0:
        errors.append("cpu.pb is empty")

    evidence = manifest.get("attachments", {})
    summary = {
        "schema": "xerj.debug_profile.inspect.v1",
        "valid": not errors and manifest.get("status") == "complete",
        "capture_status": manifest.get("status"),
        "errors": errors,
        "profiles": sorted(
            name
            for name in manifest.get("artifacts", {})
            if name in ("cpu.pb", "heap.pb.gz")
        ),
        "binary_sha256": manifest.get("binary_sha256"),
        "source_build_binding": manifest.get("source_build_binding", "UNVERIFIED"),
        "declared_build_unverified": manifest.get("declared_build_unverified"),
        "experiment": manifest.get("experiment"),
        "source": manifest.get("source"),
        "attached_evidence": sorted(evidence),
        "comparison_ready": any(
            token in label.lower()
            for label in evidence
            for token in ("result", "correct", "bench", "telemetry")
        ),
        "prompts": [
            str(pathlib.Path(__file__).resolve().parent / name)
            for name in ("PROMPT_CPU.md", "PROMPT_HEAP.md", "PROMPT_COMPARE.md")
        ],
    }
    if not summary["comparison_ready"]:
        summary["warning"] = (
            "No correctness/benchmark/telemetry attachment was identified; "
            "performance conclusions must be INCONCLUSIVE."
        )
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0 if summary["valid"] else 1


if __name__ == "__main__":
    sys.exit(main())
