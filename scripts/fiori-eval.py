#!/usr/bin/env python3
"""Fiori reliability eval harness (docs/fiori-reliability/FINDINGS.md, Phase 2).

Runs a corpus of `.flow.yaml` specs end to end and computes FAA
(first-attempt authoring rate): the fraction where `flowproof record`
produces a trace that then passes `flowproof run` three consecutive times,
with zero edits to the spec between record and green.

    python3 scripts/fiori-eval.py evals/fiori/dev
    python3 scripts/fiori-eval.py evals/fiori/dev --runs 3 --latency 1500

Two things this script does NOT do on your behalf, because getting them
wrong would silently invalidate the number:

- It passes `--no-repair` to every `record` call. `flowproof record`'s
  built-in repair loop is ON by default (up to 3 attempts, autonomously
  rewriting a failing step and retrying) - useful for a real user, fatal to
  this measurement, since FAA is defined on a spec that needed ZERO edits.
  A "successful" recording that repair silently patched is not a
  first-attempt pass; passing `--no-repair` is what makes a record failure
  actually count as one instead of being quietly rescued.
- It does not touch the spec files themselves, ever - a spec that fails
  stays exactly as written, for the archive and for the next round.

Requires the current environment to already have real credentials sourced
(this repo's convention: `set -a; source .env; set +a`) if any spec targets
the real Fiori system, and a built `flowproof` binary (`cargo build -p
flowproof-cli --all-features`, or point `--bin` at a release build).
"""
from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import sys
import time
from pathlib import Path

import yaml

REPO = Path(__file__).resolve().parent.parent


def trace_path_for(spec: Path) -> Path:
    """The trace `record` writes for a spec, by the same convention `record`
    itself uses (default `-o`): the `.flow.yaml` suffix replaced with
    `.trace.jsonl`, not just the last extension swapped."""
    name = spec.name
    if name.endswith(".flow.yaml"):
        name = name[: -len(".flow.yaml")] + ".trace.jsonl"
    else:
        name = spec.stem + ".trace.jsonl"
    return spec.with_name(name)


def is_negative_control(spec: Path) -> bool:
    """A spec whose id names it as a deliberately-wrong assertion (Phase 0c:
    "at least 2 flows that must legitimately FAIL"). Scored inverted: this
    spec PASSES the harness when record or run correctly reports it broken."""
    return "negative-control" in spec.stem


