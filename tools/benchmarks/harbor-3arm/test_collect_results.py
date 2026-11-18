#!/usr/bin/env python3
"""Pinned schema slices and a real collector CLI; no Harbor/Docker/model dependency."""
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

import collect_results

HERE = Path(__file__).resolve().parent
FIXTURE = json.loads((HERE / "fixtures/harbor-0.21-result.json").read_text())


class CollectorTests(unittest.TestCase):
    def load(self, value):
        with tempfile.TemporaryDirectory() as td:
            trial = Path(td) / "fixture__trial"
            trial.mkdir()
            (trial / "result.json").write_text(json.dumps(value))
            return list(collect_results.load_trials(Path(td)))[0][1]

    def test_pinned_harbor_021_reward_metrics_and_wall_time(self):
        value = self.load(FIXTURE)
        self.assertEqual(value["reward"], 1)
        self.assertIs(value["resolved"], True)
        self.assertEqual((value["cost"], value["in_tok"], value["out_tok"], value["sec"]),
                         (0.125, 120, 30, 5))

    def test_legacy_shape_remains_supported(self):
        value = self.load({"task_name": "fixture", "verifier_result": {"reward": 0},
                           "agent_result": {"total_cost_usd": 0.5, "input_tokens": 12,
                                            "output_tokens": 3, "elapsed_seconds": 2}})
        self.assertEqual(value["reward"], 0)
        self.assertIs(value["resolved"], False)
        self.assertEqual((value["cost"], value["in_tok"], value["out_tok"], value["sec"]),
                         (0.5, 12, 3, 2))

    def test_zero_metrics_are_not_replaced_by_legacy_fallbacks(self):
        value = self.load({"task_name": "fixture", "verifier_result": {"rewards": {"reward": 0}},
                           "agent_result": {"cost_usd": 0, "total_cost_usd": 8,
                                            "n_input_tokens": 0, "input_tokens": 99,
                                            "n_output_tokens": 0, "output_tokens": 99},
                           "elapsed_seconds": 0, "total_cost_usd": 9})
        self.assertEqual((value["reward"], value["cost"], value["in_tok"], value["out_tok"], value["sec"]),
                         (0, 0, 0, 0, 0))

    def test_multistep_tokens_and_cost_follow_pinned_context_aggregation(self):
        value = self.load({"task_name": "fixture", "verifier_result": {"rewards": {"reward": 1}},
                           "agent_result": None, "step_results": [
                               {"agent_result": {"cost_usd": 0.1, "n_input_tokens": 20}},
                               {"agent_result": {"cost_usd": 0.2, "n_input_tokens": 30, "n_output_tokens": 4}},
                           ]})
        self.assertAlmostEqual(value["cost"], 0.3)
        self.assertEqual((value["in_tok"], value["out_tok"]), (50, 4))

    def test_missing_metrics_stay_unknown(self):
        value = self.load({"task_name": "fixture", "verifier_result": None, "agent_result": None})
        self.assertIsNone(value["reward"])
        self.assertIsNone(value["cost"])
        self.assertIsNone(value["sec"])

    def test_cli_cell_totals_include_failed_attempts_but_matrix_stays_matched(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            shutil.copyfile(HERE / "collect_results.py", root / "collect_results.py")
            for arm in ("native", "xerj", "xerj-tuned"):
                cell = root / "jobs" / f"fixture.model.{arm}"
                for task, reward, cost in [("solved", 1, 0.5), ("failed", None, 0.25)]:
                    trial = cell / f"{task}__trial"
                    trial.mkdir(parents=True)
                    (trial / "result.json").write_text(json.dumps({
                        "task_name": "matched-task", "reward": reward, "total_cost_usd": cost,
                        "elapsed_seconds": 3600,
                    }))
            result = subprocess.run([sys.executable, str(root / "collect_results.py")],
                                    check=True, capture_output=True, text=True)
            rows = [line for line in result.stdout.splitlines() if line.split()[:2] == ["fixture", "model"]]
            self.assertEqual(len(rows), 3)
            self.assertTrue(all("0.75" in line and line.endswith("2.0") for line in rows), result.stdout)
            self.assertIn("$0.50   1.0h", result.stdout)


if __name__ == "__main__":
    unittest.main()
