"""The dev server's settings PATCH keeps the real server's rules (crates/web/src/resources/settings.rs). Made-up data."""

from __future__ import annotations

from .helpers import ServerCase


class SettingsRules(ServerCase):
    def patch(self, body, expect=None):
        v = self.c.get("/api/v1/settings")["version"]
        st, _, raw = self.c.request("PATCH", "/api/v1/settings", {"version": v, **body})
        return st, raw

    def test_language_characters_refused(self):
        st, raw = self.patch({"geocode": {"language": "e n;<"}})
        self.assertEqual(st, 422, raw)

    def test_language_trimmed(self):
        st, raw = self.patch({"geocode": {"language": "  de  "}})
        self.assertEqual(st, 200, raw)
        self.assertEqual(self.c.get("/api/v1/settings")["geocode"]["language"], "de")

    def test_attribution_at_most_300(self):
        st, raw = self.patch({"tiles": {"attribution": "x" * 301}})
        self.assertEqual(st, 422, raw)

    def test_car_order_unknown_message(self):
        st, raw = self.patch({"car_order": ["c999999"]})
        self.assertEqual(st, 422, raw)
        self.assertIn(b"No such car.", raw)
