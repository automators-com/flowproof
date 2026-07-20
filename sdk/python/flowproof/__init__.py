"""FlowProof — AI-native E2E testing for the apps Selenium can't reach.

Open-source AI-native E2E testing framework for enterprise Windows apps
(SAP GUI, Oracle, Citrix, legacy desktop). AI authors a flow once from a
natural-language spec and records a trace; a deterministic engine replays
the trace in CI with zero LLM calls.

The Rust engine ships inside this package as the `flowproof._native`
extension module; the `flowproof` command drives it.
See https://github.com/automators-com/flowproof
"""

from flowproof.flow import (
    ClarificationNeeded,
    Flow,
    HealResult,
    RecordResult,
    RunResult,
    StepResult,
    get_trace,
    heal,
    record,
    run,
)

__version__ = "0.1.0"

__all__ = [
    "ClarificationNeeded",
    "Flow",
    "HealResult",
    "RecordResult",
    "RunResult",
    "StepResult",
    "__version__",
    "get_trace",
    "heal",
    "record",
    "run",
]
