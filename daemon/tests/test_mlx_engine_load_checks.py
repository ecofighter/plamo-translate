import hashlib
import logging

import pytest

from conftest import StubTokenizer
from plamo_translated import mlx_engine
from plamo_translated.mlx_engine import TEMPLATE_DRIFT_WARNING, MlxEngine
from plamo_translated.pfml3 import CONTROL_TOKENS, TEMPLATE_FILE, TEMPLATE_SHA256
from plamo_translated.protocol import DaemonError

# Any bytes will do: the pinned hash is patched to this template's hash.
TEMPLATE = b"{{- bos_token -}}"


@pytest.fixture
def snapshot(tmp_path, monkeypatch):
    (tmp_path / TEMPLATE_FILE).write_bytes(TEMPLATE)
    monkeypatch.setattr("plamo_translated.pfml3.TEMPLATE_SHA256", hashlib.sha256(TEMPLATE).hexdigest())
    monkeypatch.setattr(MlxEngine, "_resolve_snapshot", lambda self, progress: str(tmp_path))
    monkeypatch.setattr(MlxEngine, "_warn_unless_gpu", lambda self: None)
    return tmp_path


def use_tokenizer(monkeypatch, tok):
    monkeypatch.setattr(mlx_engine, "load", lambda path, **kw: (object(), tok))


def test_load_reports_verified_template(snapshot, monkeypatch, caplog):
    use_tokenizer(monkeypatch, StubTokenizer())
    e = MlxEngine("m", "r", snapshot)
    assert e.template_verified is None
    with caplog.at_level(logging.WARNING):
        e.ensure_loaded(lambda p: None)
    assert (e.loaded, e.template_verified) == (True, True)
    assert TEMPLATE_DRIFT_WARNING not in caplog.text


@pytest.mark.parametrize("alter", [lambda p: p.write_bytes(TEMPLATE + b"\n"), lambda p: p.unlink()])
def test_altered_or_missing_template_loads_unverified_with_one_warning(snapshot, monkeypatch, caplog, alter):
    alter(snapshot / TEMPLATE_FILE)
    use_tokenizer(monkeypatch, StubTokenizer())
    e = MlxEngine("m", "r", snapshot)
    with caplog.at_level(logging.WARNING):
        e.ensure_loaded(lambda p: None)
        e.ensure_loaded(lambda p: None)
    assert (e.loaded, e.template_verified) == (True, False)
    assert [r.getMessage() for r in caplog.records if r.levelno == logging.WARNING] == [TEMPLATE_DRIFT_WARNING]


def test_tokenizer_without_msg_token_fails_load_once_per_process(snapshot, monkeypatch):
    ids = {t: i for i, t in enumerate(CONTROL_TOKENS, 1) if t != "<|plamo:msg|>"}
    loads = []
    monkeypatch.setattr(mlx_engine, "load", lambda path, **kw: loads.append(path) or (object(), StubTokenizer(ids=ids)))
    e = MlxEngine("m", "r", snapshot)
    errors = []
    for _ in range(2):
        with pytest.raises(DaemonError) as ex:
            e.ensure_loaded(lambda p: None)
        errors.append((ex.value.code, ex.value.message))
    assert errors[0] == errors[1]
    assert errors[0][0] == "model_load_failed"
    assert errors[0][1].startswith("model tokenizer does not expose the PFML3 control tokens")
    assert len(loads) == 1
    assert (e.loaded, e.template_verified) == (False, None)


def test_template_sha256_is_the_pinned_value():
    assert TEMPLATE_SHA256 == "cd49e7e0dfa8ec847c163db88dfca89b280dddbb73ddc60ba38c62a420eaced2"
