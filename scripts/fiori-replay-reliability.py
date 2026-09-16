#!/usr/bin/env python3
"""Fiori REPLAY reliability check - a different question from FAA.

scripts/fiori-eval.py's FAA answers "does a fresh spec record and pass with
zero edits on the first attempt" - a deliberately harsh, research-grade bar
for finding authoring-time bugs. It is not what a client experiences. A
client records a flow, iterates on the wording like any hand-written test
until it's right (completely normal test authoring, not a defect), and then
expects it to keep passing every time after that. THIS script measures that
second thing: given an ALREADY-WORKING, already-committed trace, how often
does deterministic replay (zero LLM calls, zero authoring) actually pass
against the real system, run after run?

    python3 scripts/fiori-replay-reliability.py evals/fiori/dev
    python3 scripts/fiori-replay-reliability.py evals/fiori/dev --runs 20

Only specs with an already-committed `.trace.jsonl` are included - this is
a replay-only measurement, it never calls `record` and never touches an
LLM. A spec with no committed trace yet is skipped, not scored as a
failure: it hasn't been authored, so there's nothing to measure replay
consistency of.

Like fiori-eval.py, never touches the corpus's own committed files: each
run replays from an isolated temp copy, and requires credentials already
in the environment or `flowproof config`.
"""
from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent


def trace_path_for(spec: Path) -> Path:
    name = spec.name
    if name.endswith(".flow.yaml"):
        name = name[: -len(".flow.yaml")] + ".trace.jsonl"
    else:
        name = spec.stem + ".trace.jsonl"
    return spec.with_name(name)


def values_file_for(spec: Path) -> Path | None:
    candidate = spec.parent / "values.yaml"
    return candidate if candidate.exists() else None


def run_flowproof(binary: Path, args: list[str], cwd: Path) -> tuple[int, dict | None, str]:
    proc = subprocess.run([str(binary), *args, "--json"], cwd=cwd, capture_output=True, text=True)
    stdout = proc.stdout.strip()
    parsed = None
    if stdout:
        try:
            parsed = json.loads(stdout)
        except json.JSONDecodeError:
            parsed = None
    return proc.returncode, parsed, (proc.stderr or "").strip()


def failure_detail(report: dict | None, run_err: str, code: int) -> str:
    """The real reason a run failed - a step's own `detail`, not stderr.

    `flowproof run` always eprintln!s "wrote run record -> <path>" after
    writing its run record, pass or fail (crates/flowproof-cli/src/lib.rs).
    That line was going through as the "error" for any failure that didn't
    also crash the process, burying the actual driver/assertion message that
    lives on the first non-passed step in the parsed report instead.
    """
    if report:
        for step in report.get("steps", []):
            if step.get("status") != "passed":
                detail = step.get("detail")
                if detail:
                    return detail
    return run_err or (report or {}).get("name") or f"exit {code}"


def pass_at_k(pass_rate: float, k: int) -> float:
    """P(at least one pass in k independent attempts) given a per-attempt
    pass rate - answers a client's actual question ("if CI retries a flaky
    step up to k times, how often does the suite go green?"), not just the
    harsher single-attempt number."""
    return 1 - (1 - pass_rate) ** k


