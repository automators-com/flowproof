#!/usr/bin/env python3
"""The gap ledger: the one-way path from "flowproof could not express this" to
an issue a Builder may take.

It exists because the loops both request features and judge whether they are
needed. Without a separated, counted record that is unbounded: a Migrator hits a
limitation, decides flowproof should have the feature, and its own next run
passes. Every step defensible, the sum unbounded.

So a gap must be OBSERVED before it can be BUILT. The frequency gate is N>=3
observations across M>=3 DISTINCT repositories, and the distinction matters:
three sightings inside one project is one project's idiom, not a gap in
flowproof.

Written in Python rather than shell because it edits structured data, and shell
plus YAML is a way to introduce bugs quietly.

    ledger.py record <key> <repo> <sha> <test> [--tier N]
    ledger.py promote [--open-issues]      promote what the gate allows
    ledger.py report                       the ranked table, which is the point

The ranked table is worth more than any single feature in it: it is evidence of
what real suites needed, which no amount of speculation could produce.
"""
from __future__ import annotations

import argparse
import pathlib
import subprocess
import sys

import yaml

LEDGER = pathlib.Path("docs/loop/ledger.yaml")
MIN_OBSERVATIONS = 3
MIN_DISTINCT_REPOS = 3


def load(path: pathlib.Path) -> dict:
    if not path.exists():
        return {"version": 1, "gaps": []}
    data = yaml.safe_load(path.read_text()) or {}
    data.setdefault("version", 1)
    # `gaps: []` round-trips as None through some writers; treat it as empty
    # rather than crashing on the first record of the system's life.
    data["gaps"] = data.get("gaps") or []
    return data


def save(path: pathlib.Path, data: dict) -> None:
    header = "\n".join(
        line for line in path.read_text().splitlines() if line.startswith("#")
    ) if path.exists() else ""
    body = yaml.safe_dump(data, sort_keys=False, allow_unicode=True, width=88)
    path.write_text(f"{header}\n\n{body}" if header else body)


def find(data: dict, key: str) -> dict | None:
    return next((g for g in data["gaps"] if g["key"] == key), None)


def distinct_repos(gap: dict) -> int:
    return len({o["repo"] for o in gap.get("observed", [])})


def cmd_record(args) -> int:
    if not args.sha:
        print("an observation without a pinned SHA is not evidence", file=sys.stderr)
        return 2

    data = load(LEDGER)
    gap = find(data, args.key)
    if gap is None:
        gap = {
            "key": args.key,
            "capability": args.capability or args.key.replace("-", " "),
            "tier": args.tier,
            "status": "recording",
            "observed": [],
        }
        data["gaps"].append(gap)

    if gap["status"] in ("declined", "shipped"):
        # A declined gap is not re-proposed every time it is seen again; that is
        # the whole reason `declined_why` is mandatory.
        print(f"{args.key} is {gap['status']}; not recording")
        return 0

    if any(o["repo"] == args.repo and o["test"] == args.test for o in gap["observed"]):
        print(f"{args.key} already has that observation from {args.repo}")
        return 0

    gap["observed"].append(
        {"repo": args.repo, "sha": args.sha, "test": args.test}
    )
    n, m = len(gap["observed"]), distinct_repos(gap)
    if gap["status"] == "recording" and n >= MIN_OBSERVATIONS and m >= MIN_DISTINCT_REPOS:
        gap["status"] = "eligible"
        print(f"{args.key} is now eligible ({n} observations across {m} repositories)")
    else:
        print(f"{args.key}: {n} observations across {m} repositories "
              f"(needs {MIN_OBSERVATIONS} across {MIN_DISTINCT_REPOS})")
    save(LEDGER, data)
    return 0


def cmd_promote(args) -> int:
    data = load(LEDGER)
    promoted = 0
    for gap in data["gaps"]:
        if gap["status"] != "eligible" or gap.get("issue"):
            continue
        if gap.get("charter_scope") == "out-of-scope":
            continue
        if not args.open_issues:
            print(f"would open an issue for {gap['key']}")
            promoted += 1
            continue

        body = [
            f"A Migrator hit this in **{distinct_repos(gap)} distinct repositories**. "
            "It is eligible under the frequency gate in `CHARTER.md` §6.",
            "",
            f"## What a test needed\n\n{gap['capability']}",
            "",
            "## The evidence\n",
        ]
        for o in gap["observed"]:
            body.append(f"- `{o['repo']}` @ `{o['sha'][:8]}` — {o['test']}")
        body += [
            "",
            "## Before building",
            "",
            "Describe the capability in terms of **what a test needed**, not the API "
            "someone sketched. The observations above are the requirement; the design "
            "is yours and a human reviews it.",
            "",
            f"Ledger key: `{gap['key']}`",
        ]
        out = subprocess.run(
            ["gh", "issue", "create", "--label", "ready",
             "--title", f"gap: {gap['capability'][:60]}",
             "--body", "\n".join(body)],
            capture_output=True, text=True,
        )
        if out.returncode != 0:
            print(f"could not open an issue for {gap['key']}: {out.stderr.strip()}",
                  file=sys.stderr)
            continue
        url = out.stdout.strip().splitlines()[-1]
        gap["issue"] = url
        gap["status"] = "building"
        print(f"{gap['key']} -> {url}")
        promoted += 1

    if args.open_issues:
        save(LEDGER, data)
    print(f"{promoted} gap(s) promoted")
    return 0


def cmd_report(args) -> int:
    data = load(LEDGER)
    if not data["gaps"]:
        print("the ledger is empty; no Migrator has reported a gap yet")
        return 0
    rows = sorted(
        data["gaps"],
        key=lambda g: (distinct_repos(g), len(g.get("observed", []))),
        reverse=True,
    )
    print(f"{'repos':>5}  {'obs':>4}  {'status':<10}  capability")
    for g in rows:
        print(f"{distinct_repos(g):>5}  {len(g.get('observed', [])):>4}  "
              f"{g['status']:<10}  {g['capability'][:56]}")
    return 0


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    sub = p.add_subparsers(dest="cmd", required=True)

    r = sub.add_parser("record", help="add one observation")
    r.add_argument("key")
    r.add_argument("repo")
    r.add_argument("sha")
    r.add_argument("test")
    r.add_argument("--tier", type=int, default=2)
    r.add_argument("--capability", default="")
    r.set_defaults(fn=cmd_record)

    pr = sub.add_parser("promote", help="open issues for eligible gaps")
    pr.add_argument("--open-issues", action="store_true")
    pr.set_defaults(fn=cmd_promote)

    rp = sub.add_parser("report", help="the ranked table")
    rp.set_defaults(fn=cmd_report)

    args = p.parse_args()
    return args.fn(args)


if __name__ == "__main__":
    sys.exit(main())
