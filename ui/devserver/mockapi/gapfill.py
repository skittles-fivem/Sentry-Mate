"""Import gap filling (docs/ui-api.md, Conventions, "Values filled from an import").

Where SMO and the Tesla API both have nothing for a value at a time, the import shown for that span fills it,
labelled "Imported from TeslaMate" (or TeslaFi). Rows still come from one source per span: a live drive or charge
gets only its null values from its imported twin (the TeslaMate copy of the same drive), never a second row, and
never its distance, energy, cost or `energy_basis`, so totals do not change. With `source=all` only, and while the
car's `import_gap_fill` (else the server's `import_gap_fill.enabled`, on by default) says so.

In the made-up data Atlas (`c3`, a 2017 Model S) has no cabin temperature on CAN, and its TeslaMate import has one:
its live drives in the days both cover get the import's.
"""

from __future__ import annotations

from . import sources as S

# the fields a live row may get from its import (never distance, energy, costs or the basis), as the server's
# (crates/timeline/src/gapfill.rs): a drive's `start` but never its `end` (a live drive without one is one whose data
# stopped: it keeps "Data stopped here"); a charge's `odometer_km` only with its `position`
DRIVE_FIELDS = ("start", "start_odometer_km", "end_odometer_km", "start_soc_pct", "end_soc_pct", "start_range_km",
                "end_range_km", "outside_avg_c", "cabin_avg_c", "climb_m", "descent_m")
CHARGE_FIELDS = ("position", "start_soc_pct", "end_soc_pct", "start_range_km", "end_range_km", "outside_avg_c",
                 "battery_avg_c")
# positions: a point (`start`, `position`) without coordinates is none
POSITIONS = ("start", "position")


def _missing(row, f):
    v = row.get(f)
    if f in POSITIONS:
        return v is None or v.get("lat") is None or v.get("lon") is None
    return v is None


def on(st, car_id):
    """Whether a car's import fills what its live history lacks: its own setting, else the server's (on)."""
    own = (st.car_settings.get(car_id) or {}).get("import_gap_fill")
    if own is not None:
        return own
    return (st.server_settings.get("import_gap_fill") or {}).get("enabled", True)


def twin(h, seg):
    """The import's copy of a live drive or charge that is shown there (where both imports cover a span, the chosen
    one's), else None."""
    if getattr(seg, "source", None) != "can" or getattr(seg, "kind", None) not in ("drive", "charge"):
        return None
    m = getattr(h, "_twins", None)
    if m is None:
        m = {}
        for x in h.shadow:
            of = getattr(x, "tm_copy_of", None)
            if of is not None:
                m[of] = x
        h._twins = m
    t = m.get(seg.id)
    return t if t is not None and h.site_shown(t) else None


def fill_row(app, h, seg, row, make):
    """Fills a live row's null fields from its imported twin (`make(twin)` builds the twin's row); names them in
    `value_sources` and adds the import to `sources`. Keeps what it filled, with the values it had, in `_unfilled` (for `source=` filters)."""
    row["value_sources"] = {}
    if not on(app.store, h.car.id):
        return row
    t = twin(h, seg)
    if t is None:
        return row
    fields = DRIVE_FIELDS if seg.kind == "drive" else CHARGE_FIELDS
    lacks = [f for f in fields if _missing(row, f)]
    if not lacks:
        return row
    other = make(t)
    site = S.site_of(t)
    got = [f for f in lacks if not _missing(other, f)]
    # a charge's odometer comes with the position filled (the server reads both from one import sample)
    if "position" in got and row.get("odometer_km") is None and other.get("odometer_km") is not None:
        got.append("odometer_km")
    if not got:
        return row
    ref = S.ref("import", site)
    row["_unfilled"] = {f: row.get(f) for f in got}
    for f in got:
        row[f] = other[f]
        row["value_sources"][f] = [ref]
    refs = list(row["sources"]) + ([ref] if ref not in row["sources"] else [])
    row["sources"] = refs
    row["source_label"] = S.label_of(refs)
    return row


def unfill(row):
    """A row as one source alone shows it (`source=` other than all): the values its import filled taken out."""
    got = row.get("_unfilled")
    if not got:
        return row
    row = dict(row)
    for f, was in got.items():
        row[f] = was
    row["value_sources"] = {}
    row["sources"] = [r for r in row["sources"] if r["kind"] != "import"]
    row["source_label"] = S.label_of(row["sources"])
    row.pop("_unfilled", None)
    return row