def measure_spec(binary: Path, spec: Path, runs: int) -> dict:
    """Replay an already-recorded spec `runs` times from an isolated copy of
    the spec AND its committed trace, so nothing here can ever touch the
    real corpus - a run failure must never look like an edit to the trace."""
    evidence_root = REPO / ".flowproof" / "replay-reliability"
    evidence_root.mkdir(parents=True, exist_ok=True)
    work_dir = Path(tempfile.mkdtemp(prefix="fiori-replay-", dir=evidence_root))
    work_spec = work_dir / spec.name
    shutil.copyfile(spec, work_spec)
    committed_trace = trace_path_for(spec)
    shutil.copyfile(committed_trace, trace_path_for(work_spec))
    vars_file = values_file_for(spec)
    if vars_file:
        shutil.copyfile(vars_file, work_spec.with_name(vars_file.name))
    vars_args = ["--vars", str(work_spec.with_name(vars_file.name))] if vars_file else []

    attempts = []
    for i in range(runs):
        code, run_json, run_err = run_flowproof(
            binary, ["run", str(work_spec), "--retries", "0", *vars_args], REPO
        )
        report = (run_json or {}).get("report") if run_json else None
        ok = code == 0 and report is not None and report.get("passed") is True
        detail = None if ok else failure_detail(report, run_err, code)
        attempts.append({"attempt": i + 1, "ok": ok, "detail": detail})

    passes = sum(1 for a in attempts if a["ok"])
    pass_rate = passes / runs if runs else None
    return {
        "spec": str(spec.relative_to(REPO)),
        "runs": runs,
        "passes": passes,
        "pass_rate": pass_rate,
        # pass@k: with this spec's empirical per-attempt pass rate, the odds
        # a CI job that retries up to k times ends up green. k=1 is just
        # pass_rate again; k=2/3 are the retry policies clients actually run.
        "pass_at_k": {str(k): pass_at_k(pass_rate, k) for k in (1, 2, 3)} if pass_rate is not None else None,
        "attempts": attempts,
        "evidence_dir": str(work_dir.relative_to(REPO)),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("corpus", type=Path, help="directory of *.flow.yaml specs")
    parser.add_argument("--runs", type=int, default=15, help="replay attempts per spec (default 15)")
    parser.add_argument(
        "--bin",
        type=Path,
        default=REPO / "target" / "debug" / "flowproof",
        help="path to the flowproof binary (default: target/debug/flowproof)",
    )
    parser.add_argument("--out", type=Path, default=None, help="scoreboard JSON output path")
    args = parser.parse_args()

    if args.runs < 1:
        parser.error("--runs must be at least 1")
    if not args.bin.exists():
        print(f"error: {args.bin} does not exist - build it first ", file=sys.stderr)
        print("  cargo build -p flowproof-cli --all-features", file=sys.stderr)
        return 2

    corpus_dir = args.corpus.resolve()
    all_specs = sorted(corpus_dir.glob("*.flow.yaml"))
    specs = [s for s in all_specs if trace_path_for(s).exists()]
    skipped = [s for s in all_specs if not trace_path_for(s).exists()]
    if not specs:
        print(f"error: no already-recorded specs (with a committed .trace.jsonl) in {corpus_dir}", file=sys.stderr)
        return 2

    print(f"Replaying {len(specs)} already-recorded spec(s), {args.runs} times each")
    if skipped:
        print(f"(skipping {len(skipped)} spec(s) with no committed trace: "
              f"{', '.join(s.name for s in skipped)})")
    print()

    results = []
    for spec in specs:
        print(f"  {spec.name} ...", end=" ", flush=True)
        r = measure_spec(args.bin, spec, args.runs)
        results.append(r)
        print(f"{r['passes']}/{r['runs']} ({r['pass_rate']:.0%})")

    total_runs = sum(r["runs"] for r in results)
    total_passes = sum(r["passes"] for r in results)
    overall_rate = total_passes / total_runs if total_runs else None
    overall_pass_at_k = (
        {str(k): pass_at_k(overall_rate, k) for k in (1, 2, 3)} if overall_rate is not None else None
    )

    timestamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    out_path = args.out or (REPO / "evals" / "fiori" / f"replay-reliability-{timestamp}.json")
    out_path.parent.mkdir(parents=True, exist_ok=True)
    scoreboard = {
        "timestamp": timestamp,
        "corpus": str(corpus_dir.relative_to(REPO)),
        "runs_per_spec": args.runs,
        "overall_pass_rate": overall_rate,
        "overall_pass_at_k": overall_pass_at_k,
        "total_runs": total_runs,
        "total_passes": total_passes,
        "skipped_no_trace": [str(s.relative_to(REPO)) for s in skipped],
        "specs": results,
    }
    out_path.write_text(json.dumps(scoreboard, indent=2) + "\n")

    print()
    if overall_rate is not None:
        print(f"Overall replay pass rate: {overall_rate:.2%} ({total_passes}/{total_runs})")
        print(
            "Overall pass@k (retry up to k times): "
            + ", ".join(f"k={k} {overall_pass_at_k[str(k)]:.2%}" for k in (1, 2, 3))
        )
    else:
        print("n/a")
    for r in results:
        if r["pass_rate"] < 1.0:
            failing = [a for a in r["attempts"] if not a["ok"]]
            pak = ", ".join(f"k={k} {r['pass_at_k'][str(k)]:.0%}" for k in (1, 2, 3))
            print(f"  {r['spec']}: {r['pass_rate']:.0%} (pass@k: {pak}) - first failure: {failing[0]['detail'][:150]}")
    try:
        print(f"scoreboard: {out_path.relative_to(REPO)}")
    except ValueError:
        print(f"scoreboard: {out_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
