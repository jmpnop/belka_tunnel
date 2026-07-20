#!/bin/bash
# Set up a self-hosted GitHub Actions runner for this repo on THIS Mac, so
# `git push origin vX.Y.Z` (release.yml) and pushes to main (ci.yml) build here
# instead of on GitHub-hosted runners (which are billed and blocked by the
# account's billing lock).
#
# Usage:
#   bash scripts/setup-runner.sh              # installs to ~/actions-runner
#   RUNNER_DIR=/Volumes/Ext/ar bash scripts/setup-runner.sh   # external volume
#
# Requirements:
#   - `gh` authenticated with admin on jmpnop/belka_tunnel (mints the token)
#   - Rust + uv + brew already installed (the build itself needs them)
#   - **~5 GB+ free** for the runner's _work checkout + cargo target dir.
#     A fresh CI checkout does NOT reuse this repo's warm target/.
#
# The runner is installed as a launchd service (`svc.sh`) so it survives logout
# and reboots. Re-running this script re-registers cleanly.
set -euo pipefail

REPO="jmpnop/belka_tunnel"
RUNNER_DIR="${RUNNER_DIR:-$HOME/actions-runner}"
LABELS="self-hosted,macOS,x64"

# --- preflight: disk space (build needs several GB) ---
AVAIL_KB=$(df -k "$(dirname "$RUNNER_DIR")" | tail -1 | awk '{print $4}')
AVAIL_GB=$(( AVAIL_KB / 1024 / 1024 ))
echo "==> free space at $(dirname "$RUNNER_DIR"): ${AVAIL_GB} GiB"
if [ "$AVAIL_GB" -lt 6 ]; then
    echo "!! WARNING: <6 GiB free. A fresh universal build + DMGs can exceed this." >&2
    echo "!! Free space, or set RUNNER_DIR to a volume that has room, then re-run." >&2
    read -r -p "Continue anyway? [y/N] " ans
    [ "$ans" = "y" ] || { echo "aborted"; exit 1; }
fi

# --- fetch latest runner (osx-x64; this Mac is Intel) ---
TAG=$(gh api repos/actions/runner/releases/latest --jq '.tag_name')
VER=${TAG#v}
ASSET="actions-runner-osx-x64-${VER}.tar.gz"
URL="https://github.com/actions/runner/releases/download/${TAG}/${ASSET}"

mkdir -p "$RUNNER_DIR"
cd "$RUNNER_DIR"
if [ ! -f "./config.sh" ]; then
    echo "==> downloading ${ASSET}"
    curl -fsSL -o "$ASSET" "$URL"
    tar xzf "$ASSET"
    rm -f "$ASSET"
fi

# --- register (idempotent: remove an old registration first) ---
if [ -f .runner ]; then
    echo "==> removing previous registration"
    REMOVE_TOKEN=$(gh api -X POST "repos/${REPO}/actions/runners/remove-token" --jq '.token')
    ./config.sh remove --token "$REMOVE_TOKEN" || true
fi
REG_TOKEN=$(gh api -X POST "repos/${REPO}/actions/runners/registration-token" --jq '.token')
echo "==> configuring runner for ${REPO}"
./config.sh \
    --url "https://github.com/${REPO}" \
    --token "$REG_TOKEN" \
    --name "$(hostname -s)-belka" \
    --labels "$LABELS" \
    --work "_work" \
    --unattended \
    --replace

# --- install as a launchd service ---
echo "==> installing + starting launchd service"
./svc.sh install
./svc.sh start
./svc.sh status || true

echo
echo "✓ runner installed at $RUNNER_DIR and running as a service."
echo "  Verify:  gh api repos/${REPO}/actions/runners --jq '.runners[]|{name,status}'"
echo "  Trigger: gh workflow run ci.yml   (or push a vX.Y.Z tag for release.yml)"
echo "  Remove:  cd $RUNNER_DIR && ./svc.sh stop && ./svc.sh uninstall && ./config.sh remove"
