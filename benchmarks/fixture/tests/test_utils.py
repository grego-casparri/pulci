"""
Tests for utility functions.
"""

from __future__ import annotations

from sampleapp.utils import chunk, flatten, slugify


def test_flatten_depth_0():
    assert flatten([[1, 2], [3]], depth=0) == [[1, 2], [3]]


def test_flatten_depth_1():
    assert flatten([[1, 2], [3, [4]]]) == [1, 2, 3, [4]]


def test_flatten_nested():
    assert flatten([[1, [2, [3]]]], depth=2) == [1, 2, [3]]


def test_chunk_even():
    assert chunk([1, 2, 3, 4], 2) == [[1, 2], [3, 4]]


def test_chunk_remainder():
    assert chunk([1, 2, 3], 2) == [[1, 2], [3]]


def test_slugify_basic():
    assert slugify("Hello World") == "hello-world"


def test_slugify_already_lower():
    assert slugify("foo bar") == "foo-bar"
