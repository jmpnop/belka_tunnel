"""Schema tests for the Firefox policies.json invariants.

These run on committed fixture files in tests/fixtures/, so they catch
regressions in the verification logic itself (independent of whether
Firefox is actually installed on the dev machine).
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from bt import verify


FIXTURES = Path(__file__).parent / "fixtures"


def _set_policy_path(monkeypatch, path: Path) -> None:
    """Redirect verify_policies() to the given fixture instead of /Applications."""

    real_path_class = Path

    def fake_path(p):
        if isinstance(p, str) and p.endswith(
            "/Firefox.app/Contents/Resources/distribution/policies.json"
        ):
            return path
        return real_path_class(p)

    monkeypatch.setattr(verify, "Path", fake_path)


def test_good_policy_passes(monkeypatch):
    _set_policy_path(monkeypatch, FIXTURES / "policies-good.json")
    verify.verify_policies()  # should not raise


def test_missing_disable_app_update_fails(monkeypatch):
    _set_policy_path(monkeypatch, FIXTURES / "policies-no-disable-update.json")
    with pytest.raises(SystemExit) as ei:
        verify.verify_policies()
    assert ei.value.code == 1


def test_proxy_not_locked_fails(monkeypatch):
    _set_policy_path(monkeypatch, FIXTURES / "policies-unlocked-proxy.json")
    with pytest.raises(SystemExit) as ei:
        verify.verify_policies()
    assert ei.value.code == 1


def test_invalid_json_fails(monkeypatch):
    _set_policy_path(monkeypatch, FIXTURES / "policies-malformed.json")
    with pytest.raises(SystemExit) as ei:
        verify.verify_policies()
    assert ei.value.code == 1
