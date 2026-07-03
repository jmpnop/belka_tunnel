#!/bin/bash
# Build the BelkaTunnel.app bundle from the release binary.
#
# Environment knobs (all optional — empty = ad-hoc dev build):
#   SIGN_IDENTITY    "Developer ID Application: NAME (TEAMID)" for real signing
#   USE_UNIVERSAL    set to 1 to consume target/universal/release/belka_tunnel
#                    (run `bt universal` first to populate it)
#
set -euo pipefail
cd "$(dirname "$0")"

APP_NAME="BelkaTunnel.app"
BIN_NAME="belka_tunnel"
APP="dist/${APP_NAME}"
ENTITLEMENTS="bundle/entitlements.plist"

# ---------- Pick the binary ----------
#
# `app/` is a workspace member, so cargo writes build output to the WORKSPACE
# target dir (repo-root/target), NOT app/target. Hardcoding "target/release"
# (relative to this script's dir) silently copied a stale pre-workspace
# app/target binary into the bundle — shipping old embedded assets even after
# a clean rebuild. Resolve the real target dir from cargo instead.
WORKSPACE_ROOT="$(cargo locate-project --workspace --message-format plain | xargs dirname)"
TARGET_DIR="${CARGO_TARGET_DIR:-${WORKSPACE_ROOT}/target}"

if [ "${USE_UNIVERSAL:-0}" = "1" ]; then
    SRC_BIN="${TARGET_DIR}/universal/release/${BIN_NAME}"
    if [ ! -x "$SRC_BIN" ]; then
        echo "USE_UNIVERSAL=1 but $SRC_BIN missing — run 'bt universal' first" >&2
        exit 1
    fi
    echo "==> universal binary: $(file "$SRC_BIN" | sed 's/^[^:]*: //')"
else
    echo "==> cargo build --release"
    cargo build --release
    SRC_BIN="${TARGET_DIR}/release/${BIN_NAME}"
fi

if [ ! -x "$SRC_BIN" ]; then
    echo "expected binary not found at $SRC_BIN" >&2
    exit 1
fi

# ---------- Assemble bundle ----------

echo "==> assembling ${APP} (БелкаТуннель)"
rm -rf "${APP}"
mkdir -p "${APP}/Contents/MacOS"
mkdir -p "${APP}/Contents/Resources"

cp "$SRC_BIN" "${APP}/Contents/MacOS/${BIN_NAME}"
cp "bundle/Info.plist" "${APP}/Contents/Info.plist"
cp "assets/AppIcon.icns" "${APP}/Contents/Resources/AppIcon.icns"
chmod +x "${APP}/Contents/MacOS/${BIN_NAME}"

# ---------- Codesign ----------

if [ -n "${SIGN_IDENTITY:-}" ]; then
    echo "==> codesign with Developer ID + Hardened Runtime"
    # Sign nested binary first (inside-out), then the bundle. Avoid --deep
    # (deprecated and signs incorrectly for nested frameworks).
    codesign --force --options runtime --timestamp \
        --entitlements "$ENTITLEMENTS" \
        --sign "$SIGN_IDENTITY" \
        "${APP}/Contents/MacOS/${BIN_NAME}"
    codesign --force --options runtime --timestamp \
        --entitlements "$ENTITLEMENTS" \
        --sign "$SIGN_IDENTITY" \
        "${APP}"
    codesign --verify --strict --verbose=2 "${APP}"
else
    echo "==> ad-hoc codesign (SIGN_IDENTITY unset — dev build)"
    codesign --force --sign - "${APP}"
fi

# Update LaunchServices so Finder picks it up immediately.
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister \
    -f "${APP}" 2>/dev/null || true

# Bust the icon cache so the new icon shows in Finder right away.
touch "${APP}"

echo
echo "==> done: ${PWD}/${APP}"
echo
echo "Run: open ${APP}"
echo "Or:  ${APP}/Contents/MacOS/${BIN_NAME}     # to see logs"
