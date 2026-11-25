"""Command-line flags of the dev server (see README.md)."""

from __future__ import annotations

import argparse
import dataclasses
import os
import threading
import time

# 2026-11-16T17:30:00Z: "now" at server start unless --now-ms says otherwise.
DEFAULT_NOW_MS = 1_794_850_200_000
# where --move-state starts the old server's side of a move (resources/moving.py)
MOVE_STATES = ("none", "offered", "expired", "copying", "paused", "moved", "done")
# --export: how a new export file is made (resources/moving.py EXPORT_PROFILES); --export-state: one there at start
EXPORT_PROFILES = ("quick", "slow", "stall", "fail")
EXPORT_STATES = ("none", "preparing", "stalled", "failed", "ready")


# --map-tiles: how the maps start (see parse_args)
MAP_TILES = ("online", "none", "offline")
# --release-check: off (the default), or on with this result of the last check (resources/system.py release_state)
RELEASE_CHECK = ("off", "newer", "up_to_date", "error", "never", "checking")
# --teslamate-trouble: the local TeslaMate starts in this trouble, with its admins' notice (resources/teslamate.py)
# --full-rebuild: the full rebuild the server starts with (resources/fullrebuild.py)
FULL_REBUILD = ("none", "running", "failed", "offered", "planned", "paused")
GPS_FIT = ("none", "disagree", "running", "done", "history")
TESLAMATE_TROUBLE = ("none", "connection_refused", "auth_failed", "pin_mismatch", "quiet", "catchup_failed")


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
    move_state: str = "none"
    move_peer_after_s: int = 15
    export_profile: str = "quick"
    export_state: str = "none"
    freeze_clock: bool = False
    sse_interval_ms: int = 2000
    rows_every_s: float = 0
    rebuild_running: bool = False
    health_list_max: int = 500
    gps_fit: str = "none"
    verbose: bool = False
    own_certificate: bool = False
    map_tiles: str = "online"
    release_check: str = "off"
    teslamate_trouble: str = "none"
    full_rebuild: str = "none"
    drives: int = 0


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
    ap.add_argument("--fresh", "--needs-setup", dest="needs_setup", action="store_true",
                    help="start as a fresh server: no accounts, the first-run set-up page (code SETUP-DEV-0000)")
    ap.add_argument("--move-state", choices=MOVE_STATES, default="none",
                    help="start the 'move to another server' section in this state (screenshots)")
    ap.add_argument("--move-peer-after-s", type=int, default=15,
                    help="a made-up new server claims a waiting move code this many seconds after it is made (0: never)")
    ap.add_argument("--export", dest="export_profile", choices=EXPORT_PROFILES, default="quick",
                    help="how a new export file is made: quick (3 s), slow (18 GB in 2 min), stall (stops at 40 %%) "
                         "or fail (no space at 55 %%)")
    ap.add_argument("--export-state", choices=EXPORT_STATES, default="none",
                    help="an export file there at start: preparing (slow, a third done), stalled, failed or ready "
                         "(screenshots; add --freeze-clock to hold it there)")
    ap.add_argument("--freeze-clock", action="store_true", help="keep 'now' fixed instead of advancing")
    ap.add_argument("--sse-interval-ms", type=int, default=2000, help="how often live values are pushed")
    ap.add_argument("--rows-every-s", type=float, default=0,
                    help="also send a `rows` event for every featured car this often, as if rows changed (0: only "
                         "when a piece really starts or ends); pages load their lists again on it")
    ap.add_argument("--rebuild-running", action="store_true",
                    help="every featured car's history is being rebuilt the whole time: its health says "
                         "rebuild_pending and it gets a `rows` event every second (or every --rows-every-s)")
    ap.add_argument("--health-list-max", type=int, default=500,
                    help="how many (the newest) of a car's chain gaps, dropped frames and unknown spans its health "
                         "lists; the *_total beside each counts them all (the real server: 500)")
    ap.add_argument("--gps-fit", choices=GPS_FIT, default="none",
                    help="every featured car's positions from one input are out of step (Data health's "
                         "gps_inputs_disagree, with Find GPS layout for admins), and its GPS layout fit: not run "
                         "(disagree), under way and never ending (running), or done with a layout found (done), or four finished runs with one applied (history)")
    ap.add_argument("--verbose", action="store_true", help="log every request to stderr")
    ap.add_argument("--own-certificate", action="store_true",
                    help="run with the owner's own certificate (expiring in 12 days): no new one can be made here")
    ap.add_argument("--map-tiles", choices=MAP_TILES, default="online",
                    help="map tiles at start: online (OpenStreetMap, the default here), none (a new server: offline with "
                         "no map file, every map shows its 'No map tiles yet' note) or offline (a made-up offline map file)")
    ap.add_argument("--release-check", choices=RELEASE_CHECK, default="off",
                    help="the release check at start: off (the default), or on with the last check's result "
                         "(newer: a newer build is ready; up_to_date; error: GitHub's rate limit; never: not checked "
                         "yet; checking: a check in flight)")
    ap.add_argument("--teslamate-trouble", choices=TESLAMATE_TROUBLE, default="none",
                    help="the local TeslaMate starts in trouble, with its notice for admins: its broker refuses the "
                         "connection, the user name or password, or has another certificate than the pinned one, goes "
                         "quiet, or the database catch-up (then on) fails")
    ap.add_argument("--full-rebuild", choices=FULL_REBUILD, default="none",
                    help="start with a full rebuild of every car under way (running: a long one, a third through), "
                         "one that failed on the second car (failed), or the first car's history made with an older "
                         "decoding and the automatic run off (offered: the notice offers one), or a new version's "
                         "full rebuild of every car planned, waiting its grace (planned: start now, tonight or pause), or a run paused part way after an "
                         "earlier one was cancelled (paused)")
    ap.add_argument("--drives", type=int, default=0,
                    help="give car c1 about this many drives in all (short errands from home added each day; "
                         "0: the usual history, about 1150)")
    a = ap.parse_args(argv)
    if a.cars < 0 or a.cars > 5_000_000:
        ap.error("--cars must be between 0 and 5000000")
    if a.drives < 0 or a.drives > 100_000:
        ap.error("--drives must be between 0 and 100000")
    if a.latency_ms < 0:
        ap.error("--latency-ms must be 0 or more")
    if a.rows_every_s < 0:
        ap.error("--rows-every-s must be 0 or more")
    if a.health_list_max < 1:
        ap.error("--health-list-max must be 1 or more")
    if a.move_peer_after_s < 0:
        ap.error("--move-peer-after-s must be 0 or more")
    if a.needs_setup and a.export_state != "none":
        ap.error("--export-state needs a server that is set up (not --fresh)")
    if a.needs_setup and a.move_state != "none":
        ap.error("--move-state needs a server that is set up (not --fresh)")
    if not os.path.isdir(os.path.join(a.pack, "web")):
        ap.error(f"--pack {a.pack!r} has no web/ folder")
    return Config(pack=a.pack, host=a.host, port=a.port, cars=a.cars, latency_ms=a.latency_ms, seed=a.seed,
                  signed_out=a.signed_out, role=a.role, now_ms=a.now_ms, needs_setup=a.needs_setup,
                  move_state=a.move_state, move_peer_after_s=a.move_peer_after_s,
                  export_profile=a.export_profile, export_state=a.export_state,
                  freeze_clock=a.freeze_clock, sse_interval_ms=max(200, a.sse_interval_ms),
                  rows_every_s=a.rows_every_s, rebuild_running=a.rebuild_running, health_list_max=a.health_list_max,
                  gps_fit=a.gps_fit, verbose=a.verbose,
                  own_certificate=a.own_certificate, map_tiles=a.map_tiles,
                  release_check=a.release_check, teslamate_trouble=a.teslamate_trouble,
                  full_rebuild=a.full_rebuild, drives=a.drives)


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
