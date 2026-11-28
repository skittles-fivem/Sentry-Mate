"""Supercharger invoices (docs/ui-api.md, "Integrations: Supercharger invoices", and the `invoices` blocks of
"Integrations: Tesla" and "Integrations: TeslaFi").

Made up from the dataset: every Supercharger charge of the Tesla account's cars (`c1`, `c2`) that ended before
`--now-ms` has a Tesla charging session (`tesla-1000101`, ...), matched to that charge, with a made-up site name
("Made-up Supercharger, Hilltown"), the charging fee (billed kWh at the site's made-up rate), an idle fee on every
fifth one and a tiny made-up PDF (every ninth one's PDF is still waiting). Two sessions of the account's third car
(not matched to a car here) are unmatched. The session's total is the charge's cost (`cost.source: "tesla_invoice"`)
unless an admin entered a cost, which wins (`applied: false`). TeslaFi gives no invoices (`gives_pdfs: false`): its
block has no sessions.

Each saved place where sessions are matched to charges gets dated prices marked "invoice" (`apply_prices`, at
start): the median price per kWh billed over a window of its ten newest sessions, from the day of the earliest
session at it, as the real server works them out (crates/engine/src/invoice_prices.rs); a typed price wins over
them. Supercharger Schaumburg (p36) has no typed price, so its price in force is from invoices.

The made-up server had Tesla invoices turned on by an admin before "now" (the real default is off); TeslaFi's are
off. Turning them off stops the checks (`next_check_ms` null) and keeps what was fetched.
"""

from __future__ import annotations

import datetime as _dt
import io
import zipfile

from ..conventions import Response, page
from ..errors import bad_input, not_found
from ..pdf import text_pdf
from ..placing import piece_pid
from ..util import DAY, HOUR, MIN, local_dt, r

SOURCES = ("tesla", "teslafi")
FIRST_ID = 1000101
TOWNS = ("Hilltown", "Rivermouth", "Lakeview", "Stonebridge", "Oakvale", "Fernside", "Millbrook", "Westhaven",
         "Brightwater", "Northfield", "Clearcreek", "Ashford")
NAMES = ("Made-up Supercharger", "Sample Supercharger")
TESLAFI_NOTE = "TeslaFi's API gives no invoices: only the costs TeslaFi lists for its charges are taken."
IDLE_EVERY = 5
NO_PDF_EVERY = 9
CHECK_EVERY_MS = 6 * HOUR


def seed(st, now):
    """The toggles (in each input's settings, so a PATCH bumps the input's `version`) and the fetch figures."""
    st.tesla["settings"]["invoices"] = True
    st.teslafi["settings"]["invoices"] = False
    st.invoice_state = {
        "tesla": {"last_check_ms": now - 2 * HOUR - 17 * MIN, "requests_today": 4, "backfill": "done"},
        "teslafi": {"last_check_ms": None, "requests_today": 0, "backfill": "off"},
    }


# ------------------------------------------------------------------ the made-up sessions


def _site(app, c):
    pid = piece_pid(app, c) or f"{round(c.lat, 2)},{round(c.lon, 2)}"
    k = sum(ord(ch) for ch in str(pid))
    return f"{NAMES[k % len(NAMES)]}, {TOWNS[k % len(TOWNS)]}", k


def _place(app, c):
    from ..rows import place_obj
    return place_obj(app, piece_pid(app, c))


def _fees(kwh, rate, idle_min, idle_rate):
    kwh = r(kwh, 1)
    fees = [{"kind": "charging", "amount": round(kwh * rate, 2), "rate": rate, "usage": kwh, "unit": "kwh"}]
    if idle_min:
        fees.append({"kind": "idle", "amount": round(idle_min * idle_rate, 2), "rate": idle_rate,
                     "usage": float(idle_min), "unit": "min"})
    return fees


def _make(n, *, site, start, end, kwh, currency, country, idle_min, rate, car, charge):
    idle_rate = 1.0 if currency == "CAD" else 0.5
    fees = _fees(kwh, rate, idle_min, idle_rate)
    total = round(sum(f["amount"] for f in fees), 2)
    iid = f"tesla-{n}"
    return {
        "id": iid, "source": "tesla", "site": site, "start_ms": start, "end_ms": end, "total": total,
        "currency": currency, "kwh_billed": r(kwh, 1), "minutes_billed": round((end - start) / MIN),
        "idle_fee": fees[1]["amount"] if len(fees) > 1 else 0.0, "fees": fees,
        "invoice_name": f"made-up-invoice-{n}.pdf", "pdf": (n - FIRST_ID) % NO_PDF_EVERY != NO_PDF_EVERY - 1,
        "account": "ta1", "car": car, "charge": charge, "country_code": country,
    }


