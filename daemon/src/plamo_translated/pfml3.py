"""PFML3 prompt rules: request validation, prompt construction, tokenizer checks.

Mirrors the model's bundled chat_template.jinja, which stays the oracle (slow tests).
"""

from __future__ import annotations

import hashlib
import re
from pathlib import Path

from .protocol import DaemonError, TranslateRequest

BOS = "<|plamo:bos|>"
TAG = "<|plamo:tag|>"
KEY = "<|plamo:key|>"
VAL = "<|plamo:val|>"
MSG = "<|plamo:msg|>"
INDEX = "<|plamo:index|>"
ITEM = "<|plamo:item|>"
CONTROL_TOKENS = (BOS, TAG, KEY, VAL, MSG, INDEX, ITEM)
STOP_TOKENS = (TAG, BOS)
RESERVED_PREFIX = "<|plamo:"

TEMPLATE_FILE = "chat_template.jinja"
# sha256 of chat_template.jinja at the pinned 2b revision; update only after `pytest -m slow` passes.
TEMPLATE_SHA256 = "cd49e7e0dfa8ec847c163db88dfca89b280dddbb73ddc60ba38c62a420eaced2"

_LANG_CODE = re.compile(r"[a-z]{2,3}(-[a-z]{4})?")
_LANG_MESSAGE = "must be a lowercase language code, e.g. en, ja, zh-hans, or ja(easy)."
_TOKENIZER_MESSAGE = "model tokenizer does not expose the PFML3 control tokens expected by the prompt builder: "


def _invalid(message: str) -> DaemonError:
    return DaemonError("invalid_request", message)


def is_lang_code(value: str) -> bool:
    # fullmatch, not match with $: "$" also matches before a trailing newline.
    return value == "ja(easy)" or _LANG_CODE.fullmatch(value) is not None


def _check_text(value: str, name: str, allow_empty: bool = False) -> None:
    if not allow_empty and not value.strip():
        raise _invalid(f"{name} must not be empty.")
    if RESERVED_PREFIX in value:
        raise _invalid(f"{name} must not contain reserved PFML tokens (<|plamo:...).")


def validate(req: TranslateRequest) -> None:
    """Raises invalid_request for the first rule the request breaks, in chat-template order."""
    for i, pair in enumerate(req.history):
        _check_text(pair.source, f"history[{i}].source")
        _check_text(pair.translation, f"history[{i}].translation")
    _check_text(req.text, "text")
    if not is_lang_code(req.target_lang):
        raise _invalid(f"target_lang {_LANG_MESSAGE}")
    if req.source_lang is not None and not is_lang_code(req.source_lang):
        raise _invalid(f"source_lang {_LANG_MESSAGE}")
    if req.context is not None:
        _check_text(req.context, "context", allow_empty=True)
    if req.form is not None and (req.form not in ("polite", "plain") or req.target_lang != "ja"):
        raise _invalid('form must be polite or plain and requires target_lang="ja".')
    for i, entry in enumerate(req.glossary):
        _check_text(entry.source, f"glossary[{i}].source")
        _check_text(entry.target, f"glossary[{i}].target")


def build_prompt(req: TranslateRequest) -> str:
    """The PFML3 prompt for a validated request, BOS included; user text is copied verbatim."""
    source = f"{KEY}lang{VAL}{req.source_lang}" if req.source_lang is not None else ""
    output = f"{TAG}output{KEY}lang{VAL}{req.target_lang}"
    parts = [BOS, TAG, "dataset", MSG, "translation"]
    if req.context:
        parts += [TAG, "context", MSG, req.context]
    for pair in req.history:
        parts += [TAG, "input", source, MSG, pair.source, output, MSG, pair.translation]
    if req.glossary:
        parts += [TAG, "glossary"]
        for entry in req.glossary:
            parts += [INDEX, entry.source, ITEM, entry.target]
    parts += [TAG, "input", source, MSG, req.text, output]
    if req.form is not None:
        parts += [KEY, "form", VAL, req.form]
    parts.append(MSG)
    return "".join(parts)


def prompt_ids(tokenizer, req: TranslateRequest) -> list[int]:
    # The prompt already starts with the BOS string; add_special_tokens would prepend a second one.
    return tokenizer.encode(build_prompt(req), add_special_tokens=False)


def strip_stop_token(text: str) -> str:
    """Removes one trailing stop-token string, which mlx-lm normally never emits."""
    for token in STOP_TOKENS:
        if text.endswith(token):
            return text[: -len(token)]
    return text


def check_tokenizer(tokenizer) -> None:
    """Raises model_load_failed unless every control string is one known id and the stop tokens are eos ids."""
    ids: dict[str, int] = {}
    for token in CONTROL_TOKENS:
        encoded = tokenizer.encode(token, add_special_tokens=False)
        if len(encoded) != 1:
            raise DaemonError("model_load_failed", f"{_TOKENIZER_MESSAGE}{token} encodes to {encoded}")
        if encoded[0] == tokenizer.unk_token_id:
            raise DaemonError("model_load_failed", f"{_TOKENIZER_MESSAGE}{token} encodes to the unk id {encoded[0]}")
        ids[token] = encoded[0]
    for token in STOP_TOKENS:
        if ids[token] not in tokenizer.eos_token_ids:
            raise DaemonError("model_load_failed", f"{_TOKENIZER_MESSAGE}{token} (id {ids[token]}) is not an eos id")


def template_verified(snapshot: Path) -> bool:
    try:
        data = (snapshot / TEMPLATE_FILE).read_bytes()
    except OSError:
        return False
    return hashlib.sha256(data).hexdigest() == TEMPLATE_SHA256
