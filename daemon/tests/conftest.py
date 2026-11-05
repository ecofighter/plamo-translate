import threading
import time
from collections.abc import Callable, Iterator

from plamo_translated.engine import Engine, ProgressEvent
from plamo_translated.protocol import DaemonError, TranslateRequest

FAKE_DOWNLOAD_BYTES = 1000


class FakeEngine(Engine):
    def __init__(
        self,
        tokens: list[str] | None = None,
        input_tokens: int = 5,
        template_error: str | None = None,
        load_error: str | None = None,
        download: bool = False,
        token_delay: float = 0.0,
        load_delay: float = 0.0,
    ):
        self.model = "fake/model"
        self.revision = "fake-revision"
        self.loaded = False
        self.tokens = ["ok"] if tokens is None else list(tokens)
        self.input_tokens = input_tokens
        self.template_error = template_error
        self.load_error = load_error
        self.download = download
        self.token_delay = token_delay
        self.load_delay = load_delay
        self.calls: list[TranslateRequest] = []

    @classmethod
    def from_fixture(cls, d: dict) -> "FakeEngine":
        keys = ("tokens", "input_tokens", "template_error", "load_error", "download")
        return cls(**{k: d[k] for k in keys if k in d})

    def ensure_loaded(self, progress: Callable[[ProgressEvent], None]) -> None:
        if self.loaded:
            return
        time.sleep(self.load_delay)
        if self.load_error is not None:
            raise DaemonError("model_load_failed", self.load_error)
        if self.download:
            progress(ProgressEvent("downloading", FAKE_DOWNLOAD_BYTES, FAKE_DOWNLOAD_BYTES))
        progress(ProgressEvent("loading"))
        self.loaded = True

    def count_tokens(self, req: TranslateRequest) -> int:
        self.calls.append(req)
        if self.template_error is not None:
            raise DaemonError("invalid_request", self.template_error)
        return self.input_tokens

    def stream(self, req: TranslateRequest, max_tokens: int, cancelled: threading.Event) -> Iterator[str]:
        self.calls.append(req)
        for i, token in enumerate(self.tokens[:max_tokens]):
            if i > 0:
                time.sleep(self.token_delay)
            if cancelled.is_set():
                return
            yield token
