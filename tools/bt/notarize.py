"""Notarization: submit DMG/.app to Apple, wait for clearance, staple ticket.

Driven entirely by env vars so secrets stay out of code:
  NOTARY_PROFILE     a keychain profile name created once via
                     `xcrun notarytool store-credentials <PROFILE>`
                     (use the Apple-ID + app-specific password + team ID)
  SIGN_IDENTITY      "Developer ID Application: NAME (TEAMID)" for sign step

After successful notarization the DMG is stapled so first-launch works
offline. The .app inside is implicitly covered by the DMG's ticket.
"""

from __future__ import annotations

import os
from pathlib import Path

from . import dmg as _dmg
from . import util


def cmd_notarize() -> None:
    """Sign + submit the latest dist/BelkaTunnel-<v>.dmg, then staple."""
    profile = os.environ.get("NOTARY_PROFILE")
    sign_identity = os.environ.get("SIGN_IDENTITY")
    if not profile:
        util.fail(
            "NOTARY_PROFILE is required. Create one once via:\n"
            "  xcrun notarytool store-credentials BELKA-NOTARY "
            "--apple-id you@example.com --team-id TEAMID --password APP_SPECIFIC_PWD"
        )
        raise SystemExit(2)
    if not sign_identity:
        util.fail(
            "SIGN_IDENTITY is required. Get yours from `security find-identity -v -p codesigning`\n"
            "and pass it as 'Developer ID Application: NAME (TEAMID)'."
        )
        raise SystemExit(2)

    version = _dmg.cargo_version()
    dmg_path = util.BUNDLE.parent / f"BelkaTunnel-{version}.img"
    if not dmg_path.exists():
        util.fail(f"missing {dmg_path} — run `bt dmg` first")
        raise SystemExit(1)

    util.step(f"sign {dmg_path.name}")
    util.run(
        [
            "codesign",
            "--force",
            "--sign",
            sign_identity,
            "--timestamp",
            str(dmg_path),
        ]
    )
    util.ok("DMG signed")

    util.step(f"notarytool submit {dmg_path.name} (--wait)")
    util.run(
        [
            "xcrun",
            "notarytool",
            "submit",
            str(dmg_path),
            "--keychain-profile",
            profile,
            "--wait",
        ]
    )

    util.step("stapler staple")
    util.run(["xcrun", "stapler", "staple", str(dmg_path)])
    util.run(["xcrun", "stapler", "validate", str(dmg_path)])

    util.step("spctl assessment")
    r = util.run(
        [
            "spctl",
            "-a",
            "-vv",
            "-t",
            "open",
            "--context",
            "context:primary-signature",
            str(dmg_path),
        ],
        check=False,
        capture=True,
    )
    util.console.print(r.stdout + r.stderr)
    if r.code == 0:
        util.ok("Gatekeeper accepts the notarized DMG")
    else:
        util.fail("spctl assessment did not accept the DMG")
        raise SystemExit(1)
