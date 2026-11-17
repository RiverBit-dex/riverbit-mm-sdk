from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from riverbit_mm import Signer

ROOT = Path(__file__).resolve().parents[2]
TESTDATA = ROOT / "testdata"


@pytest.fixture(scope="session")
def vectors() -> dict[str, Any]:
    data: dict[str, Any] = json.loads((TESTDATA / "vectors.json").read_text())
    return data


@pytest.fixture(scope="session")
def test_signer(vectors: dict[str, Any]) -> Signer:
    return Signer(vectors["test_key"]["private_key"])
