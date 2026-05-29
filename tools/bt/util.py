"""Shared utilities — paths, command execution, output formatting."""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import time
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Iterator

from rich.console import Console

console = Console()
err_console = Console(stderr=True, style="bold red")


# ---------- Paths ----------

REPO_ROOT = Path(__file__).resolve().parents[2]
APP_DIR = REPO_ROOT / "app"
BUNDLE = APP_DIR / "dist" / "BelkaTunnel.app"
BINARY = APP_DIR / "target" / "release" / "proxy-tunnel"
LOG_DIR = (
    Path.home()
    / "Library"
    / "Application Support"
    / "io.celestialtech.BelkaTunnel"
)
LOG_FILE = LOG_DIR / "logs" / "proxy-tunnel.log"
CONFIG_FILE = LOG_DIR / "config.json"  # the app's persisted config (JSON, not TOML)


# ---------- Process exec ----------


@dataclass
class CmdResult:
    code: int
    stdout: str
    stderr: str

    @property
    def ok(self) -> bool:
        return self.code == 0


def run(
    cmd: list[str] | str,
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
    capture: bool = False,
    check: bool = True,
    timeout: float | None = None,
) -> CmdResult:
    """Run a command, streaming output by default. Set `capture=True` for tests."""
    shell = isinstance(cmd, str)
    if not capture:
        console.print(f"[dim]$ {cmd if shell else ' '.join(map(str, cmd))}[/dim]")
    proc = subprocess.run(
        cmd,
        cwd=cwd,
        env={**os.environ, **(env or {})},
        shell=shell,
        capture_output=capture,
        text=True,
        timeout=timeout,
        check=False,
    )
    if check and proc.returncode != 0:
        err_console.print(
            f"command exited {proc.returncode}: "
            f"{cmd if shell else ' '.join(map(str, cmd))}"
        )
        if capture and proc.stderr:
            err_console.print(proc.stderr)
        sys.exit(proc.returncode)
    return CmdResult(proc.returncode, proc.stdout or "", proc.stderr or "")


def which(name: str) -> str | None:
    return shutil.which(name)


# ---------- Status helpers ----------


def step(title: str) -> None:
    console.rule(f"[bold cyan]{title}[/bold cyan]", style="cyan")


def ok(msg: str) -> None:
    console.print(f"[bold green]✓[/bold green] {msg}")


def fail(msg: str) -> None:
    err_console.print(f"[bold red]✗[/bold red] {msg}")


def warn(msg: str) -> None:
    console.print(f"[bold yellow]![/bold yellow] {msg}")


@contextmanager
def timed(label: str) -> Iterator[None]:
    t0 = time.monotonic()
    try:
        yield
    finally:
        dt = time.monotonic() - t0
        console.print(f"[dim]{label}: {dt:.2f}s[/dim]")


# ---------- App process management ----------


def app_pids() -> list[int]:
    """PIDs of running proxy-tunnel processes."""
    import psutil

    return [
        p.pid
        for p in psutil.process_iter(["name", "exe"])
        if p.info["name"] == "proxy-tunnel"
    ]


def kill_app() -> None:
    """Stop any running proxy-tunnel process and wait for it to exit."""
    import psutil

    for pid in app_pids():
        try:
            p = psutil.Process(pid)
            p.terminate()
            p.wait(timeout=3)
        except psutil.NoSuchProcess:
            pass
        except psutil.TimeoutExpired:
            try:
                psutil.Process(pid).kill()
            except psutil.NoSuchProcess:
                pass


def wait_for_listener(port: int, host: str = "127.0.0.1", timeout: float = 10.0) -> bool:
    """Block until something is accept()ing on `host:port`, or timeout.

    Uses a TCP-connect probe instead of walking psutil.net_connections — that
    is O(all-tcp-on-this-mac) per tick and needs accessibility privileges on
    recent macOS.
    """
    import socket

    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection((host, port), timeout=0.2):
                return True
        except OSError:
            time.sleep(0.1)
    return False


# ---------- Menu introspection via osascript ----------


def menu_items() -> list[str]:
    """Return the top-level menu item names of the running proxy-tunnel app."""
    script = (
        'tell application "System Events" to tell process "proxy-tunnel" '
        "to get name of every menu item of menu 1 of menu bar item 1 "
        "of menu bar 1"
    )
    r = run(["/usr/bin/osascript", "-e", script], capture=True, check=False)
    return [s.strip() for s in r.stdout.split(",")]
