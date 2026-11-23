"""The audit log: the seeded log covers every code of ui/shared/audit-codes.json with the item kind it names,
every row has a known group, the group= and who= filters apply before paging, and the writes the dev server
handles use the codes the real server writes."""

from __future__ import annotations

import json
import os

from .helpers import ROOT, ServerCase

CODES_FILE = os.path.normpath(os.path.join(ROOT, "..", "shared", "audit-codes.json"))
GROUPS = ("sign-ins", "accounts", "settings", "cars", "integrations", "imports")
KINDS = {"car", "drive", "charge", "update", "trip", "place", "import", "export", "move", "account", "session",
         "key", "token", "server", "settings", "tesla", "teslafi", "teslamate", "ui-pack"}


def codes():
    with open(CODES_FILE, encoding="utf-8") as f:
        return json.load(f)


class AuditCodes(ServerCase):
    def all_rows(self, query=""):
        return self.walk("/api/v1/audit" + (f"?{query}" if query else ""), limit=100)

    def test_code_list_is_well_formed(self):
        rows = codes()
        self.assertEqual([r["code"] for r in rows], sorted(r["code"] for r in rows))
        for r in rows:
            self.assertIn(r["group"], GROUPS, r)
            self.assertTrue(r["item"] is None or r["item"] in KINDS, r)

    def test_seeded_log_covers_every_code(self):
        rows = self.all_rows()
        self.assertGreaterEqual(len(rows), 60)  # enough for the pages' infinite scroll
        seen = {r["action"] for r in rows}
        missing = [c["code"] for c in codes() if c["code"] not in seen]
        self.assertEqual(missing, [], "codes the seeded log lacks")
        # spread over weeks, by several people and by the server itself
        self.assertGreater(rows[0]["at_ms"] - rows[-1]["at_ms"], 8 * 7 * 86_400_000)
        self.assertGreaterEqual(len({r["account"] for r in rows if r["account"]}), 4)
        self.assertTrue(any(r["account"] is None for r in rows))

    def test_rows_have_a_group_and_the_named_item(self):
        table = {c["code"]: c for c in codes()}
        for r in self.all_rows():
            self.assertEqual(set(r), {"at_ms", "account", "action", "group", "target", "car", "summary", "address"})
            want = table.get(r["action"])
            self.assertEqual(r["group"], want["group"] if want else "other", r)
            self.assertTrue(r["summary"] and "/api/" not in r["summary"], r)
            if r["address"] is not None:
                self.assertTrue(r["address"].startswith(("192.0.2.", "127.0.0.1")), r)
            if want is None:
                continue
            kind = r["target"].split(":")[0] if r["target"] else None
            self.assertEqual(kind, want["item"], r)

    def test_an_unknown_code_is_other(self):
        rows = [r for r in self.all_rows() if r["group"] == "other"]
        self.assertTrue(rows)
        self.assertTrue(all(r["action"] not in {c["code"] for c in codes()} for r in rows))

    def test_car_items_name_their_car(self):
        rows = self.all_rows()
        for r in rows:
            parts = (r["target"] or "").split(":")
            if parts[0] in ("drive", "charge", "update", "key") or parts[0] == "token" and len(parts) == 3:
                self.assertEqual(r["car"], parts[1], r)
            if parts[0] == "car":
                self.assertEqual(r["car"], parts[1], r)
        match = [r for r in rows if r["action"] == "teslamate-local-car-matched"]
        self.assertTrue(match and all(r["target"] == "teslamate:local" and r["car"] for r in match))
        self.assertIn("Matched TeslaMate car Comet to Comet.", [r["summary"] for r in match])

    def test_group_filter_before_paging(self):
        rows = self.all_rows()
        page = self.c.get("/api/v1/audit?group=integrations&limit=10")
        self.assertEqual(len(page["items"]), 10)
        self.assertTrue(all(r["group"] == "integrations" for r in page["items"]))
        self.assertIsNotNone(page["next"])
        walked = self.all_rows("group=integrations")
        self.assertEqual(walked, [r for r in rows if r["group"] == "integrations"])
        two = self.all_rows("group=sign-ins,imports")
        self.assertEqual(two, [r for r in rows if r["group"] in ("sign-ins", "imports")])
        self.assertEqual(self.all_rows("group=other"), [r for r in rows if r["group"] == "other"])
        self.c.error("GET", "/api/v1/audit?group=castles", 400, "bad_input")

    def test_who_filter(self):
        rows = self.all_rows()
        alex = self.all_rows("who=alex")
        self.assertTrue(alex)
        self.assertEqual(alex, [r for r in rows if r["account"] == "alex"])
        server = self.all_rows("who=*server")
        self.assertTrue(server)
        self.assertEqual(server, [r for r in rows if r["account"] is None])
        both = self.all_rows("who=admin&group=cars")
        self.assertEqual(both, [r for r in rows if r["account"] == "admin" and r["group"] == "cars"])
        self.assertEqual(self.c.get("/api/v1/audit?who=nobody")["items"], [])

    def test_paging_newest_first(self):
        rows = self.all_rows()
        self.assertEqual([r["at_ms"] for r in rows], sorted((r["at_ms"] for r in rows), reverse=True))
        first = self.c.get("/api/v1/audit?limit=10")
        self.assertEqual(first["items"], rows[:10])
        second = self.c.get(f"/api/v1/audit?limit=10&after={first['next']}")
        self.assertEqual(second["items"], rows[10:20])


