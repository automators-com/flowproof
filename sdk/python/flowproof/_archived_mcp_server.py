"""ARCHIVED: flowproof as MCP tools, record / run / get_trace / heal over stdio.

Archived by rename, not removed. Nothing in this repository, in flowproof
desktop, or in DataMaker calls it — DataMaker uses flowproof the other way
round, as the thing that tests its agent — and no external user is known to
have installed it. It is also the only MCP surface written in Python, which
makes it the one that needs a Python install and an extra to reach, and a
native ``flowproof mcp`` subcommand is planned to replace it.

So this module keeps working exactly as before: ``flowproof-mcp`` still runs
it, the ``mcp`` extra still installs what it needs, and ``tests/test_mcp.py``
still exercises it in CI. Only the file name says "do not build on this".
Reviving it is one ``git mv``; deleting it is a separate decision, once the
native server has shipped and been used in anger.

One thing to know before that decision: ``_native.record`` opts this process
in to running suite-authored shell and spec-authored programs
(``flowproof_cli::allow_suite_exec``), which is defensible for an embedder
driving its own suite and is not what a coding agent pointed at a freshly
cloned repository should get. The native server refuses by default instead.

Requires the ``mcp`` extra: ``pip install flowproof[mcp]`` (Python 3.10+).
Run with ``flowproof-mcp`` or ``python -m flowproof._archived_mcp_server``.
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
    def flowproof_record(
        spec: str,
        out: str | None = None,
        author: str = "auto",
        recording_detail: str = "full",
        video: bool = False,
        highlight_cursor: bool = False,
    ) -> dict[str, Any]:
        """Record a flow: perform it once against the live app and write a
        deterministic trace. Requires the target platform (Windows for UIA
        apps like calc/notepad; any OS for `app: web`). `author` accepts
        auto, rules, or llm. Returns
        {"trace_path", "steps", "reused_steps", "routing"} on success, OR
        {"needs_clarification": {step, step_index, stage, reason,
        capture_reference?, capture_candidates?, rules_error?, completed_steps,
        scene: [{target, tag?, label?, text?, type?}], hint}} when a step is
        too ambiguous to author — the scene is the live screen's
        actionable/readable inventory: rewrite the stuck step into concrete grammar
        (docs/authoring.md) targeting a listed element, consulting your data
        source for domain questions (e.g. which fields are required), then
        call this tool again. A spec whose skip_unless_env gate is not
        satisfied returns {"skipped": reason} — nothing was recorded.
        recording_detail (full/low/off), video and highlight_cursor are the
        visual-capture controls described on flowproof_run."""
        return json.loads(
            _native.record(spec, out, author, recording_detail, video, highlight_cursor)
        )

    @mcp.tool()
    def flowproof_run(
        spec: str,
        trace: str | None = None,
        recording_detail: str = "full",
        video: bool = False,
        highlight_cursor: bool = False,
    ) -> dict[str, Any]:
        """Deterministically replay a recorded flow (zero LLM calls). A
        failing test is data ({"report": {"passed": false, ...}}), not an
        error. Returns {"report", "report_path"}; a spec whose
        skip_unless_env gate is not satisfied returns a skipped report with
        {"report_path": null, "skipped": reason} — the flow never ran.
        recording_detail picks the visual-capture density: "full" (before and
        after every step), "low" (initial state, every fifth completed step,
        final state), or "off". video assembles the checkpoints into
        recording.gif (off by default); highlight_cursor draws a visible
        cursor and click halo. All three affect artifacts only, never the
        verdict."""
        return json.loads(_native.run(spec, trace, recording_detail, video, highlight_cursor))

    @mcp.tool()
    def flowproof_get_trace(path: str) -> dict[str, Any]:
        """Load a recorded trace for inspection. `path` may be the flow spec
        (the default trace next to it is used) or a .jsonl trace file.
        Returns {"header", "steps"}."""
        return json.loads(_native.get_trace(path))

    @mcp.tool()
    def flowproof_heal(
        spec: str,
        trace: str | None = None,
        apply: bool = False,
        author: str = "auto",
    ) -> dict[str, Any]:
        """Re-author the flow against the live app and propose a reviewable
        trace diff (written as *.proposed.jsonl). Never modifies the trace
        unless apply=true is passed explicitly. Returns
        {"report": {"changed", "steps_changed", ...}, "applied"}; author
        accepts auto, rules, or llm."""
        return json.loads(_native.heal(spec, trace, apply, author))


def main() -> None:
    if FastMCP is None:
        raise SystemExit(
            "flowproof-mcp (archived) requires the 'mcp' extra (Python 3.10+): "
            f"pip install 'flowproof[mcp]' ({_IMPORT_ERROR})"
        )
    mcp.run()


if __name__ == "__main__":
    main()
