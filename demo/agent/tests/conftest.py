"""A real gateway for the integration tests, started the way the conformance
harness starts one: Anvil, demo-market and `proveno-gateway serve` on free
ports, never the demo's own 8545, 8081 and 7777."""

from __future__ import annotations

import importlib.util
import json
import shutil
import socket
import subprocess
import sys
from pathlib import Path

import pytest

AGENT = Path(__file__).resolve().parents[1]
DEMO = AGENT.parent
CONFORMANCE = DEMO / "conformance"


def _load_conformance():
    spec = importlib.util.spec_from_file_location("conformance", CONFORMANCE / "conformance.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules["conformance"] = module
    # Leave no __pycache__ behind in demo/conformance.
    writes, sys.dont_write_bytecode = sys.dont_write_bytecode, True
    try:
        spec.loader.exec_module(module)
    finally:
        sys.dont_write_bytecode = writes
    return module


conformance = _load_conformance()

RUN_SH = DEMO / "run.sh"


def run_sh(*args: str) -> str:
    """`demo/run.sh` in one of its entry points that runs nothing else."""
    return subprocess.run(
        ["bash", str(RUN_SH), *args], capture_output=True, text=True, check=True
    ).stdout


# The exact task `run.sh` gives the agent. The tests use nothing else.
TASK = run_sh("--print-task").rstrip("\n")


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


@pytest.fixture(scope="session")
def world(tmp_path_factory):
    missing = [
        str(path)
        for path in (conformance.GATEWAY_BIN, conformance.MARKET_BIN, conformance.WALLET_BIN)
        if not path.exists()
    ]
    if shutil.which("anvil") is None:
        missing.append("anvil on PATH")
    if missing:
        pytest.fail(
            "the integration tests need a built gateway and demo, and Foundry: missing "
            + ", ".join(missing)
            + ". Run `make build` in demo/agent."
        )
    ports = {"anvil": _free_port(), "market": _free_port(), "gateway": _free_port()}
    world = conformance.World(tmp_path_factory.mktemp("world"), ports)
    world.write_config()
    fixture = json.loads((CONFORMANCE / "fixtures" / "rebalance.json").read_text())
    try:
        world.start_chain()
        world.start_fixture(fixture)
        world.fixture = fixture
        yield world
    finally:
        world.stop()
