import threading
from pathlib import Path

import pytest

from plamo_translated import mlx_engine
from plamo_translated.config import DEFAULT_MODEL, DEFAULT_REVISION, paths
from plamo_translated.mlx_engine import MlxEngine
from plamo_translated.protocol import DaemonError, HistoryEntry, TranslateRequest

pytestmark = pytest.mark.slow

SLOW_MODEL = "pfnet/plamo-3-translate-0.8b-MLX-4bit"
SLOW_REVISION = "e5fe5b7f26ebaeae48db0ce644b932d1d9b91aaa"


@pytest.fixture(scope="module")
def engine():
    e = MlxEngine(SLOW_MODEL, SLOW_REVISION, paths(Path.home()).hub_cache)
    e.ensure_loaded(lambda p: None)
    return e


def test_mlx_engine_translates_ja_to_en(engine):
    out = "".join(engine.stream(TranslateRequest(text="会議は来週の月曜日に変更されました。", target_lang="en"), 128, threading.Event()))
    assert "Monday" in out and "<|plamo" not in out


def test_mlx_engine_japanese_output_has_no_replacement_chars(engine):
    parts = list(engine.stream(TranslateRequest(text="The meeting has been moved to next Monday.", target_lang="ja"), 128, threading.Event()))
    assert all("�" not in p for p in parts) and "月曜" in "".join(parts)


def test_mlx_engine_reports_stop_and_verified_template(engine):
    stream = engine.stream(TranslateRequest(text="おはようございます。", target_lang="en"), 128, threading.Event())
    out = "".join(stream)
    assert stream.finish_reason == "stop" and out.strip() and "<|plamo" not in out
    assert engine.template_verified is True


def test_mlx_engine_reports_length_when_truncated(engine):
    stream = engine.stream(TranslateRequest(text="長い文章です。" * 20, target_lang="en"), 3, threading.Event())
    assert len(list(stream)) <= 3 and stream.finish_reason == "length"


def test_mlx_engine_translates_with_history(engine):
    req = TranslateRequest(
        text="オーナーでフィルタリングすることもできます。",
        target_lang="en",
        source_lang="ja",
        history=(HistoryEntry("プロジェクトの管理画面を開きます。", "Open the project dashboard."),),
    )
    stream = engine.stream(req, 128, threading.Event())
    out = "".join(stream)
    assert stream.finish_reason == "stop" and "owner" in out.lower()
    assert engine.count_tokens(req) > engine.count_tokens(TranslateRequest(text=req.text, target_lang="en", source_lang="ja"))


def test_cancel_stops_stream(engine):
    ev = threading.Event()
    it = engine.stream(TranslateRequest(text="長い文章。" * 50, target_lang="en"), 2048, ev)
    next(it)
    ev.set()
    assert len(list(it)) <= 1


def test_load_failure_maps_to_model_load_failed(tmp_path):
    e = MlxEngine("pfnet/does-not-exist-xyz", "main", tmp_path)
    with pytest.raises(DaemonError) as ex:
        e.ensure_loaded(lambda p: None)
    assert ex.value.code == "model_load_failed"


def test_default_model_loads_from_cache_with_verified_template(monkeypatch):
    def no_download(*args, **kwargs):
        raise AssertionError("the cached default snapshot must load without the download path")

    monkeypatch.setattr(mlx_engine, "DownloadProgress", no_download)
    e = MlxEngine(DEFAULT_MODEL, DEFAULT_REVISION, paths(Path.home()).hub_cache)
    e.ensure_loaded(lambda p: None)
    assert (e.loaded, e.template_verified) == (True, True)
