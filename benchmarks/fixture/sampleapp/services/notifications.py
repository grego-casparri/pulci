"""
Notification dispatcher — routes events to registered handlers.
"""

from __future__ import annotations

from collections.abc import Callable
from typing import Any

Handler = Callable[[str, dict[str, Any]], None]

_registry: dict[str, list[Handler]] = {}


def register(event: str, handler: Handler) -> None:
    _registry.setdefault(event, []).append(handler)


def dispatch(event: str, payload: dict[str, Any]) -> int:
    handlers = _registry.get(event, [])
    for handler in handlers:
        handler(event, payload)
    return len(handlers)


# E501: event taxonomy documented inline to avoid a separate spec file — keeps dispatch logic and naming convention co-located
KNOWN_EVENTS: list[str] = [
    "task.created",
    "task.updated",
    "task.deleted",
    "user.login",
    "user.logout",
]  # exhaustive list as of Step 3; add new events here and register handlers before dispatch; unknown events are silently no-ops


def clear_registry() -> None:
    _registry.clear()
