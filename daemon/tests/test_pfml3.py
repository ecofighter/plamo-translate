import hashlib

import pytest

from conftest import StubTokenizer
from pfml3_cases import CASES
from plamo_translated.pfml3 import (
    CONTROL_TOKENS,
    build_prompt,
    check_tokenizer,
    is_lang_code,
    prompt_ids,
    strip_stop_token,
    template_verified,
    validate,
)
from plamo_translated.protocol import DaemonError, GlossaryEntry, HistoryEntry, TranslateRequest

GOLDEN = {
    "minimal": "<|plamo:bos|><|plamo:tag|>dataset<|plamo:msg|>translation<|plamo:tag|>input<|plamo:msg|>会議は来週の月曜日に変更されました。<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>",
    "source_lang": "<|plamo:bos|><|plamo:tag|>dataset<|plamo:msg|>translation<|plamo:tag|>input<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>Hello<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>ja<|plamo:msg|>",
    "history_1": "<|plamo:bos|><|plamo:tag|>dataset<|plamo:msg|>translation<|plamo:tag|>input<|plamo:msg|>a<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>A<|plamo:tag|>input<|plamo:msg|>b<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>",
    "history_3_source_lang": "<|plamo:bos|><|plamo:tag|>dataset<|plamo:msg|>translation<|plamo:tag|>input<|plamo:key|>lang<|plamo:val|>ja<|plamo:msg|>プロジェクトの管理画面を開きます。<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>Open the project dashboard.<|plamo:tag|>input<|plamo:key|>lang<|plamo:val|>ja<|plamo:msg|>進行中のタスクがすべて表示されます。<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>It lists all active tasks.<|plamo:tag|>input<|plamo:key|>lang<|plamo:val|>ja<|plamo:msg|>期限切れのタスクは赤で表示されます。<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>Overdue tasks are shown in red.<|plamo:tag|>input<|plamo:key|>lang<|plamo:val|>ja<|plamo:msg|>オーナーでフィルタリングすることもできます。<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>",
    "all_fields": "<|plamo:bos|><|plamo:tag|>dataset<|plamo:msg|>translation<|plamo:tag|>context<|plamo:msg|>Software manual<|plamo:tag|>input<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>Open it.<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>ja<|plamo:msg|>開いてください。<|plamo:tag|>glossary<|plamo:index|>dashboard<|plamo:item|>ダッシュボード<|plamo:index|>task<|plamo:item|>タスク<|plamo:tag|>input<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>Hello  world<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>ja<|plamo:key|>form<|plamo:val|>polite<|plamo:msg|>",
    "context_empty": "<|plamo:bos|><|plamo:tag|>dataset<|plamo:msg|>translation<|plamo:tag|>input<|plamo:msg|>x<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>",
    "context_whitespace": "<|plamo:bos|><|plamo:tag|>dataset<|plamo:msg|>translation<|plamo:tag|>context<|plamo:msg|> \n <|plamo:tag|>input<|plamo:msg|>x<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>",
    "whitespace_text": "<|plamo:bos|><|plamo:tag|>dataset<|plamo:msg|>translation<|plamo:tag|>input<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|> spaced \n text <|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>zh-hans<|plamo:msg|>",
    "form_plain": "<|plamo:bos|><|plamo:tag|>dataset<|plamo:msg|>translation<|plamo:tag|>input<|plamo:msg|>Thanks<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>ja<|plamo:key|>form<|plamo:val|>plain<|plamo:msg|>",
    "ja_easy": "<|plamo:bos|><|plamo:tag|>dataset<|plamo:msg|>translation<|plamo:tag|>input<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>Thanks<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>ja(easy)<|plamo:msg|>",
}

RESERVED = "text must not contain reserved PFML tokens (<|plamo:...)."
LANG = "must be a lowercase language code, e.g. en, ja, zh-hans, or ja(easy)."
FORM = 'form must be polite or plain and requires target_lang="ja".'


