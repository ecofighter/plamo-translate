import shutil
import tempfile
import threading
import time
from collections.abc import Callable, Iterator
from pathlib import Path

import pytest

from plamo_translated.engine import Engine, ProgressEvent, TextStream
from plamo_translated.protocol import DaemonError, TranslateRequest

FAKE_DOWNLOAD_BYTES = 1000


class FakeEngine(Engine):
    def __init__(
        self,
        tokens: list[str] | None = None,
        input_tokens: int = 5,
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
        self.load_error = load_error
        self.download = download
        self.token_delay = token_delay
        self.load_delay = load_delay
        self.calls: list[TranslateRequest] = []
        self.saw_cancel = False

    @classmethod
    def from_fixture(cls, d: dict) -> "FakeEngine":
        keys = ("tokens", "input_tokens", "load_error", "download")
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
        return self.input_tokens

    def stream(self, req: TranslateRequest, max_tokens: int, cancelled: threading.Event) -> TextStream:
        return TextStream(self._steps(max_tokens, cancelled))

    def _steps(self, max_tokens: int, cancelled: threading.Event) -> Iterator[tuple[str, str | None]]:
        for i, token in enumerate(self.tokens[:max_tokens]):
            if i > 0:
                time.sleep(self.token_delay)
            if cancelled.is_set():
                self.saw_cancel = True
                return
            yield token, None
        # Like mlx-lm: the eos after the last token only counts if it fits within max_tokens.
        yield "", "stop" if len(self.tokens) < max_tokens else "length"


class StubTokenizer:
    """Maps each control string to one id and every other character to its code point."""

    def __init__(self, ids=None, eos_token_ids=frozenset({1, 16}), unk_token_id=0):
        self.ids = {"<|plamo:bos|>": 1, "<|plamo:tag|>": 16, "<|plamo:key|>": 17, "<|plamo:val|>": 18,
                    "<|plamo:msg|>": 19, "<|plamo:index|>": 20, "<|plamo:item|>": 21} if ids is None else ids
        self.eos_token_ids = eos_token_ids
        self.unk_token_id = unk_token_id
        self.calls = []

    def encode(self, text, add_special_tokens=True):
        self.calls.append(add_special_tokens)
        out, i = [], 0
        while i < len(text):
            for token, token_id in self.ids.items():
                if text.startswith(token, i):
                    out.append(token_id)
                    i += len(token)
                    break
            else:
                out.append(ord(text[i]))
                i += 1
        return out


# AF_UNIX paths must stay under 104 bytes; pytest's tmp_path (and the Nix build TMPDIR) can exceed that.
@pytest.fixture
def short_dir():
    d = Path(tempfile.mkdtemp(prefix="pt", dir="/tmp"))
    yield d
    shutil.rmtree(d, ignore_errors=True)
