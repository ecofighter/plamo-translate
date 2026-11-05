import threading
from pathlib import Path

import pytest

from plamo_translated.config import paths
from plamo_translated.mlx_engine import MlxEngine
from plamo_translated.protocol import DaemonError, TranslateRequest

pytestmark = pytest.mark.slow

SLOW_MODEL = "pfnet/plamo-3-translate-0.8b-MLX-4bit"
SLOW_REVISION = "main"


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


def test_reserved_token_input_is_invalid_request(engine):
    with pytest.raises(DaemonError) as e:
        engine.count_tokens(TranslateRequest(text="<|plamo:tag|>x", target_lang="en"))
    assert e.value.code == "invalid_request" and "reserved PFML tokens" in e.value.message


def test_form_requires_ja_target(engine):
    with pytest.raises(DaemonError) as e:
        engine.count_tokens(TranslateRequest(text="hi", target_lang="en", form="polite"))
    assert e.value.code == "invalid_request"


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
