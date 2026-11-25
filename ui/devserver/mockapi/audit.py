"""The audit log's codes, their groups and the made-up log a new dev server starts with.

The code list is ui/shared/audit-codes.json (written from the real server's table): `[{code, group, item}]`.
A code the list lacks reads group "other", as on the real server. The seeded log covers every code in it, with
the target kind the list names and a summary in the server's words. All of it is made up: names, places and
addresses (192.0.2.x only).
"""

from __future__ import annotations

import json
import os

from .util import DAY, HOUR, MIN

CODES_FILE = os.path.normpath(os.path.join(os.path.dirname(__file__), "..", "..", "shared", "audit-codes.json"))
GROUPS = ("sign-ins", "accounts", "settings", "cars", "integrations", "imports")



def load_codes():
    """[{code, group, item}] from ui/shared/audit-codes.json ([] when the file is not there)."""
    try:
        with open(CODES_FILE, encoding="utf-8") as f:
            return json.load(f)
    except OSError:
        return []


CODES = load_codes()
GROUP_OF = {c["code"]: c["group"] for c in CODES}


def group_of(code):
    return GROUP_OF.get(code, "other")


def car_of(target, car):
    """The car an entry's item belongs to: the given one, else the car a target names (car:<car>,
    drive:<car>:<id>, charge:, update:, key:, token:<car>:<id>)."""
    if car:
        return car
    if not isinstance(target, str):
        return None
    parts = target.split(":")
    if parts[0] == "car" and len(parts) == 2:
        return parts[1]
    if parts[0] in ("drive", "charge", "update", "key", "token") and len(parts) == 3:
        return parts[1]
    return None


# ------------------------------------------------------------------ the seeded log

# made-up people: the three dev accounts and two who had accounts once
_ADDR = {"admin": "192.0.2.10", "alex": "192.0.2.24", "watch": "192.0.2.31", "sam": "192.0.2.47",
         "jordan": "192.0.2.58"}


