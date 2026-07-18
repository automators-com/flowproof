"""flowproof as MCP tools: record, run, get_trace, heal over stdio.

Lets any MCP-capable agent (Claude, opencode, DataMaker, ...) drive
flowproof directly. Thin wrappers over the same engine the CLI uses;
every tool takes and returns JSON-serializable data.

Requires the ``mcp`` extra: ``pip install flowproof[mcp]`` (Python 3.10+).
Run with ``flowproof-mcp`` or ``python -m flowproof.mcp_server``.
"""

from __future__ import annotations

import json
from typing import Any

from flowproof import _native

try:
    from mcp.server.fastmcp import FastMCP
except ImportError as exc:  # pragma: no cover - exercised via main() guard
    FastMCP = None
    _IMPORT_ERROR = exc

if FastMCP is not None:
    mcp = FastMCP("flowproof")

    @mcp.tool()
    def flowproof_record(spec: str, out: str | None = None) -> dict[str, Any]:
        """Record a flow: perform it once against the live app and write a
        deterministic trace. Requires the target platform (Windows for UIA
        apps like calc/notepad; any OS for `app: web`). Returns
        {"trace_path", "steps"}."""
        return json.loads(_native.record(spec, out))

    @mcp.tool()
    def flowproof_run(spec: str, trace: str | None = None) -> dict[str, Any]:
        """Deterministically replay a recorded flow (zero LLM calls). A
        failing test is data ({"report": {"passed": false, ...}}), not an
        error. Returns {"report", "report_path"}."""
        return json.loads(_native.run(spec, trace))

    @mcp.tool()
    def flowproof_get_trace(path: str) -> dict[str, Any]:
        """Load a recorded trace for inspection. `path` may be the flow spec
        (the default trace next to it is used) or a .jsonl trace file.
        Returns {"header", "steps"}."""
        return json.loads(_native.get_trace(path))

    @mcp.tool()
    def flowproof_heal(spec: str, trace: str) -> dict[str, Any]:
        """Propose a reviewable fix for a trace that no longer replays.
        Not implemented yet - healing always produces a human-reviewable
        diff, never a silent mutation."""
        raise NotImplementedError(
            "healing is not implemented yet; see https://github.com/automators-com/flowproof"
        )


def main() -> None:
    if FastMCP is None:
        raise SystemExit(
            "flowproof-mcp requires the 'mcp' extra (Python 3.10+): "
            f"pip install 'flowproof[mcp]' ({_IMPORT_ERROR})"
        )
    mcp.run()


if __name__ == "__main__":
    main()
