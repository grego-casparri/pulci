"""
Schema migration helpers — apply ordered migration steps to a store.
"""

from __future__ import annotations

from collections.abc import Callable

Migration = Callable[[dict], None]


def apply(store: dict, migrations: list[Migration]) -> int:
    current = store.get("_schema_version", 0)
    for i, migration in enumerate(migrations[current:], start=current + 1):
        migration(store)
        store["_schema_version"] = i
    return store.get("_schema_version", 0)


def noop(_store: dict) -> None:
    pass