def test_golden_covers_every_case():
    assert set(GOLDEN) == set(CASES)


@pytest.mark.parametrize("name", sorted(CASES))
def test_build_prompt_golden(name):
    assert build_prompt(CASES[name]) == GOLDEN[name]


@pytest.mark.parametrize("name", sorted(CASES))
def test_golden_cases_are_valid(name):
    validate(CASES[name])


def test_build_prompt_null_and_empty_context_are_identical():
    assert build_prompt(TranslateRequest(text="x", target_lang="en", context="")) == build_prompt(
        TranslateRequest(text="x", target_lang="en")
    )


def test_prompt_ids_encode_once_without_special_tokens():
    tok = StubTokenizer()
    ids = prompt_ids(tok, CASES["minimal"])
    assert tok.calls == [False]
    assert ids[0] == 1 and 1 not in ids[1:]


def test_check_tokenizer_accepts_plamo3_ids():
    check_tokenizer(StubTokenizer())


@pytest.mark.parametrize(("tok", "detail"), [
    (StubTokenizer(ids={t: i for i, t in enumerate(CONTROL_TOKENS, 1) if t != "<|plamo:msg|>"}),
     "<|plamo:msg|> encodes to [60, 124, 112, 108, 97, 109, 111, 58, 109, 115, 103, 124, 62]"),
    (StubTokenizer(unk_token_id=17), "<|plamo:key|> encodes to the unk id 17"),
    (StubTokenizer(eos_token_ids={1}), "<|plamo:tag|> (id 16) is not an eos id"),
    (StubTokenizer(eos_token_ids={16}), "<|plamo:bos|> (id 1) is not an eos id"),
])
def test_check_tokenizer_rejects_incompatible_tokenizers(tok, detail):
    with pytest.raises(DaemonError) as e:
        check_tokenizer(tok)
    assert e.value.code == "model_load_failed"
    assert e.value.message == (
        "model tokenizer does not expose the PFML3 control tokens expected by the prompt builder: " + detail
    )




def test_template_verified(tmp_path):
    (tmp_path / "chat_template.jinja").write_bytes(b"{{ x }}")
    assert template_verified(tmp_path) is False
    assert template_verified(tmp_path / "missing") is False


def test_template_verified_matches_pinned_hash(tmp_path, monkeypatch):
    content = b"pinned"
    monkeypatch.setattr("plamo_translated.pfml3.TEMPLATE_SHA256", hashlib.sha256(content).hexdigest())
    (tmp_path / "chat_template.jinja").write_bytes(content)
    assert template_verified(tmp_path) is True


@pytest.mark.parametrize(("text", "expected"), [
    ("a<|plamo:tag|>", "a"),
    ("a<|plamo:bos|>", "a"),
    ("a<|plamo:tag|><|plamo:tag|>", "a<|plamo:tag|>"),
    ("<|plamo:tag|>a", "<|plamo:tag|>a"),
    ("a<|plamo:msg|>", "a<|plamo:msg|>"),
    (" a \n", " a \n"),
    ("", ""),
])
def test_strip_stop_token(text, expected):
    assert strip_stop_token(text) == expected


@pytest.mark.parametrize(("value", "ok"), [
    ("en", True), ("ja", True), ("abc", True), ("zh-hans", True), ("ja(easy)", True),
    ("en\n", False), ("EN", False), ("zh-Hans", False), ("auto", False), ("en-us", False),
    ("e", False), ("", False), ("abcd", False), ("en-", False), ("ja(easy)\n", False), (" en", False),
])
def test_lang_code(value, ok):
    assert is_lang_code(value) is ok


@pytest.mark.parametrize("field", ["target_lang", "source_lang"])
@pytest.mark.parametrize("value", ["en\n", "EN", "zh-Hans", "auto", "en-us", "e", ""])
def test_lang_code_rejected_by_validate(field, value):
    req = TranslateRequest(text="x", **{"target_lang": "en", field: value})
    with pytest.raises(DaemonError) as e:
        validate(req)
    assert (e.value.code, e.value.message) == ("invalid_request", f"{field} {LANG}")


