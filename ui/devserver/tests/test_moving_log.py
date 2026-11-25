"""The move's log (docs/ui-api.md, "The move's log"): `log` of GET /system/move and of the move-here SetupJob, step
by step, oldest first, a time on each line, kept across reads, nothing secret in it."""

from __future__ import annotations

import re

from .helpers import ServerCase
from .test_moving import PW, SETUP_CODE, _claimed, _code

SEC = 1000
SECRET = re.compile(r"SMMV-|SMRK-|SETUP-|[0-9a-fA-F]{32}|/|\.sealed")


def _texts(log):
    return [line["text"] for line in log]


def _in_order(case, log, steps):
    texts = _texts(log)
    at = 0
    for s in steps:
        found = next((i for i in range(at, len(texts)) if s in texts[i]), None)
        case.assertIsNotNone(found, f"no {s!r} (in order) in {texts}")
        at = found + 1


def _well_formed(case, log, secrets=()):
    times = [line["at_ms"] for line in log]
    case.assertEqual(times, sorted(times), "oldest first")
    case.assertLessEqual(len(log), 500)
    for line in log:
        case.assertEqual(set(line), {"at_ms", "text"})
        case.assertIsInstance(line["at_ms"], int)
        text = line["text"].replace("192.0.2.20:10100", "").replace("192.0.2.30", "")
        case.assertIsNone(SECRET.search(text), line["text"])
        for s in secrets:
            case.assertNotIn(s, line["text"])


class MoveHereLog(ServerCase):
    server_args = {"needs_setup": True}

    def test_the_new_servers_log_has_every_step_and_grows(self):
        c = _claimed(self.server)
        code = _code()
        j = c.json("POST", "/api/v1/setup/move-here", {"from": "192.0.2.20", "code": code}, expect=201)
        self.assertEqual(_texts(j["log"]), ["Moving here from 192.0.2.20:10100: proving the move code to the old server."])
        before = []
        for _ in range(40):
            self.server.app.clock.advance(500)
            j = c.get("/api/v1/setup/job")
            self.assertEqual(j["log"][:len(before)], before, "lines are only added")
            before = j["log"]
            if j["state"] == "done" and any(t.startswith("Inputs started here") for t in _texts(j["log"])):
                break
        _in_order(self, j["log"], ["Moving here from", "Connected:", "Round 1: copying", "Copying file 1 of",
                                   "Round 1 done", "Round 2 caught up", "Last-round pause", "Last round:",
                                   "Switching to the copied data", "Switched:", "Inputs started here:"])
        files = [t for t in _texts(j["log"]) if t.startswith("Copying file ")]
        self.assertTrue(all(re.fullmatch(r"Copying file \d+ of \d+ \([a-zA-Z' ]+\)\.", t) for t in files), files)
        _well_formed(self, j["log"], (code, SETUP_CODE))
        # read again: the same log
        self.assertEqual(c.get("/api/v1/setup/job")["log"], j["log"])


class FlakyMoveLog(ServerCase):
    server_args = {"needs_setup": True}

    def test_a_flaky_move_says_what_it_waited_for_once(self):
        c = _claimed(self.server)
        c.json("POST", "/api/v1/setup/move-here", {"from": "flaky.example", "code": _code()}, expect=201)
        self.server.app.clock.advance(30 * SEC)
        log = c.get("/api/v1/setup/job")["log"]
        waits = [t for t in _texts(log) if t.startswith("Waiting:")]
        self.assertEqual(len(waits), 1, _texts(log))
        _in_order(self, log, ["Waiting:", "Connected to the old server again", "Switched:"])
        _well_formed(self, log)


class OldServerLog(ServerCase):
    def test_the_old_servers_log_from_the_code_to_done_and_undo(self):
        c = self.c
        d = c.json("POST", "/api/v1/system/move", {"password": PW}, expect=201)
        self.assertEqual(len(d["log"]), 1)
        self.assertTrue(d["log"][0]["text"].startswith("A move code was made"))
        self.server.app.clock.advance(15 * SEC + 17 * SEC + 500)
        g = c.get("/api/v1/system/move")
        self.assertEqual(g["state"], "done")
        _in_order(self, g["log"], ["A move code was made", "connected", "Sent 10 %", "Sent 100 %", "Last-round pause",
                                   "copy matches", "Inputs stopped on this server:", "Done:"])
        _well_formed(self, g["log"], (d["move_code"],))
        g = c.json("POST", "/api/v1/system/move/undo", {"password": PW, "confirm": True})
        self.assertEqual(_texts(g["log"])[-1], "The move was undone: this server takes uploads again.")
        self.assertIn("Done:", " ".join(_texts(g["log"])), "what it said before stays")
        # a new code begins a new log
        d = c.json("POST", "/api/v1/system/move", {"password": PW}, expect=201)
        self.assertEqual(len(d["log"]), 1)


class CancelLog(ServerCase):
    def test_a_cancel_is_in_the_log(self):
        c = self.c
        c.json("POST", "/api/v1/system/move", {"password": PW}, expect=201)
        d = c.json("DELETE", "/api/v1/system/move")
        self.assertEqual(_texts(d["log"])[-1], "The move code was cancelled.")
