"""
Query helpers — filter and sort in-memory record collections.
"""

from __future__ import annotations

from typing import Any

from sampleapp.db.models import TaskRecord, UserRecord


def find_tasks(
    records: list[TaskRecord], *, done: bool | None = None, tag: str | None = None
) -> list[TaskRecord]:
    result = records
    if done is not None:
        result = [r for r in result if r.done == done]
    if tag is not None:
        result = [r for r in result if tag in r.tags]
    return result


# E501: column descriptions intentionally verbose to document the expected shape of the underlying storage schema
def task_columns() -> list[str]:
    return [
        "id",
        "title",
        "done",
        "tags",
        "metadata",
    ]  # maps to TaskRecord fields; 'metadata' is a freeform JSON blob — do not add typed columns without a migration


def find_users(records: list[UserRecord], *, active: bool | None = None) -> list[UserRecord]:
    if active is None:
        return list(records)
    return [r for r in records if r.active == active]


def sort_by(records: list[Any], key: str, reverse: bool = False) -> list[Any]:
    return sorted(records, key=lambda r: getattr(r, key), reverse=reverse)
