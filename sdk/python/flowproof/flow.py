"""Public API surface of the flowproof SDK, backed by the Rust engine.

Designed for programmatic callers (typically AI agents): every operation
returns structured data — never something that has to be scraped out of
stdout. The ``flowproof`` CLI wraps these same code paths for humans.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from flowproof import _native


@dataclass(frozen=True)
class RecordResult:
    """Outcome of recording a flow."""

    trace_path: Path
    steps: int


@dataclass(frozen=True)
class StepResult:
    """One replayed step. ``status`` is ``passed``, ``failed`` or ``skipped``."""

    id: str
    intent: str
    status: str
    duration_ms: int
    detail: str | None = None


@dataclass(frozen=True)
class RunResult:
    """Structured outcome of a replay. Truthy exactly when the flow passed."""

    name: str
    trace_id: str
    passed: bool
    duration_ms: int
    steps: tuple[StepResult, ...]
    report_path: Path
    html_path: Path
    """Human-readable rendering generated from the JSON report."""

    def __bool__(self) -> bool:
        return self.passed


def _parse_run_result(payload: str) -> RunResult:
    data = json.loads(payload)
    report = data["report"]
    return RunResult(
        name=report["name"],
        trace_id=report["trace_id"],
        passed=report["passed"],
        duration_ms=report["duration_ms"],
        steps=tuple(
            StepResult(
                id=s["id"],
                intent=s["intent"],
                status=s["status"],
                duration_ms=s["duration_ms"],
                detail=s.get("detail"),
            )
            for s in report["steps"]
        ),
        report_path=Path(data["report_path"]),
        html_path=Path(data["report_path"]).with_name("report.html"),
    )


@dataclass(frozen=True)
class HealResult:
    """Outcome of a heal pass: a PROPOSED trace diff, never a silent fix."""

    changed: bool
    steps_changed: tuple[dict[str, Any], ...]
    steps_added: int
    steps_removed: int
    proposed_path: Path | None
    applied: bool


def _parse_heal_result(payload: str) -> HealResult:
    data = json.loads(payload)
    report = data["report"]
    proposed = report.get("proposed_path")
    return HealResult(
        changed=report["changed"],
        steps_changed=tuple(report["steps_changed"]),
        steps_added=report["steps_added"],
        steps_removed=report["steps_removed"],
        proposed_path=Path(proposed) if proposed else None,
        applied=data["applied"],
    )


@dataclass(frozen=True)
class Flow:
    """A flow, defined by a YAML spec with natural-language steps."""

    spec: Path

    def __init__(self, spec: str | Path) -> None:
        object.__setattr__(self, "spec", Path(spec))

    def record(self, out: str | Path | None = None) -> RecordResult:
        """Perform the flow once against the live app and write a trace.

        Requires Windows and the target app.
        """
        data = json.loads(_native.record(self.spec, Path(out) if out else None))
        return RecordResult(trace_path=Path(data["trace_path"]), steps=data["steps"])

    def run(self, trace: str | Path | None = None) -> RunResult:
        """Deterministically replay the recorded trace (zero LLM calls).

        A failing test is a ``RunResult`` with ``passed=False``, not an
        exception; ``RuntimeError`` means the run could not execute at all
        (missing trace, unsupported platform, ...).
        """
        return _parse_run_result(_native.run(self.spec, Path(trace) if trace else None))

    def get_trace(self, trace: str | Path | None = None) -> dict[str, Any]:
        """Load the recorded trace for inspection: ``{"header": …, "steps": […]}``."""
        return json.loads(_native.get_trace(Path(trace) if trace else self.spec))

    def heal(self, trace: str | Path | None = None, apply: bool = False) -> HealResult:
        """Re-author the flow against the live app and propose a trace diff.

        Never modifies the trace unless ``apply=True`` is passed explicitly;
        the proposal lands next to the trace as ``*.proposed.jsonl``.
        """
        return _parse_heal_result(_native.heal(self.spec, Path(trace) if trace else None, apply))


def record(spec: str | Path, out: str | Path | None = None) -> RecordResult:
    """Record a flow from a YAML spec. See :meth:`Flow.record`."""
    return Flow(spec).record(out)


def run(spec: str | Path, trace: str | Path | None = None) -> RunResult:
    """Replay a recorded flow deterministically. See :meth:`Flow.run`."""
    return Flow(spec).run(trace)


def get_trace(path: str | Path) -> dict[str, Any]:
    """Load a trace (from a spec path or a ``.jsonl`` file) for inspection."""
    return json.loads(_native.get_trace(Path(path)))


def heal(spec: str | Path, trace: str | Path | None = None, apply: bool = False) -> HealResult:
    """Propose a heal diff for a stale trace. See :meth:`Flow.heal`."""
    return Flow(spec).heal(trace, apply)
