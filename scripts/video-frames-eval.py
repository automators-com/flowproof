#!/usr/bin/env python3
"""Layer A of the video-to-flow evals (plans/016-video-to-flow-evals.md):
does frame selection keep a frame showing each known action? No model is
called.

For every generated case (`flowproof-video-fixture` writes `video.mp4`,
`actions.json` and `case.json` per case), runs

    flowproof author-from-video <case>/video.mp4 --frames-out <case>/frames

and scores what came back:

- action recall: the share of actions with a kept frame from when the
  action started until the next one did (at least `--window` ms past its
  end), the stretch its result is on screen. The ceiling on anything the
  model can do, since a moment with no frame is a moment it never sees.
- the kept area against the screen minus the bands the case drew on
  purpose (a menu bar, a taskbar): how far the trim cut into the app, and
  how much of a drawn band it left in.
- the frame count against the budget, and how many changed moments the
  budget dropped.

    python3 scripts/video-frames-eval.py evals/video-to-flow/generated
    python3 scripts/video-frames-eval.py evals/video-to-flow/generated --write-baseline

Exits 1 when a case falls below `evals/video-to-flow/frames-baseline.json`.
The scoreboard (JSON) goes to `--out`, and a Markdown table to stdout.
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
BASELINE = REPO / "evals/video-to-flow/frames-baseline.json"
# Actions a person would see happen. `wait` and `tab` change nothing on
# screen worth a frame of its own.
VISIBLE = {"click", "type", "pick", "press", "keys", "out_of_scope", "slide"}
# The change test compares 128px-wide thumbnails, so the trim is only as
# exact as one thumbnail pixel.
THUMB_WIDTH = 128


def action_recall(actions: list[dict], frame_times: list[int], window_ms: int,
                  duration_ms: int | None = None) -> tuple[float, list[int]]:
    """The share of visible actions with a kept frame showing them, and the
    indices of those without one. An action shows from when it starts until
    the next visible action starts (the end of the video for the last), and
    at least until `window_ms` past its own end."""
    visible = [i for i, a in enumerate(actions) if a["kind"] in VISIBLE]
    missed = []
    seen = 0
    for n, i in enumerate(visible):
        action = actions[i]
        following = actions[visible[n + 1]]["at_ms"] if n + 1 < len(visible) else duration_ms
        low = action["at_ms"]
        # The next action's own moment already shows that action instead.
        high = max(action["end_ms"] + window_ms + 1, following or 0)
        if any(low <= t < high for t in frame_times):
            seen += 1
        else:
            missed.append(i)
    total = seen + len(missed)
    return (seen / total if total else 1.0), missed


def expected_area(case: dict, manifest: dict) -> dict:
    """The frame area that should survive the trim: everything but the
    bands the case drew, in the frames' own pixels."""
    fw, fh = manifest["frame_width"], manifest["frame_height"]
    per_css = fw / case["viewport"]["width"]
    top = round(case["bands_css_px"]["top"] * per_css)
    bottom = round(case["bands_css_px"]["bottom"] * per_css)
    return {"x": 0, "y": top, "width": fw, "height": fh - top - bottom}


def trim_error(expected: dict, kept: dict | None, frame: tuple[int, int]) -> tuple[int, int]:
    """(over, under), in frame pixels summed over the four edges: how far
    the kept area cuts into the app, and how much of a drawn band it left."""
    fw, fh = frame
    kept = kept or {"x": 0, "y": 0, "width": fw, "height": fh}
    def edges(a):
        return (a["x"], a["y"], fw - a["x"] - a["width"], fh - a["y"] - a["height"])
    over = under = 0
    for want, got in zip(edges(expected), edges(kept)):
        over += max(0, got - want)
        under += max(0, want - got)
    return over, under


def score_case(case_dir: Path, manifest: dict, window_ms: int) -> dict:
    case = json.loads((case_dir / "case.json").read_text())
    actions = json.loads((case_dir / "actions.json").read_text())
    times = [f["at_ms"] for f in manifest["frames"]]
    recall, missed = action_recall(actions, times, window_ms, manifest["duration_ms"])
    over, under = trim_error(
        expected_area(case, manifest), manifest.get("kept"),
        (manifest["frame_width"], manifest["frame_height"]))
    return {
        "case": case["id"],
        "catches": case["catches"],
        "duration_ms": manifest["duration_ms"],
        "action_recall": round(recall, 4),
        "missed": [{"index": i, "kind": actions[i]["kind"], "step": actions[i].get("step"),
                    "at_ms": actions[i]["at_ms"]} for i in missed],
        "frames": len(times),
        "max_frames": manifest["max_frames"],
        "changes": manifest["changes"],
        "dropped_by_budget": max(0, manifest["changes"] - len(times)),
        "samples": manifest["samples"],
        "kept": manifest.get("kept"),
        "over_trim_px": over,
        "under_trim_px": under,
    }