def sha256_of(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def spec_url(spec: Path) -> str | None:
    try:
        with spec.open() as f:
            doc = yaml.safe_load(f)
    except (yaml.YAMLError, OSError):
        return None
    return doc.get("url") if isinstance(doc, dict) else None


def with_latency(url: str, latency_ms: int) -> str:
    """Append MockServer's latency-injection query params (see
    examples/fiori/fixture/webapp/model/mockserver.js) - a no-op for any
    spec that doesn't target that fixture, since a real backend has no such
    param to read. This is the harness's honest limit: it cannot inject
    artificial latency into a real system, only measure against a mock that
    was built to accept the ask."""
    sep = "&" if "?" in url else "?"
    return f"{url}{sep}mockDelay={latency_ms}"


def run_flowproof(
    binary: Path, args: list[str], cwd: Path
) -> tuple[int, dict | None, str]:
    proc = subprocess.run(
        [str(binary), *args, "--json"],
        cwd=cwd,
        capture_output=True,
        text=True,
    )
    stdout = proc.stdout.strip()
    parsed = None
    if stdout:
        try:
            parsed = json.loads(stdout)
        except json.JSONDecodeError:
            parsed = None
    return proc.returncode, parsed, (proc.stderr or "").strip()


def values_file_for(spec: Path) -> Path | None:
    """The values file a spec's `${VAR}`s resolve against, by this repo's own
    convention (see examples/fiori/*.flow.yaml headers): a `values.yaml`
    sitting alongside the spec, shared by every spec in that directory -
    not per-spec. `flowproof record`/`run` do not discover this on their
    own; it must be passed explicitly via `--vars`, or business-data
    placeholders are simply unset."""
    candidate = spec.parent / "values.yaml"
    return candidate if candidate.exists() else None


def score_spec(
    binary: Path, spec: Path, runs: int, latency_ms: int | None
) -> dict:
    negative = is_negative_control(spec)
    result: dict = {
        "spec": str(spec.relative_to(REPO)),
        "negative_control": negative,
        "sha256": sha256_of(spec),
    }

    trace = trace_path_for(spec)
    trace.unlink(missing_ok=True)

    vars_file = values_file_for(spec)
    vars_args = ["--vars", str(vars_file)] if vars_file else []

    record_args = ["record", str(spec), "--no-repair", *vars_args]
    if latency_ms is not None:
        url = spec_url(spec)
        if url:
            record_args += ["--var", f"__FIORI_EVAL_URL__={with_latency(url, latency_ms)}"]
            # A `--var` override only takes effect if the spec itself reads
            # `${__FIORI_EVAL_URL__}` for its url - most specs hard-code a
            # literal url instead, so this is a best-effort hook, not a
            # guarantee. Recorded plainly rather than pretending it always
            # applies.

    code, record_json, record_err = run_flowproof(binary, record_args, REPO)
    if code != 0 or record_json is None or "error" in (record_json or {}):
        reason = (record_json or {}).get("error") if record_json else record_err
        record_ok = False
        result["record"] = {"ok": False, "reason": reason or f"exit {code}"}
    else:
        record_ok = True
        result["record"] = {"ok": True, "steps": record_json.get("steps")}

    if not record_ok:
        result["run_results"] = []
        result["passed"] = negative  # record correctly failed a negative control
        return result

    run_results = []
    for i in range(runs):
        code, run_json, run_err = run_flowproof(
            binary, ["run", str(spec), *vars_args], REPO
        )
        report = (run_json or {}).get("report") if run_json else None
        ok = code == 0 and report is not None and report.get("passed") is True
        run_results.append(
            {
                "attempt": i + 1,
                "ok": ok,
                "detail": None
                if ok
                else (run_err or (report or {}).get("name") or f"exit {code}"),
                "steps": (report or {}).get("steps") if report else None,
            }
        )
        if not ok:
            break  # one failure already breaks "three consecutive"; no point continuing
    result["run_results"] = run_results

    all_runs_passed = len(run_results) == runs and all(r["ok"] for r in run_results)
    result["passed"] = (not all_runs_passed) if negative else all_runs_passed
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("corpus", type=Path, help="directory of *.flow.yaml specs")
    parser.add_argument("--runs", type=int, default=3, help="consecutive runs required (default 3)")
    parser.add_argument(
        "--latency",
        type=int,
        default=None,
        metavar="MS",
        help="inject this much latency via MockServer's ?mockDelay= (mock-fixture specs only)",
    )
    parser.add_argument(
        "--bin",
        type=Path,
        default=REPO / "target" / "debug" / "flowproof",
        help="path to the flowproof binary (default: target/debug/flowproof)",
    )
    parser.add_argument(
        "--out",
        type=Path,
        default=None,
        help="scoreboard path (default: evals/fiori/<timestamp>.json)",
    )
    args = parser.parse_args()

    if not args.bin.exists():
        print(f"error: {args.bin} does not exist - build it first ", file=sys.stderr)
        print("  cargo build -p flowproof-cli --all-features", file=sys.stderr)
        return 2

    corpus_dir = args.corpus.resolve()
    specs = sorted(corpus_dir.glob("*.flow.yaml"))
    if not specs:
        print(f"error: no *.flow.yaml files in {corpus_dir}", file=sys.stderr)
        return 2

    results = []
    for spec in specs:
        print(f"  {spec.name} ...", end=" ", flush=True)
        r = score_spec(args.bin, spec, args.runs, args.latency)
        print("PASS" if r["passed"] else "FAIL")
        results.append(r)

    non_negative = [r for r in results if not r["negative_control"]]
    negative = [r for r in results if r["negative_control"]]
    faa = (
        sum(1 for r in non_negative if r["passed"]) / len(non_negative)
        if non_negative
        else None
    )
    negative_controls_held = all(r["passed"] for r in negative) if negative else None

    timestamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    out_path = args.out or (REPO / "evals" / "fiori" / f"{timestamp}.json")
    out_path.parent.mkdir(parents=True, exist_ok=True)
    scoreboard = {
        "timestamp": timestamp,
        "corpus": str(corpus_dir.relative_to(REPO)),
        "runs_required": args.runs,
        "latency_ms": args.latency,
        "faa": faa,
        "faa_numerator": sum(1 for r in non_negative if r["passed"]),
        "faa_denominator": len(non_negative),
        "negative_controls_held": negative_controls_held,
        "specs": results,
    }
    out_path.write_text(json.dumps(scoreboard, indent=2) + "\n")

    print()
    print(f"FAA: {faa:.2%}" if faa is not None else "FAA: n/a (no non-control specs)")
    if negative:
        state = "held (all still red)" if negative_controls_held else "BROKEN - a negative control passed"
        print(f"Negative controls: {state}")
    print(f"scoreboard: {out_path.relative_to(REPO)}")
    return 0 if (negative_controls_held is not False) else 1


if __name__ == "__main__":
    raise SystemExit(main())
