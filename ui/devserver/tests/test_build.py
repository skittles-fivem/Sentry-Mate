"""Which build the dev server runs (X-Sentry-Mate-Build on every answer, as the real server) and the stand-in for a
server update (POST /__dev/new-build). Files under a build's address give the file of that path now."""

from __future__ import annotations

from .helpers import ServerCase


class Build(ServerCase):
    def test_every_answer_names_the_build_and_an_update_changes_it(self):
        c = self.c
        builds = set()
        for path in ("/", "/health", "/api/v1/me", "/core/router.js", "/cars/c1/health"):
            st, hd, _ = c.request("GET", path)
            self.assertEqual(st, 200, path)
            builds.add(hd.get("x-sentry-mate-build"))
        self.assertEqual(len(builds), 1, builds)
        before = builds.pop()
        self.assertTrue(before)

        self.assertEqual(c.request("GET", "/__dev/new-build")[0], 405)
        after = c.json("POST", "/__dev/new-build")["build"]
        self.assertNotEqual(after, before)
        for path in ("/health", "/api/v1/me", "/"):
            self.assertEqual(c.request("GET", path)[1].get("x-sentry-mate-build"), after, path)

    def test_a_file_under_an_old_build_is_the_file_now(self):
        c = self.c
        st, hd, plain = c.request("GET", "/pages/health.js")
        self.assertEqual(st, 200)
        st, hd, old = c.request("GET", "/_v/0123456789abcdef/pages/health.js")
        self.assertEqual((st, old), (200, plain))
        self.assertEqual(hd.get("cache-control"), "no-cache")
        self.assertIn("javascript", hd.get("content-type"))
