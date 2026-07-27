import importlib.util
import json
import os
import tempfile
import tomllib
import unittest
from io import BytesIO
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]


def load(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


generate = load("generate_corpus")
analyze = load("analyze")
run_suite = load("run_suite")


class BoundedSuiteTests(unittest.TestCase):
    def test_corpus_is_reproducible(self):
        with tempfile.TemporaryDirectory() as tmp:
            left, right = Path(tmp) / "left", Path(tmp) / "right"
            a = generate.generate(left, 17)
            b = generate.generate(right, 17)
            self.assertEqual(a["sha256"], b["sha256"])
            self.assertEqual(a["bytes"], b["bytes"])
            self.assertEqual(
                (left / "documents.ndjson").read_bytes(),
                (right / "documents.ndjson").read_bytes(),
            )

    def test_toml_path_escaping_handles_quotes_and_backslashes(self):
        value = 'directory with "quotes" and a \\\\ separator'
        parsed = tomllib.loads(f"data_dir = {run_suite.toml_string(value)}\n")
        self.assertEqual(parsed["data_dir"], value)

    def test_trace_validator_rejects_gap_and_accounting_error(self):
        gauges = [
            {
                "category": category,
                "current": 0,
                "peak": 1,
                "measurement": analyze.MEASUREMENTS[category],
            }
            for category in analyze.CATEGORIES
        ]
        event = {
            "schema": analyze.TRACE_SCHEMA,
            "seq": 1,
            "event": "trace_start",
            "accounting_errors": 0,
            "dropped_events": 0,
            "logical": gauges,
            "logical_total_current": 0,
            "allocator": {"allocated": 1, "active": 2, "resident": 3, "epoch_ok": True},
            "process": {
                "rss": 2,
                "rss_ok": True,
                "cpu_time_ns": 3,
                "cpu_time_ok": True,
            },
            "flags": {
                "sampled": False,
                "logical_exact": False,
                "allocator_supported": True,
            },
        }
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "trace.ndjson"
            stop = {**event, "seq": 3, "event": "trace_stop", "accounting_errors": 1}
            path.write_text(json.dumps(event) + "\n" + json.dumps(stop) + "\n")
            result = analyze.validate_trace(path)
            self.assertIn("sequence is not contiguous from 1", result["problems"])
            self.assertIn("accounting_errors is nonzero", result["problems"])

    def test_trace_validator_rejects_category_and_total_corruption(self):
        gauges = [
            {
                "category": category,
                "current": 0,
                "peak": 0,
                "measurement": analyze.MEASUREMENTS[category],
            }
            for category in analyze.CATEGORIES
        ]
        base = {
            "schema": analyze.TRACE_SCHEMA,
            "accounting_errors": 0,
            "dropped_events": 0,
            "logical": gauges,
            "logical_total_current": 9,
            "allocator": {
                "allocated": None,
                "active": None,
                "resident": None,
                "epoch_ok": False,
            },
            "process": {
                "rss": None,
                "rss_ok": False,
                "cpu_time_ns": None,
                "cpu_time_ok": False,
            },
            "flags": {
                "sampled": False,
                "logical_exact": False,
                "allocator_supported": False,
            },
        }
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "trace.ndjson"
            start = {**base, "seq": 1, "event": "trace_start"}
            stop = {**base, "seq": 2, "event": "trace_stop"}
            stop["logical"] = list(reversed(gauges))
            path.write_text(json.dumps(start) + "\n" + json.dumps(stop) + "\n")
            result = analyze.validate_trace(path)
            self.assertTrue(
                any("logical_total_current" in problem for problem in result["problems"])
            )
            self.assertTrue(
                any("category vocabulary/order" in problem for problem in result["problems"])
            )

    def test_budget_enforces_absolute_and_relational_ceilings(self):
        result = {
            "cells": [
                {
                    "documents": 10,
                    "ingest_process": {"peak_rss_bytes": 100},
                    "restart_process": {"peak_rss_bytes": 80},
                },
                {
                    "documents": 20,
                    "ingest_process": {"peak_rss_bytes": 300},
                    "restart_process": {"peak_rss_bytes": 90},
                },
            ],
            "relational_memory": {
                "incremental_slopes": [
                    {
                        "from_documents": 10,
                        "to_documents": 20,
                        "rss_bytes_per_added_document": 20.0,
                    }
                ]
            },
        }
        budget = {
            "schema": analyze.BUDGET_SCHEMA,
            "limits": {
                "max_peak_ingest_rss_bytes_by_documents": {"20": 299},
                "max_rss_bytes_per_added_document_by_range": {"10:20": 19.9},
            },
        }
        self.assertEqual(len(analyze.apply_budget(result, budget)), 2)

    def test_result_validator_rejects_missing_phase_and_sentinel_tampering(self):
        checks = [
            {
                "phase": phase,
                "count": 64,
                "sentinels": {
                    "doc-00000000": ["doc-00000000"],
                    "doc-00000032": ["doc-00000032"],
                    "doc-00000063": ["doc-00000063"],
                },
            }
            for phase in analyze.CHECK_PHASES
        ]
        checks.pop(1)
        self.assertEqual(
            analyze.validate_checks(64, checks),
            ["correctness phases/order mismatch"],
        )
        checks.insert(
            1,
            {
                "phase": "after_flush",
                "count": 64,
                "sentinels": {"doc-00000000": ["wrong-id"]},
            },
        )
        self.assertEqual(
            analyze.validate_checks(64, checks),
            ["after_flush: sentinel result mismatch"],
        )

    def test_failed_cell_cleans_up_server_without_masking_request_error(self):
        class FakeProcess:
            pid = -1

            def __init__(self):
                self.running = True
                self.signals = []

            def poll(self):
                return None if self.running else 0

            def send_signal(self, value):
                self.signals.append(value)
                self.running = False

            def wait(self, timeout=None):
                self.running = False
                return 0

            def kill(self):
                self.running = False

        process = FakeProcess()
        stream = BytesIO()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            corpus = root / "corpus"
            corpus.mkdir()
            (corpus / "documents.ndjson").write_text("{}\n")

            def fake_start(_binary, cell, _data, suffix, _sample_ms, _embed_mode):
                return (
                    process,
                    stream,
                    "http://127.0.0.1:1",
                    cell / f"ingest-memory-{suffix}.ndjson",
                    cell / f"server-{suffix}.toml",
                )

            with mock.patch.object(run_suite, "start_server", side_effect=fake_start), mock.patch.object(
                run_suite, "request", side_effect=RuntimeError("original request failure")
            ):
                with self.assertRaisesRegex(RuntimeError, "original request failure"):
                    run_suite.run_cell(Path("/unused"), corpus, root, 1, 1, 100, "lexical")
        self.assertTrue(stream.closed)
        self.assertEqual(process.signals, [run_suite.signal.SIGINT])

    def test_derived_artifact_writer_forces_private_mode(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "analysis.json"
            path.write_text("old")
            path.chmod(0o644)
            analyze.write_private(path, '{"status":"PASS"}\n')
            self.assertEqual(path.read_text(), '{"status":"PASS"}\n')
            self.assertEqual(os.stat(path).st_mode & 0o777, 0o600)


if __name__ == "__main__":
    unittest.main()
