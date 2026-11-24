"""Tests for the layer A scoring in video-frames-eval.py."""
import importlib.util
from pathlib import Path
import unittest

module_spec = importlib.util.spec_from_file_location("video_frames_eval", Path(__file__).with_name("video-frames-eval.py"))
frames_eval = importlib.util.module_from_spec(module_spec)
module_spec.loader.exec_module(frames_eval)


def action(kind, at, end=None):
    return {"kind": kind, "at_ms": at, "end_ms": at if end is None else end, "step": None}


class ActionRecallTest(unittest.TestCase):
    def test_a_frame_inside_the_window_sees_the_action(self):
        recall, missed = frames_eval.action_recall([action("click", 1000)], [2400], 1500)
        self.assertEqual((recall, missed), (1.0, []))

    def test_a_frame_before_the_action_or_after_the_window_does_not(self):
        for t in (999, 2501):
            with self.subTest(t=t):
                self.assertEqual(frames_eval.action_recall([action("click", 1000)], [t], 1500), (0.0, [0]))

    def test_the_result_shows_until_the_next_action(self):
        actions = [action("click", 1000), action("wait", 2000, 60000), action("click", 61000)]
        # Late in a long pause, the first click's result is still on screen.
        self.assertEqual(frames_eval.action_recall(actions, [59000, 61000], 0), (1.0, []))
        # A frame after the second click no longer shows only the first.
        self.assertEqual(frames_eval.action_recall(actions, [61000], 0), (0.5, [0]))

    def test_the_last_action_shows_until_the_video_ends(self):
        self.assertEqual(frames_eval.action_recall([action("click", 1000)], [9000], 0, 9100), (1.0, []))

    def test_typing_is_seen_anywhere_from_the_first_key_to_after_the_last(self):
        recall, _ = frames_eval.action_recall([action("type", 1000, 4000)], [3000], 0)
        self.assertEqual(recall, 1.0)

    def test_waits_and_tabs_are_not_counted(self):
        actions = [action("wait", 0, 60000), action("tab", 100), action("click", 5000)]
        self.assertEqual(frames_eval.action_recall(actions, [5000], 0), (1.0, []))

    def test_a_case_with_nothing_visible_scores_full(self):
        self.assertEqual(frames_eval.action_recall([action("wait", 0)], [], 0), (1.0, []))


class TrimTest(unittest.TestCase):
    case = {"viewport": {"width": 1280}, "bands_css_px": {"top": 26, "bottom": 40}}
    manifest = {"frame_width": 1280, "frame_height": 800}

    def test_the_expected_area_leaves_out_the_drawn_bands(self):
        self.assertEqual(frames_eval.expected_area(self.case, self.manifest),
                         {"x": 0, "y": 26, "width": 1280, "height": 734})

    def test_a_retina_case_is_measured_in_frame_pixels(self):
        case = {"viewport": {"width": 640}, "bands_css_px": {"top": 26, "bottom": 0}}
        self.assertEqual(frames_eval.expected_area(case, self.manifest)["y"], 52)

    def test_over_and_under_trim_are_told_apart(self):
        expected = {"x": 0, "y": 26, "width": 1280, "height": 734}
        # Cut 80px into the app on the left, and left the 40px taskbar in.
        kept = {"x": 80, "y": 26, "width": 1200, "height": 774}
        self.assertEqual(frames_eval.trim_error(expected, kept, (1280, 800)), (80, 40))

    def test_no_trim_is_the_whole_frame(self):
        expected = {"x": 0, "y": 0, "width": 1280, "height": 800}
        self.assertEqual(frames_eval.trim_error(expected, None, (1280, 800)), (0, 0))


class GateTest(unittest.TestCase):
    result = {"frames": 12, "max_frames": 60, "action_recall": 0.75, "over_trim_px": 30}

    def test_holding_the_baseline_passes(self):
        self.assertEqual(frames_eval.compare(self.result, frames_eval.baseline_entry(self.result), 10), [])

    def test_lower_recall_or_deeper_trim_fails(self):
        base = {"min_action_recall": 0.8, "max_over_trim_px": 10}
        problems = frames_eval.compare(self.result, base, 10)
        self.assertEqual(len(problems), 2, problems)

    def test_trim_within_one_thumbnail_pixel_is_noise(self):
        self.assertEqual(frames_eval.compare(self.result, {"min_action_recall": 0.75, "max_over_trim_px": 25}, 10), [])

    def test_a_case_with_no_baseline_fails_loudly(self):
        self.assertTrue(frames_eval.compare(self.result, None, 10))

    def test_over_budget_always_fails(self):
        result = dict(self.result, frames=61)
        self.assertTrue(frames_eval.compare(result, frames_eval.baseline_entry(result), 10))


if __name__ == "__main__":
    unittest.main()
