"""One module per resource group of docs/ui-api.md. Each has `register(router)`.

To add a section: write a module here with `register(router)` (copy drives.py for the pattern),
add it to MODULES below, and delete its patterns from stubs.py. A real route replaces a stub with
the same pattern even if the stub is still listed.
"""

from __future__ import annotations

from . import auth, charges, drives, fleet, live, series, stubs, timeline


def SSE(req):  # noqa: N802 - a marker: the HTTP layer streams /api/v1/events itself
    raise RuntimeError("streamed by the HTTP layer")


MODULES = [auth, fleet, live, series, timeline, drives, charges]


def register_all(router):
    for m in MODULES:
        m.register(router)
    router.add("/api/v1/events", GET=SSE)
    stubs.register(router)
