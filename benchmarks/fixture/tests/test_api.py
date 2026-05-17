"""
Tests for API models.
"""

from __future__ import annotations

import pytest

from sampleapp.api.models import Task, TaskList, task_from_dict


def test_task_from_dict_minimal() -> None:
    t = task_from_dict({"id": 1, "title": "Buy milk"})
    assert t.id == 1
    assert t.title == "Buy milk"
    assert not t.done
    assert t.tags == []


def test_task_from_dict_with_done() -> None:
    t = task_from_dict({"id": 2, "title": "Walk dog", "done": True})
    assert t.done


def test_task_from_dict_with_tags() -> None:
    t = task_from_dict({"id": 3, "title": "Read", "tags": ["books", "leisure"]})
    assert t.tags == ["books", "leisure"]


def test_tasklist_pending_and_completed() -> None:
    tl = TaskList()
    tl.add(Task(id=1, title="a", done=False))
    tl.add(Task(id=2, title="b", done=True))
    tl.add(Task(id=3, title="c", done=False))
    assert len(tl.pending()) == 2
    assert len(tl.completed()) == 1


def test_task_from_dict_missing_title_raises() -> None:
    with pytest.raises(KeyError):
        task_from_dict({"id": 1})
