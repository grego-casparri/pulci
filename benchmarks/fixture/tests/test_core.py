"""
Tests for core modules.
"""

from __future__ import annotations

import pytest

from sampleapp.core.cache import Cache
from sampleapp.core.config import Config
from sampleapp.core.processor import ValidationError, batch_validate, validate_task


def test_cache_get_miss() -> None:
    c = Cache()
    assert c.get("missing") is None


def test_cache_set_and_get() -> None:
    c = Cache()
    c.set("key", "value")
    assert c.get("key") == "value"


def test_cache_max_size_evicts_oldest() -> None:
    c = Cache(max_size=2)
    c.set("a", 1)
    c.set("b", 2)
    c.set("c", 3)
    assert c.get("a") is None
    assert c.get("c") == 3


def test_cache_delete() -> None:
    c = Cache()
    c.set("x", 42)
    assert c.delete("x")
    assert c.get("x") is None
    assert not c.delete("x")


def test_default_config() -> None:
    cfg = Config()
    assert cfg.host == "localhost"
    assert cfg.port == 8080
    assert not cfg.debug


def test_validate_task_ok() -> None:
    result = validate_task({"id": 1, "title": "Do something"})
    assert result["title"] == "Do something"
    assert not result["done"]


def test_validate_task_missing_title() -> None:
    with pytest.raises(ValidationError, match="title is required"):
        validate_task({"id": 1})


def test_batch_validate_mixed() -> None:
    items = [
        {"id": 1, "title": "Good"},
        {"id": 2},
        {"id": 3, "title": "Also good"},
    ]
    valid, errors = batch_validate(items)
    assert len(valid) == 2
    assert len(errors) == 1
