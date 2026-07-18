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
    assert set(flowproof.__all__) == {"Flow", "__version__", "heal", "record", "run"}


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


@pytest.mark.skipif(WINDOWS, reason="on Windows the engine would actually drive the app")
def test_engine_reports_unsupported_platform_off_windows(tmp_path):
    spec = tmp_path / "calc.flow.yaml"
    spec.write_text("name: Add\napp: calc\nsteps:\n  - Type 5\n")
    with pytest.raises(RuntimeError, match="not supported on this platform"):
        flowproof.record(spec)


def test_heal_is_not_wired_yet():
    with pytest.raises(NotImplementedError):
        flowproof.heal("flows/create-order.yaml", "flow.trace.jsonl")
