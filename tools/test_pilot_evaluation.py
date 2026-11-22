"""Regression checks for the fixed-budget pilot inputs."""

import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pilot_evaluate import read_csv
from pilot_queue import METHODS, read_policy


class PilotEvaluationInputs(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.path = Path(directory.name) / "input"

    def test_policy_requires_a_positive_integer_budget(self):
        policy = {
            "validation_end_utc_exclusive": "2026-08-01",
            "random_seed": 41,
            "thresholds": dict.fromkeys(METHODS, 0.5),
        }
        self.path.write_text(json.dumps(policy), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "reviews_per_utc_day"):
            read_policy(self.path)
        for budget in (None, True, 0, -1, 1.0, "1"):
            with self.subTest(budget=budget):
                policy["reviews_per_utc_day"] = budget
                self.path.write_text(json.dumps(policy), encoding="utf-8")
                with self.assertRaisesRegex(ValueError, "reviews_per_utc_day"):
                    read_policy(self.path)
        policy["reviews_per_utc_day"] = 1
        self.path.write_text(json.dumps(policy), encoding="utf-8")
        self.assertEqual(read_policy(self.path)[-1], 1)

    def test_csv_rejects_duplicate_headers(self):
        columns = ("event_id", "served")
        self.path.write_text("event_id,served,served\ne1,0,999\n", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "duplicate CSV header"):
            read_csv(self.path, columns, lambda row: int(row["served"]))
        self.path.write_text("event_id,served,extra\ne1,0,999\n", encoding="utf-8-sig")
        self.assertEqual(read_csv(self.path, columns, lambda row: int(row["served"])), {"e1": 0})

    def test_csv_rejects_rows_with_extra_or_missing_fields(self):
        columns = ("event_id", "served")
        for text in ("event_id,served\ne1,0,999\n", "event_id,served,extra\ne1,0\n"):
            with self.subTest(text=text):
                self.path.write_text(text, encoding="utf-8")
                with self.assertRaisesRegex(ValueError, "line 2: row has extra or missing fields"):
                    read_csv(self.path, columns, lambda row: int(row["served"]))


if __name__ == "__main__":
    unittest.main()
