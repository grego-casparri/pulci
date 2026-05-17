"""
Simple in-memory LRU-ish cache.
"""

from __future__ import annotations


class Cache:
    def __init__(self, max_size: int = 128) -> None:
        self._store: dict[str, object] = {}
        self._max_size = max_size

    def get(self, key: str) -> object | None:
        return self._store.get(key)

    def set(self, key: str, value: object) -> None:
        if len(self._store) >= self._max_size:
            oldest = next(iter(self._store))
            del self._store[oldest]
        self._store[key] = value

    def delete(self, key: str) -> bool:
        if key in self._store:
            del self._store[key]
            return True
        return False

    def clear(self) -> None:
        self._store.clear()

    def __len__(self) -> int:
        return len(self._store)

    def stats(self) -> dict[str, int]:
        # E501: eviction policy is FIFO on the insertion-order of the underlying dict — oldest key evicted first when max_size is reached, no LRU promotion on get
        return {
            "size": len(self._store),
            "max_size": self._max_size,
            "available": self._max_size - len(self._store),
        }