def _ids(ds, cid, kind, n):
    """Up to n real ids of a featured car's drives, charges or updates (made-up ones when the car is not there)."""
    h = ds.featured.get(cid)
    rows = getattr(h, kind, None) if h else None
    if rows:
        step = max(1, len(rows) // (n + 1))
        return [str(r.id) for r in rows[-1::-step][:n]]
    return [f"{cid}{kind[0]}{i}" for i in range(1, n + 1)]


def seed(ds, now):
    """The made-up log, oldest first: (at_ms, account, action, target, car, summary, address)."""
    d = lambda c: _ids(ds, c, "drives", 3)  # noqa: E731
    dr1, dr2, dr3 = d("c1")
    ch1, ch2 = _ids(ds, "c1", "charges", 2)
    (ch3,) = _ids(ds, "c3", "charges", 1)
    (up1,) = _ids(ds, "c2", "updates", 1)
    S = None  # the server itself
    e = [
        # long ago: the server is set up and filled
        (400 * DAY + 2 * HOUR, S, "setup-code-made", None, None, "Made a set-up code for the first admin."),
        (400 * DAY + HOUR, S, "setup-code-wrong", None, None, "A wrong set-up code was given."),
        (400 * DAY + 50 * MIN, "admin", "setup", "account:a1", None, "Made the first admin account admin."),
        (400 * DAY + 40 * MIN, "admin", "account-created", "account:a2", None, "Made the account watch (watch, sees Comet)."),
        (400 * DAY + 30 * MIN, "admin", "token-created", "token:c1:t11", "c1", "Made the upload token t11 for Comet."),
        (399 * DAY, "admin", "token-revoked", "token:c1:t11", "c1", "Revoked the upload token t11 of Comet."),
        (399 * DAY - HOUR, "admin", "token-created", "token:c1:t12", "c1", "Made the upload token t12 for Comet."),
        (398 * DAY, "admin", "account-created", "account:a3", None, "Made the account alex (admin, sees every car)."),
        (397 * DAY, "admin", "certificate-made", "server:certificate", None,
         "Made a new certificate for localhost, sentry.home.example; its SHA-256 starts 4F:1A:9C:02."),
        (396 * DAY, "admin", "import-created", "import:im1", None, "Started uploading teslamate-backup-atlas.sql.xz (73610 bytes)."),
        (396 * DAY - 10 * MIN, "admin", "import-uploaded", "import:im1", None, "Uploaded every file of teslamate-backup-atlas.sql.xz; checking it."),
        (396 * DAY - 20 * MIN, "admin", "import-cars", "import:im1", None, "Matched the imported cars: Atlas to Atlas, Old Roadster left out."),
        (396 * DAY - 30 * MIN, "admin", "import-started", "import:im1", None, "Started importing teslamate-backup-atlas.sql.xz."),
        (395 * DAY + HOUR, S, "import-rows-moved", None, "c2",
         "Moved 1840 rows (11 drives, 3 charges), with the 1 cost and purpose entered on them, from Comet to Maple: "
         "the import TeslaFi-2025.zip matched them to the wrong car."),
        (395 * DAY + 50 * MIN, "admin", "import-rows-ignored", "import-hold:h0", None,
         "Left aside 40 rows (1 drive) that wait for a car (kept)."),
        (395 * DAY, "admin", "import-discarded", "import:im0", None, "Discarded the import teslafi-export.csv (failed). History already imported stays."),
        (300 * DAY, "admin", "account-created", "account:a7", None, "Made the account sam (watch, sees Maple)."),
        (240 * DAY, "admin", "account-created", "account:a8", None, "Made the account jordan (admin, sees every car)."),
        (200 * DAY, "admin", "account-removed", "account:a7", None, "Removed the account sam."),
        # weeks ago: cars, places, integrations
        (62 * DAY, "jordan", "tesla-account-added", "tesla:ta1", None, "Added the Tesla account Family account."),
        (62 * DAY - 5 * MIN, "jordan", "tesla-car-matched", "tesla:ta1:v1", "c1", "Matched Tesla car Comet to Comet."),
        (62 * DAY - 6 * MIN, "jordan", "tesla-poll-changed", "settings:tesla", None, "Set how often the Tesla API is asked: driving every 2.5 s, charging every 5 s, idle every 60 s, asleep (the list) every 30 s."),
        (58 * DAY, "jordan", "tesla-account-paused", "tesla:ta1", None, "Paused the Tesla account Family account."),
        (58 * DAY - HOUR, "jordan", "tesla-account-resumed", "tesla:ta1", None, "Resumed the Tesla account Family account."),
        (57 * DAY, "jordan", "tesla-account-changed", "tesla:ta1", None, "Renamed the Tesla account Family account."),
        (56 * DAY, S, "tesla-sign-in-needed", "tesla:ta1", None, "The Tesla account Family account needs signing in again."),
        (56 * DAY - 2 * HOUR, "jordan", "tesla-token-replaced", "tesla:ta1", None, "Replaced the token of the Tesla account Family account."),
        (55 * DAY, S, "tesla-account-error", "tesla:ta1", None, "The Tesla account Family account keeps failing: Tesla answered 503."),
        (55 * DAY - 3 * HOUR, S, "tesla-rate-limited", "tesla:ta1", None, "Tesla is limiting requests for the Tesla account \"Family account\", next try 2 minutes later. This server sets no limit of its own; it waits as long as Tesla asks."),
        (54 * DAY, "jordan", "tesla-account-removed", "tesla:ta9", None, "Removed the Tesla account Old account."),
        (50 * DAY, "admin", "teslafi-account-added", "teslafi:fa1", None, "Added the TeslaFi account TeslaFi."),
        (50 * DAY - 5 * MIN, "admin", "teslafi-car-matched", "teslafi:fa1:v1", "c3", "Matched TeslaFi car Made-up TeslaFi car to Atlas."),
        (50 * DAY - 6 * MIN, "admin", "teslafi-poll-changed", "settings:teslafi", None, "Changed how often TeslaFi is asked to often."),
        (49 * DAY, "admin", "teslafi-account-paused", "teslafi:fa1", None, "Paused the TeslaFi account TeslaFi."),
        (49 * DAY - HOUR, "admin", "teslafi-account-resumed", "teslafi:fa1", None, "Resumed the TeslaFi account TeslaFi."),
        (48 * DAY, "admin", "teslafi-account-changed", "teslafi:fa1", None, "Renamed the TeslaFi account TeslaFi."),
        (47 * DAY, S, "teslafi-token-refused", "teslafi:fa1", None, "TeslaFi refused the token of the TeslaFi account TeslaFi."),
        (47 * DAY - HOUR, "admin", "teslafi-token-replaced", "teslafi:fa1", None, "Replaced the token of the TeslaFi account TeslaFi."),
        (45 * DAY, S, "teslafi-account-error", "teslafi:fa1", None, "The TeslaFi account TeslaFi keeps failing: TeslaFi answered 502."),
        (44 * DAY, "admin", "teslafi-account-removed", "teslafi:fa9", None, "Removed the TeslaFi account Second TeslaFi."),
        (42 * DAY, "alex", "teslamate-local-added", "teslamate:local", None, "Added the local TeslaMate (MQTT 192.0.2.40:1883)."),
        (42 * DAY - 2 * MIN, "alex", "teslamate-local-tested", "teslamate:local", None, "Tested the local TeslaMate: connected, saw 3 cars."),
        (42 * DAY - 4 * MIN, "alex", "teslamate-local-car-matched", "teslamate:local", "c1", "Matched TeslaMate car Comet to Comet."),
        (41 * DAY, "alex", "teslamate-local-changed", "teslamate:local", None, "Changed the local TeslaMate's settings: database catch-up on."),
        (40 * DAY, S, "teslamate-local-connection-refused", "teslamate:local", None, "Local TeslaMate: the MQTT broker refused the connection."),
        (40 * DAY - 3 * HOUR, S, "teslamate-local-auth-failed", "teslamate:local", None, "Local TeslaMate: the MQTT broker refused the user name or password."),
        (39 * DAY, S, "teslamate-local-pin-mismatch", "teslamate:local", None, "Local TeslaMate: the broker's certificate is not the pinned one."),
        (38 * DAY, S, "teslamate-local-quiet", "teslamate:local", None, "Local TeslaMate: the MQTT broker has sent nothing for 6 hours."),
        (37 * DAY, S, "teslamate-local-catchup-failed", "teslamate:local", None, "Local TeslaMate: the database catch-up failed."),
        (36 * DAY, "alex", "teslamate-local-paused", "teslamate:local", None, "Paused the local TeslaMate: nothing is read from it."),
        (36 * DAY - HOUR, "alex", "teslamate-local-resumed", "teslamate:local", None, "Resumed the local TeslaMate."),
        (35 * DAY, "alex", "teslamate-local-removed", "teslamate:local", None, "Removed the local TeslaMate: the history already stored stays."),
        (34 * DAY, "alex", "teslamate-local-added", "teslamate:local", None, "Added the local TeslaMate (MQTT 192.0.2.40:1883)."),
        (33 * DAY, "admin", "mqtt-changed", "settings:mqtt", None, "Changed the MQTT settings: publishing on, broker 192.0.2.40:1883."),
        (33 * DAY - MIN, "admin", "mqtt-tested", "settings:mqtt", None, "Tested the MQTT broker: connected."),
        (2 * DAY - 5 * HOUR, S, "can-logs-rechecked", "car:c2", "c2", "Checked 12 CAN logs of Maple that had failed their check again, by this version's rules: 9 verified now"),
        (2 * DAY - 3 * HOUR, "admin", "can-logs-unsigned-used", "car:c1", "c1", "Used 9 unsigned CAN logs of Comet that had failed their check only for being unsigned (6 from before the app signed logs, 1 with no sidecar, 1 the app said were not signed, 1 the app could not sign); bad and other-key logs are never used"),
        (2 * DAY - 2 * HOUR, "admin", "can-logs-unsigned-stopped", "car:c1", "c1", "Stopped using 9 unsigned CAN logs of Comet (6 from before the app signed logs, 1 with no sidecar, 1 the app said were not signed, 1 the app could not sign)"),
        (2 * DAY - 4 * HOUR, S, "can-logs-missing", "car:c2", "c2", "Found 3 CAN logs of Maple missing on the server; asked the car to send them again"),
        (30 * DAY, "admin", "car-changed", "car:c2", "c2", "Changed Maple's time zone to America/Chicago."),
        (30 * DAY - HOUR, "admin", "car-settings-changed", "car:c3", "c3", "Changed Atlas's battery size when new to 87 kWh."),
        (29 * DAY, "admin", "car-rebuild", "car:c1", "c1", "Rebuilt Comet's drives, charges, states and visits from the stored logs (the whole history)."),
        (28 * DAY, "admin", "car-full-rebuild", "car:c1", "c1", "Started a full rebuild of Comet: everything worked out made again from the stored data, beside the history in use."),
        (27 * DAY, "admin", "car-gps-layout", "car:c1", "c1", "Set how Comet's Screenmate GPS frames are read (lat 0|28@1- (1e-6,0); lon 28|28@1- (1e-6,16.777216)); its history is rebuilt."),
        (29 * DAY - HOUR, "admin", "car-overlap", "car:c3", "c3", "Atlas shows live (SMO) history from Mar 2 to Mar 9."),
        (28 * DAY, "admin", "key-assigned", "key:c1:cbk_5b2e9f13", "c1", "Moved the CAN Bridge key cbk_5b2e9f13 to Comet."),
        (28 * DAY - MIN, "admin", "key-unassigned", "key:c2:cbk_5b2e9f13", "c2", "Moved the CAN Bridge key cbk_5b2e9f13 from Maple to Comet."),
        (28 * DAY - 2 * MIN, "admin", "key-preferred", "key:c1:cbk_8d41c0a2", "c1", "Made the CAN Bridge key cbk_8d41c0a2 Comet's preferred one."),
        (28 * DAY - 3 * MIN, "admin", "can-key-renamed", "key:c1:cbk_5b2e9f13", "c1", "Named the CAN Bridge key cbk_5b2e9f13 \u201cPi logger\u201d."),
        (27 * DAY, "admin", "token-created", "token:t40", None, "Made the upload token t40 for a new car."),
        (27 * DAY - HOUR, "admin", "token-revoked", "token:t40", None, "Revoked the upload token t40 of a new car."),
        (26 * DAY, "alex", "place-created", "place:p20", None, "Added the place Corner Bakery (12 drives and 0 charges now carry its name)."),
        (26 * DAY - HOUR, "alex", "place-changed", "place:p20", None, "Changed the radius of the place Corner Bakery to 80 m."),
        (25 * DAY, "alex", "place-repriced", "place:p1", None, "Repriced 14 past charges at Home."),
        (25 * DAY - 2 * HOUR, S, "place-invoice-priced", "place:p36", None, "Priced the place Supercharger Schaumburg from Supercharger invoices: 0.42 USD a kWh from 2025-10-12, the median of 8 recent sessions."),
        (25 * DAY - HOUR, "alex", "place-deleted", "place:p20", None, "Removed the place Corner Bakery (12 drives lost the name)."),
        (24 * DAY, "alex", "update-version", f"update:c2:{up1}", "c2", "Set the software version of an update of Maple to 2026.32.6."),
        (21 * DAY, "admin", "role-changed", "account:a2", None, "Made watch a watch account."),
        (21 * DAY - MIN, "admin", "cars-changed", "account:a2", None, "watch now sees Comet."),
        (20 * DAY, "alex", "two-step-on", "account:a3", None, "Turned two-step sign-in on for alex."),
        (20 * DAY - MIN, "alex", "recovery-codes-new", "account:a3", None, "Made new recovery codes for alex."),
        (19 * DAY, "admin", "two-step-policy", "settings:server", None, "Two-step sign-in is now needed from outside the home network."),
        (18 * DAY, "jordan", "two-step-off", "account:a8", None, "Turned two-step sign-in off for jordan."),
        (17 * DAY, "jordan", "password-changed", "account:a8", None, "Changed the password of jordan."),
        (16 * DAY, "admin", "account-changed", "account:a8", None, "Renamed the account jordan."),
        (15 * DAY, "admin", "account-removed", "account:a8", None, "Removed the account jordan."),
        (14 * DAY, "admin", "ui-pack-installed", "ui-pack:sunrise", None, "Installed the Sunrise design (version 1.2.0)."),
        (14 * DAY - HOUR, "admin", "ui-pack-chosen", "ui-pack:horizon", None, "Chose the Horizon design."),
        (13 * DAY, "admin", "ui-pack-removed", "ui-pack:sunrise", None, "Removed the Sunrise design."),
        (12 * DAY, "admin", "settings-changed", "settings:server", None, "Changed the server settings: default time zone, host names."),
        (11 * DAY, "admin", "recovery-key-shown", "server:recovery-key", None, "Showed the recovery key from its file again."),
        (11 * DAY - MIN, "admin", "recovery-key-new", "server:recovery-key", None, "Made a new recovery key; the old one stopped working."),
        (11 * DAY - 2 * MIN, "admin", "recovery-key-saved", "server:recovery-key", None, "Said the recovery key is saved; its file was removed."),
        (10 * DAY, "admin", "export-made", "export:ex5", None, "Made an export file of the whole server."),
        (10 * DAY - HOUR, "admin", "export-downloaded", "export:ex5", None, "Downloaded the export file."),
        (10 * DAY - 2 * HOUR, "admin", "export-removed", "export:ex5", None, "Removed the export file."),
        (9 * DAY, "admin", "move-code-made", "move:mv3", None, "Made a move code, usable on the home network."),
        (9 * DAY - HOUR, "admin", "move-cancelled", "move:mv3", None, "Cancelled the move code."),
        (9 * DAY - 2 * HOUR, "admin", "move-undone", "move:mv2", None, "Undid the move: this server takes uploads again."),
        (8 * DAY + 3 * HOUR, S, "setup-restore-started", "move:rs1", None, "Started restoring sentry-mate-2026-10-01.smexport."),
        (8 * DAY + 2 * HOUR, S, "setup-restore-cancelled", "move:rs1", None, "Gave up restoring sentry-mate-2026-10-01.smexport."),
        (8 * DAY, "admin", "change", None, None, "Changed the server's backup route"),
        # this week: day-to-day use
        (7 * DAY, "alex", "trip-saved", "trip:tr7", "c1", "Saved the trip Weekend at the coast."),
        (7 * DAY - HOUR, "alex", "trip-changed", "trip:tr7", "c1", "Renamed the trip Weekend at the coast."),
        (6 * DAY, "alex", "trip-removed", "trip:tr7", "c1", "Removed the trip Weekend at the coast."),
        (6 * DAY - HOUR, "admin", "drive-purpose-set", f"drive:c1:{dr1}", "c1", "Set the purpose of a drive of Comet to business."),
        (6 * DAY - 2 * HOUR, "admin", "drive-purpose-removed", f"drive:c1:{dr2}", "c1", "Removed the purpose of a drive of Comet."),
        (5 * DAY, "admin", "purpose-rule-created", "car:c1", "c1", "Added a purpose rule for Comet: Home to Work on weekdays is commute."),
        (5 * DAY - HOUR, "admin", "purpose-rule-removed", "car:c1", "c1", "Removed a purpose rule of Comet."),
        (4 * DAY, "admin", "charge-cost-entered", f"charge:c1:{ch1}", "c1", "Entered a cost for a charge of Comet: 12.40 USD."),
        (4 * DAY - HOUR, "admin", "charge-cost-removed", f"charge:c3:{ch3}", "c3", "Removed the entered cost of a charge of Atlas; the place price applies."),
        (3 * DAY + 2 * HOUR, "watch", "sign-in-failed", None, None, "Wrong password for watch."),
        (3 * DAY + HOUR, "watch", "sign-in", "session:s7a01", None, "Signed in."),
        (3 * DAY, "watch", "preferences-changed", "account:a2", None, "Changed: text size, units."),
        (3 * DAY - HOUR, "watch", "sign-out", "session:s7a01", None, "Signed out."),
        (3 * DAY - 2 * HOUR, "watch", "signed-out", "session:s7a02", None, "Signed out: two-step sign-in is now required for this account."),
        (120 * DAY, S, "setup-move-started", "move:mj1", None, "Started moving the data here from 192.0.2.40."),
        (120 * DAY - HOUR, S, "setup-move-cancelled", "move:mj1", None, "Gave up moving the data here."),
        (119 * DAY, S, "setup-move-started", "move:mj2", None, "Started moving the data here from 192.0.2.40."),
        (119 * DAY - HOUR, S, "setup-move-resumed", "move:mj2", None, "Carried on moving the data here from 192.0.2.40."),
        (119 * DAY - 2 * HOUR, S, "setup-move-finished-forced", "move:mj2", None, "Finished the move with the recovery key."),
        (2 * DAY + 5 * HOUR, "watch", "sign-in-refused", "account:a2", None, "Refused a sign-in from outside for an account without two-step sign-in."),
        (2 * DAY + 4 * HOUR, "alex", "two-step-failed", "account:a3", None, "Wrong two-step code for alex."),
        (2 * DAY + 3 * HOUR, "alex", "sign-in", "session:s7b02", None, "Signed in."),
        (2 * DAY + 2 * HOUR, "alex", "confirm-failed", "account:a3", None, "Wrong password given again to confirm a change."),
        (2 * DAY, "alex", "session-revoked", "session:s6c03", None, "Ended a session (Safari on iPad)."),
        (2 * DAY - HOUR, "alex", "sessions-revoked", "account:a3", None, "Ended 2 other sessions."),
        (DAY + 3 * HOUR, "admin", "drive-purpose-set", f"drive:c1:{dr3}", "c1", "Set the purpose of a drive of Comet to private."),
        (DAY + 2 * HOUR, "admin", "charge-cost-entered", f"charge:c1:{ch2}", "c1", "Entered a cost for a charge of Comet: 3.10 USD."),
        (DAY + HOUR, "admin", "storage-compacted", "server:storage", None, "Started packing the stored data (Compact now)."),
        (DAY, S, "release-checked", "server:release", None, "Checked GitHub for a newer build."),
        (DAY - HOUR, "admin", "teslamate-local-car-matched", "teslamate:local", "c2", "Matched TeslaMate car Maple to Maple."),
        (20 * HOUR, "admin", "backup-made", "backup:b1", None, "Made a backup."),  # a code this list does not know
        (3 * HOUR, "admin", "release-checked", "server:release", None, "Checked GitHub for a newer build."),
        (2 * HOUR, "admin", "sign-in", "session:s7c04", None, "Signed in."),
    ]
    # Supercharger invoices: only once the code list has them (the list is written from the real server's table)
    e += [x for x in [
        (61 * DAY, "jordan", "tesla-invoices-changed", "settings:tesla-invoices", None, "Turned Tesla Supercharger invoices on."),
        (49 * DAY - HOUR, "admin", "teslafi-invoices-changed", "settings:teslafi-invoices", None, "Turned TeslaFi Supercharger invoices on."),
        (48 * DAY, "admin", "teslafi-invoices-changed", "settings:teslafi-invoices", None, "Turned TeslaFi Supercharger invoices off."),
        (11 * DAY, "admin", "invoices-downloaded", None, None, "Downloaded 41 Supercharger invoices as one file."),
    ] if x[2] in GROUP_OF]
    return [(now - ago, who, action, target, car, summary, _ADDR.get(who) if who else None)
            for ago, who, action, target, car, summary in e]
