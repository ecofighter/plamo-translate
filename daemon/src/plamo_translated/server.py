from __future__ import annotations

import asyncio
import contextlib
import logging
import socket
import threading
import time
from collections import deque
from collections.abc import Awaitable, Callable
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field

from . import pfml3, protocol
from .engine import Engine, ProgressEvent
from .protocol import (
    MAX_CONTEXT_TOKENS,
    MAX_LINE_BYTES,
    PROTOCOL_VERSION,
    DaemonError,
    LoadRequest,
    StatusRequest,
    TranslateRequest,
    default_max_tokens,
    encode_event,
    parse_request,
)

MAX_QUEUE = 8
# Request-side capabilities a client may check in `status` before relying on them.
FEATURES = ("history",)
SHUTDOWN_GRACE_SECS = 2.0
_DISCARD_CHUNK = 64 * 1024
_TERMINAL = frozenset({"done", "error", "status"})

log = logging.getLogger(__name__)


@dataclass(eq=False)
class _Job:
    req: TranslateRequest | LoadRequest
    loop: asyncio.AbstractEventLoop
    events: asyncio.Queue = field(default_factory=asyncio.Queue)
    cancelled: threading.Event = field(default_factory=threading.Event)
    finished: bool = False

    def deliver(self, event: dict) -> None:
        """Loop thread only. Events after the terminal one are dropped."""
        if self.finished:
            return
        self.events.put_nowait(event)
        if event["type"] in _TERMINAL:
            self.finished = True

    def emit(self, event: dict) -> None:
        """Any thread, including huggingface_hub download threads."""
        with contextlib.suppress(RuntimeError):  # the loop is already closed after serve() returned
            self.loop.call_soon_threadsafe(self.deliver, event)


