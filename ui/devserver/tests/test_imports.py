"""Import: the seeded import matches c3's imported history; a new upload goes chunk by chunk
(offset, length and SHA-256 checked), is checked, its cars are matched, it is imported, and its
report counts what was taken; one import at a time; a broken file fails its check."""

from __future__ import annotations

import hashlib
import io
import os
import zipfile

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
        self.assertIsNone(t["geofences"]["read"])  # a table the backup lacks is null, never 0
        for name, x in t.items():
            if x["read"] is not None:
                self.assertEqual(x["read"], x["imported"] + sum(x["skipped"].values()), name)
        for car in im["report"]["cars"]:  # per car too, and the cars add up to the tables (geofences belong to none)
            self.assertEqual(car["read"], car["imported"] + sum(car["skipped"].values()), car["tm_car"])
        self.assertEqual(sum(x["read"] for x in im["report"]["cars"]), sum(x["read"] or 0 for x in t.values()))
        self.assertEqual([(f["index"], f["upload_url"]) for f in im["files"]], [(0, "/api/v1/imports/im1/files/0")])
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
        c.error("POST", "/api/v1/imports", 422, "invalid", body={"name": "x.sql", "bytes_total": 9, "compression": "bzip2"})
        job = c.json("POST", "/api/v1/imports", {"name": "backup.sql.gz", "bytes_total": n}, expect=201)
        jid = job["id"]
        self.assertEqual((job["status"], job["compression"], job["upload_url"]),
                         ("uploading", "gzip", f"/api/v1/imports/{jid}/files/0"))
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
        self.assertEqual([x["tm_car"] for x in rep["cars"]], [1])  # the left-out car is not in the report
        self.assertEqual([x["match"] for x in c.get(f"{url}/cars")["items"]], ["c3", "leave_out"])  # still matchable
        self.assertEqual([x["id"] for x in c.get("/api/v1/imports")["items"]], [jid, "im1"])
        actions = [x["action"] for x in c.get("/api/v1/audit?limit=20")["items"]]
        for want in ("import-created", "import-uploaded", "import-cars", "import-started"):
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


    def test_4_can_export_zip(self):
        c = self.c
        data = b"PK\x03\x04" + b"made-up CAN export" * 50
        n = len(data)
        job = c.json("POST", "/api/v1/imports", {"name": "can-cbk_c90a7716-20260101T0000-to-20260201T0000.zip",
                                                 "bytes_total": n}, expect=201)
        self.assertEqual((job["compression"], job["kind"]), ("zip", None))
        url = f"/api/v1/imports/{job['id']}"
        c.json("PATCH", url, data, headers=chunk(data, 0, n))
        self.server.app.clock.advance(7000)
        ready = c.get(url)
        self.assertEqual((ready["status"], ready["kind"], ready["cars_to_match"]), ("ready", "sentry_manager_can", 0))
        rep = ready["report"]
        self.assertEqual((rep["kind"], rep["imported"]), ("sentry_manager_can", False))
        lg = rep["logs"]
        self.assertEqual(lg["read"], lg["to_import"] + lg["already_here"] + lg["refused"])
        self.assertEqual(lg["refused"], sum(rep["refused"].values()))
        self.assertEqual(len(rep["refused_logs"]), lg["refused"])
        self.assertEqual([x["status"] for x in rep["cars"]], ["car", "new_key"])
        self.assertEqual(c.get(f"{url}/cars")["items"], [])
        c.json("POST", f"{url}/start")
        self.server.app.clock.advance(12_500)
        done = c.get(url)
        self.assertEqual(done["status"], "done")
        lg = done["report"]["logs"]
        self.assertTrue(done["report"]["imported"])
        self.assertEqual(lg["read"], lg["imported"] + lg["already_here"] + lg["refused"])
        self.assertGreater(lg["imported"], 0)
        # the same export again: nothing new
        job = c.json("POST", "/api/v1/imports", {"name": "again.zip", "bytes_total": n}, expect=201)
        url = f"/api/v1/imports/{job['id']}"
        c.json("PATCH", url, data, headers=chunk(data, 0, n))
        self.server.app.clock.advance(7000)
        self.assertEqual(c.get(url)["report"]["logs"]["to_import"], 0)
        c.json("DELETE", url)
        # a zip that is not a CAN export, and a "zip" that is not one at all
        for name, body, code in (("photos.zip", data, "not_a_can_export"), ("x.zip", b"-- SQL\n", "unreadable")):
            job = c.json("POST", "/api/v1/imports", {"name": name, "bytes_total": len(body)}, expect=201)
            url = f"/api/v1/imports/{job['id']}"
            c.json("PATCH", url, body, headers=chunk(body, 0, len(body)))
            self.server.app.clock.advance(7000)
            d = c.get(url)
            self.assertEqual((d["status"], d["error"]["code"]), ("failed", code), name)


    def test_5_teslafi_data(self):
        c = self.c
        with open(os.path.join(os.path.dirname(__file__), "..", "mockapi", "data", "TeslaFi32023.csv"), "rb") as f:
            data = f.read()
        n = len(data)
        job = c.json("POST", "/api/v1/imports", {"name": "TeslaFi32023.csv", "bytes_total": n}, expect=201)
        url = f"/api/v1/imports/{job['id']}"
        half = n // 2
        c.json("PATCH", url, data[:half], headers=chunk(data[:half], 0, n))
        c.json("PATCH", url, data[half:], headers=chunk(data[half:], half, n))
        cars = c.get(f"{url}/cars")["items"]
        self.assertEqual([(x["name"], x["vin_prefix"], x["match"]) for x in cars], [("Blue Whale", "5YJ3E1EA4PF", None)])
        self.server.app.clock.advance(7000)
        ready = c.get(url)
        self.assertEqual((ready["status"], ready["kind"], ready["cars_to_match"]), ("ready", "teslafi", 1))
        rep = ready["report"]
        self.assertEqual((rep["kind"], rep["imported"], rep["files"]), ("teslafi", False, 1))
        self.assertEqual(rep["rows"]["read"], rep["rows"]["used"] + sum(rep["rows"]["skipped"].values()))
        self.assertEqual((rep["found"]["drives"], rep["found"]["charges"]), (2, 2))
        self.assertTrue(rep["time_zone"]["asked"])
        self.assertEqual(rep["units"]["distance"], "mi")
        c.error("POST", f"{url}/start", 409, "cars_unmatched")
        c.json("PUT", f"{url}/cars", {"items": [{"tm_car": 1, "match": "new"}]})
        for body in ({"time_zone": "Mars/Olympus"}, {"distance_unit": "parsec"}, {"temperature_unit": "k"}):
            c.error("POST", f"{url}/start", 422, "invalid", body=body)
        c.error("POST", f"{url}/start", 400, "bad_input", body={"speed": 1})
        started = c.json("POST", f"{url}/start", {"time_zone": "Europe/Berlin", "distance_unit": "km"})
        self.assertEqual(started["status"], "importing")
        self.server.app.clock.advance(12_500)
        done = c.get(url)["report"]
        self.assertTrue(done["imported"])
        self.assertEqual(done["tables"]["drives"]["imported"], 2)
        self.assertEqual(done["time_zone"]["name"], "Europe/Berlin")
        self.assertEqual((done["units"]["distance"], done["units"]["distance_from"]), ("km", "setting"))
        # the same file again: every row was imported already
        job = c.json("POST", "/api/v1/imports", {"name": "again.csv", "bytes_total": n}, expect=201)
        url = f"/api/v1/imports/{job['id']}"
        c.json("PATCH", url, data, headers=chunk(data, 0, n))
        self.server.app.clock.advance(7000)
        again = c.get(url)["report"]
        self.assertEqual(again["rows"]["used"], 0)
        self.assertEqual(again["rows"]["skipped"], {"already_imported": again["rows"]["read"]})
        c.json("DELETE", url)
        # TeslaFi's drives list is refused with why; a TeslaMate backup takes no time zone
        drives = b'Date,"Start Time","End Time","Driving Time","Kilometers Driven"\n2023-03-04,10:15,10:45,0:30,31\n'
        job = c.json("POST", "/api/v1/imports", {"name": "drives.csv", "bytes_total": len(drives)}, expect=201)
        url = f"/api/v1/imports/{job['id']}"
        c.json("PATCH", url, drives, headers=chunk(drives, 0, len(drives)))
        self.server.app.clock.advance(7000)
        d = c.get(url)
        self.assertEqual((d["status"], d["kind"], d["error"]["code"]), ("failed", "teslafi", "teslafi_summary"))
        c.json("DELETE", url)
        sql = b"-- made-up TeslaMate backup\n" * 4
        job = c.json("POST", "/api/v1/imports", {"name": "b.sql", "bytes_total": len(sql)}, expect=201)
        url = f"/api/v1/imports/{job['id']}"
        c.json("PATCH", url, sql, headers=chunk(sql, 0, len(sql)))
        self.server.app.clock.advance(7000)
        c.json("PUT", f"{url}/cars", {"items": [{"tm_car": 2, "match": "leave_out"}]})
        c.error("POST", f"{url}/start", 422, "invalid", body={"time_zone": "UTC"})
        c.json("DELETE", url)


