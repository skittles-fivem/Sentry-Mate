"""Data health's Find GPS layout (docs/ui-api.md: /cars/{car}/gps-layout/fit): admins only, a fit started with
202 and its progress on the virtual clock to a made-up result (layouts, counts and metres only, no position),
Apply through PUT /cars/{car}/gps-layout, and --gps-fit's states."""

from __future__ import annotations

from .helpers import ServerCase

FIT = "/api/v1/cars/c1/gps-layout/fit"
LAYOUT_URL = "/api/v1/cars/c1/gps-layout"
LAYOUT = "lat 0|28@1- (1e-6,0); lon 28|28@1- (1e-6,16.777216)"


class FindAndApply(ServerCase):
    def test_a_fit_runs_ends_and_is_applied(self):
        idle = self.c.get(FIT)
        self.assertEqual((idle["state"], idle["result"], idle["current"]), ("idle", None, None))
        e, _ = self.c.error("POST", FIT, 422, "invalid", body={"days": 0})
        self.assertIn("days", e["fields"])
        self.c.error("POST", FIT, 400, "bad_input", body={"other": 1})
        r = self.c.json("POST", FIT, {"days": 30}, expect=202)
        self.assertEqual((r["state"], r["days"], r["phase"]), ("running", 30, "positions"))
        # asked again while it runs: the same fit
        self.assertEqual(self.c.json("POST", FIT, {}, expect=202)["id"], r["id"])
        self.server.app.clock.advance(5_000)
        mid = self.c.get(FIT)
        self.assertEqual(mid["phase"], "logs")
        self.assertTrue(0 < mid["percent"] < 100 and mid["done"] <= mid["total"])
        self.assertTrue(mid["eta_ms"] is None or mid["eta_ms"] > 0)
        self.server.app.clock.advance(5_000)
        done = self.c.get(FIT)
        self.assertEqual((done["state"], done["percent"], done["applied"]), ("done", 100, False))
        res = done["result"]
        self.assertEqual((res["layout"], res["is_dbc"], res["good"]), (LAYOUT, False, True))
        self.assertIsNone(res["lon"]["dbc"]["median_m"], "over 10 km is only said so")
        for k in ("lat", "lon", "lat_deg", "lon_deg", "position"):
            self.assertNotIn(k + "_value", res)
        # Apply: the PUT; the fit then says it is in use, and the layout when it was set
        before = self.c.get(LAYOUT_URL)
        self.assertEqual((before["layout"], before["set_ms"]), (None, None))
        put = self.c.json("PUT", LAYOUT_URL, {"layout": res["layout"]})
        self.assertEqual(put["layout"], LAYOUT)
        now = self.c.get(LAYOUT_URL)
        self.assertEqual((now["layout"], now["set_ms"]), (LAYOUT, put["set_ms"]))
        self.assertIsInstance(now["set_ms"], int)
        # set again: nothing changes, not even when
        self.server.app.clock.advance(60_000)
        again = self.c.json("PUT", LAYOUT_URL, {"layout": LAYOUT})
        self.assertEqual((again["job"], again["set_ms"]), (None, put["set_ms"]))
        after = self.c.get(FIT)
        self.assertEqual((after["applied"], after["current"]), (True, LAYOUT))



class WatchAccount(ServerCase):
    server_args = {"role": "watch"}

    def test_admins_only(self):
        self.c.error("GET", FIT, 403, "not_allowed")
        self.c.error("POST", FIT, 403, "not_allowed", body={})
        # the layout in use is read by everyone; only an admin sets it
        self.assertEqual(self.c.get(LAYOUT_URL)["layout"], None)
        self.c.error("PUT", LAYOUT_URL, 403, "not_allowed", body={"layout": None})


class Disagree(ServerCase):
    server_args = {"gps_fit": "disagree"}

    def test_the_notice_and_no_fit_yet(self):
        h = self.c.get("/api/v1/cars/c1/health")
        self.assertIn("gps_inputs_disagree", h["reasons"])
        self.assertEqual(h["gps_disagreement"]["not_used"], ["smo"])
        self.assertEqual(self.c.get(FIT)["state"], "idle")


