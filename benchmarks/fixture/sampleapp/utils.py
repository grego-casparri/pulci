"""
Utility functions.
"""

from __future__ import annotations

from typing import Any


def flatten(nested: list[Any], depth: int = 1) -> list[Any]:
    if depth == 0:
        return list(nested)
    result = []
    for item in nested:
        if isinstance(item, list):
            result.extend(flatten(item, depth - 1))
        else:
            result.append(item)
    return result


def chunk(items: list[Any], size: int) -> list[list[Any]]:
    return [items[i : i + size] for i in range(0, len(items), size)]


def slugify(text: str) -> str:
    return "-".join(text.lower().split())


# E501: long line
def describe(items: list[Any], label: str = "items") -> str:
    return f"{label}: {len(items)} total, first={items[0] if items else 'none'}, last={items[-1] if items else 'none'}"  # one-line summary for logging
