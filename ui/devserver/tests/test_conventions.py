"""Conventions every endpoint shares: paging, errors, access, CSRF, CSV, headers, static files."""

from __future__ import annotations

import json
import os
import unittest

from .helpers import NOW, PACK, ServerCase

CSP_START = "default-src 'self'; img-src 'self' data: blob: https://tile.openstreetmap.org"


class Paging(ServerCase):
    def test_walk_drives_has_no_duplicates_or_holes(self):
        a = NOW - 30 * 86_400_000
        path = f"/api/v1/cars/c1/drives?from_ms={a}"
        whole = self.c.get(path + "&limit=500")
        self.assertIsNone(whole["next"])
        walked = self.walk(path, limit=7)
        self.assertEqual([d["id"] for d in walked], [d["id"] for d in whole["items"]])
        self.assertEqual(len(walked), whole["summary"]["count"])
        starts = [d["start_ms"] for d in walked]
        self.assertEqual(starts, sorted(starts, reverse=True))

    def test_list_shape_and_limits(self):
        d = self.c.get("/api/v1/cars/c1/timeline?limit=3")
        self.assertEqual(set(d) >= {"items", "next", "car", "time_zone"}, True)
        self.assertEqual(len(d["items"]), 3)
        self.assertIsInstance(d["next"], str)
        self.assertEqual(len(self.c.get("/api/v1/cars/c1/timeline")["items"]), 50)  # default limit
        e, _ = self.c.error("GET", "/api/v1/cars/c1/timeline?limit=501", 400, "bad_input")
        self.assertIn("limit", e["fields"])
        e, _ = self.c.error("GET", "/api/v1/cars/c1/timeline?limit=x", 400, "bad_input")
        self.assertIn("limit", e["fields"])

    def test_cursor_is_tied_to_its_filters(self):
        d = self.c.get("/api/v1/cars/c1/drives?limit=2&sort=distance")
        e, _ = self.c.error("GET", f"/api/v1/cars/c1/drives?limit=2&sort=duration&after={d['next']}", 400,
                            "bad_input")
        self.assertIn("after", e["fields"])
        self.c.error("GET", "/api/v1/cars/c1/drives?after=not-a-cursor", 400, "bad_input")

    def test_period_filter(self):
        a, b = NOW - 3 * 86_400_000, NOW - 86_400_000
        d = self.c.get(f"/api/v1/cars/c1/timeline?from_ms={a}&to_ms={b}&limit=500")
        self.assertTrue(d["items"])
        self.assertTrue(all(a <= x["start_ms"] < b for x in d["items"]))
        e, _ = self.c.error("GET", f"/api/v1/cars/c1/timeline?from_ms={b}&to_ms={a}", 400, "bad_input")
        self.assertIn("to_ms", e["fields"])

    def test_ids_are_strings_and_pass_2_53(self):
        d = self.c.get("/api/v1/cars/c1/drives?limit=5")
        for x in d["items"]:
            self.assertIsInstance(x["id"], str)
            self.assertGreater(int(x["id"]), 2 ** 53)
            self.assertIsInstance(x["car"], str)
        ch = self.c.get("/api/v1/charges?limit=5")["items"]
        self.assertTrue(all(isinstance(x["id"], str) and int(x["id"]) > 2 ** 53 for x in ch))


class Errors(ServerCase):
    def test_error_shape(self):
        e, _ = self.c.error("GET", "/api/v1/cars/c1/drives?sort=nope", 400, "bad_input")
        self.assertEqual(set(e), {"code", "message", "fields"})
        self.assertIn("sort", e["fields"])

    def test_unknown_car_and_route(self):
        self.c.error("GET", "/api/v1/cars/c99/now", 404, "no_such_car")
        self.c.error("GET", "/api/v1/cars/zz/drives", 404, "no_such_car")
        self.c.error("GET", "/api/v1/nothing-here", 404, "not_found")
        self.c.error("GET", "/api/v1/cars/c1/drives/123", 404, "no_such_drive")
        self.c.error("GET", "/api/v1/cars/c1/charges/123", 404, "no_such_charge")

    def test_method_not_allowed(self):
        e, hd = self.c.error("POST", "/api/v1/cars/c1/now", 405, "method_not_allowed")
        self.assertIn("GET", hd["allow"])

    def test_csrf_header_is_required_for_changes(self):
        self.c.error("PATCH", "/api/v1/me/preferences", 403, "csrf", body={"theme": "dark"}, csrf=False)
        st, hd, raw = self.c.request("PATCH", "/api/v1/me/preferences", {"theme": "dark"},
                                     headers={"X-Sentry-Mate": "yes"}, csrf=False)
        self.assertEqual(st, 403)
        self.c.json("PATCH", "/api/v1/me/preferences", {"theme": "light"})

    def test_stale_version_is_409(self):
        car = self.c.get("/api/v1/cars/c2")
        e, _ = self.c.error("PATCH", "/api/v1/cars/c2", 409, "stale_version",
                            body={"version": car["version"] + 5, "name": "Other"})
        self.assertEqual(e["version"], car["version"])

    def test_422_with_fields(self):
        e, _ = self.c.error("PATCH", "/api/v1/me/preferences", 422, "invalid", body={"range": "far"})
        self.assertIn("range", e["fields"])

    def test_bad_json_is_400(self):
        e, _ = self.c.error("PATCH", "/api/v1/me/preferences", 400, "bad_input", body=b"{not json")
        self.assertIn("body", e["fields"])

    def test_every_section_is_built(self):
        self.c.get("/api/v1/cars/c1/battery")
        self.c.get("/api/v1/places")
        self.c.error("GET", "/api/v1/cars/c99/battery", 404, "no_such_car")


