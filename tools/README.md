# `bt` — БелкаТуннель build/test/harness CLI

A single uv-managed Python tool that replaces every shell snippet for
building, testing, verifying, and benchmarking БелкаТуннель.

## Setup once

```bash
brew install uv          # if you don't have it
./bt bootstrap           # installs .githooks/ + verifies toolchain
```

That's it. The first `./bt` invocation will create a `.venv` under `tools/`
and resolve dependencies.

## Cheat sheet

```bash
./bt                     # show all commands
./bt build --release     # cargo build --release
./bt bundle              # release build + dist/BelkaTunnel.app
./bt dmg                 # build dist/BelkaTunnel-<version>.dmg (voxel-tree bg)
./bt lint                # cargo clippy -- -D warnings
./bt test                # cargo test --release
./bt verify bundle       # Info.plist keys, codesign, arch, icon
./bt verify policies     # check /Applications/Firefox.app .../policies.json
./bt verify dmg          # mount the latest DMG and check its contents
./bt smoke-test          # launch app, verify menu, route HTTPS through tunnel
./bt bench               # throughput / latency / connection-rate / concurrency
./bt run-bundle          # kill any running instance, open the bundle
./bt log                 # tail the app log
./bt clean               # cargo clean + remove dist/
./bt precommit           # what the git pre-commit hook runs
./bt prepush             # what the git pre-push hook runs
./bt ci                  # everything: precommit + bundle + verify + smoke
```

## DMG installer

`./bt dmg` builds `app/dist/BelkaTunnel-<version>.dmg` via `dmgbuild`:
- Background: `app/assets/dmg-background.png` (800×448) + `…@2x.png` (1600×900 retina).
- `BelkaTunnel.app` placed at (200, 240) — over the left voxel tree.
- `/Applications` symlink at (600, 240) — over the right voxel tree.
- Window chrome hidden (no toolbar, sidebar, status bar) for a clean install UI.
- Volume name "БелкаТуннель", compressed (UDZO) → ~12 MB.

`./bt verify dmg` mounts the latest DMG read-only and asserts:
- `BelkaTunnel.app` is present.
- `Applications` is a symlink to `/Applications`.
- Then detaches.

To change layout (icon positions, window size), edit constants at the top of
`tools/bt/dmg.py`.

## Git hooks

`./bt bootstrap` sets `core.hooksPath` to `.githooks/`:

- `pre-commit` → `./bt precommit` (fmt-check + clippy + test)
- `pre-push`   → `./bt prepush` (precommit + bundle + verify + verify-policies)

Bypass once with `git commit --no-verify` or `git push --no-verify`.

## Why uv + Python instead of bash/Make/just

- **One source of truth** for build/test/harness behavior; the same logic runs
  in pre-commit, pre-push, and ad-hoc `bt` calls.
- **Real types** — `verify` reads Info.plist and policies.json as structured
  data and asserts shape, not via brittle `grep`.
- **Real HTTP client** in `smoke`/`bench` (httpx with SOCKS5 support) rather
  than Apple curl which is missing flags like `--socks5-hostname`.
- **uv is fast** — sub-second cold start, no global pollution; venv is local
  to `tools/.venv`.

## Adding commands

Edit `tools/bt/cli.py` and add a new `@app.command()` function. Heavier
logic goes into a module (e.g. `tools/bt/bench.py`).
