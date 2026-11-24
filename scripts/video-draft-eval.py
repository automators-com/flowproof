#!/usr/bin/env python3
"""Layer B of the video-to-flow evals (plans/016-video-to-flow-evals.md):
the draft a model writes from a case's committed frames, scored against
the expected flow the generator wrote.

    python3 scripts/video-draft-eval.py import evals/video-to-flow/generated
    python3 scripts/video-draft-eval.py score
    python3 scripts/video-draft-eval.py score --case 01-smoke --no-judge

`import` copies each generated case's frames (as `--frames-out` chose them),
`manifest.json`, `case.json`, `actions.json` and `expected.flow.yaml` into
`evals/video-to-flow/cases/<case>/`, so a prompt change is scored from
committed frames with no ffmpeg, Chrome or generator in the loop.

`score` drafts each case through the engine
(`author-from-video --frames-dir`, so it needs a model configured the way
`flowproof` reads one) and scores two things separately:

- invariants, pass or fail: the draft parses, holds no `assert:`, never
  contains a masked field's typed value and writes `${NAME}` for it, every
  flagged line says what it observed, the second-app case has an
  out-of-scope line, and the not-an-app case is refused.
- the step sequence, graded: the drafted actions aligned to the expected
  ones as a longest common subsequence of normalised steps, for recall,
  precision, steps out of order, and drafted / expected count. Steps that do
  not align go to a model judge (`--judge-model`), one call per gap between
  aligned steps, asking only which drafted steps do each expected one. The
  judge's answers are reported beside the score and never decide it.

`score --record` adds layer C, the round trip: the fixture is served on the
address the case was filmed at and each draft is recorded against it with
`flowproof record --no-repair`, headless. A draft that records to the end on
the first attempt is the video analogue of the Fiori corpus's FAA, and the
only number that says the feature works rather than that the text looks
plausible. `control` records each case's *expected* flow the same way, with
no model, which proves the fixtures and the harness before a draft is blamed.

Exits 1 when an invariant breaks, or with `--min-recall` when a case's step
recall is below it; 2 when the engine could not draft at all for a reason
other than the model's reading (no key, no binary).
"""
from __future__ import annotations

import argparse
import functools
import http.server
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

import yaml

REPO = Path(__file__).resolve().parent.parent
CASES = REPO / "evals/video-to-flow/cases"
SCOREBOARDS = REPO / "evals/video-to-flow"
FIXTURES = REPO / "evals/video-to-flow/fixture"
# The generator films at this address, and the drafts' `Go to` names it.
PORT = 8765
# A refusal the engine raises for a recording of no app.
NOT_APP = "shows no app being used"
# Engine failures that are about the setup, not the model's reading.
SETUP = ("no usable model backend", "unexpected argument", "No such file", "cannot read images", "not known to read images")


# --- normalising steps -------------------------------------------------------

def _clean(text: str) -> str:
    text = text.strip().strip("\"'“”„«»").strip()
    text = re.sub(r"\s+", " ", text)
    return text.rstrip(".").casefold()


_FORMS = [
    ("goto", re.compile(r"^go to (?P<value>.+)$", re.I)),
    ("type", re.compile(r"^(?:type|enter) (?P<value>.+?) into (?:the )?(?P<label>\".+?\"|“.+?”)(?: (?:field|box|input))?$", re.I)),
    ("select", re.compile(r"^(?:select|choose|pick) (?P<value>.+?) (?:from|in) (?:the )?(?P<label>\".+?\")(?: (?:field|list|dropdown))?$", re.I)),
    ("click", re.compile(r"^press (?:the )?(?P<label>\".+?\")(?: (?:button|key|tab|link))?$", re.I)),
    ("click", re.compile(r"^(?:click|tap)(?: on)? (?:the )?(?P<label>\".+?\")(?: (?:button|tab|link|field|entry|item))?$", re.I)),
    ("key", re.compile(r"^press (?P<value>[A-Za-z][A-Za-z0-9+]*)$", re.I)),
]


