"""Tests for the layer B normalisation, alignment and invariants in video-draft-eval.py."""
import importlib.util
from pathlib import Path
import unittest

module_spec = importlib.util.spec_from_file_location("video_draft_eval", Path(__file__).with_name("video-draft-eval.py"))
draft_eval = importlib.util.module_from_spec(module_spec)
module_spec.loader.exec_module(draft_eval)


class NormaliseTest(unittest.TestCase):
    def test_a_click_and_a_pressed_button_are_the_same_click(self):
        click = draft_eval.normalise('Click "Weiter"')
        for other in ['Press the "Weiter" button', 'click on the "weiter" button', 'Click "Weiter".']:
            with self.subTest(other=other):
                self.assertEqual(draft_eval.normalise(other), click)

    def test_a_type_keeps_its_field_and_value(self):
        self.assertEqual(draft_eval.normalise('Type Kranichweg into the "Street" field'), ("type", "street", "kranichweg"))
        self.assertEqual(draft_eval.normalise('Type 17 into "House number"'), ("type", "house number", "17"))

    def test_a_different_value_is_a_different_step(self):
        self.assertNotEqual(draft_eval.normalise('Type 17 into the "House number" field'),
                            draft_eval.normalise('Type 71 into the "House number" field'))

    def test_a_key_is_not_a_button(self):
        self.assertEqual(draft_eval.normalise("Press Enter"), ("key", "", "enter"))
        self.assertEqual(draft_eval.normalise('Press the "Enter" button')[0], "click")

    def test_go_to_ignores_a_trailing_slash(self):
        self.assertEqual(draft_eval.normalise("Go to http://h/app.html/"), draft_eval.normalise("Go to http://h/app.html"))

    def test_a_sentence_only_matches_itself(self):
        self.assertEqual(draft_eval.normalise("Open Produkte and select Haushaltsstrom")[0], "free")


class AlignmentTest(unittest.TestCase):
    expected = ['Click "Sign in"', 'Type 17 into the "House number" field', 'Click "Orders"']

    def test_a_perfect_draft(self):
        s = draft_eval.step_scores(self.expected, list(self.expected))
        self.assertEqual((s["recall"], s["precision"], s["out_of_order"]), (1.0, 1.0, 0))

    def test_a_short_draft_scores_low_recall_and_says_so_in_the_count(self):
        s = draft_eval.step_scores(self.expected, ['Click "Orders"'])
        self.assertEqual((s["aligned"], s["recall"], s["count_ratio"]), (1, 0.3333, 0.3333))

    def test_a_swapped_pair_is_counted_out_of_order(self):
        drafted = ['Click "Orders"', 'Click "Sign in"', 'Type 17 into the "House number" field']
        s = draft_eval.step_scores(self.expected, drafted)
        self.assertEqual((s["aligned"], s["out_of_order"]), (2, 1))

    def test_gaps_pair_up_what_did_not_align(self):
        drafted = ['Click "Sign in"', "Fill in the house number", 'Click "Orders"']
        s = draft_eval.step_scores(self.expected, drafted)
        self.assertEqual(draft_eval.gaps(s["pairs"], 3, 3), [([1], [1])])

    def test_the_judge_counts_beside_the_score(self):
        drafted = ['Click "Sign in"', "Fill in the house number", 'Click "Orders"']
        s = draft_eval.step_scores(self.expected, drafted)
        judge = lambda e, d: {"matches": [{"expected": 0, "drafted": [0], "why": "same field and value"}]}
        recall, verdicts = draft_eval.judged_recall(s, self.expected, drafted, judge)
        self.assertEqual((s["recall"], recall), (0.6667, 1.0))
        self.assertEqual(verdicts[0]["drafted"], ["Fill in the house number"])

    def test_a_judge_answer_out_of_range_is_ignored(self):
        drafted = ['Click "Sign in"', "Something", 'Click "Orders"']
        s = draft_eval.step_scores(self.expected, drafted)
        judge = lambda e, d: {"matches": [{"expected": 5, "drafted": [9], "why": ""}]}
        self.assertEqual(draft_eval.judged_recall(s, self.expected, drafted, judge)[0], 0.6667)


class InvariantTest(unittest.TestCase):
    case = {"expect": {"not_app": False, "out_of_scope": 1, "masked_literals": ["otter-lantern-42"]},
            "expected_steps": ['Type ${PASSWORD} into the "Password" field']}
    ok_run = {"code": 0, "stderr": ""}

    def test_a_clean_draft_keeps_every_promise(self):
        text = 'name: x\napp: web\nsteps:\n  - "Type ${PASSWORD} into the \\"Password\\" field"\n  - "manual step"\n'
        steps = [{"kind": "action", "text": 'Type ${PASSWORD} into the "Password" field'},
                 {"kind": "out_of_scope", "text": "manual step", "observed": "archives a mail"}]
        result = draft_eval.invariants(self.case, self.ok_run, text, steps)
        self.assertTrue(all(result.values()), result)

    def test_a_leaked_password_an_assert_and_a_missing_out_of_scope_all_break(self):
        text = 'name: x\napp: web\nsteps:\n  - "Type otter-lantern-42 into the \\"Password\\" field"\n  - assert: order placed\n'
        steps = [{"kind": "action", "text": 'Type otter-lantern-42 into the "Password" field'}]
        result = draft_eval.invariants(self.case, self.ok_run, text, steps)
        for broken in ["no_assert", "masked_value_never_written", "masked_field_is_a_variable", "other_app_is_out_of_scope"]:
            with self.subTest(broken=broken):
                self.assertFalse(result[broken])

    def test_a_flag_with_nothing_observed_breaks(self):
        steps = [{"kind": "flagged", "text": "resolve", "observed": " "}]
        result = draft_eval.invariants(self.case, self.ok_run, "name: x\napp: web\nsteps: []\n", steps)
        self.assertFalse(result["flags_say_what_they_saw"])

    def test_the_not_an_app_case_must_be_refused(self):
        case = {"expect": {"not_app": True, "out_of_scope": 0, "masked_literals": []}, "expected_steps": []}
        refused = {"code": 2, "stderr": "error: the video shows no app being used: slides"}
        self.assertEqual(draft_eval.invariants(case, refused, None, []), {"refused_as_not_an_app": True})
        self.assertEqual(draft_eval.invariants(case, self.ok_run, "steps: []", []), {"refused_as_not_an_app": False})


if __name__ == "__main__":
    unittest.main()
