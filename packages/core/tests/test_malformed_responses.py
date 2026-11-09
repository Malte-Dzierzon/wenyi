"""Malformed model responses must raise typed errors, never publish silently.

Covers the repair gate shared by translator/reviewer (structurally repaired JSON
is rejected even when it parses), plus wrong-length arrays, empty content,
non-JSON text, null fields, and polisher/precision mismatch behavior.
"""

from __future__ import annotations

import json

import pytest
from wenyi_core.agents.polisher import Polisher
from wenyi_core.agents.precision import PrecisionAgent, PrecisionError, PrecisionInputs
from wenyi_core.agents.reviewer import Reviewer, ReviewOutputError
from wenyi_core.agents.translator import AlignmentError, Translator
from wenyi_core.config import Config
from wenyi_core.llm.json_parser import parse_json_result
from wenyi_core.llm.providers.fake import FakeClient


def _translator_config(retry_limit: int = 0) -> Config:
    return Config.from_dict(
        {
            "language": {"source": "ja", "target": "zh"},
            "llm": {
                "preset": "fake",
                "models": {
                    "default_strong": {"provider": "default", "model": "deepseek-pro"},
                    "default_cheap": {"provider": "default", "model": "deepseek-flash"},
                },
            },
            "pipeline": {"align_retry_limit": retry_limit},
        }
    )


def _reviewer_config() -> Config:
    return Config.from_dict(
        {
            "language": {"source": "ja", "target": "zh"},
            "llm": {
                "preset": "fake",
                "models": {
                    "default_strong": {"provider": "default", "model": "p"},
                    "default_cheap": {"provider": "default", "model": "f"},
                },
            },
        }
    )


def _precision_inputs(*sources: str) -> PrecisionInputs:
    return PrecisionInputs(
        tuple(sources),
        (),
        "prior",
        "style",
        "synopsis",
        "digest",
        [[] for _ in sources],
        "lookahead",
        False,
    )


def _precision_agent(handler, retry_limit: int = 0):
    config = Config.from_dict(
        {
            "language": {"source": "en", "target": "zh"},
            "llm": {"preset": "fake"},
            "pipeline": {"translation_mode": "best_of_three", "align_retry_limit": retry_limit},
        }
    )
    return PrecisionAgent(FakeClient(handler=handler), config)


def _review_ok(count: int, issues: list | None = None) -> str:
    return json.dumps(
        {"issues": issues or [], "reviewed_segments": count, "complete": True},
        ensure_ascii=False,
    )


# Translator: repair gate and structural validation.


def test_translator_truncated_json_raises_alignment_error():
    client = FakeClient(handler=lambda m, t, j: '{"translations": ["\u8bd1')
    with pytest.raises(AlignmentError):
        Translator(client, _translator_config()).translate_batch(["\u3042", "\u3044"])
    assert len(client.calls) >= 1


def test_translator_structural_repair_is_rejected():
    raw = '{"translations": ["\u8bd10", "\u8bd11"'
    assert not parse_json_result(raw).safe_for_complete_payload
    client = FakeClient(handler=lambda m, t, j: raw)
    translator = Translator(client, _translator_config())
    with pytest.raises(AlignmentError):
        translator._call_batch(["\u3042", "\u3044"], [], "", "")


def test_translator_invented_items_beyond_count_are_rejected():
    raw = json.dumps({"translations": ["\u8bd10", "\u8bd11", "\u8bd1extra"]})
    client = FakeClient(handler=lambda m, t, j: raw)
    with pytest.raises(AlignmentError, match="count mismatch"):
        Translator(client, _translator_config())._call_batch(["\u3042", "\u3044"], [], "", "")


def test_translator_empty_content_raises_alignment_error():
    client = FakeClient(handler=lambda m, t, j: "")
    with pytest.raises(AlignmentError):
        Translator(client, _translator_config()).translate_batch(["\u3042"])


