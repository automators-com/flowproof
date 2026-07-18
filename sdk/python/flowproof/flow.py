"""Public API surface of the flowproof SDK, backed by the Rust engine."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

from flowproof import _native


@dataclass(frozen=True)
class Flow:
    """A flow, defined by a YAML spec with natural-language steps."""

    spec: Path

    def __init__(self, spec: str | Path) -> None:
        object.__setattr__(self, "spec", Path(spec))

    def record(self, out: str | Path | None = None) -> Path:
        """Perform the flow once against the live app and write a trace.

        Returns the trace path. Requires Windows and the target app.
        """
        return Path(_native.record(self.spec, Path(out) if out else None))

    def run(self, trace: str | Path | None = None) -> bool:
        """Deterministically replay the recorded trace (zero LLM calls).

        Returns True when every step passed. Raises RuntimeError when the
        run cannot execute (missing trace, unsupported platform, ...).
        """
        return _native.run(self.spec, Path(trace) if trace else None)

    def heal(self, trace: str | Path) -> Path:
        """Propose a reviewable diff for a trace that no longer replays."""
        raise NotImplementedError(
            "healing is not implemented yet; see https://github.com/automators-com/flowproof"
        )


def record(spec: str | Path, out: str | Path | None = None) -> Path:
    """Record a flow from a YAML spec. See :meth:`Flow.record`."""
    return Flow(spec).record(out)


def run(spec: str | Path, trace: str | Path | None = None) -> bool:
    """Replay a recorded flow deterministically. See :meth:`Flow.run`."""
    return Flow(spec).run(trace)


def heal(spec: str | Path, trace: str | Path) -> Path:
    """Propose a heal diff for a broken trace. See :meth:`Flow.heal`."""
    return Flow(spec).heal(trace)
