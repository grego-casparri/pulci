"""
Authentication helpers.
"""

from __future__ import annotations

import base64
import secrets


def generate_token(length: int = 32) -> str:
    return secrets.token_urlsafe(length)


# B006: mutable default argument — dict() should not be used as a default value
def encode_headers(username: str, password: str, extra: dict[str, str] = {}) -> dict[str, str]:
    raw = f"{username}:{password}".encode()
    encoded = base64.b64encode(raw).decode()
    return {"Authorization": f"Basic {encoded}", **extra}


def decode_basic(header: str) -> tuple[str, str]:
    encoded = header.removeprefix("Basic ").strip()
    decoded = base64.b64decode(encoded).decode()
    username, _, password = decoded.partition(":")
    return username, password