def test_translator_wrong_length_array_raises_alignment_error():
    client = FakeClient(handler=lambda m, t, j: json.dumps({"translations": ["\u53ea\u4e00\u6bb5"]}))
    with pytest.raises(AlignmentError, match="count mismatch"):
        Translator(client, _translator_config())._call_batch(
            ["\u3042", "\u3044", "\u3046"], [], "", ""
        )


def test_translator_non_json_text_raises_alignment_error():
    client = FakeClient(handler=lambda m, t, j: "The translation is done, looks good!")
    with pytest.raises(AlignmentError):
        Translator(client, _translator_config())._call_batch(["\u3042"], [], "", "")


def test_translator_null_items_are_rejected():
    raw = json.dumps({"translations": [None, "\u8bd11"]})
    client = FakeClient(handler=lambda m, t, j: raw)
    with pytest.raises(AlignmentError, match="non-string"):
        Translator(client, _translator_config())._call_batch(["\u3042", "\u3044"], [], "", "")


def test_translator_missing_translations_key_is_rejected():
    raw = json.dumps({"result": ["\u8bd1\u6587"]})
    client = FakeClient(handler=lambda m, t, j: raw)
    with pytest.raises(AlignmentError, match="translation array"):
        Translator(client, _translator_config())._call_batch(["\u3042"], [], "", "")


def test_translator_blank_items_are_rejected_without_empty_allowance():
    raw = json.dumps({"translations": ["", ""]})
    client = FakeClient(handler=lambda m, t, j: raw)
    with pytest.raises(AlignmentError, match="empty"):
        Translator(client, _translator_config())._call_batch(["\u3042", "\u3044"], [], "", "")


# Reviewer: structured protocol errors.


def _reviewer(raw: str) -> Reviewer:
    return Reviewer(FakeClient(handler=lambda m, t, j: raw), _reviewer_config())


def test_reviewer_malformed_json_raises_review_output_error():
    # Empty content is unrecoverable JSON; repairable-looking garbage instead
    # hits the structural-repair gate (covered below).
    with pytest.raises(ReviewOutputError) as exc:
        _reviewer("").review(["あ"], ["訳"])
    assert exc.value.reason == "malformed_json"


def test_reviewer_garbage_text_hits_structural_repair_gate():
    with pytest.raises(ReviewOutputError) as exc:
        _reviewer('{"issues": [broken').review(["あ"], ["訳"])
    assert exc.value.reason == "unsafe_json_repair"


def test_reviewer_structural_repair_is_rejected():
    raw = '{"issues": [{"index": 0, "type": "missing", "detail": "d", "suggestion": "s"}'
    assert not parse_json_result(raw).safe_for_complete_payload
    with pytest.raises(ReviewOutputError) as exc:
        _reviewer(raw).review(["あ"], ["訳"])
    assert exc.value.reason == "unsafe_json_repair"


def test_reviewer_boundary_only_repair_is_accepted():
    raw = _review_ok(1) + "}"
    assert parse_json_result(raw).safe_for_complete_payload
    assert _reviewer(raw).review(["\u3042"], ["\u8bd1"]) == []


def test_reviewer_reviewed_segments_mismatch_raises():
    with pytest.raises(ReviewOutputError) as exc:
        _reviewer(_review_ok(2)).review(["\u3042"], ["\u8bd1"])
    assert exc.value.reason == "reviewed_segments_mismatch"


def test_reviewer_missing_completion_marker_raises():
    raw = json.dumps({"issues": [], "reviewed_segments": 1, "complete": False})
    with pytest.raises(ReviewOutputError) as exc:
        _reviewer(raw).review(["\u3042"], ["\u8bd1"])
    assert exc.value.reason == "completion_marker_missing"


def test_reviewer_non_object_response_raises():
    with pytest.raises(ReviewOutputError) as exc:
        _reviewer('["not", "an object"]').review(["\u3042"], ["\u8bd1"])
    assert exc.value.reason == "response_not_object"


def test_reviewer_plain_text_response_raises():
    with pytest.raises(ReviewOutputError):
        _reviewer("No issues found, translation looks fine.").review(["\u3042"], ["\u8bd1"])


