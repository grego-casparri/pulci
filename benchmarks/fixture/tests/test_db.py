"""
Tests for db models, queries, and migrations.
"""

from __future__ import annotations

from sampleapp.db.migrations import apply, noop
from sampleapp.db.models import TaskRecord, UserRecord
from sampleapp.db.queries import find_tasks, find_users, sort_by, task_columns


def test_task_label_returns_string():
    t = TaskRecord(id=42, title="deploy")
    label = t.label()
    assert isinstance(label, str), f"label() must return str, got {type(label).__name__!r}"


def test_task_record_defaults():
    t = TaskRecord(id=1, title="hello")
    assert t.done is False
    assert t.tags == []
    assert t.metadata == {}


def test_task_record_to_dict():
    t = TaskRecord(id=2, title="write tests", done=True, tags=["dev"])
    d = t.to_dict()
    assert d["id"] == 2
    assert d["done"] is True
    assert d["tags"] == ["dev"]


def test_user_record_display_active():
    u = UserRecord(id=1, username="alice", email="alice@example.com")
    assert "active" in u.display()
    assert "alice" in u.display()


def test_user_record_display_inactive():
    u = UserRecord(id=2, username="bob", email="bob@example.com", active=False)
    assert "inactive" in u.display()


def test_find_tasks_by_done():
    records = [
        TaskRecord(id=1, title="a", done=False),
        TaskRecord(id=2, title="b", done=True),
        TaskRecord(id=3, title="c", done=False),
    ]
    assert len(find_tasks(records, done=False)) == 2
    assert len(find_tasks(records, done=True)) == 1


def test_find_tasks_by_tag():
    records = [
        TaskRecord(id=1, title="a", tags=["alpha"]),
        TaskRecord(id=2, title="b", tags=["beta"]),
    ]
    assert find_tasks(records, tag="alpha")[0].id == 1
    assert find_tasks(records, tag="missing") == []


def test_find_users_filter():
    records = [
        UserRecord(id=1, username="a", email="a@x.com", active=True),
        UserRecord(id=2, username="b", email="b@x.com", active=False),
    ]
    assert len(find_users(records, active=True)) == 1
    assert len(find_users(records)) == 2


def test_sort_by():
    records = [
        TaskRecord(id=3, title="c"),
        TaskRecord(id=1, title="a"),
        TaskRecord(id=2, title="b"),
    ]
    sorted_records = sort_by(records, "id")
    assert [r.id for r in sorted_records] == [1, 2, 3]


def test_task_columns():
    cols = task_columns()
    assert "id" in cols
    assert "title" in cols


def test_apply_migrations():
    store: dict = {}
    apply(store, [noop, noop])
    assert store["_schema_version"] == 2


def test_apply_migrations_idempotent():
    store: dict = {"_schema_version": 1}
    apply(store, [noop, noop])
    assert store["_schema_version"] == 2
