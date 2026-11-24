"""Supercharger invoices (docs/ui-api.md, "Integrations: Supercharger invoices"): the made-up sessions matched to
Supercharger charges, the cost they give a charge (an entered cost wins), the PDFs, the zip, the list and its CSV,
the blocks of /integrations/tesla and /teslafi and their PATCH with the password asked again."""

from __future__ import annotations

import io
import zipfile

from .helpers import NOW, ServerCase

DAY = 86_400_000
PW = "sentry-mate-dev"
BASE = "/api/v1/integrations/invoices"


class Invoices(ServerCase):
    def all_sessions(self, query=""):
        return self.walk(BASE + (f"?{query}" if query else ""), limit=100)

    def matched_c1(self):
        return next(s for s in self.all_sessions("matched=true") if s["car"] == "c1" and s["pdf"])

    def test_sessions_are_made_up_and_deterministic(self):
        rows = self.all_sessions()
        self.assertGreater(len(rows), 20)
        self.assertEqual([r["start_ms"] for r in rows], sorted((r["start_ms"] for r in rows), reverse=True))
        for r in rows:
            self.assertRegex(r["id"], r"^tesla-10\d{5}$")
            self.assertRegex(r["site"], r"^(Made-up|Sample) Supercharger, ")
            self.assertEqual(r["source"], "tesla")
            self.assertAlmostEqual(r["total"], round(sum(f["amount"] for f in r["fees"]), 2), places=2)
            self.assertIn(r["fees"][0]["kind"], ("charging", "idle", "other"))
        unmatched = [r for r in rows if r["charge"] is None]
        self.assertEqual(len(unmatched), 2)
        self.assertTrue(all(r["car"] is None for r in unmatched))
        self.assertTrue(any(r["idle_fee"] > 0 for r in rows))
        self.assertTrue(any(not r["pdf"] for r in rows))
        self.assertIn("CAD", {r["currency"] for r in rows})  # Maple's trip to Ontario
        page = self.c.get(BASE + "?limit=5")
        self.assertEqual(page["totals"]["sessions"], len(rows))
        self.assertEqual(page["totals"]["matched"] + page["totals"]["unmatched"], len(rows))
        self.assertEqual(self.c.get(BASE + "?matched=false")["totals"]["sessions"], 2)
        self.assertEqual(self.c.get(BASE + "?source=teslafi")["items"], [])

    def test_period_filter(self):
        rows = self.all_sessions()
        a, b = NOW - 30 * DAY, NOW
        some = self.all_sessions(f"from_ms={a}&to_ms={b}")
        self.assertEqual(some, [r for r in rows if a <= r["start_ms"] < b])

    def test_charge_takes_the_invoice_cost_unless_entered(self):
        s = self.matched_c1()
        base = f"/api/v1/cars/c1/charges/{s['charge']}"
        d = self.c.get(base)
        self.assertEqual(d["cost"], {"amount": s["total"], "currency": s["currency"], "source": "tesla_invoice"})
        self.assertEqual(d["cost_breakdown"]["mode"], "tesla_invoice")
        self.assertEqual(d["cost_breakdown"]["parts"][0]["rate"], "Charging")
        self.assertEqual(set(d["cost_breakdown"]["parts"][0]),
                         {"rate", "per_kwh", "per_minute", "kwh", "minutes", "cost", "from_kwh", "to_kwh"})
        self.assertEqual(len(d["invoices"]), 1)
        ref = d["invoices"][0]
        self.assertTrue(ref["applied"])
        self.assertEqual(ref["id"], s["id"])
        self.assertNotIn("charge", ref)
        # the Charges list says the same
        row = next(x for x in self.walk("/api/v1/cars/c1/charges", limit=200) if x["id"] == s["charge"])
        self.assertEqual(row["cost"]["source"], "tesla_invoice")
        # an entered cost wins
        up = self.c.json("PUT", base + "/cost", {"mode": "total", "amount": 9.5, "currency": "USD"})
        self.assertEqual(up["cost"]["source"], "manual")
        self.assertFalse(up["invoices"][0]["applied"])
        back = self.c.json("DELETE", base + "/cost")
        self.assertEqual(back["cost"]["source"], "tesla_invoice")
        # a charge with no invoice has an empty list
        ac = self.c.get("/api/v1/cars/c1/charges?kind=ac&limit=1")["items"][0]
        self.assertEqual(self.c.get(f"/api/v1/cars/c1/charges/{ac['id']}")["invoices"], [])

    def test_pdfs(self):
        s = self.matched_c1()
        for path in (f"/api/v1/cars/c1/charges/{s['charge']}/invoices/{s['id']}/pdf", f"{BASE}/{s['id']}/pdf"):
            st, hd, raw = self.c.request("GET", path)
            self.assertEqual(st, 200, path)
            self.assertEqual(hd["content-type"], "application/pdf")
            self.assertIn("attachment", hd["content-disposition"])
            self.assertTrue(raw.startswith(b"%PDF-1.4"))
        self.c.error("GET", f"/api/v1/cars/c2/charges/{s['charge']}/invoices/{s['id']}/pdf", 404, "no_such_charge")
        self.c.error("GET", f"{BASE}/tesla-9/pdf", 404, "no_such_invoice")
        waiting = next(x for x in self.all_sessions() if not x["pdf"])
        self.c.error("GET", f"{BASE}/{waiting['id']}/pdf", 404, "no_pdf")

    def test_zip_and_csv(self):
        st, hd, raw = self.c.request("GET", BASE + "/zip?source=tesla")
        self.assertEqual(st, 200)
        self.assertEqual(hd["content-type"], "application/zip")
        self.assertIn('filename="supercharger-invoices.zip"', hd["content-disposition"])
        names = zipfile.ZipFile(io.BytesIO(raw)).namelist()
        self.assertEqual(len(names), sum(1 for s in self.all_sessions() if s["pdf"]))
        self.assertTrue(all(n.endswith(".pdf") and n[:4].isdigit() and "_tesla-10" in n for n in names), names[:3])
        top = self.c.get("/api/v1/audit?limit=1")["items"][0]
        self.assertEqual(top["action"], "invoices-downloaded")
        self.assertIsNone(top["target"])
        self.c.error("GET", BASE + f"/zip?from_ms={NOW - 400 * DAY}&to_ms={NOW - 399 * DAY}", 404, "no_invoices")
        st, hd, raw = self.c.request("GET", BASE + f"?source=tesla&from_ms={NOW - 30 * DAY}&to_ms={NOW}&format=csv")
        self.assertEqual(st, 200)
        self.assertIn("text/csv", hd["content-type"])
        head = raw.decode("utf-8-sig").splitlines()[0].split(",")
        for col in ("id", "site", "start_ms", "total", "currency", "car", "charge"):
            self.assertIn(col, head)

    def test_blocks(self):
        t = self.c.get("/api/v1/integrations/tesla")["invoices"]
        self.assertEqual(t["enabled"], True)
        self.assertTrue(t["available"] and t["gives_pdfs"])
        self.assertIsNone(t["note"])
        self.assertEqual(t["sessions"], t["matched"] + t["unmatched"])
        self.assertEqual(t["sessions"], t["pdfs"] + t["pdfs_waiting"])
        self.assertEqual(t["backfill"], "done")
        self.assertGreater(t["next_check_ms"], t["last_check_ms"])
        f = self.c.get("/api/v1/integrations/teslafi")["invoices"]
        self.assertEqual((f["enabled"], f["gives_pdfs"], f["sessions"]), (False, False, 0))
        self.assertEqual(f["note"], "TeslaFi's API gives no invoices: only the costs TeslaFi lists for its charges are taken.")
        self.assertIsNone(f["next_check_ms"])

    def test_patch_asks_the_password(self):
        for src in ("tesla", "teslafi"):
            path = f"/api/v1/integrations/{src}"
            o = self.c.get(path)
            self.c.error("PATCH", path, 403, "password_needed", body={"version": o["version"], "invoices": not o["invoices"]["enabled"]})
            self.c.error("PATCH", path, 403, "wrong_password",
                         body={"version": o["version"], "invoices": True, "confirm_password": "nope"})
            self.c.error("PATCH", path, 400, "bad_input",
                         body={"version": o["version"], "invoices": "yes", "confirm_password": PW})
            want = not o["invoices"]["enabled"]
            n = self.c.json("PATCH", path, {"version": o["version"], "invoices": want, "confirm_password": PW})
            self.assertEqual(n["invoices"]["enabled"], want)
            self.assertEqual(n["version"], o["version"] + 1)
            top = self.c.get("/api/v1/audit?limit=1")["items"][0]
            self.assertEqual((top["action"], top["target"]), (f"{src}-invoices-changed", f"settings:{src}-invoices"))
            # with the poll in the same PATCH, and the stale version refused
            self.c.error("PATCH", path, 409, "stale_version", body={"version": o["version"], "invoices": not want, "confirm_password": PW})
            poll = {"idle_ms": 300_000} if src == "tesla" else "gentle"
            m = self.c.json("PATCH", path, {"version": n["version"], "invoices": not want, "poll": poll,
                                            "confirm_password": PW})
            got = m["poll"]["idle_ms"] if src == "tesla" else m["poll"]
            self.assertEqual((m["invoices"]["enabled"], got), (not want, 300_000 if src == "tesla" else "gentle"))


