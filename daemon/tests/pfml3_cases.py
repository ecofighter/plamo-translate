"""Requests covering every optional PFML3 element; shared by the golden (fast) and oracle (slow) tests."""

from plamo_translated.protocol import GlossaryEntry, HistoryEntry, TranslateRequest

H = HistoryEntry
G = GlossaryEntry

CASES: dict[str, TranslateRequest] = {
    "minimal": TranslateRequest(text="会議は来週の月曜日に変更されました。", target_lang="en"),
    "source_lang": TranslateRequest(text="Hello", target_lang="ja", source_lang="en"),
    "history_1": TranslateRequest(text="b", target_lang="en", history=(H("a", "A"),)),
    "history_3_source_lang": TranslateRequest(
        text="オーナーでフィルタリングすることもできます。",
        target_lang="en",
        source_lang="ja",
        history=(
            H("プロジェクトの管理画面を開きます。", "Open the project dashboard."),
            H("進行中のタスクがすべて表示されます。", "It lists all active tasks."),
            H("期限切れのタスクは赤で表示されます。", "Overdue tasks are shown in red."),
        ),
    ),
    "all_fields": TranslateRequest(
        text="Hello  world",
        target_lang="ja",
        source_lang="en",
        context="Software manual",
        form="polite",
        history=(H("Open it.", "開いてください。"),),
        glossary=(G("dashboard", "ダッシュボード"), G("task", "タスク")),
    ),
    "context_empty": TranslateRequest(text="x", target_lang="en", context=""),
    "context_whitespace": TranslateRequest(text="x", target_lang="en", context=" \n "),
    "whitespace_text": TranslateRequest(text=" spaced \n text ", target_lang="zh-hans", source_lang="en"),
    "form_plain": TranslateRequest(text="Thanks", target_lang="ja", form="plain"),
    "ja_easy": TranslateRequest(text="Thanks", target_lang="ja(easy)", source_lang="en"),
}


def template_arguments(req: TranslateRequest) -> tuple[list[dict], dict]:
    """The equivalent apply_chat_template(messages, **kwargs) call for a request."""
    messages = []
    for pair in req.history:
        messages += [{"role": "user", "content": pair.source}, {"role": "assistant", "content": pair.translation}]
    messages.append({"role": "user", "content": req.text})
    kwargs: dict = {"target_lang": req.target_lang}
    for key in ("source_lang", "context", "form"):
        if getattr(req, key) is not None:
            kwargs[key] = getattr(req, key)
    if req.glossary:
        kwargs["glossary"] = [{"source": g.source, "target": g.target} for g in req.glossary]
    return messages, kwargs
