"""Bundle / policy verification — strict checks for release artifacts."""

from __future__ import annotations

import json
import plistlib
from pathlib import Path

from . import util


def assert_universal(binary: Path) -> None:
    """Fail unless `binary` is a fat Mach-O with BOTH arm64 and x86_64 slices.

    Both apps ship universal, always. A thin binary that slips through would
    silently refuse to launch on the other architecture — exactly the kind of
    after-release surprise this gate exists to prevent.
    """
    r = util.run(["lipo", "-archs", str(binary)], capture=True, check=False)
    archs = set(r.stdout.split()) if r.code == 0 else set()
    missing = {"arm64", "x86_64"} - archs
    if missing:
        util.fail(
            f"{binary.name} is not universal — has {sorted(archs) or 'unknown'}, "
            f"missing {sorted(missing)}. Rebuild with `bt bundle` (universal)."
        )
        raise SystemExit(1)
    util.ok(f"universal binary: {' + '.join(sorted(archs))}")


def verify_bundle() -> None:
    """Sanity-check the .app bundle."""
    util.step("verify bundle")
    bundle = util.BUNDLE
    if not bundle.exists():
        util.fail(f"missing bundle {bundle}")
        raise SystemExit(1)

    info_plist = bundle / "Contents" / "Info.plist"
    binary = bundle / "Contents" / "MacOS" / "belka_tunnel"
    icon = bundle / "Contents" / "Resources" / "AppIcon.icns"
    code_sig = bundle / "Contents" / "_CodeSignature" / "CodeResources"

    required = {info_plist: "Info.plist", binary: "binary", icon: "icon"}
    for path, label in required.items():
        if not path.exists():
            util.fail(f"missing {label} at {path}")
            raise SystemExit(1)
    util.ok("required files present")

    # Info.plist must declare the right bundle identity + LSUIElement.
    info = plistlib.loads(info_plist.read_bytes())
    expected = {
        "CFBundleExecutable": "belka_tunnel",
        "CFBundleIdentifier": "io.celestialtech.BelkaTunnel",
        "CFBundleName": "BelkaTunnel",
        "CFBundleIconFile": "AppIcon",
        "LSUIElement": True,
    }
    for key, want in expected.items():
        got = info.get(key)
        if got != want:
            util.fail(f"Info.plist[{key}] expected {want!r}, got {got!r}")
            raise SystemExit(1)
    util.ok(f"Info.plist: {info.get('CFBundleShortVersionString')}")

    # Must be a universal (arm64 + x86_64) binary — hard gate.
    assert_universal(binary)

    # codesign --verify should pass even for ad-hoc.
    cs = util.run(["codesign", "--verify", "--strict", str(bundle)], check=False)
    if cs.code != 0:
        util.warn("codesign --verify failed (ad-hoc sign may need re-run)")
    else:
        util.ok("codesign valid")

    util.ok("bundle verification passed")


def verify_pfusers_bundle() -> None:
    """Sanity-check the pfUsers.app bundle. Mirrors verify_bundle() but for
    the windowed admin app (no LSUIElement, distinct identifier)."""
    util.step("verify pfUsers bundle")
    from .build import PFUSERS_BUNDLE

    bundle = PFUSERS_BUNDLE
    if not bundle.exists():
        util.fail(f"missing bundle {bundle}")
        raise SystemExit(1)

    info_plist = bundle / "Contents" / "Info.plist"
    binary = bundle / "Contents" / "MacOS" / "pfusers"
    code_sig = bundle / "Contents" / "_CodeSignature" / "CodeResources"

    required = {info_plist: "Info.plist", binary: "binary"}
    for path, label in required.items():
        if not path.exists():
            util.fail(f"missing {label} at {path}")
            raise SystemExit(1)
    util.ok("required files present")

    info = plistlib.loads(info_plist.read_bytes())
    expected = {
        "CFBundleExecutable": "pfusers",
        "CFBundleIdentifier": "io.celestialtech.pfUsers",
        "CFBundleName": "pfUsers",
    }
    for key, want in expected.items():
        got = info.get(key)
        if got != want:
            util.fail(f"Info.plist[{key}] expected {want!r}, got {got!r}")
            raise SystemExit(1)
    # LSUIElement must be absent or false — pfUsers is a windowed app.
    if info.get("LSUIElement"):
        util.fail("LSUIElement set on pfUsers — should be a regular windowed app")
        raise SystemExit(1)
    util.ok(f"Info.plist: {info.get('CFBundleShortVersionString')}")

    # Must be a universal (arm64 + x86_64) binary — hard gate.
    assert_universal(binary)

    cs = util.run(["codesign", "--verify", "--strict", str(bundle)], check=False)
    if cs.code != 0:
        util.warn("codesign --verify failed (ad-hoc sign may need re-run)")
    else:
        util.ok("codesign valid")

    _ = code_sig  # presence is informational; ad-hoc signing creates it
    util.ok("pfUsers bundle verification passed")


def verify_policies() -> None:
    """Validate the Firefox policies.json (if Firefox is installed)."""
    util.step("verify Firefox policies")
    ff_policy = Path(
        "/Applications/Firefox.app/Contents/Resources/distribution/policies.json"
    )
    if not ff_policy.exists():
        util.warn(
            f"no policies.json at {ff_policy} — install/reinstall Firefox first"
        )
        return

    # Parse + schema-validate the expected structure.
    raw = ff_policy.read_text(encoding="utf-8")
    try:
        doc = json.loads(raw)
    except json.JSONDecodeError as e:
        util.fail(f"policies.json is not valid JSON: {e}")
        raise SystemExit(1)

    policies = doc.get("policies")
    if not isinstance(policies, dict):
        util.fail("missing top-level `policies` object")
        raise SystemExit(1)

    # Required keys with the right shape — these are the security-critical ones.
    proxy = policies.get("Proxy") or {}
    if proxy.get("Mode") != "manual":
        util.fail("Proxy.Mode must be 'manual'")
        raise SystemExit(1)
    if proxy.get("Locked") is not True:
        util.fail("Proxy.Locked MUST be true — user could otherwise turn it off")
        raise SystemExit(1)
    if not isinstance(proxy.get("SOCKSProxy"), str) or ":" not in proxy["SOCKSProxy"]:
        util.fail(f"Proxy.SOCKSProxy malformed: {proxy.get('SOCKSProxy')!r}")
        raise SystemExit(1)
    util.ok(f"Proxy locked → {proxy['SOCKSProxy']}")

    # No auto-update — otherwise Firefox would wipe our policy on its next upgrade.
    if policies.get("DisableAppUpdate") is not True:
        util.fail("DisableAppUpdate MUST be true — Firefox auto-update would clobber the policy")
        raise SystemExit(1)
    util.ok("Firefox auto-update disabled")

    # Privacy locks worth checking.
    for must in [
        ("EnableTrackingProtection", "Locked", True),
        ("DNSOverHTTPS", "Locked", True),
        ("WebRTCIPHandling", "Locked", True),
    ]:
        section = policies.get(must[0])
        if not isinstance(section, dict) or section.get(must[1]) is not must[2]:
            util.fail(f"{must[0]}.{must[1]} must be {must[2]}")
            raise SystemExit(1)
    util.ok("EnableTrackingProtection / DNSOverHTTPS / WebRTCIPHandling locked")

    util.ok("policies.json verification passed")
