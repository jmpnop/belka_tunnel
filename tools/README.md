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
./bt lint                # cargo clippy -- -D warnings
./bt test                # cargo test --release
./bt verify bundle       # Info.plist keys, codesign, arch, icon
./bt verify policies     # check /Applications/Firefox.app .../policies.json
./bt smoke-test          # launch app, verify menu, route HTTPS through tunnel
./bt bench               # throughput / latency / connection-rate / concurrency
./bt run-bundle          # kill any running instance, open the bundle
./bt log                 # tail the app log
./bt nuke                # cargo clean + wipe app data (DESTRUCTIVE)
./bt precommit           # what the git pre-commit hook runs
./bt prepush             # what the git pre-push hook runs
./bt ci                  # everything: precommit + bundle + verify + smoke
```

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