def normalise(step: str) -> tuple[str, str, str]:
    """`(form, label, value)` for a step in flowproof's grammar, so that
    `Click "Weiter"` and `Press the "Weiter" button` are the same click and
    quoting, case and a trailing full stop do not count. A step in no known
    form is `("free", "", text)`, which only matches itself."""
    text = step.strip().rstrip(".").strip()
    for form, pattern in _FORMS:
        m = pattern.match(text)
        if m:
            parts = m.groupdict()
            value = parts.get("value") or ""
            if form == "goto":
                value = value.rstrip("/")
            return form, _clean(parts.get("label") or ""), _clean(value)
    return "free", "", _clean(text)


def lcs(expected: list[tuple], drafted: list[tuple]) -> list[tuple[int, int]]:
    """Index pairs `(i, j)` of a longest common subsequence."""
    n, m = len(expected), len(drafted)
    table = [[0] * (m + 1) for _ in range(n + 1)]
    for i in range(n - 1, -1, -1):
        for j in range(m - 1, -1, -1):
            table[i][j] = (table[i + 1][j + 1] + 1 if expected[i] == drafted[j]
                           else max(table[i + 1][j], table[i][j + 1]))
    pairs, i, j = [], 0, 0
    while i < n and j < m:
        if expected[i] == drafted[j]:
            pairs.append((i, j))
            i, j = i + 1, j + 1
        elif table[i + 1][j] >= table[i][j + 1]:
            i += 1
        else:
            j += 1
    return pairs


def step_scores(expected: list[str], drafted: list[str]) -> dict:
    e = [normalise(s) for s in expected]
    d = [normalise(s) for s in drafted]
    pairs = lcs(e, d)
    aligned_e = {i for i, _ in pairs}
    # Present in the draft but not where the order allows it.
    out_of_order = sum(1 for i, step in enumerate(e) if i not in aligned_e and step in d)
    return {
        "expected": len(e),
        "drafted": len(d),
        "aligned": len(pairs),
        "recall": round(len(pairs) / len(e), 4) if e else 1.0,
        "precision": round(len(pairs) / len(d), 4) if d else 0.0,
        "out_of_order": out_of_order,
        "count_ratio": round(len(d) / len(e), 4) if e else 0.0,
        "pairs": pairs,
    }


def gaps(pairs: list[tuple[int, int]], n: int, m: int) -> list[tuple[list[int], list[int]]]:
    """The unaligned stretches between aligned pairs: (expected indices,
    drafted indices), only where both sides have something."""
    out, pi, pj = [], 0, 0
    for i, j in pairs + [(n, m)]:
        e, d = list(range(pi, i)), list(range(pj, j))
        if e and d:
            out.append((e, d))
        pi, pj = i + 1, j + 1
    return out


# --- invariants --------------------------------------------------------------

def invariants(case: dict, run: dict, draft_text: str | None, steps: list[dict]) -> dict:
    """Each of the feature's promises that applies to the case: True, False,
    or absent when it does not apply."""
    expect = case["expect"]
    out: dict[str, bool] = {}
    if expect["not_app"]:
        out["refused_as_not_an_app"] = run["code"] != 0 and NOT_APP in run["stderr"]
        return out
    out["drafted"] = run["code"] == 0 and draft_text is not None
    if draft_text is None:
        return out
    try:
        spec = yaml.safe_load(draft_text)
        out["parses"] = isinstance(spec, dict) and isinstance(spec.get("steps"), list)
    except yaml.YAMLError:
        spec, out["parses"] = None, False
    listed = spec.get("steps", []) if isinstance(spec, dict) else []
    out["no_assert"] = not any(isinstance(s, dict) and "assert" in s for s in listed) \
        and not re.search(r"^\s*-\s*assert\s*:", draft_text, re.M)
    if expect["masked_literals"]:
        out["masked_value_never_written"] = not any(lit in draft_text for lit in expect["masked_literals"])
        masked_labels = {normalise(s)[1] for s in case["expected_steps"] if "${" in s}
        typed = [normalise(s["text"]) for s in steps if s["kind"] == "action"]
        out["masked_field_is_a_variable"] = all(
            value.startswith("${") for form, label, value in typed if form == "type" and label in masked_labels)
    flagged = [s for s in steps if s["kind"] in ("flagged", "out_of_scope")]
    out["flags_say_what_they_saw"] = all((s.get("observed") or "").strip() for s in flagged)
    if expect["out_of_scope"]:
        out["other_app_is_out_of_scope"] = any(s["kind"] == "out_of_scope" for s in steps)
    return out


