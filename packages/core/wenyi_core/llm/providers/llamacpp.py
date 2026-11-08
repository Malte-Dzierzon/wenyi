"""Local llama.cpp server gateway (OpenAI-compatible, no API key).

Default endpoint is the on-machine gateway sharing models with other local
tools (Pi agent). Models load on demand; first requests may take a while.
"""

from __future__ import annotations

from ..transport import ResolvedModel
from .openai_compatible import OpenAICompatibleClient, OpenAICompatibleOptions

DEFAULT_BASE_URL = "http://127.0.0.1:8093/v1"


def preset_models() -> dict[str, ResolvedModel[OpenAICompatibleOptions]]:
    """Return built-in local defaults: big model for strong, small for cheap/fast."""
    return {
        "strong": ResolvedModel(
            model="lokal27b",
            options=OpenAICompatibleOptions(),
        ),
        "cheap": ResolvedModel(
            model="lokal9b",
            options=OpenAICompatibleOptions(),
        ),
        "fast": ResolvedModel(
            model="lokal9b",
            options=OpenAICompatibleOptions(),
        ),
    }


class LlamaCppClient(OpenAICompatibleClient):
    default_base_url = DEFAULT_BASE_URL
