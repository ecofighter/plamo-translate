from __future__ import annotations

import threading
from collections.abc import Callable, Iterable, Iterator
from dataclasses import dataclass
from typing import Literal, Protocol

from .protocol import TranslateRequest


@dataclass(frozen=True)
class ProgressEvent:
    stage: Literal["downloading", "loading"]
    done_bytes: int | None = None
    total_bytes: int | None = None


class TextStream(Iterator[str]):
    """Non-empty decoded text pieces of one generation.

    `finish_reason` is mlx-lm's label ("stop" | "length") once the generation
    ended on its own, and stays None when iteration stopped early (cancellation).
    """

    def __init__(self, steps: Iterable[tuple[str, str | None]]):
        self._steps = iter(steps)
        self.finish_reason: str | None = None

    def __next__(self) -> str:
        while True:
            text, finish_reason = next(self._steps)
            if finish_reason is not None:
                self.finish_reason = finish_reason
            if text:
                return text


class Engine(Protocol):
    """Model backend driven by the server's single worker thread.

    Requests reach the engine already validated (pfml3.validate). Every failure
    is raised as DaemonError, `model_load_failed` for download, load or
    tokenizer-compatibility problems.
    """

    model: str
    revision: str
    loaded: bool
    # Whether the loaded snapshot's chat template is the one the PFML3 builder was verified against; None until loaded.
    template_verified: bool | None

    def ensure_loaded(self, progress: Callable[[ProgressEvent], None]) -> None:
        """Download (if needed) and load the model; a no-op once loaded.

        `progress` may be invoked from huggingface_hub download threads rather
        than the calling thread, so callers must hand events over to their event
        loop thread-safely (`loop.call_soon_threadsafe`).
        """
        ...

    def count_tokens(self, req: TranslateRequest) -> int: ...

    def stream(self, req: TranslateRequest, max_tokens: int, cancelled: threading.Event) -> TextStream: ...
