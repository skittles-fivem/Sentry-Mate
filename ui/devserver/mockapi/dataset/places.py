"""Made-up places for the dev dataset.

Home, work and the other named spots are invented; they sit on public streets in public
neighbourhoods (many reuse the spots in ui/packs/*/web/mock-api.js so the designs keep familiar
data). Chargers are named after the town only. No place is anyone's real home.
"""

from __future__ import annotations

US = ("US", "United States")
CA = ("CA", "Canada")


def _addr(short, street, neighbourhood, locality, region, country):
    return {
        "short": short, "street": street, "neighbourhood": neighbourhood, "locality": locality,
        "region": region, "country_code": country[0], "country": country[1], "source": "offline",
    }


def _price(currency, per_kwh, rates=None, per_minute=None, session_fee=None, tiers=None):
    return {"currency": currency, "per_kwh": per_kwh, "per_minute": per_minute,
            "rates": rates or [], "session_fee": session_fee, "tiers": tiers}


# Made-up block prices: the first 1,000 kWh of a billing cycle at 0.16, the next 1,000 at 0.10, the rest at 0.05;
# the cycle starts on the 1st, and the house on the same meter uses about 900 kWh a cycle before the car.
_HOME_CHI_TIERS = {"steps": [{"from_kwh": 0, "per_kwh": 0.16}, {"from_kwh": 1000, "per_kwh": 0.10},
                             {"from_kwh": 2000, "per_kwh": 0.05}], "cycle_day": 1, "base_kwh": 900}


_HOME_SF_RATES = [
    {"name": "Off-peak", "per_kwh": 0.24, "per_minute": None, "days": "mon-sun", "from": "00:00", "to": "07:00"},
    {"name": "Peak", "per_kwh": 0.45, "per_minute": None, "days": "mon-fri", "from": "16:00", "to": "21:00"},
]
_SC_RATES = [
    {"name": "Busy hours", "per_kwh": 0.56, "per_minute": None, "days": "mon-sun", "from": "16:00", "to": "21:00"},
    {"name": "Night", "per_kwh": 0.32, "per_minute": None, "days": "mon-sun", "from": "00:00", "to": "06:00"},
]

LA = "America/Los_Angeles"
CHI = "America/Chicago"
DET = "America/Detroit"
TOR = "America/Toronto"


def _p(pid, name, kind, lat, lon, radius, tz, address, price=None, created="2025-08-20"):
    return {"id": pid, "name": name, "kind": kind, "lat": lat, "lon": lon, "radius_m": radius,
            "time_zone": tz, "address": address, "price": price, "created": created}


def _dated(price, *later, invoice=None):
    """Dated prices: `price` from the start, each (from, price) of `later` from its day, and an
    invoice price (from, price, sessions) worked out from made-up Supercharger invoices."""
    out = [{"from": None, "source": "typed", "price": price}]
    out += [{"from": f, "source": "typed", "price": p} for f, p in later]
    if invoice:
        f, p, n = invoice
        out.append({"from": f, "source": "invoice", "price": p, "invoice": {"sessions": n}})
    return out


