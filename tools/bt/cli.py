"""Top-level CLI dispatcher. Run `bt --help` for the full list."""

from __future__ import annotations

import sys

import typer

from . import build, dmg, notarize, smoke, util, verify

app = typer.Typer(no_args_is_help=True, add_completion=False, help=__doc__)


# ---------- Bootstrap / hooks ----------


@app.command()
def bootstrap() -> None:
    """Install git hooks; verify toolchain (cargo, magick, ffmpeg)."""
    util.step("bootstrap")
    util.run(
        ["git", "config", "core.hooksPath", ".githooks"],
        cwd=util.REPO_ROOT,
    )
    util.ok("git hooks → .githooks/")
    for tool in ["cargo", "rustc", "magick", "ffmpeg", "hdiutil"]:
        if util.which(tool):
            util.ok(f"found: {tool}")
        else:
            util.warn(f"missing: {tool} (some tasks will fail)")


# ---------- Build / bundle ----------


@app.command(name="build")
def cmd_build(
    release: bool = typer.Option(False, "--release", "-r"),
) -> None:
    """`cargo build` (debug or release)."""
    build.cargo_build(release=release)


@app.command()
def bundle() -> None:
    """Build a UNIVERSAL (arm64+x86_64) release + assemble dist/BelkaTunnel.app."""
    build.bundle()


@app.command()
def universal() -> None:
    """Cross-build the БелкаТуннель arm64+x86_64 universal binary via lipo."""
    build.build_universal()


@app.command(name="bundle-pfusers")
def cmd_bundle_pfusers() -> None:
    """Build a UNIVERSAL (arm64+x86_64) release + assemble pfusers/dist/pfUsers.app."""
    build.bundle_pfusers()


@app.command(name="universal-pfusers")
def cmd_universal_pfusers() -> None:
    """Cross-build the pfUsers arm64+x86_64 universal binary via lipo."""
    build.build_universal_pfusers()


# ---------- Lint + test ----------


@app.command()
def fmt(check: bool = typer.Option(False, "--check")) -> None:
    """`cargo fmt` (use --check for CI)."""
    build.cargo_fmt(check=check)


@app.command()
def lint() -> None:
    """`cargo clippy --release -- -D warnings`."""
    build.cargo_clippy()


@app.command()
def test() -> None:
    """`cargo test --release` + pytest on tools/tests."""
    build.cargo_test()
    pytest()


@app.command()
def pytest() -> None:
    """Run the Python harness's own pytest suite."""
    util.step("pytest")
    util.run(
        ["uv", "run", "--dev", "--project", str(util.REPO_ROOT / "tools"), "pytest", "-q"],
        cwd=util.REPO_ROOT,
    )


# ---------- Verify ----------


verify_app = typer.Typer(help="Bundle / policy verification.")
app.add_typer(verify_app, name="verify")


@verify_app.command("bundle")
def verify_bundle() -> None:
    """Check the .app bundle: required files, Info.plist keys, codesign."""
    verify.verify_bundle()


@verify_app.command("policies")
def verify_policies() -> None:
    """Validate /Applications/Firefox.app/.../policies.json schema + locks."""
    verify.verify_policies()


@verify_app.command("dmg")
def verify_dmg() -> None:
    """Mount the latest DMG and check it contains the .app + /Applications link."""
    dmg.verify_dmg()


@verify_app.command("pfusers")
def verify_pfusers() -> None:
    """Sanity-check the pfUsers.app bundle: required files, Info.plist keys, codesign."""
    verify.verify_pfusers_bundle()


@verify_app.command("pfusers-dmg")
def verify_pfusers_dmg_cmd() -> None:
    """Mount the latest pfUsers DMG and check it contains the .app + /Applications link."""
    dmg.verify_pfusers_dmg()


# ---------- DMG ----------


@app.command(name="dmg")
def cmd_dmg() -> None:
    """Build dist/BelkaTunnel-<version>.dmg (uses dmgbuild + the voxel-tree bg)."""
    dmg.build_dmg()


@app.command(name="dmg-pfusers")
def cmd_dmg_pfusers() -> None:
    """Build pfusers/dist/pfUsers-<version>.dmg from the bundled pfUsers.app."""
    dmg.build_pfusers_dmg()


@app.command(name="notarize")
def cmd_notarize() -> None:
    """Sign + notarize + staple the latest DMG. Needs SIGN_IDENTITY + NOTARY_PROFILE env."""
    notarize.cmd_notarize()


@app.command(name="release")
def cmd_release() -> None:
    """Full release pipeline: universal → bundle → verify → dmg → notarize.

    Requires SIGN_IDENTITY and NOTARY_PROFILE env. Produces a signed +
    notarized + stapled DMG ready for distribution.
    """
    util.step("release pipeline")
    build.bundle()  # always universal + cleaned
    verify.verify_bundle()
    verify.verify_policies()
    dmg.build_dmg()
    notarize.cmd_notarize()
    util.ok("release artifact ready")


# ---------- Smoke / bench ----------


@app.command()
def smoke_test() -> None:
    """Launch the bundle, verify menu, route a real HTTPS request through the tunnel."""
    smoke.smoke()


@app.command()
def bench(
    port: int = typer.Option(1081, "--port", "-p", help="local SOCKS5 port"),
) -> None:
    """Tunnel throughput / latency / concurrency benchmarks."""
    from . import bench as _bench

    _bench.bench(socks_port=port)


# ---------- CI pipelines ----------


@app.command()
def precommit() -> None:
    """Pre-commit: fmt-check + lint + test."""
    util.step("precommit")
    build.cargo_fmt(check=True)
    build.cargo_clippy()
    build.cargo_test()
    util.ok("pre-commit passed")


@app.command()
def prepush() -> None:
    """Pre-push: precommit + bundle + verify + policies."""
    util.step("prepush")
    build.cargo_fmt(check=True)
    build.cargo_clippy()
    build.cargo_test()
    build.bundle()
    verify.verify_bundle()
    verify.verify_policies()
    util.ok("pre-push passed")


@app.command()
def ci() -> None:
    """Full local CI: precommit + bundle + verify + smoke."""
    util.step("ci")
    build.cargo_fmt(check=True)
    build.cargo_clippy()
    build.cargo_test()
    build.bundle()
    verify.verify_bundle()
    verify.verify_policies()
    smoke.smoke()
    util.ok("CI green")


# ---------- Runtime ----------


@app.command()
def run() -> None:
    """Run the release binary in the foreground (RUST_LOG=info)."""
    util.run(
        [str(util.BINARY)],
        env={"RUST_LOG": "info,russh=warn"},
        check=False,
    )


@app.command(name="run-bundle")
def run_bundle() -> None:
    """Stop any running instance and launch the bundle."""
    util.kill_app()
    util.run(["/usr/bin/open", str(util.BUNDLE)])
    util.ok(f"launched {util.BUNDLE}")


@app.command()
def kill() -> None:
    """Stop any running belka_tunnel process."""
    util.kill_app()
    util.ok("stopped")


@app.command()
def log() -> None:
    """Tail the app log."""
    util.run(["tail", "-F", str(util.LOG_FILE)], check=False)


@app.command()
def config() -> None:
    """Open the config.json in the default editor."""
    util.run(["/usr/bin/open", str(util.CONFIG_FILE)])


# ---------- Cleanup ----------


@app.command()
def clean() -> None:
    """`cargo clean` + remove dist/."""
    util.run(["cargo", "clean"], cwd=util.APP_DIR)
    util.run(["rm", "-rf", "dist"], cwd=util.APP_DIR)


if __name__ == "__main__":
    app()