class Server:
    def __init__(self, engine: Engine, idle_timeout_secs: int, version: str):
        self._engine = engine
        self._idle_timeout = idle_timeout_secs
        self._version = version
        self._started = time.monotonic()
        self._pending: deque[_Job] = deque()
        self._current: _Job | None = None
        self._connections = 0
        self._handlers: set[asyncio.Task] = set()
        self._shutting_down = False
        self._stop = asyncio.Event()
        self._job_ready = asyncio.Event()
        self._activity = asyncio.Event()
        self._idle_since = time.monotonic()
        # Test seam: runs after the idle decision, right before the listener closes.
        self._before_close: Callable[[], Awaitable[None]] | None = None

    def request_shutdown(self) -> None:
        """Call from the event loop thread (e.g. via loop.add_signal_handler)."""
        self._stop.set()

    async def serve(self, sock: socket.socket) -> None:
        loop = asyncio.get_running_loop()
        # One fixed thread: mlx-lm generation streams are thread-local.
        executor = ThreadPoolExecutor(max_workers=1, thread_name_prefix="plamo-worker")
        listener = _Listener(loop, sock, self._accepted)
        worker = asyncio.create_task(self._work(loop, executor))
        try:
            stop = asyncio.create_task(self._stop.wait())
            waiters = {stop}
            if self._idle_timeout > 0:
                waiters.add(asyncio.create_task(self._wait_idle()))
            await asyncio.wait(waiters, return_when=asyncio.FIRST_COMPLETED)
            for waiter in waiters - {stop}:
                waiter.cancel()

            if not self._stop.is_set():
                log.info("idle for %s s; closing the listener", self._idle_timeout)
                if self._before_close is not None:
                    await self._before_close()
                listener.close()
                # Connections accepted before close() are served to completion.
                drained = asyncio.create_task(self._wait_handlers())
                await asyncio.wait({drained, stop}, return_when=asyncio.FIRST_COMPLETED)
                drained.cancel()

            if self._stop.is_set():
                log.info("shutdown requested")
                listener.close()
                self._fail_all_jobs()
                await self._wait_handlers(timeout=SHUTDOWN_GRACE_SECS)
                for task in list(self._handlers):
                    task.cancel()
                await self._wait_handlers()
            stop.cancel()
        finally:
            listener.close()
            worker.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await worker
            # An in-flight model load cannot be interrupted, so do not join the thread.
            executor.shutdown(wait=False, cancel_futures=True)

    # ---- idle tracking -------------------------------------------------

    def _touch(self) -> None:
        self._idle_since = time.monotonic()
        self._activity.set()

    def _active(self) -> bool:
        return self._connections > 0 or self._current is not None or bool(self._pending)

    async def _wait_idle(self) -> None:
        while True:
            self._activity.clear()
            if self._active():
                await self._activity.wait()
                continue
            remaining = self._idle_since + self._idle_timeout - time.monotonic()
            if remaining <= 0:
                return
            with contextlib.suppress(TimeoutError):
                await asyncio.wait_for(self._activity.wait(), remaining)

    async def _wait_handlers(self, timeout: float | None = None) -> None:
        deadline = None if timeout is None else time.monotonic() + timeout
        while self._handlers:
            remaining = None if deadline is None else deadline - time.monotonic()
            if remaining is not None and remaining <= 0:
                return
            await asyncio.wait(set(self._handlers), timeout=remaining)

    # ---- worker ----------------------------------------------------------

    async def _work(self, loop: asyncio.AbstractEventLoop, executor: ThreadPoolExecutor) -> None:
        while True:
            while not self._pending:
                self._job_ready.clear()
                await self._job_ready.wait()
            job = self._pending.popleft()
            self._current = job
            try:
                await loop.run_in_executor(executor, self._run_job, job)
            except Exception:
                log.exception("worker failed outside a job")
                job.deliver(protocol.error("internal", "worker failure"))
            finally:
                self._current = None
                self._touch()

    def _run_job(self, job: _Job) -> None:
        """Runs on the worker thread; talks to the loop only through job.emit."""
        engine = self._engine
        try:
            engine.ensure_loaded(lambda p: job.emit(_progress_event(p)))
            if job.cancelled.is_set():
                return
            if isinstance(job.req, LoadRequest):
                job.emit(protocol.done("", 0, 0, engine.model, engine.revision))
                return
            req = job.req
            input_tokens = engine.count_tokens(req)
            max_tokens = _max_tokens(input_tokens, req.max_tokens)
            pieces: list[str] = []
            text_stream = engine.stream(req, max_tokens, job.cancelled)
            for piece in text_stream:
                if job.cancelled.is_set():
                    return
                pieces.append(piece)
                if req.stream:
                    job.emit(protocol.delta(piece))
            if job.cancelled.is_set():
                return
            # Every translate done must carry finish_reason; mlx-lm is pinned and the slow tests that
            # `just bump-mlx` runs assert it, so reaching this means a changed mlx-lm.
            if text_stream.finish_reason is None:
                raise DaemonError("internal", "generation ended without a finish reason")
            job.emit(
                protocol.done(
                    pfml3.strip_stop_token("".join(pieces)),
                    input_tokens,
                    len(pieces),
                    engine.model,
                    engine.revision,
                    finish_reason=text_stream.finish_reason,
                )
            )
        except DaemonError as e:
            job.emit(protocol.error(e.code, e.message))
        except Exception as e:
            log.exception("unexpected error while processing a request")
            job.emit(protocol.error("internal", f"{type(e).__name__}: {e}"))

    def _fail_all_jobs(self) -> None:
        self._shutting_down = True
        jobs = list(self._pending)
        self._pending.clear()
        if self._current is not None:
            self._current.cancelled.set()
            jobs.append(self._current)
        for job in jobs:
            job.deliver(protocol.error("shutting_down", "daemon is shutting down"))

    # ---- connections -----------------------------------------------------

    def _accepted(self, conn: socket.socket) -> None:
        # Registered in the same callback as accept(), so the idle check and the
        # drain after close() can never miss a connection the kernel handed us.
        self._connections += 1
        self._touch()
        task = asyncio.get_running_loop().create_task(self._handle(conn))
        self._handlers.add(task)
        task.add_done_callback(lambda t: self._handler_finished(t, conn))

    def _handler_finished(self, task: asyncio.Task, conn: socket.socket) -> None:
        self._handlers.discard(task)
        self._connections -= 1
        self._touch()
        conn.close()  # covers a handler cancelled before it ever ran

    async def _handle(self, conn: socket.socket) -> None:
        reader, writer = await asyncio.open_unix_connection(sock=conn, limit=MAX_LINE_BYTES)
        try:
            await self._serve_connection(reader, writer)
        except (ConnectionError, BrokenPipeError):
            pass
        except Exception as e:
            log.exception("unexpected error on a connection")
            with contextlib.suppress(Exception):
                await _send(writer, protocol.error("internal", f"{type(e).__name__}: {e}"))
        finally:
            writer.close()
            with contextlib.suppress(Exception):
                await writer.wait_closed()

    async def _serve_connection(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        try:
            line = await _read_request_line(reader)
            if line is None:
                return
            req = parse_request(line)
            if isinstance(req, TranslateRequest):
                pfml3.validate(req)
        except DaemonError as e:
            await _send(writer, protocol.error(e.code, e.message))
            return

        if isinstance(req, StatusRequest):
            await _send(writer, self._status())
            return
        if self._shutting_down:
            await _send(writer, protocol.error("shutting_down", "daemon is shutting down"))
            return
        if len(self._pending) >= MAX_QUEUE:
            await _send(writer, protocol.error("busy", f"{MAX_QUEUE} requests are already waiting"))
            return

        job = _Job(req, asyncio.get_running_loop())
        self._pending.append(job)
        self._job_ready.set()
        await self._relay(job, reader, writer)

    async def _relay(self, job: _Job, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        eof = asyncio.create_task(_wait_eof(reader))
        try:
            while True:
                get = asyncio.create_task(job.events.get())
                await asyncio.wait({get, eof}, return_when=asyncio.FIRST_COMPLETED)
                if not get.done():
                    get.cancel()
                    self._abandon(job)
                    return
                event = get.result()
                try:
                    await _send(writer, event)
                except (ConnectionError, BrokenPipeError):
                    self._abandon(job)
                    return
                if event["type"] in _TERMINAL:
                    return
        finally:
            eof.cancel()

    def _abandon(self, job: _Job) -> None:
        job.finished = True
        job.cancelled.set()
        with contextlib.suppress(ValueError):
            self._pending.remove(job)
        self._touch()

    def _status(self) -> dict:
        return protocol.status(
            model=self._engine.model,
            revision=self._engine.revision,
            loaded=self._engine.loaded,
            uptime_secs=int(time.monotonic() - self._started),
            idle_timeout_secs=self._idle_timeout,
            queue_len=len(self._pending),
            version=self._version,
            protocol=PROTOCOL_VERSION,
            features=list(FEATURES),
            template_verified=self._engine.template_verified,
        )


class _Listener:
    """Accepts with a plain reader callback instead of asyncio.start_unix_server.

    asyncio's accept path spans several loop iterations between accept() and the
    client callback; a connection accepted in that window would be lost if the
    idle exit closed the listener then, and it would no longer be in launchd's
    backlog to trigger a relaunch.
    """

    def __init__(self, loop: asyncio.AbstractEventLoop, sock: socket.socket, on_accept: Callable[[socket.socket], None]):
        self._loop = loop
        self._sock = sock
        self._on_accept = on_accept
        self._open = True
        sock.setblocking(False)
        loop.add_reader(sock.fileno(), self._ready)

    def _ready(self) -> None:
        while self._open:
            try:
                conn, _ = self._sock.accept()
            except (BlockingIOError, InterruptedError):
                return
            except OSError:
                log.exception("accept failed")
                return
            conn.setblocking(False)
            self._on_accept(conn)

    def close(self) -> None:
        if not self._open:
            return
        self._open = False
        self._loop.remove_reader(self._sock.fileno())
        self._sock.close()


def _progress_event(p: ProgressEvent) -> dict:
    return protocol.progress(p.stage, p.done_bytes, p.total_bytes)


def _max_tokens(input_tokens: int, requested: int | None) -> int:
    if requested is None:
        max_tokens = default_max_tokens(input_tokens)
        if max_tokens <= 0:
            raise DaemonError(
                "context_too_long",
                f"input is {input_tokens} tokens, leaving no room for output within {MAX_CONTEXT_TOKENS} tokens",
            )
        return max_tokens
    if input_tokens + requested > MAX_CONTEXT_TOKENS:
        raise DaemonError(
            "context_too_long",
            f"input {input_tokens} tokens + max_tokens {requested} = {input_tokens + requested} "
            f"exceeds the {MAX_CONTEXT_TOKENS}-token limit",
        )
    return requested


async def _read_request_line(reader: asyncio.StreamReader) -> bytes | None:
    """Returns None when the peer leaves before sending a complete line."""
    try:
        return (await reader.readuntil(b"\n"))[:-1]
    except asyncio.IncompleteReadError:
        return None
    except asyncio.LimitOverrunError:
        # Consume the rest of the line so the client can finish writing and read our error.
        while chunk := await reader.read(_DISCARD_CHUNK):
            if b"\n" in chunk:
                break
        raise DaemonError("invalid_request", f"request line exceeds {MAX_LINE_BYTES} bytes")


async def _wait_eof(reader: asyncio.StreamReader) -> None:
    with contextlib.suppress(ConnectionError, OSError):
        while await reader.read(_DISCARD_CHUNK):
            pass


async def _send(writer: asyncio.StreamWriter, event: dict) -> None:
    writer.write(encode_event(event))
    await writer.drain()
