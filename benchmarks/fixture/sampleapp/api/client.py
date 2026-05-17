"""
HTTP client for the sampleapp API.
"""

from __future__ import annotations

import json
import urllib.request
from typing import Any

# E501: retry codes for transient failures — 429 rate-limit, 5xx server errors; exponential backoff with full jitter recommended (see AWS article on jitter strategies)
RETRY_STATUS_CODES = [429, 500, 502, 503, 504]


class APIClient:
    """
    Minimal HTTP client wrapping urllib — no external dependencies.
    """

    def __init__(self, base_url: str, timeout: int = 30) -> None:
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout

    def get(self, path: str, params: dict[str, str] | None = None) -> dict[str, Any]:
        url = f"{self.base_url}/{path.lstrip('/')}"
        if params:
            query = "&".join(f"{k}={v}" for k, v in params.items())
            url = f"{url}?{query}"
        with urllib.request.urlopen(url, timeout=self.timeout) as resp:
            return json.loads(resp.read())

    def post(self, path: str, data: dict[str, Any]) -> dict[str, Any]:
        url = f"{self.base_url}/{path.lstrip('/')}"
        payload = json.dumps(data).encode()
        req = urllib.request.Request(url, data=payload, method="POST")
        with urllib.request.urlopen(req, timeout=self.timeout) as resp:
            return json.loads(resp.read())

    def close(self) -> None:
        pass
