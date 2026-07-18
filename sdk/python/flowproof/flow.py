"""Public API surface of the flowproof SDK.

Every entry point currently raises :class:`NotImplementedError` — the
engine bindings (PyO3) land later. The signatures are the contract.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

_NOT_WIRED = (
    "the flowproof engine bindings are not implemented yet; "
    "see https://github.com/automators-com/flowproof"
)


@dataclass(frozen=True)
class Flow:
    """A flow, defined by a YAML spec with natural-language steps."""

    spec: Path

    def __init__(self, spec: str | Path) -> None:
        object.__setattr__(self, "spec", Path(spec))

    def record(self, out: str | Path | None = None) -> Path:
        """Have the AI agent perform the flow once and write a trace."""
        raise NotImplementedError(_NOT_WIRED)

    def run(self, trace: str | Path | None = None) -> None:
        """Deterministically replay the recorded trace (zero LLM calls)."""
        raise NotImplementedError(_NOT_WIRED)

    def heal(self, trace: str | Path) -> Path:
        """Propose a reviewable diff for a trace that no longer replays."""
        raise NotImplementedError(_NOT_WIRED)


def record(spec: str | Path, out: str | Path | None = None) -> Path:
    """Record a flow from a YAML spec. See :meth:`Flow.record`."""
    return Flow(spec).record(out)


def run(spec: str | Path, trace: str | Path | None = None) -> None:
    """Replay a recorded flow deterministically. See :meth:`Flow.run`."""
    Flow(spec).run(trace)


def heal(spec: str | Path, trace: str | Path) -> Path:
    """Propose a heal diff for a broken trace. See :meth:`Flow.heal`."""
    return Flow(spec).heal(trace)
