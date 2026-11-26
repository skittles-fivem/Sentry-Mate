"""Everything that can change: accounts, sessions, preferences, the audit log and the owner's
overrides (car names, settings, purposes, costs, tokens, keys, saved trips).

The dataset itself never changes; overrides are kept here apart from the "rebuilt" rows, the way
the real server keeps owner input apart from what it works out from logs. One lock guards it all.
"""

from __future__ import annotations

import copy
import secrets
import threading

from .util import DAY, HOUR, MIN, local_date

DEV_PASSWORD = "sentry-mate-dev"
DEV_TWO_STEP_CODE = "000000"
# a recovery code is 16 symbols of Crockford base32 (no i, l, o or u), shown as xxxx-xxxx-xxxx-xxxx; one works once
# in place of the app's code. The seeded two-step account (alex) has these made-up ones.
RECOVERY_SYMBOLS = "0123456789abcdefghjkmnpqrstvwxyz"
DEV_RECOVERY_CODES = ("a3k7-m2p9-q4r8-s5t6", "b4m8-n3q0-r5s9-t6v7", "c5n9-p4r1-s6t0-v7w8", "d6p0-q5s2-t7v1-w8x9",
                      "e7q1-r6t3-v8w2-x9y0", "f8r2-s7v4-w9x3-y0z1", "g9s3-t8w5-x0y4-z1a2", "h0t4-v9x6-y1z5-a2b3",
                      "j1v5-w0y7-z2a6-b3c4", "k2w6-x1z8-a3b7-c4d5")


def normalize_recovery(typed):
    """A typed recovery code as kept (16 symbols, lower case, dashes and spaces left out, o read as 0, i and l as 1),
    or None: not a recovery code. The same reading as the real server's."""
    out = []
    for c in typed:
        if c in "- ":
            continue
        c = {"o": "0", "i": "1", "l": "1"}.get(c.lower(), c.lower())
        if c not in RECOVERY_SYMBOLS:
            return None
        out.append(c)
    return "".join(out) if len(out) == 16 else None


def new_recovery_codes(n=10):
    return ["-".join("".join(secrets.choice(RECOVERY_SYMBOLS) for _ in range(4)) for _ in range(4)) for _ in range(n)]


def keep_recovery_codes(account, codes):
    """The account's recovery codes are these (kept normalized); the old ones stop working."""
    account["recovery_codes"] = [normalize_recovery(c) for c in codes]
    account["recovery_codes_left"] = len(codes)


def take_code(account, code):
    """True when `code` is the dev server's six digits, or one of the account's recovery codes (which is used up)."""
    if not isinstance(code, str):
        return False
    if code.strip() == DEV_TWO_STEP_CODE:
        return True
    n = normalize_recovery(code)
    left = account.get("recovery_codes") or []
    if n is None or n not in left:
        return False
    left.remove(n)
    account["recovery_codes_left"] = len(left)
    return True
DEV_SETUP_CODE = "SETUP-DEV-0000"
COOKIE = "sentry-mate"
COOKIE_SIGNED_OUT = "out"
SETUP_COOKIE = "sentry-mate-setup"

DEFAULT_PREFS = {
    "units": {"distance": "km", "temperature": "c", "pressure": "bar"},
    "range": "rated",
    "theme": "device",
    "language": "auto",
    "interface_size": 100,
    "follow_screen": True,
    "map_colors": "dimmed",
    "map_style": "default",
    "ui_pack": None,
    "car_order": [],
    "pinned_cars": [],
    "main_car": None,
    "last_car": None,
}


