"""Test the real response decoding path without live inference."""

import json
from pathlib import Path  # noqa: TC003 - runtime annotation on Python 3.13
from typing import Self
from unittest.mock import patch

import pytest

from hermes_harness.core.ollama import OllamaOptions, text_task


class FakeResponse:
    status = 200

    def __enter__(self) -> Self:
        return self

    def __exit__(self, *_args: object) -> None:
        return None

    def read(self, _limit: int) -> bytes:
        return json.dumps({'response': 'ok'}).encode()


def test_ollama_body_read_once(tmp_path: Path) -> None:
    with patch('hermes_harness.core.ollama.urllib.request.urlopen', return_value=FakeResponse()):
        assert text_task('hello', options=OllamaOptions(lock_file=tmp_path / 'gpu.lock')) == 'ok'


def test_remote_endpoint_rejected(tmp_path: Path) -> None:
    with pytest.raises(PermissionError, match='loopback'):
        text_task(
            'hello',
            options=OllamaOptions(
                url='http://example.com/api/generate',
                lock_file=tmp_path / 'gpu.lock',
            ),
        )
