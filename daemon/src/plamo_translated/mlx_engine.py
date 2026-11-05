from __future__ import annotations

import os

# Same MLX settings as the model's bundled serve.py; they must be set before mlx is imported.
os.environ.setdefault("MLX_ENABLE_TF32", "0")
os.environ.setdefault("MLX_ENABLE_CACHE_THRASHING_CHECK", "0")

import logging  # noqa: E402
import threading  # noqa: E402
import time  # noqa: E402
from collections.abc import Callable, Iterator  # noqa: E402
from contextlib import closing  # noqa: E402
from pathlib import Path  # noqa: E402

import mlx.core as mx  # noqa: E402
from huggingface_hub import snapshot_download  # noqa: E402
from huggingface_hub.errors import LocalEntryNotFoundError  # noqa: E402
from mlx_lm import load, stream_generate  # noqa: E402
from mlx_lm.sample_utils import make_sampler  # noqa: E402
from tqdm.std import tqdm  # noqa: E402

from . import pfml3  # noqa: E402
from .engine import Engine, ProgressEvent, TextStream  # noqa: E402
from .protocol import DaemonError, TranslateRequest  # noqa: E402

log = logging.getLogger(__name__)

PROGRESS_MIN_INTERVAL_SECS = 0.25
# snapshot_download aggregates bytes written by every file into the bar with this description.
_RECONSTRUCT_BAR_PREFIX = "Reconstructing"


class DownloadProgress:
    """Turns snapshot_download's tqdm bars into throttled ProgressEvents."""

    def __init__(
        self,
        emit: Callable[[ProgressEvent], None],
        min_interval: float = PROGRESS_MIN_INTERVAL_SECS,
        clock: Callable[[], float] = time.monotonic,
    ):
        self._emit = emit
        self._min_interval = min_interval
        self._clock = clock
        self._lock = threading.Lock()
        self._last_emit: float | None = None
        self._latest: ProgressEvent | None = None
        self._emitted: ProgressEvent | None = None

    def _report(self, done: int, total: int) -> None:
        event = ProgressEvent("downloading", done, total)
        with self._lock:
            self._latest = event
            now = self._clock()
            if self._last_emit is not None and now - self._last_emit < self._min_interval:
                return
            self._last_emit = now
            self._emitted = event
        self._emit(event)

    def finish(self) -> None:
        """Emit the final byte count if throttling swallowed it."""
        with self._lock:
            event = self._latest
            if event is None or event == self._emitted:
                return
            self._emitted = event
        self._emit(event)

    def tqdm_class(self) -> type:
        progress = self

        class _Bar(tqdm):
            def __init__(self, *args, **kwargs):
                kwargs["file"] = _NullWriter()
                kwargs["disable"] = False
                super().__init__(*args, **kwargs)
                self._tracked = (self.desc or "").startswith(_RECONSTRUCT_BAR_PREFIX)

            def update(self, n=1):
                displayed = super().update(n)
                if self._tracked:
                    progress._report(int(self.n), int(self.total or 0))
                return displayed

        return _Bar


class _NullWriter:
    """tqdm output sink: bars must keep counting (so not disabled) but never reach stderr."""

    def write(self, s: str) -> int:
        return len(s)

    def flush(self) -> None:
        pass


TEMPLATE_DRIFT_WARNING = (
    "bundled chat template differs from the one the PFML3 builder was verified against "
    "(re-run the slow oracle tests)"
)


class MlxEngine(Engine):
    def __init__(self, model: str, revision: str, hub_cache: Path):
        self.model = model
        self.revision = revision
        self.hub_cache = hub_cache
        self.loaded = False
        self.template_verified: bool | None = None
        # Set when the tokenizer fails the PFML3 checks; the snapshot cannot change within the process.
        self._incompatible: str | None = None
        self._model = None
        self._tokenizer = None

    def ensure_loaded(self, progress: Callable[[ProgressEvent], None]) -> None:
        if self.loaded:
            return
        if self._incompatible is not None:
            raise DaemonError("model_load_failed", self._incompatible)
        try:
            path = self._resolve_snapshot(progress)
            progress(ProgressEvent("loading"))
            model, tokenizer = load(
                path, trust_remote_code=True, tokenizer_config={"trust_remote_code": True}
            )
        except Exception as e:
            raise DaemonError("model_load_failed", f"{type(e).__name__}: {e}") from e
        try:
            pfml3.check_tokenizer(tokenizer)
        except DaemonError as e:
            self._incompatible = e.message
            raise
        template_verified = pfml3.template_verified(Path(path))
        if not template_verified:
            log.warning(TEMPLATE_DRIFT_WARNING)
        self._warn_unless_gpu()
        self._model, self._tokenizer = model, tokenizer
        self.template_verified = template_verified
        self.loaded = True

    def _warn_unless_gpu(self) -> None:
        device = mx.default_device()
        if device.type != mx.gpu:
            log.error("MLX default device is %s, not GPU; generation will be very slow", device)

    def _resolve_snapshot(self, progress: Callable[[ProgressEvent], None]) -> str:
        try:
            return snapshot_download(
                self.model,
                revision=self.revision,
                cache_dir=self.hub_cache,
                local_files_only=True,
            )
        except LocalEntryNotFoundError:
            pass
        log.info("downloading %s@%s", self.model, self.revision)
        download_progress = DownloadProgress(progress)
        path = snapshot_download(
            self.model,
            revision=self.revision,
            cache_dir=self.hub_cache,
            tqdm_class=download_progress.tqdm_class(),
        )
        download_progress.finish()
        return path

    def _prompt_ids(self, req: TranslateRequest) -> list[int]:
        if not self.loaded:
            raise DaemonError("internal", "model is not loaded")
        return pfml3.prompt_ids(self._tokenizer, req)

    def count_tokens(self, req: TranslateRequest) -> int:
        return len(self._prompt_ids(req))

    def stream(self, req: TranslateRequest, max_tokens: int, cancelled: threading.Event) -> TextStream:
        # Not a generator itself, so encoding errors surface at the call rather than on first next().
        return TextStream(self._generate(self._prompt_ids(req), max_tokens, cancelled))

    def _generate(
        self, prompt: list[int], max_tokens: int, cancelled: threading.Event
    ) -> Iterator[tuple[str, str | None]]:
        responses = stream_generate(
            self._model, self._tokenizer, prompt, max_tokens=max_tokens, sampler=make_sampler(temp=0.0)
        )
        with closing(responses):
            for response in responses:
                if cancelled.is_set():
                    return
                yield response.text, response.finish_reason