class WatchSeesNone(ServerCase):
    server_args = {"role": "watch"}

    def test_admin_only(self):
        for path in (BASE, BASE + "/zip", BASE + "/tesla-1000101/pdf"):
            st, _, _ = self.c.request("GET", path)
            self.assertEqual(st, 403, path)


class InvoicePrices(ServerCase):
    """A place's prices from its Supercharger invoices: dated, marked "invoice", the median per kWh billed; the price
    in force at Supercharger Schaumburg (no typed price) is one, and the Charging places list says so."""

    def test_a_place_without_a_typed_price_is_priced_from_its_invoices(self):
        p = self.c.get("/api/v1/places/p36")
        self.assertEqual(p["price_source"], "invoice")
        inv = [x for x in p["prices"] if x["source"] == "invoice"]
        self.assertEqual(len(inv), len(p["prices"]))
        self.assertEqual(p["price"], inv[-1]["price"])
        self.assertEqual(inv[-1]["price"]["currency"], "USD")
        self.assertGreater(inv[-1]["invoice"]["sessions"], 0)
        # a rate some session was billed at (the median is always one)
        rates = {f["rate"] for s in self.walk(BASE, limit=100) if s["charge"] for f in s["fees"] if f["kind"] == "charging"}
        self.assertIn(inv[-1]["price"]["per_kwh"], rates)

    def test_a_typed_price_wins_and_says_so(self):
        p = self.c.get("/api/v1/places/p3")
        self.assertEqual(p["price_source"], "typed")
        self.assertTrue(any(x["source"] == "invoice" for x in p["prices"]))
        self.assertIsNone(self.c.get("/api/v1/places/p2")["price_source"])

    def test_the_charging_places_list_gives_the_place_price_and_who_set_it(self):
        rows = self.walk(f"/api/v1/charging-stats/places?cars=c2&from_ms={NOW - 400 * DAY}&to_ms={NOW}", limit=100)
        by = {x["place"]: x for x in rows if x["place"]}
        self.assertEqual(by["p36"]["price_source"], "invoice")
        self.assertEqual(by["p36"]["place_price"]["currency"], "USD")
        for x in rows:
            self.assertIn("place_price", x)
            if x["place"] is None:
                self.assertIsNone(x["price_source"])

    def test_the_audit_log_has_the_change(self):
        rows = self.walk("/api/v1/audit", limit=100)
        hit = [r for r in rows if r["action"] == "place-invoice-priced"]
        self.assertTrue(hit)
        self.assertEqual(hit[0]["target"], "place:p36")
