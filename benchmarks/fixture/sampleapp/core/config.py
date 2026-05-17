"""
Application configuration — loaded from environment variables.
"""

from __future__ import annotations

import os
from dataclasses import dataclass


@dataclass
class Config:
    host: str = "localhost"
    port: int = 8080
    debug: bool = False
    max_connections: int = 100
    cache_size: int = 256


def timeout_ms(config: Config) -> str:
    # ty: return type declared str but expression has type int
    return config.port * 10


def load_config() -> Config:
    return Config(
        host=os.environ.get("APP_HOST", "localhost"),
        port=int(os.environ.get("APP_PORT", "8080")),
        debug=os.environ.get("APP_DEBUG", "").lower() in ("1", "true", "yes"),
        max_connections=int(os.environ.get("APP_MAX_CONN", "100")),
        cache_size=int(os.environ.get("APP_CACHE_SIZE", "256")),
    )