class AuditWrites(ServerCase):
    def top(self):
        return self.c.get("/api/v1/audit?limit=1")["items"][0]

    def test_release_check(self):
        s = self.c.get("/api/v1/settings")
        self.c.json("PATCH", "/api/v1/settings", {"version": s["version"], "release_check": {"enabled": True}})
        self.c.json("POST", "/api/v1/release/check")
        top = self.top()
        self.assertEqual((top["action"], top["group"], top["target"]), ("release-checked", "settings", "server:release"))
        self.assertEqual(top["summary"], "Checked GitHub for a newer build.")

    def test_ui_pack_chosen(self):
        self.c.json("PATCH", "/api/v1/me/preferences", {"ui_pack": "horizon"})
        top = self.top()
        self.assertEqual((top["action"], top["target"]), ("ui-pack-chosen", "ui-pack:horizon"))
        self.assertEqual(top["summary"], "Chose the Horizon design.")
        self.c.json("PATCH", "/api/v1/me/preferences", {"ui_pack": None})
        self.c.json("PATCH", "/api/v1/me/preferences", {"interface_size": 120, "units": {"distance": "mi"}})
        top = self.top()
        self.assertEqual((top["action"], top["summary"]), ("preferences-changed", "Changed: text size, units."))

    def test_teslamate_car_matched(self):
        base = "/api/v1/integrations/teslamate"
        cars = self.c.get(base + "/cars")
        self.c.json("PUT", base + "/cars", {"version": cars["version"], "items": [
            {"teslamate_car": "1", "match": "c2"}]})
        top = self.top()
        self.assertEqual((top["action"], top["target"], top["car"]), ("teslamate-local-car-matched", "teslamate:local", "c2"))
        self.assertEqual(top["summary"], "Matched TeslaMate car Made-up Car to Maple.")

    def test_account_changes_have_their_own_codes(self):
        a = self.c.json("POST", "/api/v1/accounts", {"name": "casey", "password": "made-up-pass", "role": "watch",
                                                     "cars": ["c1"]}, expect=201)
        self.assertEqual(self.top()["action"], "account-created")
        a = self.c.json("PATCH", f"/api/v1/accounts/{a['id']}", {"version": a.get("version", 1), "role": "admin"})
        self.assertEqual((self.top()["action"], self.top()["target"]), ("role-changed", f"account:{a['id']}"))
        self.c.json("DELETE", f"/api/v1/accounts/{a['id']}")
        self.assertEqual(self.top()["action"], "account-removed")

    def test_every_write_has_a_known_group(self):
        rows = self.walk("/api/v1/audit", limit=200)
        self.assertEqual([r["action"] for r in rows if r["group"] == "other"], ["backup-made"])
