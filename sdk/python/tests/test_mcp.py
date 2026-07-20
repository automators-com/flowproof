"""Integration test: the flowproof MCP server over real stdio."""

import json
import sys

import pytest

mcp = pytest.importorskip("mcp", reason="mcp extra requires Python 3.10+")

from mcp import ClientSession, StdioServerParameters  # noqa: E402
from mcp.client.stdio import stdio_client  # noqa: E402
from test_api import _write_sample_trace  # noqa: E402

EXPECTED_TOOLS = {
    "flowproof_record",
    "flowproof_run",
    "flowproof_get_trace",
    "flowproof_heal",
}


async def test_mcp_server_lists_and_calls_tools(tmp_path):
    trace_path = tmp_path / "calc.trace.jsonl"
    _write_sample_trace(trace_path)

    params = StdioServerParameters(command=sys.executable, args=["-m", "flowproof.mcp_server"])
    async with stdio_client(params) as (read, write), ClientSession(read, write) as session:
        await session.initialize()

        tools = await session.list_tools()
        names = {tool.name for tool in tools.tools}
        assert names >= EXPECTED_TOOLS

        result = await session.call_tool("flowproof_get_trace", {"path": str(trace_path)})
        assert not result.isError
        payload = json.loads(result.content[0].text)
        assert payload["header"]["format"] == "flowproof-trace"
        assert payload["steps"][0]["intent"] == "Type 5"

        # heal with a nonexistent spec surfaces a clean tool error, not a crash.
        healed = await session.call_tool(
            "flowproof_heal", {"spec": "x.flow.yaml", "trace": str(trace_path)}
        )
        assert healed.isError


async def test_mcp_record_returns_clarification_as_data(tmp_path, monkeypatch):
    """An unauthorable step must come back as a structured tool RESULT
    (needs_clarification payload), not a tool error — the calling agent is
    expected to act on it."""
    for var in (
        "FLOWPROOF_AI_PROVIDER",
        "FLOWPROOF_AI_API_KEY",
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
    ):
        monkeypatch.delenv(var, raising=False)
    spec = tmp_path / "vague.flow.yaml"
    spec.write_text("name: Vague\napp: api\nsteps:\n  - Frobnicate the widget\n")

    params = StdioServerParameters(command=sys.executable, args=["-m", "flowproof.mcp_server"])
    async with stdio_client(params) as (read, write), ClientSession(read, write) as session:
        await session.initialize()
        result = await session.call_tool("flowproof_record", {"spec": str(spec)})
        assert not result.isError, "clarification is data, not an error"
        payload = json.loads(result.content[0].text)
        clarification = payload["needs_clarification"]
        assert clarification["step"] == "Frobnicate the widget"
        assert clarification["stage"] == "no_model"
