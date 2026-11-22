"""Map tiles as the real server reports them: online (the dev default), a new server with no map file (every map
shows its "No map tiles yet" note), a made-up offline map file served tile by tile, the offline file choice, and the
person's map colours."""

from __future__ import annotations

from .helpers import ServerCase


class OnlineByDefault(ServerCase):
    def test_config(self):
        t = self.c.get("/api/v1/maps/config")["tiles"]
        self.assertEqual(t["mode"], "online")
        self.assertTrue(t["online_template"].startswith("https://tile.openstreetmap.org/"))
        self.assertEqual((t["offline_present"], t["offline_url"], t["offline_files"]), (False, None, []))
        self.c.error("GET", "/api/v1/maps/tiles/0/0/0", 404, "no_offline_map")

    def test_map_colours(self):
        self.assertEqual(self.c.get("/api/v1/me/preferences")["map_colors"], "dimmed")
        p = self.c.json("PATCH", "/api/v1/me/preferences", {"map_colors": "original"})
        self.assertEqual(p["map_colors"], "original")
        e, _ = self.c.error("PATCH", "/api/v1/me/preferences", 422, "invalid", {"map_colors": "inverted"})
        self.assertIn("map_colors", e["fields"])
        self.c.json("PATCH", "/api/v1/me/preferences", {"map_colors": "dimmed"})


class NoTiles(ServerCase):
    server_args = {"map_tiles": "none"}

    def test_a_new_server_has_nothing_to_draw(self):
        t = self.c.get("/api/v1/maps/config")["tiles"]
        self.assertEqual((t["mode"], t["offline_present"], t["offline_url"], t["online_template"]),
                         ("offline", False, None, None))
        self.c.error("GET", "/api/v1/maps/tiles/3/1/2", 404, "no_offline_map")

    def test_an_admin_turns_openstreetmap_on(self):
        # what the maps' "Use online map (OpenStreetMap)" button sends
        cfg = self.c.get("/api/v1/maps/config")["tiles"]
        osm = next(p for p in cfg["presets"] if p["id"] == "osm")
        v = self.c.get("/api/v1/settings")["version"]
        tiles = {"mode": "online", "online_url": osm["url"], "attribution": osm["attribution"],
                 "max_zoom": osm["max_zoom"]}
        self.c.json("PATCH", "/api/v1/settings", {"version": v, "tiles": tiles})
        t = self.c.get("/api/v1/maps/config")["tiles"]
        self.assertEqual((t["mode"], t["online_template"]), ("online", osm["url"]))
        self.c.json("PATCH", "/api/v1/settings", {"version": v + 1, "tiles": {"mode": "offline"}})


class OfflineFile(ServerCase):
    server_args = {"map_tiles": "offline"}

    def test_config_and_tiles(self):
        t = self.c.get("/api/v1/maps/config")["tiles"]
        self.assertEqual((t["offline_present"], t["offline_kind"], t["offline_file"]), (True, "raster", "bay-area.pmtiles"))
        self.assertEqual(t["offline_url"], "/api/v1/maps/tiles/{z}/{x}/{y}")
        self.assertEqual([f["name"] for f in t["offline_files"]], ["bay-area.pmtiles"])
        st, hd, raw = self.c.request("GET", "/api/v1/maps/tiles/13/1310/3166")
        self.assertEqual((st, hd["content-type"]), (200, "image/png"))
        self.assertTrue(raw.startswith(b"\x89PNG"))
        self.assertTrue(hd["cache-control"].startswith("private"))
        self.assertEqual(self.c.request("GET", "/api/v1/maps/tiles/18/1/1")[0], 204)
        self.c.error("GET", "/api/v1/maps/tiles/2/9/0", 404, "no_such_tile")

    def test_file_choice_is_checked(self):
        v = self.c.get("/api/v1/settings")["version"]
        e, _ = self.c.error("PATCH", "/api/v1/settings", 422, "invalid", {"version": v, "tiles": {"offline_file": "../x.pmtiles"}})
        self.assertIn("tiles.offline_file", e["fields"])
        s = self.c.json("PATCH", "/api/v1/settings", {"version": v, "tiles": {"offline_file": "bay-area.pmtiles"}})
        self.assertEqual(s["tiles"]["offline_file"], "bay-area.pmtiles")
        self.assertEqual(self.c.get("/api/v1/maps/config")["tiles"]["offline_choice"], "bay-area.pmtiles")
        self.c.json("PATCH", "/api/v1/settings", {"version": v + 1, "tiles": {"offline_file": None}})
