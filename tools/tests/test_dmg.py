"""Smoke tests for tools/bt/dmg.py — does it parse Cargo.toml correctly?"""

from __future__ import annotations

import re

from bt import dmg


def test_cargo_version_parses() -> None:
    v = dmg.cargo_version()
    # SemVer-ish: at least major.minor.patch with optional pre-release.
    assert re.match(r"^\d+\.\d+\.\d+", v), f"unexpected version {v!r}"
