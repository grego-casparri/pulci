"""
Tests for services: email, notifications, tasks.
"""

from __future__ import annotations

import time

from sampleapp.services.email import build_message, plain_text_preview
from sampleapp.services.notifications import clear_registry, dispatch, register
from sampleapp.services.tasks import TaskRunner


def test_build_message_subject():
    msg = build_message("Hello", "body text", recipients=["a@x.com"])
    assert msg["Subject"] == "Hello"


def test_build_message_recipients():
    msg = build_message("Hi", "body", recipients=["a@x.com", "b@x.com"])
    assert "a@x.com" in msg["To"]
    assert "b@x.com" in msg["To"]


def test_plain_text_preview_truncates():
    msg = build_message("s", "x" * 300, recipients=["a@x.com"])
    preview = plain_text_preview(msg, max_chars=10)
    assert len(preview) <= 10


def test_dispatch_calls_handler():
    clear_registry()
    received = []
    register("task.created", lambda e, p: received.append(p))
    n = dispatch("task.created", {"id": 1})
    assert n == 1
    assert received == [{"id": 1}]


def test_dispatch_unknown_event_is_noop():
    clear_registry()
    n = dispatch("unknown.event", {})
    assert n == 0


def test_dispatch_multiple_handlers():
    clear_registry()
    log = []
    register("task.updated", lambda e, p: log.append("h1"))
    register("task.updated", lambda e, p: log.append("h2"))
    dispatch("task.updated", {})
    assert log == ["h1", "h2"]


def test_task_runner_submit_and_join():
    runner = TaskRunner()
    results = []
    runner.submit(results.append, 42)
    runner.join_all(timeout=2.0)
    assert results == [42]


def test_task_runner_active_count():
    runner = TaskRunner()
    runner.submit(time.sleep, 0.05)
    assert runner.active_count() >= 0
    runner.join_all(timeout=1.0)
    assert runner.active_count() == 0
