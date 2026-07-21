import json
import re
import sys
from pathlib import Path

import pytest

import flowproof
from flowproof import Flow, _native

WINDOWS = sys.platform == "win32"


def test_version_is_exposed():
    assert flowproof.__version__
    assert _native.__engine_version__


def test_every_version_location_agrees():
    """A release needs FOUR places bumped in lockstep, and a mismatch is
    only ever discovered by PyPI rejecting the upload (400 File already
    exists) or by a wheel whose --version lies. The engine version comes
    from the Rust workspace Cargo.toml, so comparing all three here covers
    Cargo.toml, pyproject.toml and __init__.py.
    """
    pyproject = Path(__file__).resolve().parents[1] / "pyproject.toml"
    declared = re.search(
        r'^version = "([^"]+)"', pyproject.read_text(encoding="utf-8"), re.MULTILINE
    )
    assert declared, "pyproject.toml has no version"
    assert flowproof.__version__ == declared.group(1), (
        f"__init__.py says {flowproof.__version__}, "
        f"pyproject.toml says {declared.group(1)}"
    )
    assert flowproof.__version__ == _native.__engine_version__, (
        f"python package says {flowproof.__version__}, "
        f"rust engine says {_native.__engine_version__}"
    )


def test_public_api_surface():
    assert set(flowproof.__all__) == {
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
    }


def test_gated_record_returns_falsy_record_skipped(tmp_path, monkeypatch):
    """A spec whose skip_unless_env gate is unsatisfied records nothing and
    returns a falsy RecordSkipped naming the reason."""
    monkeypatch.delenv("PY_SUE_FLAG", raising=False)
    spec = tmp_path / "gated.flow.yaml"
    spec.write_text(
        "name: Gated\napp: api\nskip_unless_env: [PY_SUE_FLAG]\n"
        "steps:\n  - assert_api:\n      request: GET http://127.0.0.1:1/x\n"
        "      timeout_seconds: 1\n"
    )
    result = flowproof.record(spec)
    assert isinstance(result, flowproof.RecordSkipped)
    assert not result, "RecordSkipped is falsy"
    assert "PY_SUE_FLAG" in result.reason
    assert not (tmp_path / "gated.trace.jsonl").exists()


def test_gated_run_returns_skipped_run_result(tmp_path, monkeypatch):
    monkeypatch.delenv("PY_SUE_RUN_FLAG", raising=False)
    spec = tmp_path / "gated.flow.yaml"
    spec.write_text(
        "name: Gated\napp: api\nskip_unless_env: [PY_SUE_RUN_FLAG]\n"
        "steps:\n  - assert_api:\n      request: GET http://127.0.0.1:1/x\n"
        "      timeout_seconds: 1\n"
    )
    result = flowproof.run(spec)  # no trace exists; the gate wins
    assert result.passed and bool(result), "a skip is not a failure"
    assert result.skipped and "PY_SUE_RUN_FLAG" in result.skipped
    assert result.report_path is None and result.junit_path is None


def test_ambiguous_step_raises_clarification_needed(tmp_path, monkeypatch):
    """A step no rule matches, recorded with no model backend, must surface
    the structured clarification — not a bare error string. app: api works
    on any OS (no UI is launched)."""
    for var in (
        "FLOWPROOF_AI_PROVIDER",
        "FLOWPROOF_AI_API_KEY",
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
    ):
        monkeypatch.delenv(var, raising=False)
    spec = tmp_path / "vague.flow.yaml"
    spec.write_text("name: Vague\napp: api\nsteps:\n  - Frobnicate the widget\n")
    with pytest.raises(flowproof.ClarificationNeeded) as exc:
        flowproof.record(spec)
    clarification = exc.value.clarification
    assert clarification["step"] == "Frobnicate the widget"
    assert clarification["stage"] == "no_model"
    assert clarification["step_index"] == 0
    assert "hint" in clarification and "scene" in clarification


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
    # run() now loads the spec first (skip gates + parse errors surface on
    # single runs); a nonexistent spec fails there, cleanly.
    with pytest.raises(RuntimeError, match="cannot read spec"):
        flowproof.run(tmp_path / "never-recorded.flow.yaml")
    # A real spec with no recorded trace still names the trace.
    spec = tmp_path / "real.flow.yaml"
    spec.write_text("name: r\napp: api\nsteps:\n  - assert_api:\n      request: GET http://x\n")
    with pytest.raises(RuntimeError, match="cannot read trace"):
        flowproof.run(spec)


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
                    {
                        "id": "s0001",
                        "intent": "Type 5",
                        "status": "passed",
                        "duration_ms": 30,
                        "selector_tier": "structural",
                        "degraded": True,
                    },
                    {
                        "id": "s0002",
                        "intent": "display shows 8",
                        "status": "failed",
                        "duration_ms": 25,
                        "detail": "expected display value '8', got 'Display is 9'",
                    },
                ],
                "degraded": True,
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
    # Ladder-fallback drift signals: per step and for the whole run.
    assert result.degraded
    assert result.steps[0].degraded
    assert result.steps[0].selector_tier == "structural"
    assert not result.steps[1].degraded
    assert result.steps[1].selector_tier is None


def test_public_surface_includes_heal_result():
    assert "HealResult" in flowproof.__all__


def test_heal_missing_spec_is_a_clean_error():
    with pytest.raises(RuntimeError):
        flowproof.heal("flows/create-order.yaml", "flow.trace.jsonl")


def test_heal_result_parses_engine_payload():
    from flowproof.flow import _parse_heal_result

    payload = json.dumps(
        {
            "report": {
                "changed": True,
                "steps_changed": [{"id": "s0002", "intent": "Press plus", "fields": ["selectors"]}],
                "steps_added": 0,
                "steps_removed": 0,
                "proposed_path": "/tmp/calc.proposed.jsonl",
                "diff_html": "/tmp/calc.heal.html",
            },
            "applied": False,
        }
    )
    result = _parse_heal_result(payload)
    assert result.changed
    assert result.steps_changed[0]["fields"] == ["selectors"]
    assert result.proposed_path == Path("/tmp/calc.proposed.jsonl")
    assert result.diff_html == Path("/tmp/calc.heal.html")
    assert not result.applied


def test_heal_result_diff_html_is_optional():
    from flowproof.flow import _parse_heal_result

    payload = json.dumps(
        {
            "report": {
                "changed": False,
                "steps_changed": [],
                "steps_added": 0,
                "steps_removed": 0,
            },
            "applied": False,
        }
    )
    result = _parse_heal_result(payload)
    assert not result.changed
    assert result.diff_html is None
