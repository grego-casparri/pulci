"""
Background task runner — execute callables asynchronously via a thread pool.
"""

from __future__ import annotations

import threading
from collections.abc import Callable
from typing import Any


class TaskRunner:
    def __init__(self, max_workers: int = 4) -> None:
        self._max_workers = max_workers
        self._threads: list[threading.Thread] = []

    def submit(self, fn: Callable[..., Any], *args: Any, **kwargs: Any) -> None:
        t = threading.Thread(target=fn, args=args, kwargs=kwargs, daemon=True)
        self._threads.append(t)
        t.start()

    def join_all(self, timeout: float | None = None) -> None:
        for t in self._threads:
            t.join(timeout=timeout)
        self._threads = [t for t in self._threads if t.is_alive()]

    def active_count(self) -> int:
        self._threads = [t for t in self._threads if t.is_alive()]
        return len(self._threads)