TF_FILE = os.path.join(os.path.dirname(__file__), "..", "mockapi", "data", "TeslaFi32023.csv")


class TeslafiLikeTheServer(ServerCase):
    """The dev server takes TeslaFi's uploads and settings as the server does: a zip's folders and other files passed
    over, any IANA time zone, the unit words the server takes, and no memory of a car that was left out."""

    def upload(self, name, data):
        c = self.c
        job = c.json("POST", "/api/v1/imports", {"name": name, "bytes_total": len(data)}, expect=201)
        url = f"/api/v1/imports/{job['id']}"
        c.json("PATCH", url, data, headers=chunk(data, 0, len(data)))
        self.server.app.clock.advance(7000)
        return url

    def run_it(self, url, match, body):
        self.c.json("PUT", f"{url}/cars", {"items": [{"tm_car": 1, "match": match}]})
        self.c.json("POST", f"{url}/start", body)
        self.server.app.clock.advance(12_500)
        done = self.c.get(url)
        self.assertEqual(done["status"], "done")
        return done["report"]

    def test_1_zips_as_the_server_reads_them(self):
        from mockapi.resources import imports
        with open(TF_FILE, "rb") as f:
            data = f.read()

        def zipped(*entries):
            b = io.BytesIO()
            with zipfile.ZipFile(b, "w", zipfile.ZIP_DEFLATED) as z:
                for n, d in entries:
                    z.writestr(n, d)
            return b.getvalue()

        for first in (("TeslaFi/", b""), ("readme.txt", b"hello"), ("other.csv", b"a,b\n1,2\n"),
                      ("__MACOSX/._TeslaFi32023.csv", b"\0\x05")):
            z = zipped(first, ("TeslaFi/TeslaFi32023.csv", data))
            self.assertEqual(imports.teslafi_sniff(z), "raw", first[0])
        self.assertIsNone(imports.teslafi_sniff(zipped(("key1/a.log.xz", b"x"), ("TeslaFi32023.csv", data))))
        self.assertIsNone(imports.teslafi_sniff(zipped(("readme.txt", b"hello"))))
        url = self.upload("teslafi.zip", zipped(("TeslaFi/", b""), ("TeslaFi/TeslaFi32023.csv", data)))
        j = self.c.get(url)
        self.assertEqual((j["status"], j["kind"]), ("ready", "teslafi"))
        self.c.json("DELETE", url)

    def test_2_settings_and_a_car_left_out(self):
        c = self.c
        with open(TF_FILE, "rb") as f:
            data = f.read()
        url = self.upload("TeslaFi32023.csv", data)
        rows = c.get(url)["report"]["rows"]["read"]
        c.json("PUT", f"{url}/cars", {"items": [{"tm_car": 1, "match": "new"}]})
        for body in ({"time_zone": "Mars/Olympus"}, {"time_zone": 5}, {"distance_unit": "parsec"},
                     {"temperature_unit": "kelvin"}):
            c.error("POST", f"{url}/start", 422, "invalid", body=body)
        # left out: the server keeps nothing for that car, so the same file again is all new
        rep = self.run_it(url, "leave_out", {})
        self.assertEqual(rep["rows"]["used"], 0)
        url = self.upload("TeslaFi32023.csv", data)
        again = c.get(url)["report"]
        self.assertEqual((again["rows"]["used"], again["rows"]["skipped"]), (rows, {}))
        # any IANA zone, and the unit words the server takes
        rep = self.run_it(url, "new", {"time_zone": "America/Phoenix", "distance_unit": "Miles ",
                                       "temperature_unit": "Celsius"})
        self.assertEqual(rep["time_zone"]["name"], "America/Phoenix")
        self.assertEqual((rep["units"]["distance"], rep["units"]["distance_from"]), ("mi", "setting"))
        self.assertEqual(rep["units"]["temperature"], "c")
        self.assertEqual(rep["found"]["read_again"], 0)
        # then in the same zone: nothing new; in another: read again, its earlier rows replaced
        url = self.upload("TeslaFi32023.csv", data)
        self.assertEqual(c.get(url)["report"]["rows"]["used"], 0)
        same = self.run_it(url, "new", {"time_zone": "America/Phoenix", "distance_unit": "mi"})
        self.assertEqual(same["rows"]["skipped"], {"already_imported": rows})
        url = self.upload("TeslaFi32023.csv", data)
        other = self.run_it(url, "new", {"time_zone": "Europe/Berlin"})
        self.assertEqual((other["rows"]["used"], other["found"]["read_again"]), (rows, 1))
        self.assertIn("in America/Phoenix were read again", other["warnings"][0])


