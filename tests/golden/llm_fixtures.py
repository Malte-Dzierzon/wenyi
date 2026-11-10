"""Capture LLM golden snapshots from the pinned Python oracle.

Offline only: routing fingerprints, per-kind request bodies, retry
classification, usage summaries, JSON repairs, fake-client call records.
No model API, no network.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "packages" / "core"))
sys.path.insert(0, str(ROOT / "packages" / "core" / "tests"))

from wenyi_core.config import Config  # noqa: E402
from wenyi_core.llm.errors import describe_provider_failure  # noqa: E402
from wenyi_core.llm.json_parser import parse_json_result  # noqa: E402
from wenyi_core.llm.operations import OPERATIONS  # noqa: E402
from wenyi_core.llm.providers import fake as fake_module  # noqa: E402
from wenyi_core.llm.providers._openai_compatible import base_request_kwargs  # noqa: E402
from wenyi_core.llm.providers import deepseek, gemini, openai, openrouter  # noqa: E402
from wenyi_core.llm.providers import openai_compatible, opencode_go  # noqa: E402
from wenyi_core.llm.retrying import (  # noqa: E402
    EmptyResponseError,
    TruncatedResponseError,
    is_resumable_provider_interrupt,
    retry_reason,
)
from wenyi_core.llm.routing import inference_snapshot, resolve_routes  # noqa: E402
from wenyi_core.llm.transport import ResolvedModel  # noqa: E402
from wenyi_core.llm.usage import (  # noqa: E402
    UsageSample,
    UsageTracker,
    merge_usage_summaries,
    usage_delta,
)

SNAPSHOTS = Path(__file__).resolve().parent / "snapshots"

MESSAGES = [
    {"role": "system", "content": "You translate."},
    {"role": "user", "content": "Hello world"},
]


def _write(name: str, payload: object) -> None:
    path = SNAPSHOTS / name
    path.write_text(
        json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )


class FakeHttpError(Exception):
    def __init__(self, status, headers=None):
        super().__init__(f"HTTP {status}")
        self.status_code = status
        self.headers = headers or {}


def capture_llm() -> dict:
    cfg = Config.from_dict({"llm": {"preset": "deepseek"}}).llm
    routes = resolve_routes(cfg)
    operations = sorted(OPERATIONS.keys())
    snapshot = inference_snapshot(cfg, operations)

    # Per-kind request bodies (validated options + canonical bodies).
    bodies = {}
    deepseek_model = ResolvedModel("m", deepseek.DeepSeekOptions())
    bodies["deepseek"] = deepseek.build_request_kwargs(deepseek_model, MESSAGES, max_tokens=100)
    bodies["deepseek_json"] = deepseek.build_request_kwargs(
        deepseek_model, MESSAGES, json_mode=True, max_tokens=100
    )
    bodies["deepseek_no_think"] = deepseek.build_request_kwargs(
        ResolvedModel("m", deepseek.DeepSeekOptions(thinking=False)), MESSAGES
    )
    openai_model = ResolvedModel("m", openai.OpenAIOptions())
    bodies["openai"] = openai.build_request_kwargs(openai_model, MESSAGES, max_tokens=100)
    bodies["openrouter"] = openrouter.build_request_kwargs(
        ResolvedModel("m", openrouter.OpenRouterOptions()), MESSAGES, max_tokens=100
    )
    bodies["opencode_go"] = opencode_go.build_request_kwargs(
        ResolvedModel("m", opencode_go.OpenCodeGoOptions()), MESSAGES, max_tokens=100
    )
    compat_model = ResolvedModel("m", openai_compatible.OpenAICompatibleOptions())
    for style in ("none", "deepseek", "openai", "openrouter"):
        bodies[f"compatible_{style}"] = openai_compatible.build_request_kwargs(
            compat_model, MESSAGES, max_tokens=100, reasoning_style=style
        )
    bodies["base_plain"] = base_request_kwargs("m", MESSAGES, json_mode=False)
    bodies["base_json"] = base_request_kwargs("m", MESSAGES, json_mode=True)
    bodies["gemini_contents"] = gemini.convert_messages_to_gemini(MESSAGES)

    # Retry classification over synthetic failures.
    retry_cases = []
    for status in (400, 401, 402, 403, 404, 408, 409, 422, 429, 500, 503):
        error = FakeHttpError(status)
        retry_cases.append({
            "status": status,
            "reason": retry_reason(error, operation="translation.body"),
            "resumable": is_resumable_provider_interrupt(error),
            "category": describe_provider_failure(error).category,
        })
    retry_cases.append({
        "status": None,
        "kind": "empty",
        "reason": retry_reason(EmptyResponseError("x"), operation="translation.body"),
        "category": describe_provider_failure(EmptyResponseError("x")).category,
    })
    retry_cases.append({
        "status": None,
        "kind": "truncated_synopsis",
        "reason": retry_reason(TruncatedResponseError("x"), operation="synopsis.chapter"),
        "category": describe_provider_failure(TruncatedResponseError("x")).category,
    })
    retry_cases.append({
        "status": None,
        "kind": "truncated_translation",
        "reason": retry_reason(TruncatedResponseError("x"), operation="translation.body"),
    })

    # Usage ledger: record, summarize, delta, merge.
    tracker = UsageTracker()
    tracker.record(
        "strong",
        UsageSample(100, 50, 150, cache_hit_tokens=20, cache_miss_tokens=80, completion_ms=1200),
        "translation.body",
        provider="p1",
        model="m1",
        labels={"p1": "deepseek x", "m1": "deepseek / m"},
    )
    tracker.record("cheap", UsageSample(10, 5, 15), "glossary.curate", provider="p1", model="m2")
    summary = tracker.summary()
    merged = merge_usage_summaries(summary, summary)
    delta = usage_delta(merged, summary)

    # JSON repairs.
    repairs = {}
    for name, text in {
        "strict": '{"a": 1}',
        "missing_close": '{"a": 1',
        "extra_close": '{"a": 1}}}',
        "fenced": '```json\n{"a": 1}\n```',
        "prose": "The answer is 42.",
    }.items():
        try:
            result = parse_json_result(text)
            repairs[name] = {
                "value": result.value,
                "repaired": result.repaired,
                "kind": result.repair_kind,
            }
        except Exception as error:
            repairs[name] = {"error": type(error).__name__}

    # Fake client call records.
    client = fake_module.FakeClient()
    client.complete(MESSAGES, operation="translation.body")
    client.complete(MESSAGES, operation="glossary.curate", json_mode=True, max_tokens=50)

    return {
        "operations": operations,
        "snapshot": snapshot,
        "route_describe": routes["translation.body"].describe(),
        "bodies": bodies,
        "retry": retry_cases,
        "usage_summary": summary,
        "usage_merged": merged,
        "usage_delta": delta,
        "repairs": repairs,
        "fake_calls": client.calls,
    }


def main() -> None:
    _write("llm.json", capture_llm())
    print("wrote llm.json")


if __name__ == "__main__":
    main()
