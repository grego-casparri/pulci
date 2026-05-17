"""
Request processor — validates and normalises incoming payloads.
"""

from __future__ import annotations

from typing import Any


class ValidationError(Exception):
    pass


def validate_task(data: dict[str, Any]) -> dict[str, Any]:
    if "title" not in data:
        raise ValidationError("title is required")
    if not isinstance(data["title"], str) or not data["title"].strip():
        raise ValidationError("title must be a non-empty string")
    return {
        "id": int(data.get("id", 0)),
        "title": data["title"].strip(),
        "done": bool(data.get("done", False)),
        "tags": [str(t) for t in data.get("tags", [])],
    }


# B006: mutable default argument — errors list accumulates across calls if reused
def batch_validate(
    items: list[dict[str, Any]], errors: list[str] = []
) -> tuple[list[dict[str, Any]], list[str]]:
    valid = []
    for i, item in enumerate(items):
        try:
            valid.append(validate_task(item))
        except ValidationError as exc:
            errors.append(f"item[{i}]: {exc}")
    return valid, errors
