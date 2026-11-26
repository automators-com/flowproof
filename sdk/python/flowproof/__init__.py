"""flowproof - a generic open-source automation framework for the AI-agent era.

Automated testing and agentic process automation across web, desktop, and
Citrix. An agent authors a flow once from a natural-language spec and
records a trace; a deterministic engine replays the trace in CI with zero
LLM calls.

The Rust engine ships inside this package as the `flowproof._native`
extension module; the `flowproof` command drives it.
See https://github.com/automators-com/flowproof
"""

import os as _os

# The wheel carries a decode-only ffmpeg for `author-from-video`. The engine
# runs inside this process, not as a binary with ffmpeg beside it, so it is
# pointed at this copy; an FLOWPROOF_FFMPEG the user set still wins.
_FFMPEG = _os.path.join(
    _os.path.dirname(__file__), "bin", "ffmpeg.exe" if _os.name == "nt" else "ffmpeg"
)
if _os.path.isfile(_FFMPEG):
    _os.environ.setdefault("FLOWPROOF_FFMPEG", _FFMPEG)

from flowproof.flow import (  # noqa: E402
    ClarificationNeeded,
    Flow,
    HealResult,
    RecordResult,
    RecordSkipped,
    RunResult,
    StepResult,
    get_trace,
    heal,
    record,
    run,
)

__version__ = "0.26.0"

__all__ = [
    "ClarificationNeeded",
    "Flow",
    "HealResult",
    "RecordResult",
    "RecordSkipped",
    "RunResult",
    "StepResult",
    "__version__",
    "get_trace",
    "heal",
    "record",
    "run",
]
