"""Fault-injection regression tests (offline, deterministic).

Covers localhost fail-fast, cloud 429 Retry-After honoring, 500 exhaustion
with events, mid-generation timeout resumability, split connect/read timeouts
and probe adapter sharing. No live network; backoff sleeps are injected
recorders, never real waits.
"""

from __future__ import annotations

import time
from types import SimpleNamespace
from typing import Any

import httpx
import pytest
from openai import APIConnectionError, APITimeoutError
from wenyi_core.llm.configuration import LLMConfig
from wenyi_core.llm.connectivity import PROBE_OUTPUT_TOKENS, probe_connections
from wenyi_core.llm.errors import describe_provider_failure
from wenyi_core.llm.providers._openai_compatible import OpenAICompatibleBaseClient
from wenyi_core.llm.retrying import (
    RetryReporter,
    TruncatedResponseError,
    is_resumable_provider_interrupt,
    is_retryable_provider_error,
    provider_retry,
    retry_reason,
)
from wenyi_core.llm.router import RoutedLLMClient

from tests.model_fixtures import model_config

SECRET = "fake-fault-secret-not-a-real-key"
_NO_THINKING = {"thinking": False}


def _transport_config(*, kind="deepseek", base_url="https://example.invalid/v1", max_retries=4, **extra):
    return model_config(
        kind=kind,
        base_url=base_url,
        api_key_env="TEST_LLM_KEY",
        timeout=1,
        max_retries=max_retries,
        profiles={"strong": dict(model="test-model")},
        **extra,
    )


class _HttpError(Exception):
    def __init__(self, status_code: int, *, headers: dict[str, str] | None = None):
        super().__init__(f"HTTP {status_code}")
        self.status_code = status_code
        self.request_id = "req-test"
        self.response = SimpleNamespace(status_code=status_code, headers=headers or {})


def _response(content: str = "ok") -> Any:
    return SimpleNamespace(
        choices=[SimpleNamespace(message=SimpleNamespace(content=content))],
        usage=None,
    )


class _CompletionsStub:
    def __init__(self, outcomes: list[Any]):
        self.outcomes = list(outcomes)
        self.calls = 0
        self.kwargs: list[dict[str, Any]] = []

    def create(self, **kwargs: Any) -> Any:
        self.calls += 1
        self.kwargs.append(kwargs)
        outcome = self.outcomes.pop(0)
        if isinstance(outcome, BaseException):
            raise outcome
        return outcome


class _ClientStub:
    def __init__(self, outcomes: list[Any]):
        self.completions = _CompletionsStub(outcomes)
        self.chat = SimpleNamespace(completions=self.completions)


def _client(max_retries: int, outcomes: list[Any], *, local_base: str | None = None):
    config = _transport_config(
        max_retries=max_retries,
        **({"base_url": local_base} if local_base else {}),
    )
    client = RoutedLLMClient(config)
    client.adapter("default")._client = _ClientStub(outcomes)
    return client


def _retry_events(client: RoutedLLMClient) -> list[dict[str, Any]]:
    events: list[dict[str, Any]] = []
    client.set_event_sink(
        lambda event, **data: (
            events.append({"event": event, **data}) if event.startswith("llm_retry_") else None
        )
    )
    return events


def _reporter() -> RetryReporter:
    return RetryReporter(
        provider="t", tier="direct", stage="translation.body", max_attempts=5,
        emit=lambda *args, **kwargs: None,
    )


def test_localhost_connection_refused_fails_fast(monkeypatch):
    calls: list[int] = []
    sleeps: list[float] = []

    @provider_retry(4, _reporter(), sleep=sleeps.append, local=True)
    def fail():
        calls.append(1)
        raise httpx.ConnectError("connection refused")

    started = time.monotonic()
    with pytest.raises(httpx.ConnectError):
        fail()
    assert time.monotonic() - started < 2.0
    assert len(calls) == 1
    assert sleeps == []


