#!/usr/bin/env python3
"""Validate a bounded run and report relational memory slopes."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

RUN_SCHEMA = "xerj.bounded_run.v1"
TRACE_SCHEMA = "xerj.ingest_memory.v1"
PROC_SCHEMA = "xerj.process_sample.v1"
BUDGET_SCHEMA = "xerj.bounded_budget.v1"
CHECK_PHASES = ["after_refresh", "after_flush", "after_merge", "after_restart"]
CATEGORIES = [
    "http_body",
    "http_rewrite_buffer",
    "raw_source",
    "parsed_source",
    "parsed_vector",
    "prepared_document",
    "prepared_vector",
    "memtable_active",
    "flush_drained",
    "merge_decoded",
    "merge_survivor",
    "merge_json_buffer",
    "merge_parsed",
    "merge_encoded",
    "cache_stored_slices",
    "cache_doc_values",
    "cache_stored_values",
    "cache_sort_shadow",
    "cache_id_positions",
    "cache_row_sequences",
    "cache_decoded_stored",
]
MEASUREMENTS = {
    "http_body": "serialized_size",
    "http_rewrite_buffer": "exact_capacity",
    "raw_source": "serialized_size",
    "parsed_source": "estimated",
    "parsed_vector": "exact_capacity",
    "prepared_document": "estimated",
    "prepared_vector": "exact_capacity",
    "memtable_active": "estimated",
    "flush_drained": "estimated",
    "merge_decoded": "unavailable",
    "merge_survivor": "unavailable",
    "merge_json_buffer": "unavailable",
    "merge_parsed": "unavailable",
    "merge_encoded": "unavailable",
    "cache_stored_slices": "unavailable",
    "cache_doc_values": "unavailable",
    "cache_stored_values": "unavailable",
    "cache_sort_shadow": "unavailable",
    "cache_id_positions": "unavailable",
    "cache_row_sequences": "unavailable",
    "cache_decoded_stored": "unavailable",
}


def hash_file(path: Path) -> str:
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    return digest


def write_private(path: Path, content: str) -> None:
    path.write_text(content, encoding="utf-8")
    path.chmod(0o600)


def read_ndjson(path: Path) -> list[dict]:
    rows = []
    for number, line in enumerate(path.read_text().splitlines(), 1):
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError as error:
            raise ValueError(f"{path}:{number}: invalid JSON: {error}") from error
    if not rows:
        raise ValueError(f"{path}: no records")
    return rows


def validate_trace(path: Path) -> dict:
    rows = read_ndjson(path)
    problems = []
    if any(row.get("schema") != TRACE_SCHEMA for row in rows):
        problems.append("schema mismatch")
    seq = [row.get("seq") for row in rows]
    if seq != list(range(1, len(rows) + 1)):
        problems.append("sequence is not contiguous from 1")
    if rows[0].get("event") != "trace_start":
        problems.append("first event is not trace_start")
    if rows[-1].get("event") != "trace_stop":
        problems.append("last event is not trace_stop")
    expected_events = ["trace_start"] + ["trace_snapshot"] * (len(rows) - 2) + ["trace_stop"]
    if [row.get("event") for row in rows] != expected_events:
        problems.append("summary trace contains an unexpected event or event order")
    if max(int(row.get("accounting_errors", 0)) for row in rows) != 0:
        problems.append("accounting_errors is nonzero")
    if max(int(row.get("dropped_events", 0)) for row in rows) != 0:
        problems.append("dropped_events is nonzero")
    for position, row in enumerate(rows, 1):
        gauges = row.get("logical")
        if not isinstance(gauges, list) or [gauge.get("category") for gauge in gauges] != CATEGORIES:
            problems.append(f"record {position}: logical category vocabulary/order mismatch")
            continue
        if any(
            gauge.get("measurement") != MEASUREMENTS[gauge["category"]] for gauge in gauges
        ):
            problems.append(f"record {position}: logical measurement mismatch")
        if any(
            not isinstance(gauge.get("current"), int)
            or not isinstance(gauge.get("peak"), int)
            or gauge["current"] < 0
            or gauge["peak"] < gauge["current"]
            for gauge in gauges
        ):
            problems.append(f"record {position}: invalid current/peak gauge")
        if row.get("logical_total_current") != sum(gauge["current"] for gauge in gauges):
            problems.append(f"record {position}: logical_total_current is not the gauge sum")
        allocator = row.get("allocator", {})
        process = row.get("process", {})
        flags = row.get("flags", {})
        allocator_values = [allocator.get(key) for key in ("allocated", "active", "resident")]
        allocator_ok = allocator.get("epoch_ok") is True
        if allocator_ok != all(type(value) is int and value >= 0 for value in allocator_values):
            problems.append(f"record {position}: allocator availability/value mismatch")
        if flags.get("allocator_supported") is not allocator_ok:
            problems.append(f"record {position}: allocator_supported disagrees with epoch_ok")
        rss_ok = process.get("rss_ok") is True
        if rss_ok != (type(process.get("rss")) is int and process["rss"] >= 0):
            problems.append(f"record {position}: RSS availability/value mismatch")
        cpu_ok = process.get("cpu_time_ok") is True
        if cpu_ok != (
            type(process.get("cpu_time_ns")) is int and process["cpu_time_ns"] >= 0
        ):
            problems.append(f"record {position}: CPU-time availability/value mismatch")
        expected_sampled = row.get("event") == "trace_snapshot"
        if flags.get("sampled") is not expected_sampled or flags.get("logical_exact") is not False:
            problems.append(f"record {position}: event flags mismatch")
    stop_nonzero = {
        gauge["category"]: gauge["current"]
        for gauge in rows[-1].get("logical", [])
        if gauge.get("measurement") != "unavailable" and gauge.get("current") != 0
    }
    if stop_nonzero:
        problems.append(f"retained logical bytes at trace_stop: {stop_nonzero}")
    return {
        "records": len(rows),
        "peak_rss_bytes": max(
            (int(row["process"]["rss"]) for row in rows if row["process"].get("rss") is not None),
            default=None,
        ),
        "peak_allocated_bytes": max(
            (
                int(row["allocator"]["allocated"])
                for row in rows
                if row["allocator"].get("allocated") is not None
            ),
            default=None,
        ),
        "peak_logical_bytes": max(int(row.get("logical_total_current", 0)) for row in rows),
        "peak_logical_bytes_by_category": {
            category: max(
                int(
                    next(
                        gauge["peak"]
                        for gauge in row["logical"]
                        if gauge["category"] == category
                    )
                )
                for row in rows
            )
            for category in CATEGORIES
        }
        if not any("logical category vocabulary/order mismatch" in problem for problem in problems)
        else None,
        "problems": problems,
    }


def validate_proc(path: Path) -> dict:
    rows = read_ndjson(path)
    problems = []
    if any(row.get("schema") != PROC_SCHEMA for row in rows):
        problems.append("schema mismatch")
    timestamps = [int(row["ts_unix_ns"]) for row in rows]
    if timestamps != sorted(timestamps) or len(set(timestamps)) != len(timestamps):
        problems.append("timestamps are not strictly increasing")
    rss = [int(row["vmrss_bytes"]) for row in rows if "vmrss_bytes" in row]
    if not rss:
        problems.append("no VmRSS samples")
    return {
        "records": len(rows),
        "peak_rss_bytes": max(rss, default=None),
        "last_rss_bytes": rss[-1] if rss else None,
        "peak_threads": max((int(row.get("threads", 0)) for row in rows), default=None),
        "peak_fds": max((int(row.get("fds", 0)) for row in rows), default=None),
        "write_bytes": max((int(row.get("io_write_bytes", 0)) for row in rows), default=None),
        "problems": problems,
    }


def validate_checks(documents: int, checks) -> list[str]:
    problems = []
    expected_ids = [
        "doc-00000000",
        f"doc-{documents // 2:08d}",
        f"doc-{documents - 1:08d}",
    ]
    expected_sentinels = {doc_id: [doc_id] for doc_id in expected_ids}
    if not isinstance(checks, list) or not all(isinstance(check, dict) for check in checks):
        return ["correctness checks are not an object list"]
    if [check.get("phase") for check in checks] != CHECK_PHASES:
        problems.append("correctness phases/order mismatch")
        return problems
    for check in checks:
        if check.get("count") != documents:
            problems.append(f"{check['phase']}: correctness count mismatch")
        if check.get("sentinels") != expected_sentinels:
            problems.append(f"{check['phase']}: sentinel result mismatch")
    return problems


def analyze(root: Path) -> dict:
    manifest = json.loads((root / "run_manifest.json").read_text())
    problems = []
    if manifest.get("schema") != RUN_SCHEMA:
        problems.append("run manifest schema mismatch")
    results = []
    for cell in manifest["cells"]:
        directory = root / f"docs-{cell['documents']}"
        artifact_problems = []
        for name, expected in cell["artifacts"].items():
            path = directory / name
            if not path.exists() or path.stat().st_size != expected["bytes"]:
                artifact_problems.append(f"{name}: size mismatch or missing")
            elif hash_file(path) != expected["sha256"]:
                artifact_problems.append(f"{name}: sha256 mismatch")
        ingest_trace = validate_trace(directory / "ingest-memory-ingest.ndjson")
        restart_trace = validate_trace(directory / "ingest-memory-restart.ndjson")
        ingest_proc = validate_proc(directory / "proc-ingest.ndjson")
        restart_proc = validate_proc(directory / "proc-restart.ndjson")
        cell_problems = (
            artifact_problems
            + ingest_trace["problems"]
            + restart_trace["problems"]
            + ingest_proc["problems"]
            + restart_proc["problems"]
        )
        category_peaks = ingest_trace["peak_logical_bytes_by_category"]
        if category_peaks is not None and category_peaks["prepared_vector"] == 0:
            cell_problems.append("semantic embedding path produced no prepared vectors")
        cell_problems.extend(validate_checks(cell["documents"], cell.get("checks")))
        results.append(
            {
                "documents": cell["documents"],
                "ingest_trace": ingest_trace,
                "restart_trace": restart_trace,
                "ingest_process": ingest_proc,
                "restart_process": restart_proc,
                "problems": cell_problems,
            }
        )
        problems.extend(f"docs-{cell['documents']}: {item}" for item in cell_problems)

    points = [
        (result["documents"], result["ingest_process"]["peak_rss_bytes"])
        for result in results
        if result["ingest_process"]["peak_rss_bytes"] is not None
    ]
    slopes = []
    for (docs_a, rss_a), (docs_b, rss_b) in zip(points, points[1:]):
        slopes.append(
            {
                "from_documents": docs_a,
                "to_documents": docs_b,
                "rss_bytes_per_added_document": (rss_b - rss_a) / (docs_b - docs_a),
            }
        )
    per_doc = [
        {"documents": docs, "peak_rss_bytes_per_document": rss / docs}
        for docs, rss in points
    ]
    relational = {
        "incremental_slopes": slopes,
        "peak_rss_per_document": per_doc,
        "non_accelerating_incremental_slope": all(
            later["rss_bytes_per_added_document"] <= earlier["rss_bytes_per_added_document"]
            for earlier, later in zip(slopes, slopes[1:])
        )
        if len(slopes) > 1
        else None,
        "declining_peak_rss_per_document": all(
            later["peak_rss_bytes_per_document"] <= earlier["peak_rss_bytes_per_document"]
            for earlier, later in zip(per_doc, per_doc[1:])
        )
        if per_doc
        else None,
        "interpretation": (
            "These are relational diagnostics, not an absolute memory budget. "
            "A false verdict identifies acceleration worth investigating; it is not by itself "
            "a release failure because fixed startup cost and host noise affect small cells."
        ),
    }
    return {
        "schema": "xerj.bounded_analysis.v1",
        "status": "PASS" if not problems else "FAIL",
        "problems": problems,
        "cells": results,
        "relational_memory": relational,
    }


def apply_budget(result: dict, budget: dict) -> list[str]:
    problems = []
    if budget.get("schema") != BUDGET_SCHEMA:
        return ["budget schema mismatch"]
    limits = budget.get("limits", {})
    by_docs = {str(cell["documents"]): cell for cell in result["cells"]}
    for metric, result_key in (
        ("max_peak_ingest_rss_bytes_by_documents", ("ingest_process", "peak_rss_bytes")),
        ("max_peak_restart_rss_bytes_by_documents", ("restart_process", "peak_rss_bytes")),
    ):
        for documents, ceiling in limits.get(metric, {}).items():
            cell = by_docs.get(documents)
            if cell is None:
                problems.append(f"budget requires missing {documents}-document cell")
                continue
            observed = cell[result_key[0]][result_key[1]]
            if observed is None or observed > ceiling:
                problems.append(f"{metric}[{documents}] observed {observed}, ceiling {ceiling}")
    slope_by_range = {
        f"{slope['from_documents']}:{slope['to_documents']}": slope[
            "rss_bytes_per_added_document"
        ]
        for slope in result["relational_memory"]["incremental_slopes"]
    }
    for span, ceiling in limits.get("max_rss_bytes_per_added_document_by_range", {}).items():
        observed = slope_by_range.get(span)
        if observed is None or observed > ceiling:
            problems.append(
                f"max_rss_bytes_per_added_document_by_range[{span}] "
                f"observed {observed}, ceiling {ceiling}"
            )
    return problems


def make_budget(result: dict, source_manifest: dict, headroom: float) -> dict:
    def ceilings(process: str):
        return {
            str(cell["documents"]): int(cell[process]["peak_rss_bytes"] * headroom)
            for cell in result["cells"]
            if cell[process]["peak_rss_bytes"] is not None
        }

    return {
        "schema": BUDGET_SCHEMA,
        "source": {
            "binary_sha256": source_manifest["binary"]["sha256"],
            "corpus_sha256": source_manifest["corpus"]["sha256"],
            "documents": source_manifest["configuration"]["documents"],
            "headroom_multiplier": headroom,
            "note": "Use a pinned-host repeated-run envelope; one noisy run is not a stable budget.",
        },
        "limits": {
            "max_peak_ingest_rss_bytes_by_documents": ceilings("ingest_process"),
            "max_peak_restart_rss_bytes_by_documents": ceilings("restart_process"),
            "max_rss_bytes_per_added_document_by_range": {
                f"{slope['from_documents']}:{slope['to_documents']}": (
                    max(0.0, slope["rss_bytes_per_added_document"]) * headroom
                )
                for slope in result["relational_memory"]["incremental_slopes"]
            },
        },
    }


def render(result: dict) -> str:
    lines = [
        "# Bounded ingest diagnostic result",
        "",
        f"Integrity and correctness: **{result['status']}**",
        "",
        "| Documents | Peak ingest RSS | Peak restart RSS | Trace records |",
        "|---:|---:|---:|---:|",
    ]
    for cell in result["cells"]:
        lines.append(
            f"| {cell['documents']} | {cell['ingest_process']['peak_rss_bytes']} | "
            f"{cell['restart_process']['peak_rss_bytes']} | {cell['ingest_trace']['records']} |"
        )
    lines += ["", "## Relational memory diagnostics", ""]
    for slope in result["relational_memory"]["incremental_slopes"]:
        lines.append(
            f"- {slope['from_documents']} → {slope['to_documents']}: "
            f"{slope['rss_bytes_per_added_document']:.2f} RSS bytes per added document"
        )
    lines += [
        "",
        f"- Non-accelerating incremental slope: "
        f"`{result['relational_memory']['non_accelerating_incremental_slope']}`",
        f"- Declining peak RSS/document: "
        f"`{result['relational_memory']['declining_peak_rss_per_document']}`",
        "",
        result["relational_memory"]["interpretation"],
        "",
    ]
    if result["problems"]:
        lines += ["## Problems", ""] + [f"- {problem}" for problem in result["problems"]] + [""]
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("run", type=Path)
    parser.add_argument("--budget", type=Path, help="apply versioned absolute/relational ceilings")
    parser.add_argument("--write-baseline-budget", type=Path)
    parser.add_argument(
        "--budget-headroom",
        type=float,
        default=1.0,
        help="explicit multiplier when writing a budget (default: exact observed envelope)",
    )
    args = parser.parse_args()
    result = analyze(args.run)
    manifest = json.loads((args.run / "run_manifest.json").read_text())
    if args.budget:
        budget_problems = apply_budget(result, json.loads(args.budget.read_text()))
        result["budget"] = {"path": str(args.budget), "problems": budget_problems}
        result["problems"].extend(f"budget: {problem}" for problem in budget_problems)
        if budget_problems:
            result["status"] = "FAIL"
    if args.write_baseline_budget:
        if args.budget_headroom <= 0:
            parser.error("--budget-headroom must be positive")
        budget = make_budget(result, manifest, args.budget_headroom)
        args.write_baseline_budget.write_text(
            json.dumps(budget, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        args.write_baseline_budget.chmod(0o600)
    write_private(
        args.run / "analysis.json",
        json.dumps(result, indent=2, sort_keys=True) + "\n",
    )
    write_private(args.run / "report.md", render(result))
    print(render(result))
    return 0 if result["status"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
