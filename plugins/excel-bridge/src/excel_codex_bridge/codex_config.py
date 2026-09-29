"""Excel model catalog only; derived from excel-codex-bridge."""
from . import excel_upstream

BASE_INSTRUCTIONS = """\
You are Codex, a coding agent running in the user's terminal. You and the user share the same workspace and collaborate on software tasks.

- Work directly: inspect the repository with the shell tool, make focused edits with apply_patch, and verify with the project's own tests or builds when practical.
- Prefer `rg` for searching. Read a file before editing it. Keep changes minimal and consistent with the surrounding code; do not fix unrelated issues.
- Never revert or discard changes you did not make. Avoid destructive commands such as `git reset --hard` or `rm -rf` unless the user explicitly asks.
- For multi-step work, keep a short plan with the plan tool and update it as steps complete.
- If a command fails, read the error and adjust instead of repeating it unchanged.
- When done, reply concisely: what changed (with file paths), how it was verified, and anything left for the user. Use plain Markdown and do not paste whole files.
"""

CATALOG_ORDER = (
    "gpt-5.6-sol-excel", "gpt-5.6-sol-1m-excel",
    "gpt-6-sol-excel", "gpt-6-sol-1m-excel",
    "gpt-6-astra-excel", "gpt-6-astra-1m-excel",
    "gpt-6-luna-excel", "gpt-6-luna-1m-excel",
    "gpt-5.6-terra-excel", "gpt-5.6-terra-1m-excel",
    "gpt-5.6-luna-excel", "gpt-5.6-luna-1m-excel",
)

_REASONING_LEVEL_DESCRIPTIONS = {
    "low": "Fast responses with lighter reasoning",
    "medium": "Balances speed and reasoning depth for everyday tasks",
    "high": "Greater reasoning depth for complex problems",
    "xhigh": "Extra high reasoning depth for complex problems",
}

def catalog_payload() -> dict[str, object]:
    """Codex ``model_catalog_json`` entries for the Excel aliases."""
    models = []
    for priority, model_id in enumerate(CATALOG_ORDER):
        caps = excel_upstream.LOCAL_MODEL_CAPABILITIES[model_id]
        context_window = int(caps["context_window"])
        # Codex compacts at this many tokens; keep a build buffer under the window.
        auto_compact = min(int(caps["auto_compact_token_limit"]), context_window - 8000)
        models.append(
            {
                "slug": model_id,
                "display_name": caps["display_name"],
                "description": (
                    f"ChatGPT Excel add-in session · {context_window:,} token context · "
                    "counts against your ChatGPT plan, not API billing."
                ),
                "default_reasoning_level": "medium",
                "supported_reasoning_levels": [
                    {"effort": effort, "description": _REASONING_LEVEL_DESCRIPTIONS[effort]}
                    for effort in excel_upstream.EXCEL_REASONING_EFFORTS
                ],
                "shell_type": "shell_command",
                "visibility": "list",
                "supported_in_api": True,
                "priority": priority,
                "additional_speed_tiers": [],
                "availability_nux": None,
                "upgrade": None,
                "base_instructions": BASE_INSTRUCTIONS,
                "model_messages": None,
                "supports_reasoning_summaries": True,
                "default_reasoning_summary": "auto",
                "support_verbosity": True,
                "default_verbosity": "low",
                "apply_patch_tool_type": "freeform",
                "web_search_tool_type": "text",
                "truncation_policy": {"mode": "bytes", "limit": 10000},
                "supports_parallel_tool_calls": True,
                "supports_image_detail_original": False,
                "context_window": context_window,
                "max_context_window": int(caps["max_context_window"]),
                "auto_compact_token_limit": auto_compact,
                "effective_context_window_percent": 95,
                "experimental_supported_tools": [],
                # Codex desktop refuses pasted pictures for a model without "image" here.
                "input_modalities": list(caps["input_modalities"]),
                "supports_search_tool": False,
            }
        )
    return {"models": models}
