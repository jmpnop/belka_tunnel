"""Smoke test: launch the .app, verify the tunnel routes traffic correctly."""

from __future__ import annotations

import time

import subprocess

import httpx

from . import util


SOCKS_PORT_DEFAULT = 1081  # matches Pasha's pasha-lan config
DDNS_HOST = "aurora.celestialtech.io"


def expected_exit_ip() -> str:
    """Resolve the tunnel's expected exit IP from the DDNS A-record.

    Hardcoding Pasha's WAN IP would false-fail every time his ISP rotates
    the lease — the whole DDNS setup exists precisely so the IP can change.
    """
    r = subprocess.run(
        ["dig", "+short", DDNS_HOST, "@1.1.1.1"],
        capture_output=True,
        text=True,
        timeout=5,
        check=True,
    )
    ip = r.stdout.strip().splitlines()
    if not ip or not ip[-1]:
        raise SystemExit(f"DDNS A-record for {DDNS_HOST} is empty")
    return ip[-1]


def smoke(socks_port: int = SOCKS_PORT_DEFAULT) -> None:
    util.step("smoke test")
    util.kill_app()

    util.run(["/usr/bin/open", str(util.BUNDLE)])
    if not util.wait_for_listener(socks_port, timeout=15):
        util.fail(f"SOCKS5 listener never bound to :{socks_port}")
        raise SystemExit(1)
    util.ok(f"SOCKS5 listening on :{socks_port}")

    # Menu structure — confirm key items exist.
    items = util.menu_items()
    must_have = [
        "Browse via tunnel (Firefox)",
        "Edit Configuration…",
        "Restart (apply config changes)",
        "Quit",
    ]
    for name in must_have:
        # Items may have status-prefix text appended (e.g. "Connected to ...")
        if not any(name in it for it in items):
            util.fail(f"missing menu item starting with: {name}")
            util.console.print(items)
            raise SystemExit(1)
    util.ok(f"{len(items)} menu items present, required items found")

    # Route a real HTTPS request through the tunnel.
    proxy = f"socks5h://127.0.0.1:{socks_port}"
    util.console.print(f"[dim]GET https://ifconfig.me through {proxy}[/dim]")
    expected_ip = expected_exit_ip()
    util.console.print(f"[dim]expected exit IP from DDNS: {expected_ip}[/dim]")
    try:
        # httpx removed the `proxies=` parameter in 0.28; `proxy=` is the
        # current form and works back to 0.26.
        r = httpx.get("https://ifconfig.me", timeout=10, proxy=proxy)
    except Exception as e:
        util.fail(f"tunnel request failed: {e}")
        raise SystemExit(1)

    if r.status_code != 200:
        util.fail(f"unexpected status {r.status_code}: {r.text[:80]}")
        raise SystemExit(1)

    exit_ip = r.text.strip()
    if exit_ip != expected_ip:
        util.fail(f"tunnel exit IP {exit_ip!r} != DDNS A-record {expected_ip!r}")
        raise SystemExit(1)
    util.ok(f"tunnel exit IP = {exit_ip}")
    util.ok("smoke test passed")