def test_localhost_fail_fast_without_status_only(monkeypatch):
    """Loopback fail-fast applies to status-less transport errors, not HTTP 5xx."""
    calls: list[int] = []

    @provider_retry(4, _reporter(), sleep=lambda seconds: None, local=True)
    def refused():
        calls.append(1)
        raise httpx.ConnectError("refused")

    with pytest.raises(httpx.ConnectError):
        refused()
    assert len(calls) == 1
    assert is_retryable_provider_error(httpx.ConnectError("refused"))
    # HTTP statuses on localhost still retry: the daemon is up, the request failed.
    assert retry_reason(_HttpError(503), operation="translation.body") == "http_503"


def test_localhost_500_still_retries():
    calls: list[int] = []

    @provider_retry(2, _reporter(), sleep=lambda seconds: None, local=True)
    def flaky():
        calls.append(1)
        if len(calls) < 3:
            raise _HttpError(500)
        return "ok"

    assert flaky() == "ok"
    assert len(calls) == 3


def test_cloud_429_honors_retry_after_then_succeeds():
    client = _client(2, [_HttpError(429, headers={"retry-after": "7"}), _response()])
    events = _retry_events(client)

    assert client.complete([{"role": "user", "content": "x"}], operation="translation.body") == "ok"
    assert [event["event"] for event in events] == ["llm_retry_wait"]
    assert events[0]["reason"] == "http_429"
    assert events[0]["failed_attempt"] == 1
    assert events[0]["next_attempt"] == 2
    assert events[0]["wait_seconds"] == 7
    assert events[0]["wait_source"] == "server"


def test_cloud_429_retry_after_ms_honored():
    client = _client(2, [_HttpError(429, headers={"retry-after-ms": "250"}), _response()])
    events = _retry_events(client)

    assert client.complete([{"role": "user", "content": "x"}], operation="translation.body") == "ok"
    assert events[0]["wait_seconds"] == pytest.approx(0.25)
    assert events[0]["wait_source"] == "server"


def test_retry_after_capped_at_safe_upper_bound():
    from wenyi_core.llm.retrying import wait_for_provider_retry

    state = SimpleNamespace(outcome=SimpleNamespace(exception=lambda: _HttpError(429, headers={"retry-after": "3600"})))
    assert wait_for_provider_retry(state) == pytest.approx(30.0)


def test_500_exhausts_with_wait_and_exhausted_events():
    client = _client(2, [_HttpError(500, headers={"retry-after-ms": "0"})] * 3)
    events = _retry_events(client)

    with pytest.raises(_HttpError):
        client.complete([{"role": "user", "content": "x"}], operation="translation.body")

    assert client.adapter("default")._client.completions.calls == 3
    assert [event["event"] for event in events] == [
        "llm_retry_wait",
        "llm_retry_wait",
        "llm_retry_exhausted",
    ]
    assert events[-1]["attempts"] == 3
    assert events[-1]["reason"] == "http_500"
    assert events[-1]["stage"] == "translation.body"


def test_permanent_401_not_retried_and_no_exhaustion_event():
    client = _client(4, [_HttpError(401)])
    events = _retry_events(client)

    with pytest.raises(_HttpError):
        client.complete([{"role": "user", "content": "x"}], operation="translation.body")

    assert client.adapter("default")._client.completions.calls == 1
    assert events == []


def test_timeout_mid_generation_is_retryable_and_resumable():
    request = httpx.Request("POST", "https://example.invalid/v1")
    error = APITimeoutError(request)
    assert retry_reason(error) == "timeout"
    assert retry_reason(TimeoutError("timed out")) == "timeout"
    assert is_retryable_provider_error(error)
    assert is_resumable_provider_interrupt(error)


def test_timeout_event_category_is_timeout():
    request = httpx.Request("POST", "https://example.invalid/v1")
    failure = describe_provider_failure(APITimeoutError(request))
    assert failure.category == "timeout"
    assert failure.status_code is None


