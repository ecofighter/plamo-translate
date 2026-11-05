import json
from dataclasses import dataclass

PROTOCOL_VERSION = 1
MAX_LINE_BYTES = 4 * 1024 * 1024
MAX_CONTEXT_TOKENS = 32768


@dataclass(frozen=True)
class GlossaryEntry:
    source: str
    target: str


@dataclass(frozen=True)
class HistoryEntry:
    source: str
    translation: str


@dataclass(frozen=True)
class TranslateRequest:
    text: str
    target_lang: str
    source_lang: str | None = None
    context: str | None = None
    glossary: tuple[GlossaryEntry, ...] = ()
    form: str | None = None
    stream: bool = True
    max_tokens: int | None = None
    history: tuple[HistoryEntry, ...] = ()


@dataclass(frozen=True)
class StatusRequest:
    pass


@dataclass(frozen=True)
class LoadRequest:
    pass


class DaemonError(Exception):
    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code
        self.message = message


def _invalid(message: str) -> DaemonError:
    return DaemonError("invalid_request", message)


def _str_field(obj: dict, key: str, required: bool = False) -> str | None:
    value = obj.get(key)
    if value is None:
        if required:
            raise _invalid(f"missing required field: {key}")
        return None
    if not isinstance(value, str):
        raise _invalid(f"field {key} must be a string")
    return value


def _parse_glossary(raw) -> tuple[GlossaryEntry, ...]:
    if raw is None:
        return ()
    if not isinstance(raw, list):
        raise _invalid("field glossary must be an array")
    entries = []
    for item in raw:
        if not isinstance(item, dict):
            raise _invalid("glossary entries must be objects")
        source, target = item.get("source"), item.get("target")
        if not isinstance(source, str) or not isinstance(target, str):
            raise _invalid("glossary entries need string source and target")
        entries.append(GlossaryEntry(source, target))
    return tuple(entries)


def _parse_history(raw) -> tuple[HistoryEntry, ...]:
    if raw is None:
        return ()
    if not isinstance(raw, list):
        raise _invalid("field history must be an array")
    entries = []
    for item in raw:
        if not isinstance(item, dict):
            raise _invalid("history entries must be objects")
        source, translation = item.get("source"), item.get("translation")
        if not isinstance(source, str) or not isinstance(translation, str):
            raise _invalid("history entries need string source and translation")
        entries.append(HistoryEntry(source, translation))
    return tuple(entries)


def parse_request(line: bytes) -> TranslateRequest | StatusRequest | LoadRequest:
    if len(line) > MAX_LINE_BYTES:
        raise _invalid("request line exceeds 4 MiB")
    try:
        obj = json.loads(line.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise _invalid(f"malformed JSON: {exc}") from exc
    if not isinstance(obj, dict):
        raise _invalid("request must be a JSON object")

    version = obj.get("v")
    if version != PROTOCOL_VERSION or isinstance(version, bool):
        raise DaemonError("unsupported_version", f"unsupported protocol version: {version!r}")

    op = obj.get("op")
    if op == "status":
        return StatusRequest()
    if op == "load":
        return LoadRequest()
    if op != "translate":
        raise _invalid(f"unknown op: {op!r}")

    stream = obj.get("stream", True)
    if not isinstance(stream, bool):
        raise _invalid("field stream must be a boolean")
    max_tokens = obj.get("max_tokens")
    if max_tokens is not None and (
        not isinstance(max_tokens, int) or isinstance(max_tokens, bool) or max_tokens < 1
    ):
        raise _invalid("field max_tokens must be a positive integer")

    return TranslateRequest(
        text=_str_field(obj, "text", required=True),
        target_lang=_str_field(obj, "target_lang", required=True),
        source_lang=_str_field(obj, "source_lang"),
        context=_str_field(obj, "context"),
        glossary=_parse_glossary(obj.get("glossary")),
        form=_str_field(obj, "form"),
        stream=stream,
        max_tokens=max_tokens,
        history=_parse_history(obj.get("history")),
    )


def default_max_tokens(input_tokens: int) -> int:
    return min(4 * input_tokens + 256, MAX_CONTEXT_TOKENS - input_tokens)


def encode_event(event: dict) -> bytes:
    return json.dumps(event, ensure_ascii=False).encode("utf-8") + b"\n"


def _event(type_: str, **fields) -> dict:
    return {"type": type_, **{k: v for k, v in fields.items() if v is not None}}


def progress(stage: str, done_bytes: int | None = None, total_bytes: int | None = None) -> dict:
    return _event("progress", stage=stage, done_bytes=done_bytes, total_bytes=total_bytes)


def delta(text: str) -> dict:
    return _event("delta", text=text)


def done(
    text: str,
    input_tokens: int,
    output_tokens: int,
    model: str,
    revision: str,
    finish_reason: str | None = None,
) -> dict:
    return _event(
        "done",
        text=text,
        finish_reason=finish_reason,
        usage={"input_tokens": input_tokens, "output_tokens": output_tokens},
        model=model,
        revision=revision,
    )


def error(code: str, message: str) -> dict:
    return _event("error", code=code, message=message)


def status(**fields) -> dict:
    return _event("status", **fields)