# --- the judge ---------------------------------------------------------------

JUDGE_SYSTEM = (
    "You compare test steps. Expected steps are what a person did in an app; drafted steps were written "
    "by someone watching a recording of it. For each expected step, say which drafted steps (if any) "
    "perform that same action. Wording, quoting and a different but equivalent phrasing do not matter; "
    "one expected step may be done by two drafted steps, and a drafted step that does something else "
    "is not a match. A different value or a different field is not a match."
)

JUDGE_SCHEMA = {
    "type": "object",
    "properties": {
        "matches": {
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "expected": {"type": "integer"},
                    "drafted": {"type": "array", "items": {"type": "integer"}},
                    "why": {"type": "string"},
                },
                "required": ["expected", "drafted", "why"],
                "additionalProperties": False,
            },
        }
    },
    "required": ["matches"],
    "additionalProperties": False,
}


def judge_gap(client, model: str, expected: list[str], drafted: list[str]) -> dict:
    """One judge call for one unaligned stretch. Numbers in the answer are
    positions in the two lists given."""
    listing = "Expected:\n" + "\n".join(f"{i}. {s}" for i, s in enumerate(expected)) + \
        "\n\nDrafted:\n" + "\n".join(f"{j}. {s}" for j, s in enumerate(drafted))
    response = client.beta.messages.create(
        model=model,
        max_tokens=4000,
        betas=["server-side-fallback-2026-07-01"],
        fallbacks="default",
        system=JUDGE_SYSTEM,
        messages=[{"role": "user", "content": listing}],
        output_config={"effort": "low", "format": {"type": "json_schema", "schema": JUDGE_SCHEMA}},
    )
    if response.stop_reason == "refusal":
        return {"error": "the judge declined", "matches": []}
    text = next((b.text for b in response.content if b.type == "text"), "{}")
    return json.loads(text)


def judged_recall(scores: dict, expected: list[str], drafted: list[str], judge) -> tuple[float, list[dict]]:
    """Recall once the judge's matches for unaligned steps are counted, and
    every verdict it gave, so a run can be read back and argued with."""
    matched = scores["aligned"]
    verdicts = []
    for e_idx, d_idx in gaps(scores["pairs"], len(expected), len(drafted)):
        answer = judge([expected[i] for i in e_idx], [drafted[j] for j in d_idx])
        for match in answer.get("matches", []):
            local = match.get("expected")
            if not isinstance(local, int) or not 0 <= local < len(e_idx):
                continue
            picks = [d_idx[k] for k in match.get("drafted", []) if isinstance(k, int) and 0 <= k < len(d_idx)]
            verdicts.append({"expected": expected[e_idx[local]], "drafted": [drafted[j] for j in picks],
                             "why": match.get("why", "")})
            matched += bool(picks)
        if answer.get("error"):
            verdicts.append({"error": answer["error"]})
    return (round(matched / len(expected), 4) if expected else 1.0), verdicts


# --- running -----------------------------------------------------------------

def load_case(case_dir: Path) -> dict:
    case = json.loads((case_dir / "case.json").read_text())
    expected = case_dir / "expected.flow.yaml"
    case["expected_steps"] = yaml.safe_load(expected.read_text())["steps"] if expected.exists() else []
    return case


def draft(binary: str, case_dir: Path, case: dict, out: Path) -> dict:
    cmd = [binary, "author-from-video", "--frames-dir", str(case_dir / "frames"),
           "--app", case["app"], "--name", case["name"], "--out", str(out), "--json"]
    if case.get("start_url"):
        cmd += ["--url", case["start_url"]]
    started = time.monotonic()
    run = subprocess.run(cmd, capture_output=True, text=True)
    report = {}
    if run.returncode == 0:
        try:
            report = json.loads(run.stdout.strip().splitlines()[-1])
        except (json.JSONDecodeError, IndexError):
            report = {}
    return {"code": run.returncode, "stderr": run.stderr, "report": report,
            "seconds": round(time.monotonic() - started, 1)}