def test_truncated_response_resumable_only_for_summary_operations():
    error = TruncatedResponseError("cut off")
    assert retry_reason(error, operation="synopsis.chapter") == "truncated_response"
    assert retry_reason(error, operation="synopsis.book") == "truncated_response"
    assert retry_reason(error, operation="translation.body") is None


def test_connection_failure_category_without_status():
    request = httpx.Request("POST", "https://example.invalid/v1")
    failure = describe_provider_failure(APIConnectionError(request=request))
    assert failure.category == "connection_failed"
    assert failure.status_code is None


def _status_error_response(status: int) -> SimpleNamespace:
    request = httpx.Request(
        "POST", "https://example.invalid", headers={"Authorization": f"Bearer {SECRET}"}
    )
    return SimpleNamespace(
        status=status, body={"error": "boom"}, request=request,
        status_code=status, headers={},
    )


def test_402_insufficient_balance_is_resumable_interrupt():
    assert is_resumable_provider_interrupt(_HttpError(402))
    assert is_resumable_provider_interrupt(RuntimeError("Insufficient Balance"))
    assert not is_resumable_provider_interrupt(ValueError("invalid review config"))


def test_cloud_retries_exhaust_attempt_budget():
    calls: list[int] = []

    @provider_retry(4, _reporter(), sleep=lambda seconds: None, local=False)
    def fail():
        calls.append(1)
        raise httpx.ConnectError("cloud unreachable")

    with pytest.raises(httpx.ConnectError):
        fail()
    assert len(calls) == 5


def test_sdk_timeout_splits_connect_and_read_phases():
    import os
    from unittest.mock import patch

    raw = {
        "providers": {
            "a": {
                "kind": "deepseek",
                "base_url": "https://example.invalid/v1",
                "api_key_env": "TEST_X_KEY",
                "timeout": 600,
                "connect_timeout": 7,
            }
        },
        "models": {"one": {"provider": "a", "model": "x"}},
        "tiers": {"strong": "one", "cheap": "one", "fast": "one"},
    }
    client = RoutedLLMClient(LLMConfig.model_validate(raw))
    with (
        patch.dict(os.environ, {"TEST_X_KEY": "secret"}),
        patch("openai.OpenAI") as openai_type,
    ):
        client.adapter("a")._ensure_client()
    _, kwargs = openai_type.call_args
    timeout = kwargs["timeout"]
    assert isinstance(timeout, httpx.Timeout)
    assert timeout.connect == 7
    assert timeout.read == 600
    assert timeout.write == 30.0
    assert timeout.pool == 7


def test_adapter_client_constructed_once_per_connection(monkeypatch):
    import os
    from unittest.mock import patch

    raw = {
        "providers": {
            "a": {
                "kind": "deepseek",
                "base_url": "https://example.invalid/v1",
                "api_key_env": "TEST_X_KEY",
            }
        },
        "models": {"one": {"provider": "a", "model": "x"}},
        "tiers": {"strong": "one", "cheap": "one", "fast": "one"},
    }
    client = RoutedLLMClient(LLMConfig.model_validate(raw))
    with (
        patch.dict(os.environ, {"TEST_X_KEY": "secret"}),
        patch("openai.OpenAI") as openai_type,
    ):
        first = client.adapter("a")
        second = client.adapter("a")
        assert first is second
        # SDK client is lazy: adapter caching alone constructs nothing.
        assert openai_type.call_count == 0
        first._ensure_client()
        second._ensure_client()
        assert openai_type.call_count == 1


