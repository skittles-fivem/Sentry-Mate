"""Command-line flags of the dev server (see README.md)."""

from __future__ import annotations

import argparse
import dataclasses
import os
import threading
import time

# 2026-11-16T17:30:00Z: "now" at server start unless --now-ms says otherwise.
DEFAULT_NOW_MS = 1_794_850_200_000


@dataclasses.dataclass
class Config:
    pack: str
    host: str = "127.0.0.1"
    port: int = 8000
    cars: int = 3
    latency_ms: int = 0
    seed: int = 1
    signed_out: bool = False
    role: str = "admin"
    now_ms: int = DEFAULT_NOW_MS
    needs_setup: bool = False
    freeze_clock: bool = False
    sse_interval_ms: int = 2000
    verbose: bool = False


def parse_args(argv=None) -> Config:
    ap = argparse.ArgumentParser(prog="serve.py", description="Sentry Mate dev API server (made-up data).")
    ap.add_argument("--pack", required=True, help="UI pack folder, e.g. ui/packs/instrument (serves its web/)")
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=8000, help="0 picks a free port")
    ap.add_argument("--cars", type=int, default=3, help="fleet size; above 3 adds light synthetic cars")
    ap.add_argument("--latency-ms", type=int, default=0, help="delay every /api answer by this much")
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--signed-out", action="store_true", help="start with no session (test the sign-in page)")
    ap.add_argument("--role", choices=("admin", "watch"), default="admin")
    ap.add_argument("--now-ms", type=int, default=DEFAULT_NOW_MS, help="virtual 'now' at start (epoch ms)")
    ap.add_argument("--needs-setup", action="store_true", help="start with no accounts (test the set-up page)")
    ap.add_argument("--freeze-clock", action="store_true", help="keep 'now' fixed instead of advancing")
    ap.add_argument("--sse-interval-ms", type=int, default=2000, help="how often live values are pushed")
    ap.add_argument("--verbose", action="store_true", help="log every request to stderr")
    a = ap.parse_args(argv)
    if a.cars < 0 or a.cars > 5_000_000:
        ap.error("--cars must be between 0 and 5000000")
    if a.latency_ms < 0:
        ap.error("--latency-ms must be 0 or more")
    if not os.path.isdir(os.path.join(a.pack, "web")):
        ap.error(f"--pack {a.pack!r} has no web/ folder")
    return Config(pack=a.pack, host=a.host, port=a.port, cars=a.cars, latency_ms=a.latency_ms, seed=a.seed,
                  signed_out=a.signed_out, role=a.role, now_ms=a.now_ms, needs_setup=a.needs_setup,
                  freeze_clock=a.freeze_clock, sse_interval_ms=max(200, a.sse_interval_ms), verbose=a.verbose)


class Clock:
    """The virtual clock: starts at --now-ms and advances with real time (unless frozen)."""

    def __init__(self, now0_ms: int, frozen: bool = False):
        self.now0 = now0_ms
        self.frozen = frozen
        self._t0 = time.monotonic()
        self._lock = threading.Lock()
        self._offset = 0

    def now(self) -> int:
        if self.frozen:
            return self.now0 + self._offset
        return self.now0 + self._offset + int((time.monotonic() - self._t0) * 1000)

    def advance(self, ms: int):
        """Move the clock forward (tests)."""
        with self._lock:
            self._offset += ms
