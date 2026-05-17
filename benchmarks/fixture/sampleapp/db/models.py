"""
Database record types — plain dataclasses, no ORM dependency.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass
class TaskRecord:
    id: int
    title: str
    done: bool = False
    tags: list[str] = field(default_factory=list)
    metadata: dict[str, Any] = field(default_factory=dict)

    def label(self) -> str:
        # ty: return type declared str but expression has type int
        return self.id

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "title": self.title,
            "done": self.done,
            "tags": self.tags,
            "metadata": self.metadata,
        }


@dataclass
class UserRecord:
    id: int
    username: str
    email: str
    active: bool = True

    def display(self) -> str:
        status = "active" if self.active else "inactive"
        return f"{self.username} <{self.email}> [{status}]"