def sessions(app):
    """Every made-up session, oldest first (worked out once, from the dataset as it is at `--now-ms`)."""
    cache = app.cache.get("invoices")
    if cache is not None:
        return cache
    base_now = app.cfg.now_ms
    found = []
    for cid in ("c1", "c2"):
        h = app.ds.history(cid)
        if h is None:
            continue
        for c in h.charges:
            if c.ckind != "dc" or c.station_kw < 250 or c.source != "can" or c.data_end > base_now - HOUR:
                continue
            found.append((c.start, cid, c))
    found.sort(key=lambda x: (x[0], x[1]))
    out = []
    n = FIRST_ID
    for _, cid, c in found:
        site, k = _site(app, c)
        pl = _place(app, c)
        country = ((pl or {}).get("address") or {}).get("country_code") or "US"
        price = (pl or {}).get("price") or {}
        currency = price.get("currency") or ("CAD" if country == "CA" else "USD")
        f = c.figures()
        kwh = f["inlet"] if f["inlet"] is not None else f["added"]
        rate = (0.58 if currency == "CAD" else 0.42) + (k % 7) * 0.02
        idle = 4 + k % 6 if (n - FIRST_ID) % IDLE_EVERY == IDLE_EVERY - 1 else 0
        out.append(_make(n, site=site, start=c.start + 40_000, end=c.data_end - 20_000, kwh=kwh, currency=currency,
                         country=country, idle_min=idle, rate=round(rate, 2), car=cid, charge=c.id))
        n += 1
    # two sessions of the account's third car (the "Guest car", not matched to a car here)
    for i, days in enumerate((23, 9)):
        start = base_now - days * DAY + 13 * HOUR + i * 37 * MIN
        out.append(_make(n, site=f"{NAMES[i]}, {TOWNS[(i * 5 + 3) % len(TOWNS)]}", start=start,
                         end=start + (31 + 9 * i) * MIN, kwh=38.4 + 7.3 * i, currency="USD", country="US",
                         idle_min=0, rate=0.46, car=None, charge=None))
        n += 1
    out.sort(key=lambda s: (s["start_ms"], s["id"]))
    app.cache["invoices"] = out
    app.cache["invoices_by_charge"] = by = {}
    for s in out:
        if s["charge"]:
            by.setdefault(s["charge"], []).append(s)
    return out


# ------------------------------------------------------------------ place prices from invoices

RECENT = 10


