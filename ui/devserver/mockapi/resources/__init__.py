"""One module per resource group of docs/ui-api.md. Each has `register(router)`.

To add a resource: write a module here with `register(router)` (copy drives.py for the pattern)
and add it to MODULES below. Every route of docs/ui-api.md is built; tests/test_contract.py
checks that against the document.
"""

from __future__ import annotations

from . import (auth, battery, charges, drives, firmware, fleet, imports, integrations, live, moving, places, reports,
               series, software, system, tesla, timeline, travel)


def SSE(req):  # noqa: N802 - a marker: the HTTP layer streams /api/v1/events itself
    raise RuntimeError("streamed by the HTTP layer")


MODULES = [auth, fleet, live, series, timeline, drives, charges, battery, reports, travel, software, firmware,
           places, integrations, tesla, system, imports, moving]


def register_all(router):
    for m in MODULES:
        m.register(router)
    router.add("/api/v1/events", GET=SSE)
