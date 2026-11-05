import threading

import pytest

from conftest import FakeEngine
from plamo_translated.engine import ProgressEvent
from plamo_translated.mlx_engine import DownloadProgress
from plamo_translated.protocol import DaemonError, TranslateRequest

REQ = TranslateRequest(text="hi", target_lang="ja")


class FakeClock:
    def __init__(self):
        self.now = 0.0

    def __call__(self):
        return self.now


def make_bars(cls):
    """Create bars the way huggingface_hub.snapshot_download does."""
    transfer = cls(desc="Downloading bytes", total=0, initial=0, unit="B", unit_scale=True)
    reconstruct = cls(desc="Reconstructing (incomplete total...)", total=0, initial=0, unit="B", unit_scale=True)
    files = cls(desc="Fetching 2 files", total=2)
    return transfer, reconstruct, files


def test_download_progress_reports_reconstructed_bytes():
    events = []
    clock = FakeClock()
    cls = DownloadProgress(events.append, clock=clock).tqdm_class()
    transfer, reconstruct, files = make_bars(cls)
    reconstruct.total = (reconstruct.total or 0) + 1000
    transfer.update(500)
    files.update(1)
    reconstruct.update(400)
    assert events == [ProgressEvent("downloading", 400, 1000)]


def test_download_progress_throttles_to_four_per_second_and_finish_reports_last_count():
    events = []
    clock = FakeClock()
    progress = DownloadProgress(events.append, clock=clock)
    _, reconstruct, _ = make_bars(progress.tqdm_class())
    reconstruct.total = 1000
    reconstruct.update(100)
    clock.now = 0.1
    reconstruct.update(100)
    clock.now = 0.3
    reconstruct.update(100)
    clock.now = 0.35
    reconstruct.update(700)
    progress.finish()
    progress.finish()
    assert events == [
        ProgressEvent("downloading", 100, 1000),
        ProgressEvent("downloading", 300, 1000),
        ProgressEvent("downloading", 1000, 1000),
    ]


def test_download_progress_finish_without_updates_emits_nothing():
    events = []
    DownloadProgress(events.append).finish()
    assert events == []


def test_download_progress_bar_supports_tqdm_api_used_by_huggingface_hub():
    cls = DownloadProgress(lambda e: None).tqdm_class()
    with cls(desc="Fetching 1 files", total=1) as bar:
        bar.update(1)
        bar.refresh()
        bar.set_description("done")
        bar.set_description_str("done")
        bar.set_postfix_str("x")
        assert "rate" in bar.format_dict
    bar.close()


def test_fake_engine_streams_tokens_and_records_calls():
    e = FakeEngine(tokens=["a", "b"], input_tokens=7)
    assert e.count_tokens(REQ) == 7
    assert list(e.stream(REQ, 10, threading.Event())) == ["a", "b"]
    assert e.calls == [REQ]


def test_fake_engine_stops_when_cancelled():
    e = FakeEngine(tokens=["a", "b", "c"])
    ev = threading.Event()
    it = e.stream(REQ, 10, ev)
    assert next(it) == "a"
    ev.set()
    assert list(it) == []
    assert e.saw_cancel


def test_fake_engine_load_progress():
    events = []
    e = FakeEngine(download=True)
    assert not e.loaded
    e.ensure_loaded(events.append)
    e.ensure_loaded(events.append)
    assert e.loaded
    assert [ev.stage for ev in events] == ["downloading", "loading"]
    assert events[0].done_bytes is not None and events[0].total_bytes is not None

    events = []
    FakeEngine().ensure_loaded(events.append)
    assert events == [ProgressEvent("loading")]


def test_fake_engine_errors():
    with pytest.raises(DaemonError) as ex:
        FakeEngine(load_error="boom").ensure_loaded(lambda p: None)
    assert (ex.value.code, ex.value.message) == ("model_load_failed", "boom")


def test_fake_engine_from_fixture():
    e = FakeEngine.from_fixture(
        {"tokens": ["x"], "input_tokens": 3, "load_error": "l", "download": True}
    )
    assert (e.tokens, e.input_tokens, e.load_error, e.download) == (["x"], 3, "l", True)
    d = FakeEngine.from_fixture({})
    assert (d.tokens, d.input_tokens, d.load_error, d.download) == (["ok"], 5, None, False)
