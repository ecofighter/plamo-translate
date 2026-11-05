import asyncio
import json
import socket
import time
from pathlib import Path

import pytest

from conftest import FakeEngine
from plamo_translated.engine import TextStream
from plamo_translated.protocol import MAX_LINE_BYTES
from plamo_translated.server import MAX_QUEUE, Server

VERSION = "9.9.9"

_sock_dir: Path | None = None
_started: list[tuple[Server, asyncio.Task]] = []


@pytest.fixture(autouse=True)
async def server_cleanup(short_dir):
    global _sock_dir
    _sock_dir = short_dir
    _started.clear()
    yield
    for server, task in _started:
        if not task.done():
            server.request_shutdown()
        await asyncio.wait_for(task, 5)
    _started.clear()


async def run_server(engine, idle=600, sock_cls=socket.socket) -> tuple[Server, Path, asyncio.Task]:
    path = _sock_dir / f"d{len(_started)}.sock"
    sock = sock_cls(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.bind(str(path))
    sock.listen(64)
    server = Server(engine, idle_timeout_secs=idle, version=VERSION)
    task = asyncio.create_task(server.serve(sock))
    _started.append((server, task))
    return server, path, task


async def send(path, line: bytes):
    reader, writer = await asyncio.open_unix_connection(str(path), limit=MAX_LINE_BYTES * 2)
    writer.write(line + b"\n")
    await writer.drain()
    return reader, writer


async def read_event(reader) -> dict:
    line = await reader.readline()
    assert line, "connection closed before a terminal event"
    return json.loads(line)


async def read_all(reader) -> list[dict]:
    events = []
    while line := await reader.readline():
        events.append(json.loads(line))
    return events


async def roundtrip(path, line: bytes) -> list[dict]:
    reader, writer = await send(path, line)
    try:
        return await asyncio.wait_for(read_all(reader), 10)
    finally:
        writer.close()


async def roundtrip_with(engine, line: bytes) -> list[dict]:
    _, path, _ = await run_server(engine)
    return await roundtrip(path, line)


def translate(text="x", **fields) -> bytes:
    return json.dumps({"v": 1, "op": "translate", "text": text, "target_lang": "ja", **fields}).encode()


STATUS = b'{"v":1,"op":"status"}'
LOAD = b'{"v":1,"op":"load"}'


async def status_of(path) -> dict:
    (ev,) = await roundtrip(path, STATUS)
    return ev


async def wait_until(predicate, timeout=3.0):
    deadline = time.monotonic() + timeout
    while not predicate():
        assert time.monotonic() < deadline, "condition not reached in time"
        await asyncio.sleep(0.01)


async def wait_queue_len(path, n, timeout=3.0):
    deadline = time.monotonic() + timeout
    while (await status_of(path))["queue_len"] != n:
        assert time.monotonic() < deadline, f"queue_len never reached {n}"
        await asyncio.sleep(0.01)


def loaded(**kw) -> FakeEngine:
    engine = FakeEngine(**kw)
    engine.loaded = True
    return engine


async def test_done_text_equals_concatenated_deltas():
    ev = await roundtrip_with(loaded(tokens=["会", "議は", "変更"]), b'{"v":1,"op":"translate","text":"x","target_lang":"ja"}')
    assert "".join(e["text"] for e in ev if e["type"] == "delta") == ev[-1]["text"] == "会議は変更"
    assert ev[-1]["usage"] == {"input_tokens": 5, "output_tokens": 3}


async def test_first_translate_relays_load_progress():
    ev = await roundtrip_with(FakeEngine(tokens=["a"], download=True), translate())
    assert [e["type"] for e in ev] == ["progress", "progress", "delta", "done"]
    assert [e["stage"] for e in ev[:2]] == ["downloading", "loading"]


async def test_requests_are_processed_fifo():
    engine = loaded(tokens=["a", "b", "c"], token_delay=0.05)
    _, path, _ = await run_server(engine)
    arrivals = []

    async def client(i):
        events = await roundtrip(path, translate(str(i)))
        assert events[-1]["type"] == "done"
        arrivals.append(i)

    tasks = []
    for i in range(3):
        tasks.append(asyncio.create_task(client(i)))
        await asyncio.sleep(0.02)
    await asyncio.gather(*tasks)
    assert [r.text for r in engine.calls] == ["0", "1", "2"]
    assert arrivals == [0, 1, 2]


async def test_busy_when_queue_full():
    _, path, _ = await run_server(loaded(tokens=["a", "b"], token_delay=1))
    clients = [await send(path, translate("running"))]
    assert (await read_event(clients[0][0]))["type"] == "delta"
    for i in range(MAX_QUEUE):
        clients.append(await send(path, translate(f"queued{i}")))
    await wait_queue_len(path, MAX_QUEUE)

    ev = await asyncio.wait_for(roundtrip(path, translate("rejected")), 0.5)
    assert len(ev) == 1
    assert (ev[0]["type"], ev[0]["code"]) == ("error", "busy")
    for _, writer in clients:
        writer.close()


async def test_load_op_is_queued_like_translate_and_status_bypasses_queue():
    _, path, _ = await run_server(loaded(tokens=["a", "b"], token_delay=1))
    clients = [await send(path, translate("running"))]
    await read_event(clients[0][0])
    for i in range(MAX_QUEUE):
        clients.append(await send(path, translate(f"queued{i}")))
    await wait_queue_len(path, MAX_QUEUE)

    (ev,) = await asyncio.wait_for(roundtrip(path, LOAD), 0.5)
    assert ev["code"] == "busy"
    assert (await asyncio.wait_for(status_of(path), 0.5))["queue_len"] == MAX_QUEUE
    for _, writer in clients:
        writer.close()


async def test_second_request_during_load_is_queued_not_rejected():
    _, path, _ = await run_server(FakeEngine(tokens=["a"], load_delay=0.5))
    results = await asyncio.gather(roundtrip(path, translate("1")), roundtrip(path, translate("2")))
    assert [r[-1]["type"] for r in results] == ["done", "done"]


async def test_disconnect_during_generation_frees_worker():
    engine = loaded(tokens=["a"] * 1000, token_delay=0.01)
    _, path, _ = await run_server(engine)
    reader, writer = await send(path, translate("long"))
    assert (await read_event(reader))["type"] == "delta"
    writer.close()

    ev = await asyncio.wait_for(roundtrip(path, translate("short", max_tokens=1)), 0.5)
    assert ev[-1]["type"] == "done"
    assert engine.saw_cancel


async def test_disconnect_during_nonstream_generation_frees_worker():
    engine = loaded(tokens=["a"] * 1000, token_delay=0.01)
    _, path, _ = await run_server(engine)
    _, writer = await send(path, translate("long", stream=False))
    await wait_until(lambda: engine.calls)
    await asyncio.sleep(0.05)
    writer.close()

    ev = await asyncio.wait_for(roundtrip(path, translate("short", max_tokens=1)), 0.5)
    assert ev[-1]["type"] == "done"
    assert engine.saw_cancel


async def test_disconnect_while_queued_removes_request():
    engine = loaded(tokens=["a", "b", "c"], token_delay=0.5)
    _, path, _ = await run_server(engine)
    first_reader, first_writer = await send(path, translate("first"))
    assert (await read_event(first_reader))["type"] == "delta"
    _, second_writer = await send(path, translate("second"))
    await wait_queue_len(path, 1)
    second_writer.close()
    await wait_queue_len(path, 0, timeout=0.3)  # freed while "first" is still generating

    rest = await asyncio.wait_for(read_all(first_reader), 3)
    first_writer.close()
    assert rest[-1]["type"] == "done"
    await asyncio.sleep(0.1)
    assert [r.text for r in engine.calls] == ["first"]


async def test_stream_false_has_only_done():
    ev = await roundtrip_with(loaded(tokens=["a", "b"]), translate(stream=False))
    assert [e["type"] for e in ev] == ["done"]
    assert ev[0]["text"] == "ab"
    assert ev[0]["usage"] == {"input_tokens": 5, "output_tokens": 2}


async def test_load_op_returns_empty_done():
    engine = FakeEngine()
    ev = await roundtrip_with(engine, LOAD)
    assert [e["type"] for e in ev] == ["progress", "done"]
    assert ev[-1]["text"] == ""
    assert ev[-1]["usage"] == {"input_tokens": 0, "output_tokens": 0}
    assert engine.loaded


async def test_load_failure_keeps_serving_and_next_request_retries():
    engine = FakeEngine(tokens=["a"], load_error="network down")
    _, path, task = await run_server(engine)
    ev = await roundtrip(path, translate())
    assert [(e["type"], e.get("code"), e.get("message")) for e in ev] == [
        ("error", "model_load_failed", "network down")
    ]
    engine.load_error = None
    ev = await roundtrip(path, translate())
    assert ev[-1]["type"] == "done"
    assert not task.done()


async def test_context_too_long_with_explicit_max_tokens_reports_counts():
    (ev,) = await roundtrip_with(loaded(input_tokens=32000), translate(max_tokens=1000))
    assert ev["code"] == "context_too_long"
    assert "32000" in ev["message"] and "1000" in ev["message"]


async def test_context_at_limit_is_allowed():
    engine = loaded(input_tokens=32000, tokens=["a"])
    ev = await roundtrip_with(engine, translate(max_tokens=768))
    assert ev[-1]["type"] == "done"


async def test_default_max_tokens_nonpositive_is_context_too_long():
    (ev,) = await roundtrip_with(loaded(input_tokens=32768), translate())
    assert ev["code"] == "context_too_long"
    assert "32768" in ev["message"]


async def test_default_max_tokens_caps_generation():
    engine = loaded(input_tokens=32766, tokens=["a", "b", "c", "d"])
    ev = await roundtrip_with(engine, translate())
    assert ev[-1]["text"] == "ab"


async def test_unexpected_engine_exception_is_internal_and_server_survives():
    class Exploding(FakeEngine):
        def count_tokens(self, req):
            if req.text == "boom":
                raise RuntimeError("kaboom")
            return super().count_tokens(req)

    _, path, task = await run_server(Exploding(tokens=["a"]))
    (ev,) = [e for e in await roundtrip(path, translate("boom")) if e["type"] != "progress"]
    assert (ev["type"], ev["code"]) == ("error", "internal")
    assert "kaboom" in ev["message"]
    assert (await roundtrip(path, translate("fine")))[-1]["type"] == "done"
    assert not task.done()


async def test_idle_timeout_exits_serve():
    _, path, task = await run_server(loaded(), idle=1)
    assert (await roundtrip(path, translate()))[-1]["type"] == "done"
    await asyncio.wait_for(task, 3)
    assert not path.exists() or not _connectable(path)


def _connectable(path) -> bool:
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        s.connect(str(path))
        return True
    except OSError:
        return False
    finally:
        s.close()


async def test_idle_zero_disables_idle_exit():
    _, path, task = await run_server(loaded(), idle=0)
    assert (await roundtrip(path, translate()))[-1]["type"] == "done"
    await asyncio.sleep(1.2)
    assert not task.done()


async def test_idle_timer_does_not_fire_while_connection_open():
    _, path, task = await run_server(loaded(), idle=1)
    reader, writer = await asyncio.open_unix_connection(str(path))
    await asyncio.sleep(1.5)
    assert not task.done()
    writer.write(translate() + b"\n")
    assert (await asyncio.wait_for(read_all(reader), 3))[-1]["type"] == "done"
    writer.close()
    await asyncio.wait_for(task, 3)


async def test_idle_does_not_drop_connection_accepted_at_close():
    server, path, task = await run_server(loaded(tokens=["a", "b"], token_delay=0.5), idle=1)
    late = {}

    async def before_close():
        late["conn"] = await send(path, translate("late"))
        await wait_until(lambda: server._connections > 0)

    server._before_close = before_close
    await asyncio.wait_for(task, 5)
    reader, writer = late["conn"]
    # serve() must not return before the late request completed: everything is already sent.
    events = await asyncio.wait_for(read_all(reader), 0.2)
    writer.close()
    assert events[-1]["type"] == "done"
    assert events[-1]["text"] == "ab"


async def test_idle_does_not_drop_connection_accepted_but_not_yet_handled():
    accepted = asyncio.Event()

    class ObservedListener(socket.socket):
        def accept(self):
            conn = super().accept()
            accepted.set()
            return conn

    server, path, task = await run_server(loaded(tokens=["a", "b"]), idle=1, sock_cls=ObservedListener)
    late = {}

    async def before_close():
        late["conn"] = await send(path, translate("late"))
        # Close as soon as the kernel-level accept() has happened, before the server can react further.
        await accepted.wait()

    server._before_close = before_close
    await asyncio.wait_for(task, 5)
    reader, writer = late["conn"]
    events = await asyncio.wait_for(read_all(reader), 0.2)
    writer.close()
    assert [e["type"] for e in events] == ["delta", "delta", "done"]


async def test_shutdown_sends_shutting_down():
    server, path, task = await run_server(loaded(tokens=["a"] * 1000, token_delay=0.01))
    running_reader, running_writer = await send(path, translate("running"))
    assert (await read_event(running_reader))["type"] == "delta"
    queued_reader, queued_writer = await send(path, translate("queued"))
    await wait_queue_len(path, 1)

    server.request_shutdown()
    running = await asyncio.wait_for(read_all(running_reader), 3)
    queued = await asyncio.wait_for(read_all(queued_reader), 3)
    running_writer.close()
    queued_writer.close()
    assert (running[-1]["type"], running[-1]["code"]) == ("error", "shutting_down")
    assert [(e["type"], e["code"]) for e in queued] == [("error", "shutting_down")]
    await asyncio.wait_for(task, 3)


async def test_shutdown_while_idle_returns():
    server, _, task = await run_server(loaded())
    await asyncio.sleep(0.05)
    server.request_shutdown()
    await asyncio.wait_for(task, 3)


async def test_line_too_long_is_invalid_request():
    (ev,) = await roundtrip_with(loaded(), b"x" * (MAX_LINE_BYTES + 1))
    assert (ev["type"], ev["code"]) == ("error", "invalid_request")


async def test_line_of_exactly_max_bytes_is_accepted():
    line = STATUS + b" " * (MAX_LINE_BYTES - len(STATUS))
    assert len(line) == MAX_LINE_BYTES
    (ev,) = await roundtrip_with(loaded(), line)
    assert ev["type"] == "status"


async def test_eof_before_request_closes_quietly():
    _, path, task = await run_server(loaded())
    reader, writer = await asyncio.open_unix_connection(str(path))
    writer.write(b'{"v":1,')
    await writer.drain()
    writer.write_eof()
    assert await asyncio.wait_for(reader.read(), 3) == b""
    writer.close()
    assert (await status_of(path))["type"] == "status"
    assert not task.done()


async def test_status_reports_fields():
    _, path, _ = await run_server(FakeEngine())
    ev = await status_of(path)
    assert ev == {
        "type": "status",
        "model": "fake/model",
        "revision": "fake-revision",
        "loaded": False,
        "uptime_secs": ev["uptime_secs"],
        "idle_timeout_secs": 600,
        "queue_len": 0,
        "version": VERSION,
        "protocol": 1,
    }
    assert isinstance(ev["uptime_secs"], int) and ev["uptime_secs"] >= 0
    await roundtrip(path, LOAD)
    assert (await status_of(path))["loaded"] is True


async def test_validation_precedes_load():
    engine = FakeEngine()
    ev = await roundtrip_with(engine, b'{"v":1,"op":"translate","text":"<|plamo:tag|>x","target_lang":"en"}')
    assert ev == [{"type": "error", "code": "invalid_request", "message": "text must not contain reserved PFML tokens (<|plamo:...)."}]
    assert engine.loaded is False and engine.calls == []


async def test_validation_precedes_busy_and_shutting_down():
    server, path, _ = await run_server(loaded(tokens=["a", "b"], token_delay=1))
    clients = [await send(path, translate("running"))]
    await read_event(clients[0][0])
    for i in range(MAX_QUEUE):
        clients.append(await send(path, translate(f"queued{i}")))
    await wait_queue_len(path, MAX_QUEUE)
    (ev,) = await asyncio.wait_for(roundtrip(path, translate("", target_lang="en")), 0.5)
    assert (ev["code"], ev["message"]) == ("invalid_request", "text must not be empty.")
    server._shutting_down = True
    (ev,) = await asyncio.wait_for(roundtrip(path, translate("x", form="formal")), 0.5)
    assert ev["code"] == "invalid_request"
    for _, writer in clients:
        writer.close()


async def test_done_strips_one_trailing_stop_token():
    ev = await roundtrip_with(loaded(tokens=["Hi", "<|plamo:tag|>"]), translate())
    assert ev[-1]["text"] == "Hi"
    assert [e["text"] for e in ev if e["type"] == "delta"] == ["Hi", "<|plamo:tag|>"]
    assert ev[-1]["usage"]["output_tokens"] == 2


@pytest.mark.parametrize(("tokens", "text"), [
    (["a<|plamo:bos|>"], "a"),
    (["a", "<|plamo:tag|>", "<|plamo:tag|>"], "a<|plamo:tag|>"),
    (["<|plamo:tag|>", "a"], "<|plamo:tag|>a"),
    (["a ", "\n"], "a \n"),
    (["a<|plamo:msg|>"], "a<|plamo:msg|>"),
])
async def test_done_text_is_unmodified_except_one_stop_suffix(tokens, text):
    ev = await roundtrip_with(loaded(tokens=tokens), translate(stream=False))
    assert ev[-1]["text"] == text


async def test_done_reports_finish_reason():
    stop = await roundtrip_with(loaded(tokens=["a", "b"]), translate(max_tokens=3))
    length = await roundtrip_with(loaded(tokens=["a", "b", "c"]), translate(max_tokens=2))
    assert (stop[-1]["finish_reason"], length[-1]["finish_reason"]) == ("stop", "length")
    assert length[-1]["text"] == "ab"


async def test_load_done_has_no_finish_reason():
    ev = await roundtrip_with(FakeEngine(), LOAD)
    assert "finish_reason" not in ev[-1]


async def test_engine_without_finish_reason_is_internal_error():
    class NoReason(FakeEngine):
        def stream(self, req, max_tokens, cancelled):
            return TextStream([("a", None)])

    ev = await roundtrip_with(NoReason(), translate(stream=False))
    assert (ev[-1]["type"], ev[-1]["code"]) == ("error", "internal")