def _probe_config(**extra) -> LLMConfig:
    return LLMConfig.model_validate(
        {
            "providers": {
                "main": {"kind": "openai", "api_key_env": "PROBE_MAIN_KEY", "max_retries": 4},
                "backup": {
                    "kind": "openai-compatible",
                    "base_url": "https://backup.invalid/v1",
                    "api_key_env": "PROBE_BACKUP_KEY",
                },
            },
            "models": {
                "strong": {"provider": "main", "model": "big", "options": _NO_THINKING},
                "small": {"provider": "main", "model": "small", "options": _NO_THINKING},
                "spare": {"provider": "backup", "model": "spare"},
            },
            "tiers": {"strong": "strong", "cheap": "small", "fast": "small"},
            **extra,
        }
    )


@pytest.fixture
def probe_sdk(monkeypatch):
    """Inject SDK responses; count adapter constructions per connection."""
    calls: list[dict] = []
    behaviors: dict[str, object] = {}
    constructs: list[str] = []

    def create(**kwargs):
        calls.append(kwargs)
        behavior = behaviors.get(kwargs["model"], _sdk_response())
        if isinstance(behavior, Exception):
            raise behavior
        return behavior

    def ensure(self):
        # Mirror _ensure_client locking: one SDK client per shared adapter.
        with self._client_lock:
            if self._client is None:
                constructs.append(self.cfg.base_url or self.cfg.kind)
                self.validate_credentials()
                self._client = SimpleNamespace(
                    chat=SimpleNamespace(completions=SimpleNamespace(create=create))
                )
            return self._client

    monkeypatch.setattr(OpenAICompatibleBaseClient, "_ensure_client", ensure)
    monkeypatch.setenv("PROBE_MAIN_KEY", SECRET)
    monkeypatch.setenv("PROBE_BACKUP_KEY", SECRET)
    return SimpleNamespace(calls=calls, behaviors=behaviors, constructs=constructs)


def _sdk_response(content="OK", finish_reason="stop"):
    return SimpleNamespace(
        usage=None,
        choices=[
            SimpleNamespace(finish_reason=finish_reason, message=SimpleNamespace(content=content))
        ],
    )


def test_probe_shares_adapter_per_connection(probe_sdk):
    results = probe_connections(_probe_config(), ("translation.body", "glossary.extract"), timeout=5)
    assert [result.profile for result in results] == ["strong", "small"]
    assert all(result.ok for result in results)
    # Both profiles ride the same "main" connection: one adapter construction.
    assert probe_sdk.constructs == ["openai"]
    # Calls use the frozen probe budget: single attempt, small output.
    assert probe_sdk.calls[0]["max_completion_tokens"] == PROBE_OUTPUT_TOKENS
    probe_calls = [call for call in probe_sdk.calls]
    assert len(probe_calls) == 2


def test_probe_failure_never_retries(probe_sdk):
    from openai import APIStatusError

    request = httpx.Request(
        "POST", "https://example.invalid", headers={"Authorization": f"Bearer {SECRET}"}
    )
    probe_sdk.behaviors["big"] = APIStatusError(
        f"Bearer {SECRET}", response=httpx.Response(503, request=request), body={"key": SECRET}
    )
    [result] = probe_connections(_probe_config(), ("translation.body",))
    assert not result.ok
    assert result.category == "provider_unavailable"
    assert len(probe_sdk.calls) == 1
    assert SECRET not in str(result.describe())


def test_probe_max_retries_zero_frozen(probe_sdk, monkeypatch):
    seen: list[dict] = []

    def ensure(self):
        seen.append({"timeout": self.cfg.timeout, "max_retries": self.cfg.max_retries})
        return probe_sdk_constructs(self)

    probe_sdk_constructs = OpenAICompatibleBaseClient._ensure_client
    monkeypatch.setattr(OpenAICompatibleBaseClient, "_ensure_client", ensure)
    probe_connections(_probe_config(), ("translation.body", "glossary.extract"), timeout=5)
    assert seen == [{"timeout": 5, "max_retries": 0}] * 2


def test_probe_each_connection_constructed_once(probe_sdk):
    results = probe_connections(_probe_config(), ("translation.body",), timeout=5)
    assert results[0].ok
    assert probe_sdk.constructs.count("openai") == 1