class MultiFile(ServerCase):
    """Several files make one import: TeslaFi's months only."""

    def send(self, job, files):
        for f, data in zip(job["files"], files):
            half = len(data) // 2
            self.c.json("PATCH", f["upload_url"], data[:half], headers=chunk(data[:half], 0, len(data)))
            d = self.c.json("PATCH", f["upload_url"], data[half:], headers=chunk(data[half:], half, len(data)))
        return d

    def test_1_teslafi_months(self):
        c = self.c
        with open(TF_FILE, "rb") as f:
            data = f.read()
        n = len(data)
        c.error("POST", "/api/v1/imports", 400, "bad_input", body={"files": []})
        c.error("POST", "/api/v1/imports", 422, "invalid", body={"files": [{"name": "a.csv", "bytes_total": 0}]})
        names = ["TeslaFi12023.csv", "TeslaFi22023.csv", "TeslaFi32023.csv"]
        job = c.json("POST", "/api/v1/imports", {"files": [{"name": x, "bytes_total": n} for x in names]}, expect=201)
        jid = job["id"]
        self.assertEqual((job["name"], job["bytes_total"]), ("TeslaFi12023.csv and 2 more files", 3 * n))
        self.assertEqual([f["upload_url"] for f in job["files"]], [f"/api/v1/imports/{jid}/files/{i}" for i in range(3)])
        self.assertEqual(job["upload_url"], job["files"][0]["upload_url"])
        c.error("PATCH", f"/api/v1/imports/{jid}/files/3", 404, "no_such_file", body=data, headers=chunk(data, 0, n))
        # offsets are per file: file 1 starts at 0 while file 0 is half sent
        c.json("PATCH", job["files"][0]["upload_url"], data[:n // 2], headers=chunk(data[:n // 2], 0, n))
        d = c.json("PATCH", job["files"][1]["upload_url"], data[:n // 2], headers=chunk(data[:n // 2], 0, n))
        self.assertEqual((d["offset"], d["uploaded_bytes"], d["status"]), (n // 2, 2 * (n // 2), "uploading"))
        c.json("PATCH", job["files"][0]["upload_url"], data[n // 2:], headers=chunk(data[n // 2:], n // 2, n))
        c.json("PATCH", job["files"][1]["upload_url"], data[n // 2:], headers=chunk(data[n // 2:], n // 2, n))
        d = c.json("PATCH", job["files"][2]["upload_url"], data, headers=chunk(data, 0, n))
        self.assertEqual(d["status"], "checking")
        self.assertEqual([f["uploaded_bytes"] for f in d["files"]], [n, n, n])
        self.server.app.clock.advance(7000)
        ready = c.get(f"/api/v1/imports/{jid}")
        self.assertEqual((ready["status"], ready["kind"]), ("ready", "teslafi"))
        rep = ready["report"]
        self.assertEqual([u["name"] for u in rep["uploads"]], names)
        self.assertEqual(rep["files"], 3)
        self.assertEqual(rep["rows"]["read"], sum(u["rows_read"] for u in rep["uploads"]))
        # left out: gone from the report and its counts, still in the matching list
        c.json("PUT", f"/api/v1/imports/{jid}/cars", {"items": [{"tm_car": 1, "match": "leave_out"}]})
        rep = c.get(f"/api/v1/imports/{jid}")["report"]
        self.assertEqual((rep["cars"], rep["cars_count"], rep["rows"]["read"]), ([], 0, 0))
        self.assertEqual(c.get(f"/api/v1/imports/{jid}/cars")["items"][0]["match"], "leave_out")
        c.json("DELETE", f"/api/v1/imports/{jid}")

    def test_2_one_file_only(self):
        c = self.c
        sql = b"-- made-up TeslaMate backup\n" * 4
        job = c.json("POST", "/api/v1/imports", {"files": [{"name": "a.sql", "bytes_total": len(sql)},
                                                           {"name": "b.sql", "bytes_total": len(sql)}]}, expect=201)
        self.send(job, [sql, sql])
        self.server.app.clock.advance(7000)
        d = c.get(f"/api/v1/imports/{job['id']}")
        self.assertEqual((d["status"], d["error"]["code"]), ("failed", "one_file_only"))
        c.json("DELETE", f"/api/v1/imports/{job['id']}")
        with open(TF_FILE, "rb") as f:
            data = f.read()
        job = c.json("POST", "/api/v1/imports", {"files": [{"name": "TeslaFi32023.csv", "bytes_total": len(data)},
                                                           {"name": "b.sql", "bytes_total": len(sql)}]}, expect=201)
        self.send(job, [data, sql])
        self.server.app.clock.advance(7000)
        d = c.get(f"/api/v1/imports/{job['id']}")
        self.assertEqual((d["error"]["code"], d["kind"]), ("not_a_teslafi_export", "teslafi"))
        self.assertIn("b.sql", d["error"]["message"])


class WatchImports(ServerCase):
    server_args = {"role": "watch"}

    def test_watch(self):
        self.c.error("GET", "/api/v1/imports", 403, "not_allowed")
        self.c.error("POST", "/api/v1/imports", 403, "not_allowed", body={"bytes_total": 4})
        self.c.error("GET", "/api/v1/imports/im1", 403, "not_allowed")
        self.c.error("GET", "/api/v1/imports/im1/cars", 403, "not_allowed")
        self.c.error("POST", "/api/v1/imports/im1/start", 403, "not_allowed")
