"""What the window knows about the router's roles and the role tags (no Qt needed)."""

from __future__ import annotations

from mm_gui.models import (
    is_chat_model,
    model_label,
    roles_of,
    suits_role,
    without_role_tags,
    without_tags_in_last_user_message,
)


def test_a_role_tag_goes_with_the_space_after_it():
    assert without_role_tags("@deep prove this") == "prove this"
    assert without_role_tags("hello @main world") == "hello world"
    assert without_role_tags("hi @fast") == "hi"
    assert without_role_tags("@FAST hi") == "hi"


def test_a_line_break_stays_and_a_message_without_a_tag_is_unchanged():
    assert without_role_tags("line one\n@fast line two") == "line one\nline two"
    assert without_role_tags("  keep   the  spacing ") == "  keep   the  spacing "


def test_a_word_that_only_contains_a_tag_is_not_one():
    assert without_role_tags("mail me@fast.com") == "mail me@fast.com"
    assert without_role_tags("the @fastest way") == "the @fastest way"


def test_only_the_last_user_message_loses_its_tags():
    history = [
        {"role": "user", "content": "@fast first"},
        {"role": "assistant", "content": "ok"},
        {"role": "user", "content": "@deep second"},
    ]
    out = without_tags_in_last_user_message(history)
    assert [m["content"] for m in out] == ["@fast first", "ok", "second"]
    assert history[2]["content"] == "@deep second"  # the conversation itself is untouched
    only_system = [{"role": "system", "content": "@fast"}]
    assert without_tags_in_last_user_message(only_system) is only_system


def test_each_role_takes_the_models_that_suit_it():
    for role in ("fast", "main", "deep"):
        assert suits_role(role, "qwen3-8b", True)
        assert not suits_role(role, "nomic-embed-text", False)
    assert suits_role("embed", "nomic-embed-text", False)
    assert suits_role("embed", "bge-m3", False)
    assert not suits_role("embed", "bge-reranker-v2-m3", False)
    assert suits_role("rerank", "bge-reranker-v2-m3", False)
    assert not suits_role("rerank", "nomic-embed-text", False)
    assert not suits_role("embed", "qwen3-8b", True)


def test_the_roles_that_name_a_model():
    status = {
        "roles": [{"role": "fast", "model": "a"}, {"role": "main", "model": "b"}, {"role": "deep", "model": None}],
        "support": [{"role": "embed", "model": "b"}],
    }
    assert roles_of(status, "b") == ["main", "embed"]
    assert roles_of(status, "zzz") == []
    assert roles_of(None, "b") == []


def test_the_router_is_a_chat_choice_with_its_own_label():
    router = {"id": "auto", "source": "router", "router": True, "ready": True}
    assert is_chat_model(router)
    assert model_label(router).startswith("Auto: the router picks")
