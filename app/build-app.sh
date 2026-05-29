#!/bin/bash
# Build the ProxyTunnel.app bundle from the release binary.
set -euo pipefail
cd "$(dirname "$0")"

APP_NAME="BelkaTunnel.app"
BIN_NAME="proxy-tunnel"
APP="dist/${APP_NAME}"

echo "==> cargo build --release"
cargo build --release

echo "==> assembling ${APP} (БелкаТуннель)"
rm -rf "${APP}"
mkdir -p "${APP}/Contents/MacOS"
mkdir -p "${APP}/Contents/Resources"

cp "target/release/${BIN_NAME}" "${APP}/Contents/MacOS/${BIN_NAME}"
cp "bundle/Info.plist" "${APP}/Contents/Info.plist"
cp "assets/AppIcon.icns" "${APP}/Contents/Resources/AppIcon.icns"

chmod +x "${APP}/Contents/MacOS/${BIN_NAME}"

# Ad-hoc sign — without this, recent macOS will refuse to run unsigned binaries
# from the .app bundle. "-" means ad-hoc (no identity).
echo "==> ad-hoc codesign"
codesign --force --deep --sign - "${APP}"

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
