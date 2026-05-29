"""Smoke test: launch the .app, verify the tunnel routes traffic correctly."""

from __future__ import annotations

import time

import httpx

from . import util


SOCKS_PORT_DEFAULT = 1081  # matches Pasha's pasha-lan config
EXPECTED_EXIT_IP = "173.77.254.243"


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
    try:
        r = httpx.get(
            "https://ifconfig.me",
            timeout=10,
            proxies=proxy,
        )
    except Exception as e:
        util.fail(f"tunnel request failed: {e}")
        raise SystemExit(1)

    if r.status_code != 200:
        util.fail(f"unexpected status {r.status_code}: {r.text[:80]}")
        raise SystemExit(1)

    exit_ip = r.text.strip()
    if exit_ip != EXPECTED_EXIT_IP:
        util.fail(f"tunnel exit IP {exit_ip!r} != expected {EXPECTED_EXIT_IP!r}")
        raise SystemExit(1)
    util.ok(f"tunnel exit IP = {exit_ip}")
    util.ok("smoke test passed")