def score_case(binary: str, case_dir: Path, judge, work: Path, round_trip: bool = False) -> dict:
    case = load_case(case_dir)
    out = work / f"{case['id']}.flow.yaml"
    run = draft(binary, case_dir, case, out)
    last = (run["stderr"].strip().splitlines() or [""])[-1]
    setup_error = run["code"] != 0 and any(s in run["stderr"] for s in SETUP)
    text = out.read_text() if run["code"] == 0 and out.exists() else None
    steps = run["report"].get("steps", [])
    result = {
        "case": case["id"], "catches": case["catches"], "frames": run["report"].get("frames"),
        "seconds": run["seconds"], "error": last if run["code"] != 0 else None,
        "setup_error": setup_error,
        "invariants": invariants(case, run, text, steps), "draft": text,
    }
    if text is not None and case["expected_steps"]:
        drafted = [s["text"] for s in steps if s["kind"] == "action"]
        scores = step_scores(case["expected_steps"], drafted)
        if judge is not None:
            scores["judged_recall"], scores["judge"] = judged_recall(scores, case["expected_steps"], drafted, judge)
        scores.pop("pairs")
        result["steps"] = scores
    if round_trip and text is not None:
        result["round_trip"] = record(binary, out, work)
    return result


# --- layer C: the round trip -------------------------------------------------

def serve_fixtures(port: int = PORT) -> http.server.ThreadingHTTPServer:
    """The fixtures on 127.0.0.1, as the generator served them while filming."""
    handler = functools.partial(_QuietHandler, directory=str(FIXTURES))
    server = http.server.ThreadingHTTPServer(("127.0.0.1", port), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


class _QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args):
        pass


def variables(spec_text: str) -> list[str]:
    """`--var NAME=…` for every `${NAME}` a spec uses: the fixtures accept
    any password, so the value only has to be there."""
    names = sorted(set(re.findall(r"\$\{([A-Z][A-Z0-9_]*)\}", spec_text)))
    return [arg for name in names for arg in ("--var", f"{name}=video-eval")]


def record(binary: str, spec: Path, work: Path) -> dict:
    """One first attempt: `record --no-repair`, headless, no screenshots."""
    trace = work / (spec.name.removesuffix(".flow.yaml") + ".trace.jsonl")
    cmd = [binary, "record", str(spec), "--no-repair", "--headless", "--recording-detail", "off",
           "-o", str(trace), *variables(spec.read_text())]
    started = time.monotonic()
    run = subprocess.run(cmd, capture_output=True, text=True, cwd=work)
    last = (run.stderr.strip().splitlines() or [""])[-1]
    return {"recorded": run.returncode == 0, "error": None if run.returncode == 0 else last,
            "seconds": round(time.monotonic() - started, 1)}


def control(binary: str, dirs: list[Path], work: Path) -> list[dict]:
    """Records every expected flow: if one fails, the fixture or the
    harness is wrong, not the draft."""
    results = []
    for case_dir in dirs:
        expected = case_dir / "expected.flow.yaml"
        if not expected.exists():
            continue
        spec = work / f"{case_dir.name}.flow.yaml"
        shutil.copy2(expected, spec)
        results.append({"case": case_dir.name, **record(binary, spec, work)})
    return results


def import_generated(generated: Path) -> int:
    count = 0
    for src in sorted(p for p in generated.iterdir() if (p / "frames/manifest.json").exists()):
        dst = CASES / src.name
        if dst.exists():
            shutil.rmtree(dst)
        shutil.copytree(src / "frames", dst / "frames")
        for name in ("case.json", "actions.json", "expected.flow.yaml"):
            if (src / name).exists():
                shutil.copy2(src / name, dst / name)
        count += 1
    print(f"imported {count} cases into {CASES.relative_to(REPO)}")
    return 0 if count else 2


def table(results: list[dict]) -> str:
    rows = ["| Case | Invariants | Recall | Judged | Precision | Order | Drafted/expected | Records |",
            "|---|---|---|---|---|---|---|---|"]
    for r in results:
        broken = [k for k, ok in r["invariants"].items() if not ok]
        inv = "ok" if not broken else "**" + ", ".join(broken) + "**"
        trip = r.get("round_trip")
        records = "-" if trip is None else ("yes" if trip["recorded"] else "no")
        s = r.get("steps")
        if s:
            judged = f"{s['judged_recall']:.2f}" if "judged_recall" in s else "-"
            rows.append(f"| {r['case']} | {inv} | {s['recall']:.2f} | {judged} | {s['precision']:.2f} "
                        f"| {s['out_of_order']} | {s['drafted']}/{s['expected']} | {records} |")
        else:
            rows.append(f"| {r['case']} | {inv} | - | - | - | - | - | {records} |")
    return "\n".join(rows)


