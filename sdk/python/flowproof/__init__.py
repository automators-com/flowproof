"""FlowProof — AI-native E2E testing for the apps Selenium can't reach.

Open-source AI-native E2E testing framework for enterprise Windows apps
(SAP GUI, Oracle, Citrix, legacy desktop). AI authors a flow once from a
natural-language spec and records a trace; a deterministic engine replays
the trace in CI with zero LLM calls.

This SDK is an early skeleton: the API surface is real, the engine calls
are not wired up yet (they will bind to the Rust engine via PyO3/maturin).
See https://github.com/automators-com/flowproof
"""

from flowproof.flow import Flow, heal, record, run

__version__ = "0.0.1"

__all__ = ["Flow", "__version__", "heal", "record", "run"]
