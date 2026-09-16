"""Regression tests for the replay-reliability baseline comparison."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

module_spec = importlib.util.spec_from_file_location(
    "fiori_replay_reliability", Path(__file__).with_name("fiori-replay-reliability.py")
)
harness = importlib.util.module_from_spec(module_spec)
module_spec.loader.exec_module(harness)


class CompareToBaselineTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)

    def write_baseline(self, overall_pass_rate, specs):
        path = Path(self.tmp.name) / "baseline.json"
        import json

        path.write_text(json.dumps({
            "overall_pass_rate": overall_pass_rate,
            "specs": [{"spec": name, "pass_rate": rate} for name, rate in specs.items()],
        }))
        return path

    def test_a_drop_within_threshold_is_not_a_regression(self):
        baseline = harness.load_baseline(self.write_baseline(0.80, {"a.flow.yaml": 0.80}))
        results = [{"spec": "a.flow.yaml", "pass_rate": 0.73}]
        comparison = harness.compare_to_baseline(results, 0.73, baseline, threshold=0.10)
        self.assertEqual(comparison["regressions"], [])

    def test_a_drop_beyond_threshold_is_flagged(self):
        baseline = harness.load_baseline(self.write_baseline(0.80, {"a.flow.yaml": 0.80}))
        results = [{"spec": "a.flow.yaml", "pass_rate": 0.60}]
        comparison = harness.compare_to_baseline(results, 0.60, baseline, threshold=0.10)
        self.assertEqual(len(comparison["regressions"]), 2, "both the per-spec and overall drops should be flagged")
        flagged_specs = {r["spec"] for r in comparison["regressions"]}
        self.assertEqual(flagged_specs, {"a.flow.yaml", "OVERALL"})

    def test_an_improvement_is_never_flagged(self):
        baseline = harness.load_baseline(self.write_baseline(0.50, {"a.flow.yaml": 0.50}))
        results = [{"spec": "a.flow.yaml", "pass_rate": 1.0}]
        comparison = harness.compare_to_baseline(results, 1.0, baseline, threshold=0.10)
        self.assertEqual(comparison["regressions"], [])

    def test_a_spec_absent_from_the_baseline_is_skipped_not_crashed_on(self):
        baseline = harness.load_baseline(self.write_baseline(0.80, {"a.flow.yaml": 0.80}))
        results = [{"spec": "brand-new-spec.flow.yaml", "pass_rate": 0.0}]
        comparison = harness.compare_to_baseline(results, None, baseline, threshold=0.10)
        self.assertEqual(comparison["regressions"], [])
        self.assertEqual(comparison["comparisons"], [])

    def test_main_returns_exit_code_3_on_a_real_regression(self):
        from unittest.mock import patch

        repo = Path(self.tmp.name).resolve()
        corpus = repo / "corpus"
        corpus.mkdir()
        spec = corpus / "a.flow.yaml"
        spec.write_text("name: a\napp: web\nsteps: []\n")
        (corpus / "a.trace.jsonl").write_text("{}\n")
        binary = repo / "flowproof"
        binary.write_text("")
        baseline_path = self.write_baseline(0.80, {"corpus/a.flow.yaml": 0.80})
        out_path = repo / "out.json"

        with patch.object(harness, "REPO", repo), patch.object(
            harness, "run_flowproof", return_value=(1, {"report": {"passed": False, "steps": []}}, "boom")
        ):
            with patch(
                "sys.argv",
                [
                    "fiori-replay-reliability.py",
                    str(corpus),
                    "--bin",
                    str(binary),
                    "--runs",
                    "1",
                    "--out",
                    str(out_path),
                    "--baseline",
                    str(baseline_path),
                ],
            ):
                self.assertEqual(harness.main(), 3)


if __name__ == "__main__":
    unittest.main()
