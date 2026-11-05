import asyncio
import json
from pathlib import Path

import pytest

from conftest import FakeEngine
from test_server import roundtrip, run_server, server_cleanup  # noqa: F401  (autouse fixture)

FIXTURES = sorted((Path(__file__).parents[2] / "protocol" / "fixtures").glob("*.ndjson"))
TERMINAL = {"done", "error", "status"}
ANY = "<any>"


def assert_matches(expected, actual, where="event"):
    if expected == ANY:
        return
    if isinstance(expected, dict):
        assert isinstance(actual, dict), f"{where}: expected object, got {actual!r}"
        for key, value in expected.items():
            if key.startswith("x_"):
                continue
            assert key in actual, f"{where}: missing key {key!r} in {actual!r}"
            assert_matches(value, actual[key], f"{where}.{key}")
    else:
        assert expected == actual, f"{where}: expected {expected!r}, got {actual!r}"


def engine_for(fake: dict) -> FakeEngine:
    engine = FakeEngine.from_fixture(fake)
    # Fixtures that do not exercise loading describe a daemon whose model is already resident.
    engine.loaded = not (fake.get("download") or "load_error" in fake)
    return engine


@pytest.mark.parametrize("path", FIXTURES, ids=lambda p: p.stem)
async def test_fixture_conformance(path):
    fake_line, request, *expected = [json.loads(line) for line in path.read_text().splitlines() if line.strip()]
    line = request["raw"].encode() if set(request) == {"raw"} else json.dumps(request).encode()
    _, sock, _ = await run_server(engine_for(fake_line["fake"]))

    actual = await asyncio.wait_for(roundtrip(sock, line), 5)  # returns only after the daemon closes (EOF)

    assert len(actual) == len(expected), f"expected {len(expected)} events, got {actual!r}"
    for i, (exp, act) in enumerate(zip(expected, actual)):
        assert_matches(exp, act, f"event[{i}]")
    assert [e["type"] in TERMINAL for e in actual] == [False] * (len(actual) - 1) + [True]