def test_reviewer_null_issue_index_raises():
    issues = [{"index": None, "type": "missing", "detail": "d", "suggestion": "s"}]
    with pytest.raises(ReviewOutputError) as exc:
        _reviewer(_review_ok(1, issues)).review(["\u3042"], ["\u8bd1"])
    assert exc.value.reason == "invalid_issue_index"


def test_reviewer_out_of_range_issue_index_raises():
    issues = [{"index": 5, "type": "missing", "detail": "d", "suggestion": "s"}]
    with pytest.raises(ReviewOutputError) as exc:
        _reviewer(_review_ok(1, issues)).review(["\u3042"], ["\u8bd1"])
    assert exc.value.reason == "invalid_issue_index"


def test_reviewer_invalid_issue_type_raises():
    issues = [{"index": 0, "type": "invented", "detail": "d", "suggestion": "s"}]
    with pytest.raises(ReviewOutputError) as exc:
        _reviewer(_review_ok(1, issues)).review(["\u3042"], ["\u8bd1"])
    assert exc.value.reason == "invalid_issue_type"


def test_reviewer_blank_issue_detail_raises():
    issues = [{"index": 0, "type": "missing", "detail": "  ", "suggestion": "s"}]
    with pytest.raises(ReviewOutputError) as exc:
        _reviewer(_review_ok(1, issues)).review(["\u3042"], ["\u8bd1"])
    assert exc.value.reason == "invalid_issue_detail"


# Polisher: keep input on mismatch.


def _polisher(raw: str) -> tuple[Polisher, FakeClient]:
    client = FakeClient(handler=lambda m, t, j: raw)
    return Polisher(client, _reviewer_config()), client


def test_polisher_wrong_length_array_keeps_input():
    polisher, _ = _polisher(json.dumps({"polished": ["\u53ea\u4e00\u6bb5"]}))
    assert polisher.polish(["\u7532", "\u4e59"]) == ["\u7532", "\u4e59"]
    assert polisher.last_polish_failed is True


def test_polisher_non_list_value_keeps_input():
    polisher, _ = _polisher(json.dumps({"polished": "notalist"}))
    assert polisher.polish(["\u7532", "\u4e59"]) == ["\u7532", "\u4e59"]
    assert polisher.last_polish_failed is True


def test_polisher_empty_content_keeps_input():
    polisher, _ = _polisher("")
    assert polisher.polish(["\u7532", "\u4e59"]) == ["\u7532", "\u4e59"]
    assert polisher.last_polish_failed is True


def test_polisher_missing_key_keeps_input():
    polisher, _ = _polisher(json.dumps({"other": ["\u7532", "\u4e59"]}))
    assert polisher.polish(["\u7532", "\u4e59"]) == ["\u7532", "\u4e59"]
    assert polisher.last_polish_failed is True


def test_polisher_continue_mismatch_returns_none():
    client = FakeClient(
        handler=lambda m, t, j: json.dumps({"polished": ["\u53ea\u4e00\u6bb5"]})
    )
    turn = [
        {"role": "system", "content": "sys"},
        {"role": "user", "content": "translate"},
        {"role": "assistant", "content": json.dumps({"translations": ["\u7532", "\u4e59"]})},
    ]
    assert Polisher(client, _reviewer_config()).polish_continue(turn, n=2) is None


# Precision path: errors, never silent publish.


def test_precision_malformed_response_raises_not_silent():
    worker = _precision_agent(lambda m, t, j: "definitely not json{{{{")
    with pytest.raises(PrecisionError):
        worker.translate(_precision_inputs("one", "two"))


def test_precision_wrong_length_array_raises_not_silent():
    worker = _precision_agent(lambda m, t, j: json.dumps({"translations": ["only"]}))
    with pytest.raises(PrecisionError, match="count"):
        worker.translate(_precision_inputs("one", "two"))


def test_precision_null_item_raises_not_silent():
    worker = _precision_agent(lambda m, t, j: json.dumps({"translations": ["ok", None]}))
    with pytest.raises(PrecisionError, match="must be a string"):
        worker.translate(_precision_inputs("one", "two"))
