"""Bundle / policy verification — strict checks for release artifacts."""

from __future__ import annotations

import json
import plistlib
from pathlib import Path

from . import util


def verify_bundle() -> None:
    """Sanity-check the .app bundle."""
    util.step("verify bundle")
    bundle = util.BUNDLE
    if not bundle.exists():
        util.fail(f"missing bundle {bundle}")
        raise SystemExit(1)

    info_plist = bundle / "Contents" / "Info.plist"
    binary = bundle / "Contents" / "MacOS" / "proxy-tunnel"
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
        "CFBundleExecutable": "proxy-tunnel",
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

    # Binary architecture(s).
    r = util.run(["file", str(binary)], capture=True)
    util.console.print(f"  arch: {r.stdout.strip().split(': ', 1)[1]}")

    # codesign --verify should pass even for ad-hoc.
    cs = util.run(["codesign", "--verify", "--strict", str(bundle)], check=False)
    if cs.code != 0:
        util.warn("codesign --verify failed (ad-hoc sign may need re-run)")
    else:
        util.ok("codesign valid")

    util.ok("bundle verification passed")


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
