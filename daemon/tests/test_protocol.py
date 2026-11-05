import json
from pathlib import Path

import pytest

from plamo_translated.protocol import (
    DaemonError,
    GlossaryEntry,
    HistoryEntry,
    LoadRequest,
    StatusRequest,
    TranslateRequest,
    default_max_tokens,
    delta,
    done,
    encode_event,
    error,
    parse_request,
    progress,
    status,
)

FIXTURES = sorted((Path(__file__).parents[2] / "protocol" / "fixtures").glob("*.ndjson"))


def test_parse_translate_minimal():
    r = parse_request(b'{"v":1,"op":"translate","text":"hi","target_lang":"ja"}')
    assert r == TranslateRequest(text="hi", target_lang="ja")


def test_parse_glossary_entries():
    r = parse_request('{"v":1,"op":"translate","text":"x","target_lang":"en","glossary":[{"source":"推論","target":"inference"}]}'.encode())
    assert r.glossary == (GlossaryEntry("推論", "inference"),)


def test_parse_history_entries_in_order_ignoring_extra_keys():
    r = parse_request('{"v":1,"op":"translate","text":"x","target_lang":"en","history":[{"source":"a","translation":"A","note":1},{"source":"b","translation":"B"}]}'.encode())
    assert r.history == (HistoryEntry("a", "A"), HistoryEntry("b", "B"))


@pytest.mark.parametrize("history", ["", ',"history":null', ',"history":[]'])
def test_parse_absent_null_and_empty_history_are_equal(history):
    line = f'{{"v":1,"op":"translate","text":"x","target_lang":"en"{history}}}'.encode()
    assert parse_request(line) == TranslateRequest(text="x", target_lang="en")


@pytest.mark.parametrize(("history", "message"), [
    ('"x"', "field history must be an array"),
    ('{"source":"a","translation":"A"}', "field history must be an array"),
    ('["a"]', "history entries must be objects"),
    ('[{"source":"a"}]', "history entries need string source and translation"),
    ('[{"source":"a","translation":1}]', "history entries need string source and translation"),
    ('[{"source":"a","target":"A"}]', "history entries need string source and translation"),
])
def test_parse_rejects_malformed_history(history, message):
    with pytest.raises(DaemonError) as e:
        parse_request(f'{{"v":1,"op":"translate","text":"x","target_lang":"en","history":{history}}}'.encode())
    assert (e.value.code, e.value.message) == ("invalid_request", message)


def test_parse_type_errors_precede_history_errors():
    with pytest.raises(DaemonError) as e:
        parse_request(b'{"v":1,"op":"translate","text":"x","target_lang":"en","form":1,"history":"x"}')
    assert e.value.message == "field form must be a string"


def test_done_carries_finish_reason_after_text():
    assert list(done("t", 1, 2, "m", "r", finish_reason="stop")) == ["type", "text", "finish_reason", "usage", "model", "revision"]


def test_parse_load():
    assert parse_request(b'{"v":1,"op":"load"}') == LoadRequest()


def test_parse_rejects_wrong_version():
    with pytest.raises(DaemonError) as e:
        parse_request(b'{"v":2,"op":"status"}')
    assert e.value.code == "unsupported_version"


@pytest.mark.parametrize("line", [
    b"{not json",
    b'{"v":1,"op":"translate","text":"x"}',
    b'{"v":1,"op":"nope"}',
    b'{"v":1,"op":"translate","text":1,"target_lang":"en"}',
    b'{"v":1,"op":"translate","text":"x","target_lang":"en","glossary":[{"source":"a"}]}',
    b'[1]',
    b'\xff\xfe',
    b'{"v":1,"op":"translate","text":"x","target_lang":"en","stream":"yes"}',
    b'{"v":1,"op":"translate","text":"x","target_lang":"en","max_tokens":true}',
])
def test_parse_invalid_request(line):
    with pytest.raises(DaemonError) as e:
        parse_request(line)
    assert e.value.code == "invalid_request"


def test_parse_rejects_oversized_line():
    with pytest.raises(DaemonError) as e:
        parse_request(b" " * (4 * 1024 * 1024 + 1))
    assert e.value.code == "invalid_request"


def test_parse_ignores_unknown_fields():
    assert parse_request(b'{"v":1,"op":"status","extra":1}') == StatusRequest()


def test_default_max_tokens():
    assert default_max_tokens(10) == 296
    assert default_max_tokens(10000) == 22768


def test_encode_event_keeps_japanese_and_drops_none():
    line = encode_event(progress("loading"))
    assert line.endswith(b"\n") and line.count(b"\n") == 1
    assert json.loads(line) == {"type": "progress", "stage": "loading"}
    assert "会議".encode() in encode_event(delta("会議"))


def test_event_helpers():
    assert done("t", 1, 2, "m", "r") == {
        "type": "done", "text": "t",
        "usage": {"input_tokens": 1, "output_tokens": 2}, "model": "m", "revision": "r",
    }
    assert error("busy", "full") == {"type": "error", "code": "busy", "message": "full"}
    assert status(model="m", revision=None, loaded=True) == {"type": "status", "model": "m", "loaded": True}


def _fixture_lines(path):
    return [json.loads(l) for l in path.read_text(encoding="utf-8").splitlines()]


@pytest.mark.parametrize("path", FIXTURES, ids=lambda p: p.stem)
def test_fixture_request_matches_expected_outcome(path):
    fake, request, *events = _fixture_lines(path)
    assert "fake" in fake and events
    if "raw" in request:
        line = request["raw"].encode()
    else:
        line = json.dumps(request, ensure_ascii=False).encode()
    terminal = events[-1]
    if path.stem in {"error_template", "error_context_too_long", "error_model_load_failed"}:
        assert parse_request(line) is not None
    elif terminal["type"] == "error":
        with pytest.raises(DaemonError) as e:
            parse_request(line)
        assert e.value.code == terminal["code"]
    else:
        parse_request(line)