def compare(result: dict, baseline: dict | None, tolerance_px: int) -> list[str]:
    """Why a case fails the gate; empty when it holds."""
    problems = []
    if result["frames"] > result["max_frames"]:
        problems.append(f"{result['frames']} frames is over the {result['max_frames']}-frame budget")
    if baseline is None:
        return problems + ["no baseline for this case (run with --write-baseline)"]
    if result["action_recall"] + 1e-9 < baseline["min_action_recall"]:
        problems.append(f"action recall {result['action_recall']:.2f} < baseline {baseline['min_action_recall']:.2f}")
    if result["over_trim_px"] > baseline["max_over_trim_px"] + tolerance_px:
        problems.append(f"trim cuts {result['over_trim_px']}px into the app, baseline {baseline['max_over_trim_px']}px")
    return problems


def baseline_entry(result: dict) -> dict:
    return {"min_action_recall": result["action_recall"], "max_over_trim_px": result["over_trim_px"]}


def extract(flowproof: str, case_dir: Path) -> dict:
    frames = case_dir / "frames"
    if frames.exists():
        shutil.rmtree(frames)
    run = subprocess.run(
        [flowproof, "author-from-video", str(case_dir / "video.mp4"), "--frames-out", str(frames), "--json"],
        capture_output=True, text=True)
    if run.returncode != 0:
        raise RuntimeError(f"{case_dir.name}: {run.stderr.strip().splitlines()[-1:]}")
    return json.loads((frames / "manifest.json").read_text())


def table(results: list[dict], failures: dict[str, list[str]]) -> str:
    rows = ["| Case | Recall | Missed | Frames | Dropped | Over-trim | Under-trim | Gate |",
            "|---|---|---|---|---|---|---|---|"]
    for r in results:
        gate = "; ".join(failures.get(r["case"], [])) or "ok"
        rows.append(f"| {r['case']} | {r['action_recall']:.2f} | {len(r['missed'])} | {r['frames']}/{r['max_frames']} "
                    f"| {r['dropped_by_budget']} | {r['over_trim_px']}px | {r['under_trim_px']}px | {gate} |")
    return "\n".join(rows)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("generated", type=Path, help="the generator's --out folder")
    parser.add_argument("--bin", default=os.environ.get("FLOWPROOF", "target/debug/flowproof"))
    parser.add_argument("--window", type=int, default=1500,
                        help="the least time after an action ends that a frame still shows it, in ms")
    parser.add_argument("--out", type=Path, help="scoreboard JSON (default: a timestamped file beside the baseline)")
    parser.add_argument("--write-baseline", nargs="?", type=Path, const=BASELINE, metavar="PATH",
                        help="record these results as a baseline, by default the committed one; CI writes "
                             "one beside its artifacts, since fonts render differently on each system")
    args = parser.parse_args()

    cases = sorted(p for p in args.generated.iterdir() if (p / "case.json").exists())
    if not cases:
        print(f"no generated cases under {args.generated}", file=sys.stderr)
        return 2
    results = [score_case(c, extract(args.bin, c), args.window) for c in cases]
    stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    board = {"timestamp": stamp, "layer": "A", "window_ms": args.window, "cases": results}
    out = args.out or BASELINE.with_name(f"frames-{stamp}.json")
    out.write_text(json.dumps(board, indent=2) + "\n")

    # Gate against the committed baseline as it was before this run.
    baseline = json.loads(BASELINE.read_text())["cases"] if BASELINE.exists() else {}
    if args.write_baseline:
        args.write_baseline.write_text(json.dumps({
            "timestamp": stamp, "window_ms": args.window,
            "cases": {r["case"]: baseline_entry(r) for r in results}}, indent=2) + "\n")
        print(f"baseline written to {args.write_baseline}")
        if args.write_baseline.resolve() == BASELINE.resolve():
            baseline = {r["case"]: baseline_entry(r) for r in results}
    tolerance = max(1, round(1280 / THUMB_WIDTH))
    failures = {r["case"]: p for r in results if (p := compare(r, baseline.get(r["case"]), tolerance))}
    print(table(results, failures))
    print(f"\nscoreboard: {out}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
