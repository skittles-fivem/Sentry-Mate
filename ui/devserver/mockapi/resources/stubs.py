"""Routes of docs/ui-api.md that are not built yet: they answer 501 { error: { code: "not_built" } }
after the usual sign-in, role and car checks, so pages can already handle them.

To build one: implement it in a resource module (see drives.py), register it there, and remove its
line here. A real registration replaces the stub even if the line stays.
"""

from __future__ import annotations

from ..errors import not_built

# (pattern, methods, admin methods)
STUBS = [
    # Battery and efficiency
    ("/api/v1/cars/{car}/battery", "GET", ""),
    ("/api/v1/cars/{car}/battery/capacity", "GET", ""),
    ("/api/v1/cars/{car}/battery-level", "GET", ""),
    ("/api/v1/cars/{car}/projected-range", "GET", ""),
    ("/api/v1/cars/{car}/parked", "GET", ""),
    ("/api/v1/cars/{car}/efficiency", "GET", ""),
    ("/api/v1/cars/{car}/temperatures", "GET", ""),
    # Reports
    ("/api/v1/statistics", "GET", ""),
    ("/api/v1/cars/{car}/statistics", "GET", ""),
    ("/api/v1/cars/{car}/mileage", "GET", ""),
    ("/api/v1/cars/{car}/totals", "GET", ""),
    ("/api/v1/cars/{car}/gaps", "GET", ""),
    ("/api/v1/compare", "GET", ""),
    ("/api/v1/visits", "GET", ""),
    ("/api/v1/visits/summary", "GET", ""),
    ("/api/v1/visits/places", "GET", ""),
    ("/api/v1/visits/cities", "GET", ""),
    ("/api/v1/visits/regions", "GET", ""),
    ("/api/v1/visits/countries", "GET", ""),
    ("/api/v1/visits/days", "GET", ""),
    ("/api/v1/tracks", "GET", ""),
    ("/api/v1/cars/{car}/updates", "GET", ""),
    ("/api/v1/cars/{car}/updates/{update}", "PATCH", "PATCH"),
    ("/api/v1/fleet/versions", "GET", ""),
    ("/api/v1/cars/{car}/events", "GET", ""),
    # Places
    ("/api/v1/places", "GET POST", "POST"),
    ("/api/v1/places/{place}", "GET PATCH DELETE", "PATCH DELETE"),
    ("/api/v1/places/preview", "POST", ""),
    ("/api/v1/places/{place}/reprice", "POST", "POST"),
    ("/api/v1/places/suggestions", "GET", ""),
    ("/api/v1/places/at", "GET", ""),
    # Integrations
    ("/api/v1/settings/mqtt", "GET PUT", "PUT"),
    ("/api/v1/settings/mqtt/status", "GET", ""),
    ("/api/v1/settings/mqtt/preview", "GET", ""),
    ("/api/v1/settings/mqtt/test", "POST", "POST"),
    ("/api/v1/network", "GET", "GET"),
    # Server, settings and health
    ("/api/v1/settings", "GET PATCH", "PATCH"),
    ("/api/v1/maps/config", "GET", ""),
    ("/api/v1/geocode/status", "GET", ""),
    ("/api/v1/notices", "GET", ""),
    ("/api/v1/about", "GET", ""),
    ("/api/v1/cars/{car}/health", "GET", ""),
    ("/api/v1/cars/{car}/rebuild", "POST", "POST"),
    ("/api/v1/cars/{car}/overlaps", "GET PATCH", "PATCH"),
    ("/api/v1/cars/{car}/storage", "GET", ""),
    ("/api/v1/system/storage", "GET", "GET"),
    ("/api/v1/system/health", "GET", "GET"),
    # Import
    ("/api/v1/imports", "POST", "POST"),
    ("/api/v1/imports/{import}", "GET PATCH DELETE", "GET PATCH DELETE"),
    ("/api/v1/imports/{import}/cars", "GET PUT", "GET PUT"),
    ("/api/v1/imports/{import}/start", "POST", "POST"),
]


def _stub(req):
    raise not_built(req.path)


def register(router):
    for pattern, methods, admin in STUBS:
        router.add(pattern, admin=tuple(admin.split()), stub=True, **{m: _stub for m in methods.split()})
