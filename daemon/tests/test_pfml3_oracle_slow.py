"""The bundled chat_template.jinja is the oracle for the PFML3 builder.

PLAMO_ORACLE_MODELS overrides the snapshots: comma-separated `repo@revision`, resolved
from the daemon's hub cache without network access.
"""

import os
from pathlib import Path

import pytest
from huggingface_hub import snapshot_download
from mlx_lm.utils import load_config, load_tokenizer

from pfml3_cases import CASES, template_arguments
from plamo_translated.config import DEFAULT_MODEL, DEFAULT_REVISION, paths
from plamo_translated.pfml3 import build_prompt, check_tokenizer, prompt_ids, validate
from plamo_translated.protocol import DaemonError, GlossaryEntry, HistoryEntry, TranslateRequest

pytestmark = pytest.mark.slow

DEFAULT_ORACLE_MODELS = (
    f"{DEFAULT_MODEL}@{DEFAULT_REVISION},"
    "pfnet/plamo-3-translate-0.8b-MLX-4bit@e5fe5b7f26ebaeae48db0ce644b932d1d9b91aaa"
)
# The tokenizer and template files only; the 2b weights may be absent or partial in the cache.
TOKENIZER_FILES = ["*.json", "*.py", "*.jinja", "*.jsonl"]
MODELS = os.environ.get("PLAMO_ORACLE_MODELS", DEFAULT_ORACLE_MODELS).split(",")


@pytest.fixture(scope="module", params=MODELS)
def tokenizer(request):
    repo, revision = request.param.split("@")
    snapshot = Path(
        snapshot_download(
            repo,
            revision=revision,
            cache_dir=paths(Path.home()).hub_cache,
            local_files_only=True,
            allow_patterns=TOKENIZER_FILES,
        )
    )
    # Same tokenizer construction as mlx_lm.load in MlxEngine, without loading weights.
    return load_tokenizer(
        snapshot, {"trust_remote_code": True}, eos_token_ids=load_config(snapshot).get("eos_token_id")
    )


@pytest.mark.parametrize("name", sorted(CASES))
def test_oracle_prompt_string(tokenizer, name):
    messages, kwargs = template_arguments(CASES[name])
    expected = tokenizer.apply_chat_template(messages, add_generation_prompt=True, tokenize=False, **kwargs)
    assert build_prompt(CASES[name]) == expected


@pytest.mark.parametrize("name", sorted(CASES))
def test_oracle_prompt_ids(tokenizer, name):
    messages, kwargs = template_arguments(CASES[name])
    expected = tokenizer.apply_chat_template(
        messages, add_generation_prompt=True, tokenize=True, return_dict=False, **kwargs
    )
    ids = prompt_ids(tokenizer, CASES[name])
    assert ids == list(expected)
    assert ids[0] == tokenizer.bos_token_id and tokenizer.bos_token_id not in ids[1:]


def test_oracle_tokenizer_invariants(tokenizer):
    check_tokenizer(tokenizer)


def _req(**kw) -> TranslateRequest:
    return TranslateRequest(**{"text": "x", "target_lang": "ja", **kw})


# Accept/reject agreement with the template (messages differ by design: field names and indexes).
AGREEMENT = {
    "text_ideographic_space": (_req(text="\u3000"), False),
    "text_nbsp": (_req(text="\u00a0\n"), False),
    "text_unicode_padded": (_req(text="\u3000a\u00a0"), True),
    "text_reserved": (_req(text="a<|plamo:"), False),
    "text_reserved_case": (_req(text="<|PLAMO:tag|>"), True),
    "target_newline": (_req(target_lang="en\n"), False),
    "target_upper": (_req(target_lang="EN"), False),
    "target_region_case": (_req(target_lang="zh-Hans"), False),
    "target_auto": (_req(target_lang="auto"), False),
    "target_empty": (_req(target_lang=""), False),
    "target_three": (_req(target_lang="abc"), True),
    "source_bad": (_req(source_lang="en-us"), False),
    "context_reserved": (_req(context="<|plamo:x"), False),
    "context_ideographic_space": (_req(context="\u3000"), True),
    "form_en": (_req(target_lang="en", form="polite"), False),
    "form_easy": (_req(target_lang="ja(easy)", form="plain"), False),
    "form_unknown": (_req(form="formal"), False),
    "glossary_empty_target": (_req(glossary=(GlossaryEntry("s", "\u3000"),)), False),
    "glossary_reserved": (_req(glossary=(GlossaryEntry("<|plamo:item|>", "t"),)), False),
    "history_empty_translation": (_req(history=(HistoryEntry("a", " "),)), False),
    "history_reserved_source": (_req(history=(HistoryEntry("<|plamo:msg|>", "A"),)), False),
}


def _accepts(fn) -> bool:
    try:
        fn()
    except DaemonError:
        return False
    except Exception as e:  # the template raises jinja2.TemplateError
        if type(e).__name__ != "TemplateError":
            raise
        return False
    return True


@pytest.mark.parametrize("name", sorted(AGREEMENT))
def test_oracle_validate_agrees_with_template(tokenizer, name):
    req, accepted = AGREEMENT[name]
    messages, kwargs = template_arguments(req)
    template = _accepts(lambda: tokenizer.apply_chat_template(messages, add_generation_prompt=True, tokenize=False, **kwargs))
    assert (_accepts(lambda: validate(req)), template) == (accepted, accepted)