def _req(**kw) -> TranslateRequest:
    return TranslateRequest(**{"text": "x", "target_lang": "ja", **kw})


@pytest.mark.parametrize(("req", "message"), [
    (_req(history=(HistoryEntry(" \n", "A"),)), "history[0].source must not be empty."),
    (_req(history=(HistoryEntry("a<|plamo:x", "A"),)), "history[0].source must not contain reserved PFML tokens (<|plamo:...)."),
    (_req(history=(HistoryEntry("a", "A"), HistoryEntry("b", ""))), "history[1].translation must not be empty."),
    (_req(history=(HistoryEntry("a", "<|plamo:tag|>"),)), "history[0].translation must not contain reserved PFML tokens (<|plamo:...)."),
    (_req(text="\t"), "text must not be empty."),
    (_req(text="a<|plamo:msg|>b"), RESERVED),
    (_req(text="<|plamo:foo|>"), RESERVED),
    (_req(target_lang="EN"), f"target_lang {LANG}"),
    (_req(source_lang="auto"), f"source_lang {LANG}"),
    (_req(context="<|plamo:"), "context must not contain reserved PFML tokens (<|plamo:...)."),
    (_req(form="formal"), FORM),
    (_req(target_lang="en", form="polite"), FORM),
    (_req(target_lang="ja(easy)", form="plain"), FORM),
    (_req(glossary=(GlossaryEntry(" ", "t"),)), "glossary[0].source must not be empty."),
    (_req(glossary=(GlossaryEntry("s", "t"), GlossaryEntry("s", "<|plamo:item|>"))), "glossary[1].target must not contain reserved PFML tokens (<|plamo:...)."),
    (_req(glossary=(GlossaryEntry("<|plamo:index|>", "t"),)), "glossary[0].source must not contain reserved PFML tokens (<|plamo:...)."),
    (_req(glossary=(GlossaryEntry("s", "\n"),)), "glossary[0].target must not be empty."),
])
def test_validate_messages(req, message):
    with pytest.raises(DaemonError) as e:
        validate(req)
    assert (e.value.code, e.value.message) == ("invalid_request", message)


@pytest.mark.parametrize("req", [
    _req(context=""), _req(context=" "), _req(text="<|PLAMO:tag|>"), _req(text="<|plamo"), _req(form="plain"),
    _req(history=(HistoryEntry(" a ", " b "),)),
])
def test_validate_accepts(req):
    validate(req)


@pytest.mark.parametrize(("req", "message"), [
    # every field broken: the first history field wins
    (TranslateRequest(text="", target_lang="EN", source_lang="x", context="<|plamo:", form="x",
                      history=(HistoryEntry("", "<|plamo:"),), glossary=(GlossaryEntry("", ""),)),
     "history[0].source must not be empty."),
    (_req(history=(HistoryEntry("a", "A"), HistoryEntry("<|plamo:", ""))), "history[1].source must not contain reserved PFML tokens (<|plamo:...)."),
    (_req(text=" <|plamo:tag|>"), RESERVED),
    (_req(text="", target_lang="EN"), "text must not be empty."),
    (_req(target_lang="EN", source_lang="EN"), f"target_lang {LANG}"),
    (_req(source_lang="EN", context="<|plamo:"), f"source_lang {LANG}"),
    (_req(context="<|plamo:", form="x"), "context must not contain reserved PFML tokens (<|plamo:...)."),
    (_req(form="x", glossary=(GlossaryEntry("", ""),)), FORM),
    (_req(glossary=(GlossaryEntry("s", ""), GlossaryEntry("", "t"))), "glossary[0].target must not be empty."),
])
def test_validate_precedence(req, message):
    with pytest.raises(DaemonError) as e:
        validate(req)
    assert e.value.message == message