class Throttle(ServerCase):
    server_args = {"signed_out": True}

    def test_429_after_wrong_passwords(self):
        c = self.server.client()
        for _ in range(5):
            c.error("POST", "/api/v1/session", 401, "wrong_credentials", body={"name": "watch", "password": "nope"})
        e, hd = c.error("POST", "/api/v1/session", 429, "throttled", body={"name": "watch", "password": "nope"})
        self.assertGreaterEqual(int(hd["retry-after"]), 1)
        # an unknown name gets the same answer as a wrong password
        c.error("POST", "/api/v1/session", 401, "wrong_credentials", body={"name": "nobody", "password": "x" * 12})

    def test_signed_out_is_401(self):
        c = self.server.client()
        c.error("GET", "/api/v1/me", 401, "not_signed_in")
        c.error("GET", "/api/v1/cars", 401, "not_signed_in")
        self.assertEqual(c.get("/api/v1/setup")["needs_setup"], False)


class WatchRole(ServerCase):
    server_args = {"role": "watch"}

    def test_hidden_car_looks_unknown(self):
        hidden, _ = self.c.error("GET", "/api/v1/cars/c2/now", 404, "no_such_car")
        unknown, _ = self.c.error("GET", "/api/v1/cars/c77/now", 404, "no_such_car")
        self.assertEqual(hidden, unknown)
        self.c.error("GET", "/api/v1/cars/c2/drives", 404, "no_such_car")
        self.c.error("GET", "/api/v1/charges?cars=c1,c2", 404, "no_such_car")

    def test_sees_only_its_cars(self):
        cars = self.c.get("/api/v1/cars")
        self.assertEqual([x["id"] for x in cars["items"]], ["c1"])
        self.assertEqual(self.c.get("/api/v1/fleet/summary")["cars"], 1)
        ch = self.c.get("/api/v1/charges?limit=500")
        self.assertTrue(all(x["car"] == "c1" for x in ch["items"]))

    def test_admin_only_is_403(self):
        car = self.c.get("/api/v1/cars/c1")
        self.c.error("PATCH", "/api/v1/cars/c1", 403, "not_allowed", body={"version": car["version"], "name": "x"})
        self.c.error("GET", "/api/v1/accounts", 403, "not_allowed")
        self.c.error("GET", "/api/v1/audit", 403, "not_allowed")
        # own preferences are allowed
        p = self.c.get("/api/v1/me/preferences")
        self.assertEqual(p["units"]["distance"], "mi")
        self.c.json("PATCH", "/api/v1/me/preferences", {"interface_size": 125})


class Csv(ServerCase):
    def test_format_csv_and_accept_header(self):
        st, hd, raw = self.c.request("GET", "/api/v1/cars/c1/drives?format=csv&from_ms=%d" % (NOW - 7 * 86_400_000))
        self.assertEqual(st, 200)
        self.assertTrue(hd["content-type"].startswith("text/csv"))
        self.assertIn("attachment", hd["content-disposition"])
        text = raw.decode("utf-8-sig")
        lines = text.strip().splitlines()
        self.assertTrue(lines[0].startswith("id,car,time_zone,start_ms"))
        n = self.c.get("/api/v1/cars/c1/drives?from_ms=%d&limit=500" % (NOW - 7 * 86_400_000))["summary"]["count"]
        self.assertEqual(len(lines) - 1, n)  # every matching row, not one page
        st, hd, raw = self.c.request("GET", "/api/v1/cars/c1/timeline?limit=5", headers={"Accept": "text/csv"})
        self.assertTrue(hd["content-type"].startswith("text/csv"))

    def test_columns_as_csv(self):
        st, hd, raw = self.c.request("GET", "/api/v1/cars/c1/series?fields=soc_pct&format=csv")
        self.assertEqual(st, 200)
        head = raw.decode("utf-8-sig").splitlines()[0]
        self.assertTrue(head.startswith("t_ms,soc_pct"))

    def test_not_a_list(self):
        e, _ = self.c.error("GET", "/api/v1/cars/c1/now?format=csv", 400, "bad_input")
        self.assertIn("format", e["fields"])


