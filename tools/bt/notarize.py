"""Signing + notarization for release artifacts (БелкаТуннель + pfUsers).

Driven entirely by env vars so secrets stay out of code:
  NOTARY_PROFILE   a keychain profile name created once via
                   `xcrun notarytool store-credentials <PROFILE> ...`
                   (Apple-ID + app-specific password + team ID, or an
                   App Store Connect API key).
  SIGN_IDENTITY    "Developer ID Application: NAME (TEAMID)" — the codesign
                   identity. The .app is already signed + hardened by
                   build-app.sh at bundle time; this module reuses the same
                   identity to sign the .dmg.

Gold-standard flow per product (see the reference-macos-codesign memory):

  1. bundle (build-app.sh) signs the .app with Developer ID + hardened runtime
  2. notarize + staple the .app          ← so an installed copy launches OFFLINE
  3. build the DMG from the STAPLED .app  ← ticket travels inside the DMG
  4. sign + notarize + staple the DMG     ← the downloaded artifact clears Gatekeeper

`cmd_notarize()` / `cmd_notarize_pfusers()` own steps 2–4 end to end (they
rebuild the DMG between stapling the app and signing the image), so the
distributable DMG always carries a stapled app. Stapling BOTH means first
launch works with no network.
"""

from __future__ import annotations

import os
from pathlib import Path

from . import util


# ---------- Env / credentials ----------


def _require_creds() -> tuple[str, str]:
    """Return (notary_profile, sign_identity) or exit with setup guidance."""
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
    return profile, sign_identity


# ---------- Reusable primitives ----------


def notarize_app(app: Path, *, profile: str) -> None:
    """Submit the (already Developer-ID-signed) .app to Apple and staple it.

    `notarytool` refuses a bare .app, so we zip it with ditto (keeping the
    bundle as the top-level entry), submit the zip, then staple the ticket to
    the .app itself — a stapled app launches offline, without a Gatekeeper
    round-trip. The zip is staged next to the app and removed afterwards.
    """
    if not app.exists():
        util.fail(f"missing {app} — build the bundle first")
        raise SystemExit(1)

    zip_path = app.with_suffix(app.suffix + ".notarize.zip")
    util.step(f"notarize {app.name}")
    util.run(
        ["ditto", "-c", "-k", "--keepParent", str(app), str(zip_path)]
    )
    try:
        util.run(
            [
                "xcrun",
                "notarytool",
                "submit",
                str(zip_path),
                "--keychain-profile",
                profile,
                "--wait",
            ]
        )
    finally:
        zip_path.unlink(missing_ok=True)

    util.run(["xcrun", "stapler", "staple", str(app)])
    util.run(["xcrun", "stapler", "validate", str(app)])

    r = util.run(
        ["spctl", "-a", "-vv", "--type", "exec", str(app)],
        check=False,
        capture=True,
    )
    util.console.print(r.stdout + r.stderr)
    if r.code != 0:
        util.fail("spctl did not accept the notarized .app")
        raise SystemExit(1)
    util.ok(f"{app.name} notarized + stapled (launches offline)")


def sign_notarize_dmg(dmg: Path, *, profile: str, sign_identity: str) -> None:
    """Sign the DMG with Developer ID, notarize it, staple, and assess."""
    if not dmg.exists():
        util.fail(f"missing {dmg} — build the DMG first")
        raise SystemExit(1)

    util.step(f"sign {dmg.name}")
    util.run(
        [
            "codesign",
            "--force",
            "--sign",
            sign_identity,
            "--timestamp",
            str(dmg),
        ]
    )
    util.ok("DMG signed")

    util.step(f"notarytool submit {dmg.name} (--wait)")
    util.run(
        [
            "xcrun",
            "notarytool",
            "submit",
            str(dmg),
            "--keychain-profile",
            profile,
            "--wait",
        ]
    )

    util.step("stapler staple")
    util.run(["xcrun", "stapler", "staple", str(dmg)])
    util.run(["xcrun", "stapler", "validate", str(dmg)])

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
            str(dmg),
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


# ---------- Artifact path helpers ----------


def _belka_dmg() -> Path:
    from . import dmg as _dmg

    return util.BUNDLE.parent / f"BelkaTunnel-{_dmg.cargo_version()}.dmg"


def _pfusers_dmg() -> Path:
    from . import dmg as _dmg
    from .build import PFUSERS_BUNDLE

    return PFUSERS_BUNDLE.parent / f"pfUsers-{_dmg.pfusers_cargo_version()}.dmg"


# ---------- Product-level pipelines ----------


def cmd_notarize() -> None:
    """БелкаТуннель: staple the .app, rebuild the DMG from it, sign + notarize + staple the DMG.

    Assumes `bt bundle` already ran (and signed the .app with SIGN_IDENTITY).
    The DMG is rebuilt here so it carries the stapled app, then the image
    itself is signed and notarized. Needs SIGN_IDENTITY + NOTARY_PROFILE.
    """
    from . import dmg as _dmg

    profile, sign_identity = _require_creds()
    notarize_app(util.BUNDLE, profile=profile)
    _dmg.build_dmg()  # rebuild so the stapled app is the one inside the DMG
    sign_notarize_dmg(_belka_dmg(), profile=profile, sign_identity=sign_identity)
    util.ok("БелкаТуннель fully notarized (app + dmg stapled)")


def cmd_notarize_pfusers() -> None:
    """pfUsers: staple the .app, rebuild the DMG from it, sign + notarize + staple the DMG.

    Assumes `bt bundle-pfusers` already ran (and signed the .app). Needs
    SIGN_IDENTITY + NOTARY_PROFILE.
    """
    from . import dmg as _dmg
    from .build import PFUSERS_BUNDLE

    profile, sign_identity = _require_creds()
    notarize_app(PFUSERS_BUNDLE, profile=profile)
    _dmg.build_pfusers_dmg()  # rebuild so the stapled app is inside the DMG
    sign_notarize_dmg(_pfusers_dmg(), profile=profile, sign_identity=sign_identity)
    util.ok("pfUsers fully notarized (app + dmg stapled)")
