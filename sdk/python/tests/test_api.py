from pathlib import Path

import pytest

import flowproof
from flowproof import Flow


def test_version_is_exposed():
    assert flowproof.__version__


def test_public_api_surface():
    assert set(flowproof.__all__) == {"Flow", "__version__", "heal", "record", "run"}


def test_flow_normalizes_spec_to_path():
    flow = Flow("flows/create-order.yaml")
    assert flow.spec == Path("flows/create-order.yaml")


@pytest.mark.parametrize("method", ["record", "run"])
def test_engine_calls_are_not_wired_yet(method):
    flow = Flow("flows/create-order.yaml")
    with pytest.raises(NotImplementedError):
        getattr(flow, method)()


def test_heal_is_not_wired_yet():
    with pytest.raises(NotImplementedError):
        flowproof.heal("flows/create-order.yaml", "flow.trace.jsonl")