def first_attempt_rate(results: list[dict]) -> float | None:
    trips = [r["round_trip"]["recorded"] for r in results if "round_trip" in r]
    return round(sum(trips) / len(trips), 4) if trips else None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    imp = sub.add_parser("import", help="commit a generated corpus's frames as the cases layer B scores")
    imp.add_argument("generated", type=Path)
    sc = sub.add_parser("score", help="draft every case and score it")
    sc.add_argument("--bin", default=os.environ.get("FLOWPROOF", "target/debug/flowproof"))
    sc.add_argument("--case", action="append", help="only these case ids (repeatable)")
    sc.add_argument("--judge-model", default=os.environ.get("VIDEO_EVAL_JUDGE_MODEL", "claude-opus-5-5"))
    sc.add_argument("--no-judge", action="store_true", help="score the alignment only")
    sc.add_argument("--min-recall", type=float, help="fail a case whose step recall is below this")
    sc.add_argument("--out", type=Path, help="scoreboard JSON (default: a timestamped file in evals/video-to-flow)")
    sc.add_argument("--record", action="store_true",
                    help="layer C: record each draft against the served fixture with --no-repair")
    ctl = sub.add_parser("control", help="record every expected flow against its fixture, with no model")
    ctl.add_argument("--bin", default=os.environ.get("FLOWPROOF", "target/debug/flowproof"))
    ctl.add_argument("--case", action="append", help="only these case ids (repeatable)")
    args = parser.parse_args()
    if args.command == "import":
        return import_generated(args.generated)
    args.bin = str(Path(args.bin).resolve())
    if args.command == "control":
        dirs = sorted(p for p in CASES.iterdir() if (p / "case.json").exists() and (not args.case or p.name in args.case))
        server = serve_fixtures()
        try:
            with tempfile.TemporaryDirectory() as work:
                results = control(args.bin, dirs, Path(work))
        finally:
            server.shutdown()
        for r in results:
            print(f"{r['case']}: {'records' if r['recorded'] else 'FAILS: ' + (r['error'] or '')}")
        return 0 if all(r["recorded"] for r in results) else 1

    judge = None
    if not args.no_judge:
        import anthropic  # only the judge needs it
        client = anthropic.Anthropic()
        judge = lambda e, d: judge_gap(client, args.judge_model, e, d)  # noqa: E731
    dirs = sorted(p for p in CASES.iterdir() if (p / "case.json").exists())
    if args.case:
        dirs = [d for d in dirs if d.name in args.case]
    if not dirs:
        print("no cases with committed frames; run `import` first", file=sys.stderr)
        return 2
    server = serve_fixtures() if args.record else None
    try:
        with tempfile.TemporaryDirectory() as work:
            results = [score_case(args.bin, d, judge, Path(work), args.record) for d in dirs]
    finally:
        if server:
            server.shutdown()
    stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    board = {"timestamp": stamp, "layer": "B+C" if args.record else "B",
             "judge_model": None if args.no_judge else args.judge_model,
             "first_attempt_rate": first_attempt_rate(results), "cases": results}
    out = args.out or SCOREBOARDS / f"drafts-{stamp}.json"
    out.write_text(json.dumps(board, indent=2) + "\n")
    print(table(results))
    if board["first_attempt_rate"] is not None:
        print(f"\nfirst-attempt record rate: {board['first_attempt_rate']:.2f}")
    print(f"\nscoreboard: {out}")
    if any(r["setup_error"] for r in results):
        print("the engine could not draft: " + next(r["error"] for r in results if r["setup_error"]), file=sys.stderr)
        return 2
    broken = any(not ok for r in results for ok in r["invariants"].values())
    low = args.min_recall is not None and any(
        r.get("steps", {}).get("recall", 1.0) < args.min_recall for r in results)
    return 1 if broken or low else 0


if __name__ == "__main__":
    sys.exit(main())
