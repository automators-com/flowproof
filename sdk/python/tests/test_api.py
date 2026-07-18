import json
import sys
from pathlib import Path

import pytest

import flowproof
from flowproof import Flow, _native

WINDOWS = sys.platform == "win32"


def test_version_is_exposed():
    assert flowproof.__version__
    assert _native.__engine_version__


def test_public_api_surface():
    assert set(flowproof.__all__) == {
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
    }


def test_flow_normalizes_spec_to_path():
    flow = Flow("flows/create-order.yaml")
    assert flow.spec == Path("flows/create-order.yaml")


def test_cli_help_exits_zero():
    assert _native.cli_main(["--help"]) == 0


def test_cli_unknown_command_errors():
    assert _native.cli_main(["frobnicate"]) == 2


def test_missing_spec_is_a_clean_error():
    with pytest.raises(RuntimeError):
        flowproof.record("does-not-exist.flow.yaml")


def test_missing_trace_is_a_clean_error(tmp_path):
    with pytest.raises(RuntimeError, match="cannot read trace"):
        flowproof.run(tmp_path / "never-recorded.flow.yaml")


@pytest.mark.skipif(WINDOWS, reason="on Windows the engine would actually drive the app")
def test_engine_reports_unsupported_platform_off_windows(tmp_path):
    spec = tmp_path / "calc.flow.yaml"
    spec.write_text("name: Add\napp: calc\nsteps:\n  - Type 5\n")
    with pytest.raises(RuntimeError, match="not supported on this platform"):
        flowproof.record(spec)


def _write_sample_trace(path: Path) -> None:
    header = {
        "format": "flowproof-trace",
        "version": 1,
        "trace_id": "5f0f2f6e-6f0a-4c25-9b1c-1a2b3c4d5e6f",
        "recorded_at": "2026-07-18T10:12:33Z",
        "spec": {"name": "Add two numbers"},
        "app": {"name": "calc", "adapter": "uia", "window_title": "Calculator"},
        "env": {"os": "windows", "resolution": [1920, 1080]},
    }
    step = {
        "id": "s0001",
        "intent": "Type 5",
        "action": {"type": "click", "params": {}},
        "selectors": [
            {
                "tier": "native_id",
                "provenance": "uia",
                "confidence": 1.0,
                "payload": {"automation_id": "num5Button", "name": "Five"},
            }
        ],
        "sync": {
            "pre": [{"kind": "element_exists", "selector_ref": 0, "timeout_ms": 5000}],
            "post": [],
        },
        "artifacts": {},
    }
    path.write_text(json.dumps(header) + "\n" + json.dumps(step) + "\n")


def test_get_trace_returns_structured_trace(tmp_path):
    trace_path = tmp_path / "calc.trace.jsonl"
    _write_sample_trace(trace_path)

    # Directly from the trace file...
    trace = flowproof.get_trace(trace_path)
    assert trace["header"]["format"] == "flowproof-trace"
    assert trace["header"]["app"]["name"] == "calc"
    assert len(trace["steps"]) == 1
    assert trace["steps"][0]["selectors"][0]["payload"]["automation_id"] == "num5Button"

    # ...and via the spec path (default trace next to it).
    trace_via_spec = Flow(tmp_path / "calc.flow.yaml").get_trace()
    assert trace_via_spec == trace


def test_get_trace_missing_file_is_a_clean_error(tmp_path):
    with pytest.raises(RuntimeError, match="cannot read trace"):
        flowproof.get_trace(tmp_path / "nope.trace.jsonl")


def test_run_result_parses_engine_payload():
    from flowproof.flow import _parse_run_result

    payload = json.dumps(
        {
            "report": {
                "name": "Add two numbers",
                "trace_id": "abc",
                "passed": False,
                "duration_ms": 1200,
                "steps": [
                    {"id": "s0001", "intent": "Type 5", "status": "passed", "duration_ms": 30},
                    {
                        "id": "s0002",
                        "intent": "display shows 8",
                        "status": "failed",
                        "duration_ms": 25,
                        "detail": "expected display value '8', got 'Display is 9'",
                    },
                ],
            },
            "report_path": "/tmp/result.json",
        }
    )
    result = _parse_run_result(payload)
    assert not result  # truthiness == passed
    assert result.steps[0].status == "passed"
    assert result.steps[1].detail is not None
    assert result.report_path == Path("/tmp/result.json")
    assert result.html_path == Path("/tmp/report.html")
    assert result.junit_path == Path("/tmp/junit.xml")
    assert result.recording is None
    assert result.steps[1].started_ms == 0


def test_public_surface_includes_heal_result():
    assert "HealResult" in flowproof.__all__


def test_heal_missing_spec_is_a_clean_error():
    with pytest.raises(RuntimeError):
        flowproof.heal("flows/create-order.yaml", "flow.trace.jsonl")
