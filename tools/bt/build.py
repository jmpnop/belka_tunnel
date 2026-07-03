"""Build / bundle commands."""

from __future__ import annotations

import os
from pathlib import Path

from . import util


def cargo_clean(cwd: Path | None = None, package: str | None = None) -> None:
    """Wipe compiled output so the next build is from scratch.

    NOT called by `bundle()` anymore — `app/build.rs` registers the embedded
    asset dirs with `cargo:rerun-if-changed`, so changing the animation frames
    already invalidates the cached binary. This stays available for a manual
    `bt clean`-style full reset when you want one.
    """
    args = ["cargo", "clean"]
    if package:
        args += ["-p", package]
    util.run(args, cwd=cwd or util.APP_DIR)


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


# Both apps ship as universal (arm64 + x86_64) binaries — ALWAYS. There is no
# host-arch-only bundle path: a thin slice would refuse to launch on the other
# architecture, and we never want to discover that after a release. `bundle()`
# and `bundle_pfusers()` cross-build both slices and lipo them every time.
UNIVERSAL_TARGETS = ["aarch64-apple-darwin", "x86_64-apple-darwin"]


def _build_universal(bin_name: str, package: str | None = None) -> Path:
    """Cross-compile both arches and lipo them into one fat binary.

    Returns the path to the universal binary under the WORKSPACE target dir
    (workspace-target/universal/release/<bin_name>) — where both build-app.sh
    scripts look when USE_UNIVERSAL=1. `app/`/`pfusers/` are workspace members,
    so all cargo output lands at the repo-root target, never a per-crate one.
    """
    target_dir = util.REPO_ROOT / "target"
    build = ["cargo", "build", "--release"]
    if package:
        build += ["-p", package]
    for t in UNIVERSAL_TARGETS:
        util.run(["rustup", "target", "add", t])
        util.run(build + ["--target", t], cwd=util.REPO_ROOT)
    universal_dir = target_dir / "universal" / "release"
    universal_dir.mkdir(parents=True, exist_ok=True)
    out = universal_dir / bin_name
    util.run(
        ["lipo", "-create", "-output", str(out)]
        + [str(target_dir / t / "release" / bin_name) for t in UNIVERSAL_TARGETS]
    )
    r = util.run(["lipo", "-info", str(out)], capture=True)
    util.console.print(r.stdout.strip())
    util.ok(f"universal binary at {out}")
    return out


def build_universal() -> Path:
    """Cross-build the БелкаТуннель universal binary (arm64 + x86_64)."""
    return _build_universal("belka_tunnel")


def build_universal_pfusers() -> Path:
    """Cross-build the pfUsers universal binary (arm64 + x86_64)."""
    return _build_universal("pfusers", package="pfusers")


def bundle() -> None:
    """Build a UNIVERSAL release binary + assemble dist/BelkaTunnel.app.

    Always cross-builds arm64 + x86_64 and lipos them — there is no thin
    fallback. Incremental: `app/build.rs` invalidates the cache when embedded
    assets change, so a full `cargo clean` isn't needed for correctness.
    """
    build_universal()
    util.run(["bash", "./build-app.sh"], cwd=util.APP_DIR, env={"USE_UNIVERSAL": "1"})
    util.ok(f"bundle ready at {util.BUNDLE}")


PFUSERS_DIR = util.REPO_ROOT / "pfusers"
PFUSERS_BUNDLE = PFUSERS_DIR / "dist" / "pfUsers.app"


def bundle_pfusers() -> None:
    """Build a UNIVERSAL pfUsers binary + assemble pfusers/dist/pfUsers.app.

    Mirrors `bundle()`: always arm64 + x86_64, incremental. Built artifacts
    live under the WORKSPACE target dir, not a per-crate one.
    """
    build_universal_pfusers()
    util.run(["bash", "./build-app.sh"], cwd=PFUSERS_DIR, env={"USE_UNIVERSAL": "1"})
    util.ok(f"pfUsers bundle ready at {PFUSERS_BUNDLE}")