def derive(sessions, currency):
    """Dated invoice prices of one place: `sessions` is [(local day, price per kWh)] in time order. A window of the
    RECENT newest up to each session; a new price whenever its median (the lower middle one) changes."""
    n = len(sessions)
    if not n:
        return []
    out = []
    for i in range(min(RECENT, n) - 1, n):
        window = sessions[max(0, i + 1 - RECENT):i + 1]
        rates = sorted(round(x[1], 5) for x in window)
        median = rates[(len(rates) - 1) // 2]
        if out and out[-1][1] == median:
            continue
        frm = next(x[0] for x in window if round(x[1], 5) == median)
        while out and out[-1][0] >= frm:
            out.pop()
        if out and out[-1][1] == median:
            continue
        out.append((frm, median, len(window)))
    return [{"from": f, "source": "invoice", "invoice": {"sessions": k},
             "price": {"currency": currency, "per_kwh": m, "per_minute": None, "rates": [], "session_fee": None,
                       "tiers": None}} for f, m, k in out]


def place_prices(app):
    """{place id: its invoice prices} from the sessions matched to charges at a saved place."""
    from ..placing import date_str
    from ..rows import place_obj
    at = {}
    by_id = {}
    for s in sessions(app):
        if not s["charge"] or not s["currency"]:
            continue
        rate = next((f["rate"] for f in s["fees"] if f["kind"] == "charging" and f["unit"] == "kwh"), None)
        if rate is None:
            continue
        if s["car"] not in by_id:
            h = app.ds.history(s["car"])
            by_id[s["car"]] = {c.id: c for c in h.charges} if h else {}
        c = by_id[s["car"]].get(s["charge"])
        pl = place_obj(app, piece_pid(app, c)) if c else None
        if not pl:
            continue
        day = date_str(s["start_ms"], pl.get("time_zone") or "UTC")
        at.setdefault(pl["id"], []).append((s["start_ms"], day, s["currency"], rate))
    out = {}
    for pid, v in at.items():
        v.sort()
        currency = v[-1][2]
        out[pid] = derive([(d, x) for _, d, c, x in v if c == currency], currency)
    return out


def apply_prices(app):
    """Gives each place with matched sessions its invoice prices (next to its typed ones, which win)."""
    from .. import placing
    for pid, inv in sorted(place_prices(app).items()):
        p = app.store.places.get(pid)
        if p is None:
            continue
        typed = [x for x in placing.prices_of(p) if x.get("source", "typed") == "typed"]
        p["prices"] = placing.sort_prices(typed + inv)
        p["price"] = placing.price_on(p, app.cfg.now_ms)


def for_charge(app, charge_id):
    """The sessions matched to one charge ([] for none)."""
    sessions(app)
    return app.cache["invoices_by_charge"].get(charge_id, [])


def charge_cost(app, c, f, gap_ms):
    """(cost, price_per_kwh, cost_breakdown) from the charge's invoice, or None when it has none. Called after an
    entered cost was looked for (an entered cost wins)."""
    inv = for_charge(app, c.id)
    if not inv:
        return None
    s = inv[0]
    parts = []
    for fee in s["fees"]:
        per_kwh = fee["rate"] if fee["unit"] == "kwh" else None
        per_min = fee["rate"] if fee["unit"] == "min" else None
        parts.append({"rate": {"charging": "Charging", "idle": "Idle fee"}.get(fee["kind"], "Other fee"),
                      "per_kwh": per_kwh, "per_minute": per_min,
                      "kwh": fee["usage"] if per_kwh is not None else None,
                      "minutes": fee["usage"] if per_min is not None else None, "cost": fee["amount"],
                      "from_kwh": None, "to_kwh": None})
    added = f["added"]
    cost = {"amount": s["total"], "currency": s["currency"], "source": "tesla_invoice"}
    ppk = {"value": round(s["total"] / added, 4), "basis": "pack"} if added else None
    return cost, ppk, {"parts": parts, "session_fee": None, "total": s["total"], "gap_ms": gap_ms,
                       "mode": "tesla_invoice", "cycle": None}


def refs(app, charge_id, manual):
    """The charge detail's `invoices`: InvoiceRef for each session matched to it."""
    keep = ("id", "source", "site", "start_ms", "end_ms", "total", "currency", "kwh_billed", "minutes_billed",
            "idle_fee", "fees", "invoice_name", "pdf")
    out = []
    for i, s in enumerate(for_charge(app, charge_id)):
        x = {k: s[k] for k in keep}
        x["fees"] = [dict(f) for f in s["fees"]]
        x["applied"] = not manual and i == 0
        out.append(x)
    return out


# ------------------------------------------------------------------ the PDFs


def pdf_bytes(app, s):
    tz = app.store.car_meta(s["car"])["time_zone"] if s["car"] else "America/Los_Angeles"
    a = local_dt(s["start_ms"], tz)
    lines = [
        "MADE-UP INVOICE - not a real document",
        "",
        f"Invoice:   {s['id']}",
        f"Site:      {s['site']}",
        f"Date:      {a.strftime('%Y-%m-%d %H:%M')} ({tz})",
        f"Duration:  {s['minutes_billed']} min",
        "",
    ]
    for fee in s["fees"]:
        unit = "kWh" if fee["unit"] == "kwh" else "min"
        lines.append(f"{fee['kind']:<10} {fee['usage']:>8} {unit} x {fee['rate']:.2f} = {fee['amount']:.2f} {s['currency']}")
    lines += ["", f"Total:     {s['total']:.2f} {s['currency']}"]
    return text_pdf("Supercharger invoice (made up)", lines, landscape=False)


def _pdf_response(app, s):
    if not s["pdf"]:
        raise not_found("no_pdf", "The invoice (PDF) of this session is not downloaded yet.")
    return Response(200, pdf_bytes(app, s), "application/pdf",
                    {"Content-Disposition": f'attachment; filename="{s["invoice_name"]}"'})


def charge_pdf(req):
    from .charges import _charge
    _, c = _charge(req)
    s = next((x for x in for_charge(req.app, c.id) if x["id"] == req.params["invoice"]), None)
    if s is None:
        raise not_found("no_such_invoice", "There is no such invoice for this charge.")
    return _pdf_response(req.app, s)


def invoice_pdf(req):
    s = next((x for x in sessions(req.app) if x["id"] == req.params["invoice"]), None)
    if s is None:
        raise not_found("no_such_invoice", "There is no such invoice.")
    return _pdf_response(req.app, s)


# ------------------------------------------------------------------ the blocks of Tesla and TeslaFi


def block(req, source):
    st = req.store
    settings = (st.tesla if source == "tesla" else st.teslafi)["settings"]
    state = st.invoice_state[source]
    accounts = (st.tesla if source == "tesla" else st.teslafi)["accounts"]
    enabled = bool(settings.get("invoices"))
    rows = [s for s in sessions(req.app) if s["source"] == source]
    matched = sum(1 for s in rows if s["charge"])
    last = state["last_check_ms"]
    if source == "tesla":
        available = not accounts or any(a["region"] == "global" for a in accounts.values())
    else:
        available = True
    out = {
        "enabled": enabled, "available": available, "gives_pdfs": source == "tesla",
        "note": TESLAFI_NOTE if source == "teslafi" else None,
        "sessions": len(rows), "matched": matched, "unmatched": len(rows) - matched,
        "pdfs": sum(1 for s in rows if s["pdf"]), "pdfs_waiting": sum(1 for s in rows if not s["pdf"]),
        "backfill": state["backfill"] if enabled or state["backfill"] == "done" else "off",
        "last_check_ms": last,
        "next_check_ms": (max(last + CHECK_EVERY_MS, req.now) if last else req.now + 5 * MIN) if enabled and accounts else None,
        "requests_today": state["requests_today"], "last_error": None,
    }
    if source == "teslafi":
        out["skipped_days"] = []
    return out


def patch_value(body):
    """The `invoices` field of a PATCH of /integrations/tesla or /teslafi: True, False or None (not sent)."""
    v = body.get("invoices")
    if "invoices" in body and not isinstance(v, bool):
        raise bad_input({"invoices": "Must be true or false."})
    return v


def apply_patch(req, source, value):
    """Under the store's lock, after the version check: turn the source's invoices on or off. True when changed."""
    st = req.store
    settings = (st.tesla if source == "tesla" else st.teslafi)["settings"]
    if value is None or bool(settings.get("invoices")) == value:
        return False
    settings["invoices"] = value
    state = st.invoice_state[source]
    if value and state["backfill"] == "off":
        state["backfill"] = "running" if source == "tesla" else "done"
    name = "Tesla" if source == "tesla" else "TeslaFi"
    st.record(req.account, f"{source}-invoices-changed", f"settings:{source}-invoices",
              f"Turned {name} Supercharger invoices {'on' if value else 'off'}.")
    return True


# ------------------------------------------------------------------ the list and the downloads


def _rows(req):
    source = req.enum("source", SOURCES)
    a, b = req.period()
    matched = req.bool_param("matched")
    out = []
    for s in sessions(req.app):
        if source and s["source"] != source:
            continue
        if not a <= s["start_ms"] < b:
            continue
        if matched is not None and bool(s["charge"]) != matched:
            continue
        if s["car"] and not req.store.sees(req.account, s["car"]):
            continue
        out.append(s)
    return out


def list_invoices(req):
    rows = _rows(req)
    items, nxt = page(req, [dict(s, fees=[dict(f) for f in s["fees"]]) for s in rows],
                      key=lambda s: (s["start_ms"], s["id"]), reverse=True)
    by_cur = {}
    for s in rows:
        by_cur[s["currency"]] = by_cur.get(s["currency"], 0.0) + s["total"]
    m = sum(1 for s in rows if s["charge"])
    return {"items": items, "next": nxt, "totals": {
        "sessions": len(rows), "matched": m, "unmatched": len(rows) - m, "pdfs": sum(1 for s in rows if s["pdf"]),
        "cost_by_currency": [{"currency": k, "amount": round(v, 2)} for k, v in sorted(by_cur.items())]}}


def _file_name(req, s):
    tz = req.store.car_meta(s["car"])["time_zone"] if s["car"] else "America/Los_Angeles"
    day = local_dt(s["start_ms"], tz).date().isoformat()
    site = "".join(ch if ch.isalnum() else "-" for ch in s["site"]).strip("-")
    while "--" in site:
        site = site.replace("--", "-")
    return f"{day}_{site}_{s['id']}.pdf"


def zip_invoices(req):
    rows = [s for s in _rows(req) if s["pdf"]]
    if not rows:
        raise not_found("no_invoices", "There is no invoice (PDF) in this period.")
    buf = io.BytesIO()
    stamp = _dt.datetime.fromtimestamp(req.app.cfg.now_ms / 1000, tz=_dt.timezone.utc).timetuple()[:6]
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as z:
        for s in rows:
            info = zipfile.ZipInfo(_file_name(req, s), date_time=stamp)
            info.compress_type = zipfile.ZIP_DEFLATED
            z.writestr(info, pdf_bytes(req.app, s))
    req.store.record(req.account, "invoices-downloaded", None,
                     f"Downloaded {len(rows)} Supercharger invoices as one file.")
    return Response(200, buf.getvalue(), "application/zip",
                    {"Content-Disposition": 'attachment; filename="supercharger-invoices.zip"'})


def register(router):
    admin_all = ("GET",)
    router.add("/api/v1/integrations/invoices", GET=list_invoices, admin=admin_all)
    router.add("/api/v1/integrations/invoices/zip", GET=zip_invoices, admin=admin_all)
    router.add("/api/v1/integrations/invoices/{invoice}/pdf", GET=invoice_pdf, admin=admin_all)
    router.add("/api/v1/cars/{car}/charges/{charge}/invoices/{invoice}/pdf", GET=charge_pdf)
