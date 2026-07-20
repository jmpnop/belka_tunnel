# Release workflow

> ⚠️ **`release.yml` is currently broken** — every tag-triggered run fails in
> ~0s with GitHub's "workflow file issue" (a pre-existing problem). **Do not rely
> on pushing a tag to produce a release.** Until it's fixed, build + publish
> releases **locally**:
> ```bash
> ./bt bundle && ./bt dmg \
>   && gh release create vX.Y.Z app/dist/BelkaTunnel-X.Y.Z.dmg
> ```
> The section below documents the *intended* workflow behaviour once repaired.

`release.yml` is tag-driven. To cut a release:

```bash
# Bump the version, commit, tag, push.
sed -i '' 's/^version = ".*"/version = "0.2.0"/' app/Cargo.toml
git commit -am "Release 0.2.0"
git tag v0.2.0
git push origin main --tags
```

That triggers a build on `macos-14` that:
1. Runs `bt precommit` (fmt-check + clippy + cargo test + pytest).
2. Cross-builds arm64 + x86_64 via `bt universal`.
3. Bundles into `BelkaTunnel.app`. If `SIGN_IDENTITY` is set, signs with
   Developer ID + Hardened Runtime + entitlements; otherwise ad-hoc.
4. Builds `BelkaTunnel-<version>.dmg` via `bt dmg`.
5. If notarization secrets are set, runs `bt notarize` (which signs the DMG,
   submits via `notarytool --wait`, then staples).
6. Uploads the .dmg as a GitHub Release asset.

## Required secrets

These are set in **Settings → Secrets and variables → Actions**:

| Secret                       | Purpose                                                          |
|------------------------------|------------------------------------------------------------------|
| `CERTIFICATES_P12`           | Base64-encoded `.p12` export of your Developer ID Application cert |
| `CERTIFICATES_P12_PASSWORD`  | Password for the `.p12`                                          |
| `SIGN_IDENTITY`              | `Developer ID Application: NAME (TEAMID)`                       |
| `APPLE_ID`                   | Your Apple ID email                                              |
| `APPLE_TEAM_ID`              | 10-character Team ID (`developer.apple.com → Membership`)        |
| `APPLE_APP_PASSWORD`         | App-specific password (created at appleid.apple.com)             |

If a secret is missing the signing/notarization steps are skipped and you
get an ad-hoc-signed .dmg that requires right-click → Open on first launch.

## Exporting your cert as base64 .p12

```bash
# In Keychain Access → My Certificates → right-click "Developer ID Application"
# → Export → save as DevID.p12 with a password.
base64 -i DevID.p12 | pbcopy
# Paste into the CERTIFICATES_P12 secret.
```
