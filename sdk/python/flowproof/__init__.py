"""flowproof — a generic open-source automation framework for the AI-agent era.

Automated testing and agentic process automation across web, desktop, and
Citrix. An agent authors a flow once from a natural-language spec and
records a trace; a deterministic engine replays the trace in CI with zero
LLM calls.

The Rust engine ships inside this package as the `flowproof._native`
extension module; the `flowproof` command drives it.
See https://github.com/automators-com/flowproof
"""

from flowproof.flow import (
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

__version__ = "0.2.4"

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
