#!/bin/bash
# Build the pfUsers.app bundle from the release binary.
#
# Environment knobs (all optional — empty = ad-hoc dev build):
#   SIGN_IDENTITY    "Developer ID Application: NAME (TEAMID)" for real signing
#   USE_UNIVERSAL    set to 1 to consume target/universal/release/pfusers
#                    (run `bt universal-pfusers` first to populate it)
#
# Run from the workspace root or pfusers/ — the script cds to pfusers/.
set -euo pipefail
cd "$(dirname "$0")"

APP_NAME="pfUsers.app"
BIN_NAME="pfusers"
APP="dist/${APP_NAME}"
ENTITLEMENTS="bundle/entitlements.plist"

# Workspace target dir lives one level up from pfusers/.
WORKSPACE_TARGET="../target"

# ---------- Pick the binary ----------

if [ "${USE_UNIVERSAL:-0}" = "1" ]; then
    SRC_BIN="${WORKSPACE_TARGET}/universal/release/${BIN_NAME}"
    if [ ! -x "$SRC_BIN" ]; then
        echo "USE_UNIVERSAL=1 but $SRC_BIN missing — run 'bt universal-pfusers' first" >&2
        exit 1
    fi
    echo "==> universal binary: $(file "$SRC_BIN" | sed 's/^[^:]*: //')"
else
    echo "==> cargo build --release -p pfusers"
    (cd .. && cargo build --release -p pfusers)
    SRC_BIN="${WORKSPACE_TARGET}/release/${BIN_NAME}"
fi

# ---------- Assemble bundle ----------

echo "==> assembling ${APP} (pfUsers)"
rm -rf "${APP}"
mkdir -p "${APP}/Contents/MacOS"
mkdir -p "${APP}/Contents/Resources"

cp "$SRC_BIN" "${APP}/Contents/MacOS/${BIN_NAME}"
cp "bundle/Info.plist" "${APP}/Contents/Info.plist"
chmod +x "${APP}/Contents/MacOS/${BIN_NAME}"

# Optional custom icon (pfUsers doesn't ship one yet; fall back to macOS default).
if [ -f "assets/AppIcon.icns" ]; then
    cp "assets/AppIcon.icns" "${APP}/Contents/Resources/AppIcon.icns"
fi

# ---------- Codesign ----------

if [ -n "${SIGN_IDENTITY:-}" ]; then
    echo "==> codesign with Developer ID + Hardened Runtime"
    codesign --force --options runtime --timestamp \
        --entitlements "$ENTITLEMENTS" \
        --sign "$SIGN_IDENTITY" \
        "${APP}/Contents/MacOS/${BIN_NAME}"
    codesign --force --options runtime --timestamp \
        --entitlements "$ENTITLEMENTS" \
        --sign "$SIGN_IDENTITY" \
        "${APP}"
    echo "==> codesign verify"
    codesign --verify --strict --verbose=2 "${APP}"
else
    echo "==> ad-hoc codesign (no SIGN_IDENTITY set)"
    codesign --force --sign - "${APP}/Contents/MacOS/${BIN_NAME}"
    codesign --force --sign - "${APP}"
fi

echo "==> done: $(pwd)/${APP}"
echo ""
echo "Run: open ${APP}"
echo "Or:  ${APP}/Contents/MacOS/${BIN_NAME}     # to see logs"
