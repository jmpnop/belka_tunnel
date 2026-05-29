"""Tunnel performance benchmarks — throughput / latency / concurrency."""

from __future__ import annotations

import concurrent.futures
import time
from statistics import mean, median, quantiles

import httpx

from . import util

SOCKS_PORT_DEFAULT = 1081
BASE = "https://speed.cloudflare.com"


def proxy_url(port: int) -> str:
    return f"socks5h://127.0.0.1:{port}"


def _get(client: httpx.Client, path: str) -> tuple[float, int]:
    t0 = time.monotonic()
    r = client.get(f"{BASE}{path}", timeout=60)
    dt = time.monotonic() - t0
    return dt, len(r.content)


def bench(socks_port: int = SOCKS_PORT_DEFAULT) -> None:
    util.step("tunnel benchmark")
    proxy = proxy_url(socks_port)

    with httpx.Client(proxies=proxy, http2=True) as cl:
        # A) Throughput — single 10 MB GET
        util.console.print("\n[bold]A) Throughput (10 MB single stream)[/bold]")
        dt, n = _get(cl, "/__down?bytes=10485760")
        mbps = (n * 8) / dt / 1_000_000
        util.console.print(
            f"  {n / 1024 / 1024:.1f} MB in {dt:.2f}s → "
            f"{n / dt / 1_000_000:.2f} MB/s ({mbps:.1f} Mbps)"
        )

        # B) Latency — 5x small GETs, report TTFB-ish total
        util.console.print("\n[bold]B) Latency (5 × 1-byte GET)[/bold]")
        latencies_ms = []
        for _ in range(5):
            t0 = time.monotonic()
            cl.get(f"{BASE}/__down?bytes=1", timeout=5)
            latencies_ms.append((time.monotonic() - t0) * 1000)
        util.console.print(
            f"  mean {mean(latencies_ms):.0f}ms  median {median(latencies_ms):.0f}ms  "
            f"min {min(latencies_ms):.0f}ms  max {max(latencies_ms):.0f}ms"
        )

        # C) Connection rate — 30 sequential small GETs
        util.console.print("\n[bold]C) Connection rate (30 × 10KB)[/bold]")
        t0 = time.monotonic()
        ok = 0
        for _ in range(30):
            try:
                cl.get(f"{BASE}/__down?bytes=10240", timeout=5)
                ok += 1
            except Exception:
                pass
        dt = time.monotonic() - t0
        util.console.print(
            f"  {ok}/30 succeeded in {dt:.2f}s → {ok / dt:.1f} req/s"
        )

    # D) Concurrency — 20 parallel 200 KB GETs (separate connections)
    util.console.print("\n[bold]D) Concurrency (20 × 200KB parallel)[/bold]")
    durations: list[float] = []

    def one(_: int) -> float:
        with httpx.Client(proxies=proxy) as c:
            t0 = time.monotonic()
            c.get(f"{BASE}/__down?bytes=204800", timeout=30)
            return time.monotonic() - t0

    t0 = time.monotonic()
    with concurrent.futures.ThreadPoolExecutor(max_workers=20) as pool:
        for d in pool.map(one, range(20)):
            durations.append(d)
    wall = time.monotonic() - t0
    p50, p95 = (durations[len(durations) // 2], sorted(durations)[18])
    util.console.print(
        f"  wall {wall:.2f}s · per-stream p50 {p50 * 1000:.0f}ms  p95 {p95 * 1000:.0f}ms"
    )

    util.ok("benchmarks complete")