class StaticAndHeaders(ServerCase):
    def test_spa_fallback_and_files(self):
        st, hd, raw = self.c.request("GET", "/cars/c1/drives")
        self.assertEqual(st, 200)
        self.assertIn(b"<html", raw[:200].lower())
        st, hd, raw2 = self.c.request("GET", "/")
        self.assertEqual(raw, raw2)
        js_name = sorted(f for f in os.listdir(os.path.join(PACK, "web")) if f.endswith(".js"))[0]  # boot.js, app.js...
        st, hd, js = self.c.request("GET", "/" + js_name)
        self.assertEqual(st, 200)
        self.assertTrue(hd["content-type"].startswith("text/javascript"))
        self.assertNotEqual(js, raw)  # the file itself, not the index.html fallback
        st, hd, raw3 = self.c.request("GET", "/../../../etc/passwd")
        self.assertEqual(raw3, raw)  # never outside the pack

    def test_security_headers(self):
        for path in ("/", "/api/v1/me", "/api/v1/nothing", "/boot.js"):
            st, hd, _ = self.c.request("GET", path)
            self.assertTrue(hd["content-security-policy"].startswith(CSP_START), path)
            self.assertIn("frame-ancestors 'none'", hd["content-security-policy"])
            self.assertEqual(hd["x-content-type-options"], "nosniff")
            # pages: their origin only, while the dataset's online tiles are on (test_system checks both modes)
            self.assertEqual(hd["referrer-policy"], "no-referrer" if path.startswith("/api/") else "strict-origin")
        st, hd, _ = self.c.request("GET", "/api/v1/me")
        self.assertEqual(hd["cache-control"], "no-store")
        # the virtual clock, for the pages' "now" (api.js reads it)
        self.assertLess(abs(int(hd["x-sentry-mate-now-ms"]) - self.server.app.clock.now()), 5000)

    def test_health(self):
        st, hd, raw = self.server.client().request("GET", "/health")
        self.assertEqual((st, raw), (200, b""))

    def test_first_page_load_signs_in(self):
        c = self.server.client()
        st, hd, _ = c.request("GET", "/")
        cookie = hd["set-cookie"]
        self.assertIn("HttpOnly", cookie)
        self.assertIn("SameSite=Strict", cookie)
        self.assertNotIn("Secure", cookie)
        self.assertEqual(c.get("/api/v1/me")["role"], "admin")


class Audit(ServerCase):
    def test_changes_are_audited(self):
        before = self.c.get("/api/v1/audit?limit=500")["items"]
        car = self.c.get("/api/v1/cars/c3")
        self.c.json("PATCH", "/api/v1/cars/c3", {"version": car["version"], "name": "Atlas II"})
        after = self.c.get("/api/v1/audit?limit=500")["items"]
        self.assertEqual(len(after), len(before) + 1)
        top = after[0]
        self.assertEqual((top["account"], top["action"], top["target"]), ("admin", "car.update", "car:c3"))
        self.assertIn("Atlas II", top["summary"])
        self.assertEqual(set(top), {"at_ms", "account", "action", "target", "car", "summary"})
        self.assertEqual(top["car"], "c3")

    def test_audit_rows_name_the_car(self):
        d = self.c.get("/api/v1/cars/c1/drives?limit=1")["items"][0]
        self.c.json("PUT", f"/api/v1/cars/c1/drives/{d['id']}/purpose", {"purpose": "business"})
        top = self.c.get("/api/v1/audit?limit=1")["items"][0]
        self.assertEqual((top["target"], top["car"]), (f"drive:{d['id']}", "c1"))
        self.c.json("DELETE", f"/api/v1/cars/c1/drives/{d['id']}/purpose")
        settings_rows = [e for e in self.c.get("/api/v1/audit?limit=500")["items"] if e["target"].startswith("account:")]
        self.assertTrue(settings_rows and all(e["car"] is None for e in settings_rows))

    def test_certificate_names_follow_the_running_certificate(self):
        before = self.c.get("/api/v1/network")["certificate"]["names"]
        s = self.c.get("/api/v1/settings")
        self.c.json("PATCH", "/api/v1/settings", {"version": s["version"], "host_names": s["host_names"] + ["nas.example"]})
        self.assertEqual(self.c.get("/api/v1/network")["certificate"]["names"], before)
        s = self.c.get("/api/v1/settings")
        self.c.json("PATCH", "/api/v1/settings", {"version": s["version"], "host_names": [n for n in s["host_names"] if n != "nas.example"]})


class ServeScript(unittest.TestCase):
    def test_prints_one_listening_line(self):
        import os
        import subprocess
        import sys
        from .helpers import PACK, ROOT
        p = subprocess.Popen([sys.executable, os.path.join(ROOT, "serve.py"), "--pack", PACK, "--port", "0"],
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            line = p.stdout.readline()
            self.assertRegex(line, r"^listening on http://127\.0\.0\.1:\d+/\n$")
            import http.client
            port = int(line.rsplit(":", 1)[1].strip("/\n"))
            c = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
            c.request("GET", "/api/v1/cars/c1/now")
            r = c.getresponse()
            self.assertEqual(r.status, 200)
            self.assertEqual(json.loads(r.read())["car"], "c1")
        finally:
            p.terminate()
            p.wait(10)
            p.stdout.close()
            p.stderr.close()

