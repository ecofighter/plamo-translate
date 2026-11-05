from __future__ import annotations

import threading
from collections.abc import Callable, Iterator
from dataclasses import dataclass
from typing import Literal, Protocol

from .protocol import TranslateRequest


@dataclass(frozen=True)
class ProgressEvent:
    stage: Literal["downloading", "loading"]
    done_bytes: int | None = None
    total_bytes: int | None = None


class Engine(Protocol):
    """Model backend driven by the server's single worker thread.

    Every failure is raised as DaemonError: `model_load_failed` for download or
    load problems, `invalid_request` for chat template validation errors.
    """

    model: str
    revision: str
    loaded: bool

    def ensure_loaded(self, progress: Callable[[ProgressEvent], None]) -> None: ...

    def count_tokens(self, req: TranslateRequest) -> int: ...

    def stream(self, req: TranslateRequest, max_tokens: int, cancelled: threading.Event) -> Iterator[str]: ...