class Store:
    def __init__(self, cfg, ds, clock):
        self.cfg = cfg
        self.ds = ds
        self.clock = clock
        self.lock = threading.RLock()
        now = cfg.now_ms
        self.accounts = {}
        # the two-step rule, server-wide (not per account): everyone, or from outside the home network
        self.two_step_policy = {"everyone": False, "outside": False}
        self.sessions = {}  # token -> session
        self.ended = set()  # tokens this server ended (they stay signed out)
        self.session_rows = {}  # account id -> [made-up older sessions]
        self.prefs = {}
        self.audit = []
        self.throttle = {}
        self.pending = {}  # two-step pending token -> (account id, expires_ms, tries)
        self.next_id = 100
        self.rev = 0  # goes up on every change (cached rows are keyed by it)
        pack_name = cfg.pack.rstrip("/").split("/")[-1]
        self.default_prefs = copy.deepcopy(DEFAULT_PREFS)
        self.default_prefs["ui_pack"] = pack_name
        self.needs_setup = cfg.needs_setup
        # a server started fresh never signs anyone in by itself: after a move or a restore people sign in
        # with the old server's accounts, after "Start fresh" the set-up signs the new admin in
        self.fresh_start = cfg.needs_setup
        if not cfg.needs_setup:
            self._seed_accounts(now)
        self.car_over = {}  # car id -> {name, time_zone (the owner's; None: automatic), config: {key: value}, version}
        self.car_settings = {}
        self.tokens = {}  # car id -> [token rows]
        self.new_car_tokens = []  # upload tokens for a car that has not uploaded yet
        self.keys = []
        self.purposes = {}  # drive id -> {purpose, relation, note, at_ms}
        self.rules = {}  # car id -> [rules]
        self.costs = {}  # charge id -> {mode, amount, currency, energy_basis}
        self.trips = {}
        from .placing import seed_places
        self.places = seed_places()  # place id -> the place as it is now (dataset places plus the owner's changes)
        self.places_rev = 0  # goes up on every place change (place matches are cached by it)
        self.charge_price = {}  # charge id -> {place, price, at_ms}: the price a charge keeps (see placing.py)
        self.update_versions = {}  # update id -> version typed by the owner
        self.first_versions = {}  # (car id, module) -> version typed on a module's first fingerprint segment
        self.builds = {}  # build id -> {version, from_update}: versions learned from the owner
        self.rebuilds = {}  # car id -> [{from_ms, to_ms, at_ms}]
        self._seed_cars(now)
        self._seed_audit(now)
        from .dataset import extras
        self.notices = extras.notices(now)
        self.server_settings = extras.server_settings(now)
        # --map-tiles: online (the default here), none (a new server: offline, no map file) or offline (a file)
        self.map_file = getattr(cfg, "map_tiles", "online") == "offline"
        if getattr(cfg, "map_tiles", "online") != "online":
            self.server_settings["tiles"]["mode"] = "offline"
        # the names in the certificate the server runs with: made at start, not changed by saving host_names.
        # The server's own always has localhost first, as the real one makes it.
        self.cert_names = ["localhost"] + [n for n in self.server_settings["host_names"] if n != "localhost"]
        # the owner's own certificate (--own-certificate): the only kind that expires soon (one the server makes
        # is valid until 4096), and the only kind no new one is made for
        self.cert_own = bool(getattr(cfg, "own_certificate", False))
        # --release-check: the result the made-up check gives (resources/system.py release_state); a check turned
        # on later in the session finds a newer build
        rc = getattr(cfg, "release_check", "off")
        self.release_result = "newer" if rc == "off" else rc
        self.server_settings["release_check"]["enabled"] = rc != "off"
        self.release_checked_ms = None if rc in ("off", "never") else now - 2 * 3_600_000
        self.mqtt = extras.mqtt(now)
        self.imports = extras.imports(now, ds.history("c3"))
        # a TeslaFi import whose rows were checked again: a move, and rows that wait for a car
        self.holds = {}
        if self.imports and ds.exists("c1") and ds.exists("c2"):
            job, self.holds = extras.teslafi_rehomed(now)
            job["_holds"] = self.holds
            self.imports[job["id"]] = job
        self.storage = extras.storage(now)
        self.compaction = None  # the compaction's state, made on the first read (resources/system.py)
        from .resources import moving
        moving.seed(self, cfg, now)  # first-run set-up, the move to another server, export files
        from .resources import tesla
        tesla.seed(self, now)  # the Tesla API input: one account, its cars
        from .resources import teslafi
        teslafi.seed(self, now)  # the TeslaFi input: one account, its car
        from .resources import invoices
        invoices.seed(self, now)  # Supercharger invoices: Tesla's on, TeslaFi's off
        from .resources import teslamate
        teslamate.seed(self, now)  # the local TeslaMate input: MQTT on, the database catch-up off, two cars
        teslamate.seed_trouble(self, getattr(cfg, "teslamate_trouble", "none"), now)  # --teslamate-trouble
        from .resources import fullrebuild
        fullrebuild.seed(self, cfg, now)  # --full-rebuild: none, running, failed or offered
        from .resources import repair
        repair.seed(self, now)  # c1's drives table was repaired a few days ago

    # ------------------------------------------------------------ seeding
    def seed_accounts(self, now):
        """The old server's accounts arrive (a move or a restore finished on a fresh server)."""
        self._seed_accounts(now)

    def _seed_accounts(self, now):
        mk = lambda i, name, role, cars, two: {  # noqa: E731
            "id": i, "name": name, "role": role, "cars": cars, "two_step": two,
            "password": DEV_PASSWORD, "created_ms": now - 400 * DAY + int(i[1:]) * DAY, "version": 1,
        }
        for a in (mk("a1", "admin", "admin", "all", False), mk("a2", "watch", "watch", ["c1"], False),
                  mk("a3", "alex", "admin", "all", True)):
            self.accounts[a["id"]] = a
        keep_recovery_codes(self.accounts["a3"], DEV_RECOVERY_CODES)
        self.prefs["a2"] = {"units": {"distance": "mi", "temperature": "f", "pressure": "psi"}, "range": "ideal",
                            "theme": "light"}
        self.session_rows["a1"] = [
            {"id": "s-older-1", "created_ms": now - 9 * DAY, "last_seen_ms": now - 2 * DAY,
             "device": "Firefox on Linux", "address": "192.0.2.14"},
            {"id": "s-older-2", "created_ms": now - 20 * DAY, "last_seen_ms": now - 5 * HOUR,
             "device": "Safari on iPad", "address": "192.0.2.27"},
        ]

    def _seed_cars(self, now):
        for cid, h in self.ds.featured.items():
            car = h.car
            s = {
                "version": 1,
                "battery_chemistry": {"setting": "nca_nmc" if cid == "c3" else "auto",
                                      "detected": car.chemistry if cid != "c3" else None,
                                      "evidence": {"c1": "Pack voltage range and the BMS pack type message.",
                                                   "c2": "Flat voltage curve between 20 % and 90 %.",
                                                   "c3": None}.get(cid)},
                "free_supercharging": [{"from_date": "2017-03-01", "to_date": None}] if cid == "c3" else [],
                "capacity_new_kwh": None if cid != "c3" else 87.0,
                "full_range_new_km": None,
                "warranty_threshold_pct": 70,
                "efficiency_wh_per_km_override": None,
                "software_version_typed": None,
                "pack_replaced_date": None,
                "pack_new_kwh": None,
                # Comet's charges with no price (its Los Angeles hotel) are estimated at this on Statistics
                "default_price_per_kwh": {"value": 0.14, "currency": "USD"} if cid == "c1" else None,
            }
            self.car_settings[cid] = s
            sold = getattr(car, "sold_date_days", None)
            if sold is not None:
                self.car_over.setdefault(cid, {}).update(
                    {"sold": True, "sold_date": local_date(now + sold * DAY, car.time_zone).isoformat()})
        first = self.ds.featured.get("c1")
        t0 = first.first_ms if first else now - 400 * DAY
        self.tokens = {
            "c1": [{"id": "t11", "created_ms": t0 - 2 * DAY, "revoked": True, "key": None, "last_upload_ms": None},
                   {"id": "t12", "created_ms": t0 - DAY, "revoked": False, "key": "cbk_8d41c0a2",
                    "last_upload_ms": now - 70_000}],
            "c2": [{"id": "t21", "created_ms": t0 - DAY, "revoked": False, "key": "cbk_31f7e9b5",
                    "last_upload_ms": now - 45_000}],
            "c3": [{"id": "t31", "created_ms": t0 + 30 * DAY, "revoked": False, "key": "cbk_c90a7716",
                    "last_upload_ms": now - 55 * HOUR}],
        }
        self.keys = [
            {"key": "cbk_8d41c0a2", "first_seen_ms": t0 - DAY + HOUR, "last_upload_ms": now - 70_000, "logs_waiting": 0,
             "car": "c1", "preferred": True, "device": "Screenmate One"},
            {"key": "cbk_5b2e9f13", "first_seen_ms": t0 + 120 * DAY, "last_upload_ms": now - 3 * DAY, "logs_waiting": 0,
             "car": "c1", "preferred": False, "device": "Pi logger"},
            {"key": "cbk_31f7e9b5", "first_seen_ms": t0, "last_upload_ms": now - 45_000, "logs_waiting": 0,
             "car": "c2", "preferred": True, "device": None},
            {"key": "cbk_c90a7716", "first_seen_ms": t0 + 30 * DAY, "last_upload_ms": now - 55 * HOUR,
             "logs_waiting": 2, "car": "c3", "preferred": True, "device": None},
            {"key": "cbk_e04d6a58", "first_seen_ms": now - 3 * HOUR, "last_upload_ms": now - 20 * MIN,
             "logs_waiting": 3, "car": None, "preferred": False, "device": None},
        ]
        self.keys = [k for k in self.keys if k["car"] is None or k["car"] in self.ds.featured]
        self.tokens = {c: v for c, v in self.tokens.items() if c in self.ds.featured}
        self.rules = {"c1": [{"id": "r1", "from_place": "p1", "to_place": "p2", "days": "mon-fri", "purpose": "commute"},
                             {"id": "r2", "from_place": "p2", "to_place": "p1", "days": "mon-fri", "purpose": "commute"}]}
        # a few purposes typed by the owner
        h = self.ds.featured.get("c1")
        if h:
            for d in h.drives[-60::7]:
                if d.to_pid == "p11":
                    self.purposes[d.id] = {"purpose": "business", "relation": "Customer visit", "note": None,
                                           "at_ms": d.end}
        self.trips = {}
        if h:
            rt = h.car.special["road_trip"]
            day0 = now - (-rt[0]) * DAY
            self.trips["tr1"] = {"id": "tr1", "car": "c1", "account": "a1", "from_ms": day0 - 6 * HOUR,
                                 "to_ms": day0 + 5 * DAY, "name": "Los Angeles in July"}

    def _seed_audit(self, now):
        from .audit import seed
        for at, who, action, target, car, summary, address in seed(self.ds, now):
            self.audit.append({"at_ms": at, "account": who, "action": action, "target": target, "car": car,
                               "summary": summary, "address": address})

    # ------------------------------------------------------------ ids, audit
    def new_id(self, prefix):
        with self.lock:
            self.next_id += 1
            return f"{prefix}{self.next_id}"

    def record(self, account, action, target, summary, car=None, address=None):
        """car: the car the changed item belongs to (a drive's, a charge's, a trip's, an update's car, the car a
        TeslaMate car was matched to); a target that names its car (car:<id>, drive:<car>:<id>, ...) gives it.
        address: where the request came from (the dev server's own browser: 127.0.0.1); None for the server."""
        from .audit import car_of
        if address is None and account:
            address = "127.0.0.1"
        with self.lock:
            self.rev += 1
            self.audit.append({"at_ms": self.clock.now(), "account": account["name"] if account else None,
                               "action": action, "target": target, "car": car_of(target, car),
                               "summary": summary, "address": address})

    # ------------------------------------------------------------ accounts and sessions
    def account_by_name(self, name):
        for a in self.accounts.values():
            if a["name"].lower() == str(name).lower():
                return a
        return None

    def start_session(self, account, device="Browser"):
        with self.lock:
            token = "smd_" + secrets.token_urlsafe(24)
            sid = "s" + secrets.token_hex(4)
            now = self.clock.now()
            s = {"token": token, "id": sid, "account": account, "created_ms": now, "last_seen_ms": now,
                 "device": device}
            self.sessions[token] = s
            mine = [x for x in self.sessions.values() if x["account"]["id"] == account["id"]]
            if len(mine) > 50:  # like the real server: the least recently used one is signed out
                oldest = min(mine, key=lambda x: x["last_seen_ms"])
                self.end_session(oldest["token"])
            return s

    def session_for(self, token):
        with self.lock:
            s = self.sessions.get(token)
            if s is None or s["account"]["id"] not in self.accounts:
                return None
            s["account"] = self.accounts[s["account"]["id"]]
            s["last_seen_ms"] = self.clock.now()
            return s

    def end_session(self, token):
        with self.lock:
            self.ended.add(token)
            return self.sessions.pop(token, None)

    def default_account(self):
        if self.needs_setup or self.fresh_start:
            return None
        return self.accounts["a2" if self.cfg.role == "watch" else "a1"]

    def sees(self, account, car_id):
        if account is None:
            return False
        cars = account["cars"]
        return cars == "all" or car_id in cars

    def visible_car_count(self, account):
        if account["cars"] == "all":
            return self.ds.total_cars()
        return sum(1 for c in account["cars"] if self.ds.exists(c))

    def get_prefs(self, account):
        with self.lock:
            p = copy.deepcopy(self.default_prefs)
            # units, range and car order default from the server settings
            ss = getattr(self, "server_settings", None)
            if ss:
                p["units"] = copy.deepcopy(ss["units"])
                p["range"] = ss["range"]
                p["car_order"] = list(ss["car_order"])
            own = self.prefs.get(account["id"], {})
            for k, v in own.items():
                if k == "units":
                    p["units"].update(v)
                else:
                    p[k] = copy.deepcopy(v)
            if p.get("ui_pack") is None:  # null: the server's choice
                p["ui_pack"] = self.default_prefs["ui_pack"]
            # only cars this person may see (the server's order names every car; an account's list can shrink)
            for k in ("car_order", "pinned_cars"):
                p[k] = [c for c in p[k] if self.ds.exists(c) and self.sees(account, c)]
            for k in ("main_car", "last_car"):
                if p.get(k) is not None and not (self.ds.exists(p[k]) and self.sees(account, p[k])):
                    p[k] = None
            return p

    # ------------------------------------------------------------ cars
    def car_meta(self, car_id):
        """Name, time zone and configuration of a car, with the owner's overrides applied."""
        h = self.ds.history(car_id)
        over = self.car_over.get(car_id, {})
        if h:
            car = h.car
            base = {"name": car.name, "model": car.model, "year": car.year, "trim": car.trim,
                    "vin_prefix": car.vin_prefix, "profile": car.profile, "time_zone": car.time_zone,
                    "time_zone_source": getattr(car, "time_zone_source", "position"),
                    "config": {k: {"value": v, "source": s} for k, (v, s) in car.config.items()},
                    "build": {"chemistry": car.chemistry, "full_now_kwh": car.full_now_kwh,
                              "buffer_kwh": car.buffer_kwh, "full_range_new_km": car.full_range_new_km}}
        else:
            sm = self.ds.synthetic.summary(int(car_id[1:]))
            base = {k: sm[k] for k in ("name", "model", "year", "trim", "vin_prefix", "profile", "time_zone",
                                       "time_zone_source")}
            base["config"] = {"trim": {"value": sm["trim"], "source": "vin"}}
        base["version"] = over.get("version", 1)
        if "name" in over:
            base["name"] = over["name"]
        if over.get("time_zone"):
            base["time_zone"], base["time_zone_source"] = over["time_zone"], "owner"
        elif "time_zone" in over and base["time_zone_source"] == "owner":
            # back to automatic: the made-up car's own zone, as if from its position
            base["time_zone_source"] = "position"
        base["sold"] = bool(over.get("sold"))
        base["sold_date"] = over.get("sold_date") if base["sold"] else None
        for k, v in over.get("config", {}).items():
            base["config"][k] = {"value": v, "source": "owner"}
        return base
