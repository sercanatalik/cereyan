"""What an agent can be given on this host: the Pydantic AI capabilities and
common tools that import here, with the options a definition may set and what
each needs before it is ready. The server computes it once at start and serves
it under ``/api/agents/catalogue`` beside its own MCP tools and the allowlist."""

from __future__ import annotations

import importlib
import importlib.util
from typing import Any

# Capabilities by name, with the options the editor offers. Each option schema
# is a JSON schema fragment; the runner passes the values as keyword arguments.
CAPABILITIES: dict[str, dict[str, Any]] = {
    "WebSearch": {
        "description": "Search the web through the model provider's own search tool",
        "needs": ["model:native web search"],
        "options": {
            "allowed_domains": {"type": "array", "items": {"type": "string"}},
            "blocked_domains": {"type": "array", "items": {"type": "string"}},
            "max_uses": {"type": "integer", "minimum": 1},
            "search_context_size": {"enum": ["low", "medium", "high"]},
        },
    },
    "WebFetch": {
        "description": "Fetch a URL the model asks for, through the provider",
        "needs": ["model:native web fetch"],
        "options": {
            "allowed_domains": {"type": "array", "items": {"type": "string"}},
            "blocked_domains": {"type": "array", "items": {"type": "string"}},
            "max_uses": {"type": "integer", "minimum": 1},
        },
    },
    "XSearch": {
        "description": "Search X posts through the provider",
        "needs": ["model:native X search"],
        "options": {
            "allowed_x_handles": {"type": "array", "items": {"type": "string"}},
            "excluded_x_handles": {"type": "array", "items": {"type": "string"}},
        },
    },
    "ImageGeneration": {
        "description": "Generate or edit images through the provider",
        "needs": ["model:native image generation"],
        "options": {
            "quality": {"enum": ["low", "medium", "high", "auto"]},
            "output_format": {"enum": ["png", "webp", "jpeg"]},
        },
    },
    "Thinking": {
        "description": "Extended reasoning, where the model supports it",
        "needs": [],
        "options": {"effort": {"enum": ["low", "medium", "high"]}},
    },
    "ToolSearch": {
        "description": "Let the model search its tools instead of seeing them all",
        "needs": [],
        "options": {"max_results": {"type": "integer", "minimum": 1}},
    },
    "Caching": {
        "description": "Prompt caching, where the provider supports it",
        "needs": [],
        "options": {},
    },
}

# Common tools: the Pydantic AI module, the factory, the package it imports,
# and the secret Variable it reads.
COMMON_TOOLS: dict[str, dict[str, Any]] = {
    "duckduckgo_search": {
        "module": "pydantic_ai.common_tools.duckduckgo", "factory": "duckduckgo_search_tool",
        "package": "ddgs", "secret": None, "description": "Web search through DuckDuckGo; no key",
        "options": {"max_results": {"type": "integer", "minimum": 1}},
    },
    "tavily_search": {
        "module": "pydantic_ai.common_tools.tavily", "factory": "tavily_search_tool",
        "package": "tavily-python", "secret": "tavily/api_key", "description": "Web search through Tavily",
        "options": {},
    },
    "exa_search": {
        "module": "pydantic_ai.common_tools.exa", "factory": "exa_search_tool",
        "package": "exa-py", "secret": "exa/api_key", "description": "Web search through Exa",
        "options": {},
    },
    "web_fetch": {
        "module": "pydantic_ai.common_tools.web_fetch", "factory": "web_fetch_tool",
        "package": "markdownify", "secret": None, "description": "Fetch a page as markdown from this host",
        "options": {
            "allowed_domains": {"type": "array", "items": {"type": "string"}},
            "blocked_domains": {"type": "array", "items": {"type": "string"}},
            "max_content_length": {"type": "integer", "minimum": 1000},
        },
    },
}

# Model providers: the id prefix, the package that serves it, and the key variable
# the runner exports for the run.
PROVIDERS: dict[str, dict[str, str]] = {
    "openai": {"package": "openai", "env": "OPENAI_API_KEY"},
    "anthropic": {"package": "anthropic", "env": "ANTHROPIC_API_KEY"},
    "google-gla": {"package": "google.genai", "env": "GEMINI_API_KEY"},
    "google-vertex": {"package": "google.genai", "env": "GOOGLE_API_KEY"},
    "groq": {"package": "groq", "env": "GROQ_API_KEY"},
    "mistral": {"package": "mistralai", "env": "MISTRAL_API_KEY"},
    "cohere": {"package": "cohere", "env": "CO_API_KEY"},
    "bedrock": {"package": "boto3", "env": "AWS_ACCESS_KEY_ID"},
    # Scripted models in tests read their key here, so key handling is testable.
    "test": {"package": "pydantic_ai", "env": "TEST_MODEL_API_KEY"},
}


def _importable(module: str) -> bool:
    try:
        return importlib.util.find_spec(module) is not None
    except (ImportError, ValueError):
        return False


def _loads(module: str) -> bool:
    try:
        importlib.import_module(module)
        return True
    except Exception:  # noqa: BLE001 - a missing optional package is the normal case
        return False


def describe() -> dict[str, Any]:
    """The catalogue for this host, as JSON: what is installed and what each entry needs."""
    if not _importable("pydantic_ai.durable_exec"):
        return {"installed": False, "version": None, "capabilities": [], "common": [], "mcp": {"ready": False,
                "needs": ["package:cereyan[pydantic-ai]"]}, "providers": []}
    import pydantic_ai
    from pydantic_ai import capabilities as caps

    capabilities = []
    for name, spec in CAPABILITIES.items():
        if getattr(caps, name, None) is None:
            continue
        capabilities.append({"name": name, "description": spec["description"], "needs": list(spec["needs"]),
                             "options": {"type": "object", "properties": spec["options"]}, "ready": True})
    common = []
    for name, spec in COMMON_TOOLS.items():
        needs = []
        if not _loads(spec["module"]):
            needs.append(f"package:{spec['package']}")
        if spec["secret"]:
            needs.append(f"secret:{spec['secret']}")
        common.append({"name": name, "description": spec["description"], "needs": needs,
                       "options": {"type": "object", "properties": spec["options"]},
                       "ready": not any(n.startswith("package:") for n in needs)})
    mcp_ready = _loads("pydantic_ai.mcp")
    providers = [{"id": pid, "env": spec["env"], "installed": _importable(spec["package"])}
                 for pid, spec in PROVIDERS.items()]
    return {
        "installed": True,
        "version": pydantic_ai.__version__,
        "capabilities": capabilities,
        "common": common,
        "mcp": {"ready": mcp_ready, "needs": [] if mcp_ready else ["package:pydantic-ai-slim[mcp]"]},
        "providers": providers,
    }
