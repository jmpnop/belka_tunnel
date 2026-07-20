# CI/CD — self-hosted

Both workflows run on **our own self-hosted macOS runner** (this repo's build
Mac), not GitHub-hosted runners. GitHub-hosted macOS runners are billed and are
currently blocked by the account's billing lock, so hosted builds fail to start.
Self-hosted runners are free and unaffected by that lock.

- **`ci.yml`** — on every push to `main` + every PR: `bt precommit` (fmt +
  clippy + tests) → `bt bundle` + `bt verify bundle` → same for pfUsers.
- **`release.yml`** — on `git push origin vX.Y.Z`: builds both apps universal,
  verifies, builds both DMGs, notarizes *if* signing secrets are set, and
  publishes a GitHub Release with both DMGs attached.

Both are validated with [`actionlint`](https://github.com/rhysd/actionlint).

## One-time runner setup

```bash
bash scripts/setup-runner.sh
```

This downloads the runner, registers it against `jmpnop/belka_tunnel` (token
minted via `gh`), and installs it as a launchd service that survives reboot.

### Prerequisites (both are real blockers)

1. **Disk space** — a fresh CI checkout builds its own `target/` and does *not*
   reuse this repo's warm one; budget **~5–6 GB free**. If the main disk is
   full, put the runner on another volume: `RUNNER_DIR=/Volumes/Ext/ar bash
   scripts/setup-runner.sh`.
2. **`gh` admin auth** on the repo (to mint the registration token).

### Signing / notarization (optional)

Without secrets the release ships **ad-hoc-signed** DMGs (Gatekeeper warning on
download) — same as the current local builds. To notarize, set repo secrets
`SIGN_IDENTITY`, `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_APP_PASSWORD`. On a
self-hosted Mac the Developer ID key is already in the login keychain, so no
`CERTIFICATES_P12` import is needed — `SIGN_IDENTITY` alone drives `bt`'s signing.

> **Do not** reference `secrets.*` in a step `if:` — the `secrets` context isn't
> available there and GitHub rejects the whole file at parse time (a 0-second
> "workflow file issue"). Signing is gated on the `Detect signing` step's
> outputs instead.

## Cutting a release

```bash
# bump the single source of truth, commit, tag, push
sed -i '' 's/^version = ".*"/version = "0.3.0"/' app/Cargo.toml   # [package].version
git commit -am "release: 0.3.0"
git tag v0.3.0
git push origin main --tags
```

The runner picks up the tag and publishes the release. `bt dmg` derives the DMG
version from `app/Cargo.toml [package].version`; the About window + Info.plist
derive from it too (see build-app.sh version stamping).

## Managing the runner

```bash
gh api repos/jmpnop/belka_tunnel/actions/runners --jq '.runners[]|{name,status}'
gh workflow run ci.yml                 # manual trigger
cd ~/actions-runner && ./svc.sh status # service state
```
