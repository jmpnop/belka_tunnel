"""Build / bundle commands."""

from __future__ import annotations

import os
from pathlib import Path

from . import util


def cargo_build(release: bool = False) -> None:
    args = ["cargo", "build"]
    if release:
        args.append("--release")
    util.run(args, cwd=util.APP_DIR)


def cargo_fmt(check: bool = False) -> None:
    args = ["cargo", "fmt"]
    if check:
        args.append("--check")
    util.run(args, cwd=util.APP_DIR)


def cargo_clippy() -> None:
    util.run(
        [
            "cargo",
            "clippy",
            "--release",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
        cwd=util.APP_DIR,
    )


def cargo_test() -> None:
    util.run(["cargo", "test", "--release"], cwd=util.APP_DIR)


def bundle(universal: bool = False) -> None:
    """Build release + assemble dist/BelkaTunnel.app.

    When `universal=True`, expects `bt universal` to have produced a fat
    binary at target/universal/release/proxy-tunnel and consumes it directly
    instead of running another `cargo build --release`.
    """
    env: dict[str, str] = {}
    if universal:
        env["USE_UNIVERSAL"] = "1"
    else:
        cargo_build(release=True)
    util.run(["bash", "./build-app.sh"], cwd=util.APP_DIR, env=env)
    util.ok(f"bundle ready at {util.BUNDLE}")


def build_universal() -> None:
    """Cross-compile arm64 + x86_64, lipo into a fat binary."""
    targets = ["aarch64-apple-darwin", "x86_64-apple-darwin"]
    for t in targets:
        util.run(["rustup", "target", "add", t])
        util.run(
            ["cargo", "build", "--release", "--target", t],
            cwd=util.APP_DIR,
        )
    universal_dir = util.APP_DIR / "target" / "universal"
    universal_dir.mkdir(parents=True, exist_ok=True)
    out = universal_dir / "proxy-tunnel"
    util.run(
        [
            "lipo",
            "-create",
            "-output",
            str(out),
            str(util.APP_DIR / "target/aarch64-apple-darwin/release/proxy-tunnel"),
            str(util.APP_DIR / "target/x86_64-apple-darwin/release/proxy-tunnel"),
        ]
    )
    r = util.run(["lipo", "-info", str(out)], capture=True)
    util.console.print(r.stdout.strip())
    util.ok(f"universal binary at {out}")
