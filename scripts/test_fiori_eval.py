"""Regression tests for Fiori eval verdicts and immutable input evidence."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

module_spec = importlib.util.spec_from_file_location("fiori_eval", Path(__file__).with_name("fiori-eval.py"))
eval_harness = importlib.util.module_from_spec(module_spec)
module_spec.loader.exec_module(eval_harness)


class FioriEvalTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.spec = self.root / "negative-control-title.flow.yaml"
        self.intent = "page shows Impossible XYZ"
        self.spec.write_text(f"name: Negative control\napp: web\nsteps:\n  - assert: {self.intent}\n")
        self.repo = patch.object(eval_harness, "REPO", self.root)
        self.repo.start()
        self.addCleanup(self.repo.stop)

    def score_error(self, reason):
        with patch.object(eval_harness, "run_flowproof", return_value=(1, {"error": reason}, "")):
            return eval_harness.score_spec(Path("flowproof"), self.spec, 3, None)

    def test_only_the_intended_assertion_failure_counts(self):
        reason = f"error: assertion '{self.intent}' does not hold while recording: expected 'Impossible XYZ', page shows 'Home'"
        self.assertTrue(self.score_error(reason)["passed"])
        for reason in [
            "missing variable SAP_PASSWORD", "browser transport disconnected", "permission denied",
            "error: assertion 'page shows Home' does not hold while recording: expected 'Home', page shows 'Login'",
            f"error: assertion '{self.intent}' does not hold while recording: expected a readable target, browser failed",
            f"error: assertion '{self.intent}' does not hold while recording: expected 'Impossible XYZ', page shows ''",
        ]:
            with self.subTest(reason=reason):
                self.assertFalse(self.score_error(reason)["passed"])

    def test_a_successful_negative_recording_is_never_rescued_by_replay_failure(self):
        with patch.object(eval_harness, "run_flowproof", return_value=(0, {"steps": 1}, "")) as run:
            self.assertFalse(eval_harness.score_spec(Path("flowproof"), self.spec, 3, None)["passed"])
            self.assertEqual(run.call_count, 1)

    def test_recording_cannot_overwrite_the_existing_cassette(self):
        trace = eval_harness.trace_path_for(self.spec)
        trace.write_text("original immutable evidence")
        def record(_binary, args, _cwd):
            copied = Path(args[1])
            self.assertNotEqual(copied.parent, self.root)
            eval_harness.trace_path_for(copied).write_text("new recording")
            return (1, {"error": "missing variable"}, "")
        with patch.object(eval_harness, "run_flowproof", side_effect=record):
            eval_harness.score_spec(Path("flowproof"), self.spec, 3, None)
        self.assertEqual(trace.read_text(), "original immutable evidence")

    def test_new_evidence_survives_scoring_in_its_own_directory(self):
        def record(_binary, args, _cwd):
            copied = Path(args[1])
            eval_harness.trace_path_for(copied).write_text("fresh trace")
            (copied.parent / "filmstrip.png").write_bytes(b"fresh recording")
            return (1, {"error": "deliberate failure"}, "")
        with patch.object(eval_harness, "run_flowproof", side_effect=record):
            result = eval_harness.score_spec(Path("flowproof"), self.spec, 3, None)
        evidence = self.root / result["evidence_dir"]
        self.assertEqual(eval_harness.trace_path_for(evidence / self.spec.name).read_text(), "fresh trace")
        self.assertEqual((evidence / "filmstrip.png").read_bytes(), b"fresh recording")

    def test_zero_replays_is_invalid(self):
        with self.assertRaises(ValueError):
            eval_harness.score_spec(Path("flowproof"), self.spec, 0, None)


if __name__ == "__main__":
    unittest.main()