PLACES = [
    # --- San Francisco (car c1, a few shared with c3)
    _p("p1", "Home", "home", 37.75080, -122.43300, 60, LA,
       _addr("Sanchez St, Noe Valley", "Sanchez St", "Noe Valley", "San Francisco", "California", US),
       _price("USD", 0.31, _HOME_SF_RATES)),
    _p("p2", "Work", "work", 37.76800, -122.39250, 150, LA,
       _addr("Third St, Mission Bay", "Third St", "Mission Bay", "San Francisco", "California", US)),
    _p("p3", "Supercharger Mission Bay", "charger", 37.76790, -122.39330, 60, LA,
       _addr("Owens St, Mission Bay", "Owens St", "Mission Bay", "San Francisco", "California", US),
       _price("USD", 0.48, _SC_RATES)),
    _p("p4", "Tartine Manufactory", "other", 37.76160, -122.41160, 50, LA,
       _addr("Alabama St, Mission District", "Alabama St", "Mission District", "San Francisco", "California", US)),
    _p("p5", "Costco", "other", 37.77000, -122.41050, 120, LA,
       _addr("10th St, SoMa", "10th St", "SoMa", "San Francisco", "California", US)),
    _p("p6", "Ferry Building", "other", 37.79540, -122.39370, 100, LA,
       _addr("The Embarcadero, Financial District", "The Embarcadero", "Financial District", "San Francisco", "California", US)),
    _p("p7", "Crissy Field", "other", 37.80380, -122.46650, 150, LA,
       _addr("Mason St, Presidio", "Mason St", "Presidio", "San Francisco", "California", US)),
    _p("p8", "Ocean Beach", "other", 37.76020, -122.50850, 200, LA,
       _addr("Great Hwy, Outer Sunset", "Great Hwy", "Outer Sunset", "San Francisco", "California", US)),
    _p("p9", "Golden Gate Park", "other", 37.77100, -122.46870, 250, LA,
       _addr("John F Kennedy Dr, Inner Richmond", "John F Kennedy Dr", "Inner Richmond", "San Francisco", "California", US)),
    _p("p10", "Trader Joe's", "other", 37.78260, -122.44660, 60, LA,
       _addr("Masonic Ave, Laurel Heights", "Masonic Ave", "Laurel Heights", "San Francisco", "California", US)),
    _p("p11", "Stanford Shopping Center", "other", 37.44340, -122.17120, 200, LA,
       _addr("Sand Hill Rd, Palo Alto", "Sand Hill Rd", None, "Palo Alto", "California", US),
       _price("USD", 0.35, session_fee=1.00)),
    _p("p12", "Muir Woods", "other", 37.89700, -122.58110, 200, LA,
       _addr("Muir Woods Rd, Mill Valley", "Muir Woods Rd", None, "Mill Valley", "California", US)),
    _p("p13", "Half Moon Bay", "other", 37.46360, -122.42860, 250, LA,
       _addr("Main St, Half Moon Bay", "Main St", None, "Half Moon Bay", "California", US)),
    _p("p14", "Fourth Street", "other", 37.86900, -122.30100, 120, LA,
       _addr("Fourth St, West Berkeley", "Fourth St", "West Berkeley", "Berkeley", "California", US)),
    _p("p15", "Supercharger Daly City", "charger", 37.70500, -122.46900, 60, LA,
       _addr("Serramonte Blvd, Daly City", "Serramonte Blvd", None, "Daly City", "California", US),
       _price("USD", 0.46, _SC_RATES)),
    _p("p16", "Supercharger Burlingame", "charger", 37.59000, -122.34600, 60, LA,
       _addr("Adrian Rd, Burlingame", "Adrian Rd", None, "Burlingame", "California", US),
       _price("USD", 0.47, _SC_RATES)),
    _p("p17", "Supercharger Emeryville", "charger", 37.83900, -122.29300, 60, LA,
       _addr("Shellmound St, Emeryville", "Shellmound St", None, "Emeryville", "California", US),
       _price("USD", 0.49, _SC_RATES)),
    _p("p18", "Service centre", "service", 37.77350, -122.40950, 80, LA,
       _addr("Mission St, SoMa", "Mission St", "SoMa", "San Francisco", "California", US)),
    _p("p19", "Supercharger Gilroy", "charger", 37.02470, -121.56540, 80, LA,
       _addr("Leavesley Rd, Gilroy", "Leavesley Rd", None, "Gilroy", "California", US),
       _price("USD", 0.44, _SC_RATES)),
    _p("p20", "Supercharger Kettleman City", "charger", 35.98870, -119.96140, 100, LA,
       _addr("Bernard Dr, Kettleman City", "Bernard Dr", None, "Kettleman City", "California", US),
       _price("USD", 0.43, _SC_RATES)),
    _p("p21", "Supercharger Lebec", "charger", 34.98760, -118.94460, 80, LA,
       _addr("Frazier Mountain Park Rd, Lebec", "Frazier Mountain Park Rd", None, "Lebec", "California", US),
       _price("USD", 0.45, _SC_RATES)),
    _p("p22", "Hotel Santa Monica", "other", 34.01700, -118.49700, 80, LA,
       _addr("Ocean Ave, Santa Monica", "Ocean Ave", None, "Santa Monica", "California", US)),
    _p("p23", "Griffith Observatory", "other", 34.11840, -118.30040, 150, LA,
       _addr("W Observatory Rd, Los Feliz", "W Observatory Rd", "Los Feliz", "Los Angeles", "California", US)),
    # Overlaps Home on purpose: the nearest centre wins, so Home keeps its drives.
    _p("p24", "Noe Valley", "other", 37.75120, -122.43150, 400, LA,
       _addr("24th St, Noe Valley", "24th St", "Noe Valley", "San Francisco", "California", US)),
    # --- Chicago (car c2) and its road trip to Toronto
    _p("p30", "Home", "home", 41.92310, -87.70930, 60, CHI,
       _addr("N Kedzie Ave, Logan Square", "N Kedzie Ave", "Logan Square", "Chicago", "Illinois", US),
       _price("USD", None, tiers=_HOME_CHI_TIERS)),
    _p("p31", "Work garage", "work", 41.89240, -87.63450, 120, CHI,
       _addr("N LaSalle Dr, River North", "N LaSalle Dr", "River North", "Chicago", "Illinois", US),
       _price("USD", 0.30, session_fee=0.50)),
    _p("p32", "Grocery", "other", 41.92140, -87.65130, 80, CHI,
       _addr("N Clark St, Lincoln Park", "N Clark St", "Lincoln Park", "Chicago", "Illinois", US)),
    _p("p33", "Gym", "other", 41.90880, -87.67960, 60, CHI,
       _addr("N Milwaukee Ave, Wicker Park", "N Milwaukee Ave", "Wicker Park", "Chicago", "Illinois", US)),
    _p("p34", "Family", "other", 41.79430, -87.59600, 60, CHI,
       _addr("E 53rd St, Hyde Park", "E 53rd St", "Hyde Park", "Chicago", "Illinois", US)),
    _p("p35", "Supercharger West Loop", "charger", 41.88400, -87.65200, 60, CHI,
       _addr("W Randolph St, West Loop", "W Randolph St", "West Loop", "Chicago", "Illinois", US),
       _price("USD", 0.42, _SC_RATES)),
    _p("p36", "Supercharger Schaumburg", "charger", 42.03340, -88.08340, 80, CHI,
       _addr("E Golf Rd, Schaumburg", "E Golf Rd", None, "Schaumburg", "Illinois", US),
       _price("USD", 0.40, _SC_RATES)),
    _p("p37", "Supercharger Benton Harbor", "charger", 42.08660, -86.42060, 80, DET,
       _addr("Pipestone St, Benton Harbor", "Pipestone St", None, "Benton Harbor", "Michigan", US),
       _price("USD", 0.41, _SC_RATES)),
    _p("p38", "Supercharger Kalamazoo", "charger", 42.24700, -85.58910, 80, DET,
       _addr("S Westnedge Ave, Kalamazoo", "S Westnedge Ave", None, "Kalamazoo", "Michigan", US),
       _price("USD", 0.41, _SC_RATES)),
    _p("p39", "Supercharger Ann Arbor", "charger", 42.24200, -83.74800, 80, DET,
       _addr("Eisenhower Pkwy, Ann Arbor", "Eisenhower Pkwy", None, "Ann Arbor", "Michigan", US),
       _price("USD", 0.43, _SC_RATES)),
    _p("p40", "Supercharger London", "charger", 42.98490, -81.24530, 80, TOR,
       _addr("Wellington Rd, London", "Wellington Rd", None, "London", "Ontario", CA),
       _price("CAD", 0.62)),
    _p("p41", "Hotel Toronto", "other", 43.64500, -79.38500, 100, TOR,
       _addr("Front St W, Entertainment District", "Front St W", "Entertainment District", "Toronto", "Ontario", CA)),
    # --- East Bay (car c3)
    _p("p50", "Home", "home", 37.84400, -122.25100, 60, LA,
       _addr("College Ave, Rockridge", "College Ave", "Rockridge", "Oakland", "California", US),
       _price("USD", 0.29)),
    _p("p51", "Office", "work", 37.87000, -122.27000, 100, LA,
       _addr("Shattuck Ave, Downtown Berkeley", "Shattuck Ave", "Downtown Berkeley", "Berkeley", "California", US)),
    _p("p52", "Lake Merritt", "other", 37.80300, -122.25800, 200, LA,
       _addr("Lakeshore Ave, Grand Lake", "Lakeshore Ave", "Grand Lake", "Oakland", "California", US)),
    _p("p53", "Supercharger Walnut Creek", "charger", 37.90000, -122.06000, 80, LA,
       _addr("N Main St, Walnut Creek", "N Main St", None, "Walnut Creek", "California", US),
       _price("USD", 0.47, _SC_RATES)),
]

