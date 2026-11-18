"""Import: the seeded import matches c3's imported history; a new upload goes chunk by chunk
(offset, length and SHA-256 checked), is checked, its cars are matched, it is imported, and its
report counts what was taken; one import at a time; a broken file fails its check."""

from __future__ import annotations

import hashlib

from .helpers import ServerCase


def chunk(data, offset, total, sha=None):
    return {"Upload-Offset": str(offset), "Upload-Length": str(total),
            "Upload-SHA256": sha or hashlib.sha256(data).hexdigest(),
            "Content-Type": "application/offset+octet-stream"}


class Imports(ServerCase):
    def test_1_seeded_import_matches_history(self):
        items = self.c.get("/api/v1/imports")["items"]
        self.assertEqual([x["id"] for x in items], ["im1"])
        im = self.c.get("/api/v1/imports/im1")
        self.assertEqual(im["status"], "done")
        tm = self.c.get("/api/v1/cars/c3/totals?source=teslamate")
        t = im["report"]["tables"]
        self.assertEqual(t["drives"]["imported"], tm["drives"])
        self.assertEqual(t["charging_processes"]["imported"], tm["charges"])
        self.assertEqual(t["updates"]["imported"], tm["updates"])
        for name, x in t.items():
            self.assertEqual(x["read"], x["imported"] + sum(x["skipped"].values()), name)
        first = self.c.get("/api/v1/cars/c3/drives?source=teslamate&sort=start&dir=asc&limit=1")["items"][0]
        self.assertLessEqual(im["report"]["span"]["from_ms"], first["start_ms"])
        self.assertEqual(self.c.get("/api/v1/imports/im1/cars")["items"][0]["match"], "c3")
        self.assertFalse(self.c.get("/api/v1/setup/status")["import_running"])

    def test_2_upload_check_match_import(self):
        c = self.c
        data = b"-- made-up TeslaMate backup\n" * 40
        n = len(data)
        e, _ = c.error("POST", "/api/v1/imports", 400, "bad_input", body={"name": "x.sql"})
        self.assertIn("bytes_total", e["fields"])
        c.error("POST", "/api/v1/imports", 422, "invalid", body={"name": "x.sql", "bytes_total": 0})
        c.error("POST", "/api/v1/imports", 422, "invalid", body={"name": "x.sql", "bytes_total": 9, "compression": "zip"})
        job = c.json("POST", "/api/v1/imports", {"name": "backup.sql.gz", "bytes_total": n}, expect=201)
        jid = job["id"]
        self.assertEqual((job["status"], job["compression"], job["upload_url"]),
                         ("uploading", "gzip", f"/api/v1/imports/{jid}"))
        c.error("POST", "/api/v1/imports", 409, "import_running", body={"name": "y.sql", "bytes_total": 5})
        self.assertTrue(c.get("/api/v1/setup/status")["import_running"])
        c.error("GET", f"/api/v1/imports/{jid}/cars", 409, "not_checked")
        c.error("POST", f"/api/v1/imports/{jid}/start", 409, "not_ready")
        url = f"/api/v1/imports/{jid}"
        a, b = data[:500], data[500:]
        e, hd = c.error("PATCH", url, 409, "wrong_offset", body=b, headers=chunk(b, 500, n))
        self.assertEqual((e["offset"], hd["upload-offset"]), (0, "0"))
        c.error("PATCH", url, 422, "checksum_mismatch", body=a, headers=chunk(a, 0, n, sha="00" * 32))
        c.error("PATCH", url, 422, "invalid", body=a, headers=chunk(a, 0, n + 1))
        e, _ = c.error("PATCH", url, 400, "bad_input", body=a, headers={"Upload-Offset": "0"})
        self.assertIn("Upload-SHA256", e["fields"])
        d = c.json("PATCH", url, a, headers=chunk(a, 0, n))
        self.assertEqual((d["offset"], d["status"]), (500, "uploading"))
        d = c.json("PATCH", url, b, headers=chunk(b, 500, n))
        self.assertEqual((d["status"], d["pass"]), ("checking", 1))
        c.error("PATCH", url, 409, "not_uploading", body=b, headers=chunk(b, n, n))
        cars = c.get(f"{url}/cars")["items"]
        self.assertEqual([x["tm_car"] for x in cars], [1, 2])
        self.assertEqual((cars[0]["suggested"], cars[0]["match"]), ("c3", "c3"))  # same VIN prefix
        self.assertIsNone(cars[1]["match"])
        self.server.app.clock.advance(3000)
        mid = c.get(url)
        self.assertEqual(mid["status"], "checking")
        self.assertTrue(0 < mid["bytes_read"] < n)
        self.server.app.clock.advance(3500)
        ready = c.get(url)
        self.assertEqual((ready["status"], ready["report"]["imported"]), ("ready", False))
        self.assertEqual(ready["cars_to_match"], 1)
        c.error("POST", f"{url}/start", 409, "cars_unmatched")
        e, _ = c.error("PUT", f"{url}/cars", 422, "invalid",
                       body={"items": [{"tm_car": 2, "match": "c3"}, {"tm_car": 9, "match": "new"}]})
        self.assertEqual(set(e["fields"]), {"items.0.match", "items.1.tm_car"})
        c.error("PUT", f"{url}/cars", 422, "invalid", body={"items": [{"tm_car": 2, "match": "c99"}]})
        c.error("PUT", f"{url}/cars", 400, "bad_input", body={"items": [{"tm_car": 2, "match": "new", "x": 1}]})
        got = c.json("PUT", f"{url}/cars", {"items": [{"tm_car": 2, "match": "leave_out"}]})
        self.assertEqual([x["match"] for x in got["items"]], ["c3", "leave_out"])
        started = c.json("POST", f"{url}/start")
        self.assertEqual((started["status"], started["pass"]), ("importing", 2))
        self.server.app.clock.advance(12_500)
        done = c.get(url)
        self.assertEqual(done["status"], "done")
        rep = done["report"]
        self.assertTrue(rep["imported"])
        self.assertEqual(rep["tables"]["drives"]["imported"], cars[0]["drives"])  # the left-out car is not counted
        self.assertEqual(rep["tables"]["cars"]["imported"], 1)
        self.assertEqual([x["id"] for x in c.get("/api/v1/imports")["items"]], [jid, "im1"])
        actions = [x["action"] for x in c.get("/api/v1/audit?limit=20")["items"]]
        for want in ("import.create", "import.uploaded", "import.cars", "import.start"):
            self.assertIn(want, actions)
        d = c.json("DELETE", url)
        self.assertEqual((d["deleted"], d["id"]), (True, jid))
        c.error("GET", url, 404, "no_such_import")

    def test_3_broken_file_fails(self):
        c = self.c
        data = b"not a backup"
        job = c.json("POST", "/api/v1/imports", {"name": "broken.sql", "bytes_total": len(data)}, expect=201)
        url = f"/api/v1/imports/{job['id']}"
        c.json("PATCH", url, data, headers=chunk(data, 0, len(data)))
        self.server.app.clock.advance(7000)
        d = c.get(url)
        self.assertEqual((d["status"], d["error"]["code"]), ("failed", "not_a_backup"))
        self.assertFalse(c.get("/api/v1/setup/status")["import_running"])
        c.error("POST", f"{url}/start", 409, "not_ready")
        c.json("DELETE", url)


class WatchImports(ServerCase):
    server_args = {"role": "watch"}

    def test_watch(self):
        self.c.error("GET", "/api/v1/imports", 403, "not_allowed")
        self.c.error("POST", "/api/v1/imports", 403, "not_allowed", body={"bytes_total": 4})
        self.c.error("GET", "/api/v1/imports/im1", 403, "not_allowed")
        self.c.error("GET", "/api/v1/imports/im1/cars", 403, "not_allowed")
        self.c.error("POST", "/api/v1/imports/im1/start", 403, "not_allowed")