class Running(ServerCase):
    server_args = {"gps_fit": "running"}

    def test_a_fit_that_never_ends(self):
        r = self.c.get(FIT)
        # 272 of 22,402 logs read: not 30 % (the bar is weighed by the work, the logs are nearly all of it)
        self.assertEqual((r["state"], r["phase"], r["percent"]), ("running", "logs", 1))
        self.assertEqual((r["done"], r["total"], r["positions_total"], r["logs_total"]), (272, 22_402, 155, 22_402))
        # the time left from the speed so far: 272 logs in 55 s
        self.assertEqual(r["eta_ms"], int((22_402 - 272) * 55_000 / 272))
        self.server.app.clock.advance(3_600_000)
        self.assertEqual(self.c.get(FIT)["state"], "running")


class History(ServerCase):
    server_args = {"gps_fit": "history"}
    FITS = FIT + "s"

    def test_every_run_is_kept_listed_opened_applied_and_removed(self):
        page = self.c.get(self.FITS + "?limit=3")
        self.assertEqual((page["total"], len(page["items"])), (4, 3))
        rows = page["items"]
        self.assertEqual([r["days"] for r in rows], [14, 30, 7])
        self.assertEqual((rows[1]["applied"], rows[1]["applied_by"]), (True, "admin"))
        self.assertEqual((rows[2]["weak"], rows[2]["good"]), (True, True))
        rest = self.c.get(self.FITS + "?limit=3&after=" + page["next"])
        self.assertEqual(([r["good"] for r in rest["items"]], rest["next"]), ([False], None))
        for k in ("lat", "lon", "position"):
            self.assertNotIn(k, rows[0])
        one = self.c.get(f"{self.FITS}/{rows[1]['id']}")
        self.assertEqual((one["state"], one["result"]["layout"]), ("done", LAYOUT))
        self.c.error("GET", f"{self.FITS}/99", 404, "no_such_fit")
        # the layouts over time, newest first, who and from which run
        ch = self.c.get(LAYOUT_URL + "/changes")
        self.assertEqual((ch["total"], ch["items"][0]["by"], ch["items"][0]["fit"]), (2, "admin", rows[1]["id"]))
        self.assertEqual(ch["items"][1]["before"], None)
        # apply the newest: the password is asked again
        newest = rows[0]["id"]
        self.c.error("POST", f"{self.FITS}/{newest}/apply", 403, "password_needed", body={})
        self.c.error("POST", f"{self.FITS}/{newest}/apply", 403, "wrong_password", body={"confirm_password": "no"})
        self.c.json("POST", f"{self.FITS}/{newest}/apply", {"confirm_password": "sentry-mate-dev"})
        self.assertEqual(self.c.get(self.FITS)["items"][0]["applied_by"], "admin")
        # remove one: audited
        self.c.json("DELETE", f"{self.FITS}/{newest}", {})
        self.c.error("DELETE", f"{self.FITS}/{newest}", 404, "no_such_fit", body={})
        self.assertEqual(self.c.get(self.FITS)["total"], 3)
        audit = [e["action"] for e in self.c.get("/api/v1/audit?limit=20")["items"]]
        self.assertIn("car-gps-fit-removed", audit)

    def test_zz_a_run_started_from_a_page_is_one_more_row_and_a_running_one_cannot_be_removed(self):
        self.total_before = self.c.get(self.FITS)["total"]
        r = self.c.json("POST", FIT, {"days": 30}, expect=202)
        self.assertEqual(self.c.get(self.FITS)["items"][0]["state"], "running")
        self.c.error("DELETE", f"{self.FITS}/{r['id']}", 409, "fit_running", body={})
        self.server.app.clock.advance(10_000)
        self.assertEqual(self.c.get(self.FITS)["items"][0]["state"], "done")
        self.assertEqual(self.c.get(self.FITS)["total"], self.total_before + 1)


class WatchHistory(ServerCase):
    server_args = {"role": "watch", "gps_fit": "history"}

    def test_admins_only(self):
        for path in (FIT + "s", FIT + "s/1", LAYOUT_URL + "/changes"):
            self.c.error("GET", path, 403, "not_allowed")
        self.c.error("DELETE", FIT + "s/1", 403, "not_allowed", body={})
        self.c.error("POST", FIT + "s/1/apply", 403, "not_allowed", body={})


class Done(ServerCase):
    server_args = {"gps_fit": "done"}

    def test_a_fit_done_at_start(self):
        r = self.c.get(FIT)
        self.assertEqual((r["state"], r["result"]["layout"]), ("done", LAYOUT))