BY_ID = {p["id"]: p for p in PLACES}


def place_ref(pid):
    """The short { id, name } a row carries, or None."""
    if pid is None:
        return None
    p = BY_ID[pid]
    return {"id": p["id"], "name": p["name"]}


# Waypoints [lat, lon, elevation m] for the two road trips (along the interstates, made up).
ROAD_SF_LA = [
    [37.7508, -122.4330, 60], [37.7080, -122.4050, 20], [37.6000, -122.3800, 5], [37.4500, -122.1800, 10],
    [37.3300, -121.8900, 30], [37.1800, -121.6800, 100], [37.0247, -121.5654, 60], [36.9700, -121.3800, 70],
    [36.9300, -121.0600, 90], [36.8000, -120.7800, 60], [36.5500, -120.5200, 90], [36.2500, -120.2300, 80],
    [35.9887, -119.9614, 70], [35.7000, -119.6800, 90], [35.4000, -119.3500, 110], [35.1500, -119.0500, 170],
    [34.9876, -118.9446, 1060], [34.8300, -118.8800, 1250], [34.6500, -118.7500, 1000], [34.4800, -118.6200, 450],
    [34.3300, -118.5200, 380], [34.1600, -118.4700, 230], [34.0700, -118.4600, 90], [34.0170, -118.4970, 20],
]
ROAD_CHI_TOR = [
    [41.9231, -87.7093, 180], [41.8800, -87.6300, 180], [41.7000, -87.5300, 180], [41.6400, -87.2000, 190],
    [41.7000, -86.9000, 200], [41.8800, -86.6200, 200], [42.0866, -86.4206, 190], [42.1800, -86.0000, 230],
    [42.2470, -85.5891, 260], [42.2600, -85.1800, 280], [42.2600, -84.6000, 290], [42.2400, -84.1500, 280],
    [42.2420, -83.7480, 260], [42.3000, -83.3000, 190], [42.3200, -83.0400, 180], [42.3000, -82.6000, 190],
    [42.4200, -82.1800, 190], [42.7000, -81.7000, 230], [42.9849, -81.2453, 260], [43.1300, -80.7600, 300],
    [43.2000, -80.3000, 290], [43.4200, -79.8000, 120], [43.6000, -79.5500, 100], [43.6450, -79.3850, 80],
]


# made-up dated prices: a new Mission Bay price from next year, and a price from invoices at London
# (the typed price there covers every day, so it wins)
for _q in PLACES:
    if _q["id"] == "p3":
        _q["prices"] = _dated(_q["price"], ("2027-01-01", _price("USD", 0.52, _SC_RATES)))
    elif _q["id"] == "p40":
        _q["prices"] = _dated(_q["price"], invoice=("2026-09-04", _price("CAD", 0.64), 10))
