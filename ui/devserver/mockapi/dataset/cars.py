"""The three featured made-up cars: what they are and how their owners use them.

VINs are only ever kept as the 11-character prefix (the serial is never stored); these prefixes are
made up. Special days (road trips, a holiday, gaps, software changes) are given as days before the
"now" day, so the dataset works for any --now-ms.
"""

from __future__ import annotations

import math

from ..util import DAY, h01, local_hour


class Area:
    """Weather for a made-up area: yearly mean, seasonal and daily swings (°C)."""

    def __init__(self, name, mean, season, daily, noise, cold_snaps=()):
        self.name = name
        self.mean = mean
        self.season = season
        self.daily = daily
        self.noise = noise
        self.cold_snaps = cold_snaps  # (first day offset, last day offset, extra °C)


SF = Area("sf", 14.5, 3.0, 4.0, 2.0)
CHI = Area("chi", 10.0, 14.0, 5.0, 4.0, cold_snaps=((-300, -293, -14.0), (-285, -282, -8.0)))
EB = Area("eb", 15.5, 4.5, 5.0, 2.0)


def outside_c(car, t_ms, lat=None, lon=None):
    """Made-up outside temperature at a time (and place, for the road trips)."""
    area = car.area
    day = int(t_ms // DAY)
    doy = (day + 4) % 365  # epoch day 0 is 1 Jan; good enough for seasons
    season = -area.season * math.cos(2 * math.pi * (doy - 20) / 365)
    hour = local_hour(t_ms, car.time_zone)
    daily = -area.daily * math.cos(2 * math.pi * (hour - 3.5) / 24)
    noise = (h01("wx", area.name, day) - 0.5) * 2 * area.noise
    t = area.mean + season + daily + noise
    rel = day - car.now_day_index
    for a, b, extra in area.cold_snaps:
        if a <= rel <= b:
            t += extra
    if lat is not None and lon is not None:
        if area is SF and lon > -121.4 and lat < 37.1:  # inland valley and Los Angeles: warmer
            t += 4 + 0.6 * max(0.0, season + area.season) * 3
        if area is CHI and lon > -84.5:  # Michigan and Ontario: a touch cooler
            t -= 1.5
    return t


class CarSpec:
    def __init__(self, **kw):
        self.__dict__.update(kw)
        self.now_day_index = 0  # set by the dataset from now_ms

    # --- the pack over time (slowly degrading)
    def full_pack_kwh(self, t_ms):
        f = (t_ms - self.hist_start_ms) / max(1, self.now_ms - self.hist_start_ms)
        f = min(1.05, max(0.0, f))
        return self.full_start_kwh - (self.full_start_kwh - self.full_now_kwh) * f

    def usable_kwh(self, t_ms):
        return self.full_pack_kwh(t_ms) - self.buffer_kwh

    def full_range_km(self, t_ms):
        return self.usable_kwh(t_ms) * 1000 / self.rated_wh_per_km


def featured_specs():
    """Fresh specs for c1, c2 and c3 (the dataset fills in times)."""
    c1 = CarSpec(
        id="c1", n=1, name="Comet", model="Model Y", year=2025, trim="Long Range AWD", vin_prefix="7SAYGDEE0SF",
        profile="tesla-3y", time_zone="America/Los_Angeles", area=SF, chemistry="nca_nmc",
        full_new_kwh=78.4, full_start_kwh=78.1, full_now_kwh=76.6, buffer_kwh=3.0, full_range_new_km=526.0,
        rated_wh_per_km=143.0, ideal_factor=1.09, base_wh_per_km=152.0, mass_kg=2000, cda=0.60, crr=0.0095,
        charge_limit=80, home="p1", work="p2", home_ac=(240, 32, 1), dc_peak_kw=250, dc_knee=(0.12, 0.35),
        odo_start_km=2140.0, soc_start=72.0, tyre_rec_kpa=290, tyre_base=(292, 291, 290, 293),
        history_days=441, cooldown=0.012, asleep_kw=0.012, sentry_at_work=0.45, scheduled_charging=True,
        config={"trim": ("Long Range AWD", "can"), "drivetrain": ("AWD", "can"), "performance_package": (False, "can"),
                "pack_kwh_code": ("78", "can"), "wheel": ("19 in Gemini", "can"), "paint": ("Stealth Grey", "can"),
                "roof": ("Glass", "can"), "spoiler": ("None", "can"), "right_hand_drive": (False, "can"),
                "map_region": ("North America", "can"), "supercharging_access": ("Paid", "can")},
        updates=[(-286, "2026.2.6", "owner", "build_change"), (-49, "2026.32.5", "build_match", "build_change")],
        first_version=("2025.26.7", "owner"),
        special={"road_trip": (-137, -133), "holiday": (-238, -230)},
        gaps=[  # (days before now, local hour, minutes, reason, mode)
            (-378, 13.0, 26 * 60, "no_log", "moved"),
            (-268, 1.5, 300, "not_recorded", "parked"),
            (-181, 18.05, 520, "no_log", "drive_cut"),
            (-150, 3.0, 110, "failed_check", "parked"),
            (-86, 0.6, 430, "no_log", "charge_cut"),
            (-44, 14.0, 6, "dropped_frames", "parked"),
            (-12, 2.2, 4, "dropped_frames", "parked"),
        ],
    )
    c2 = CarSpec(
        id="c2", n=2, name="Maple", model="Model 3", year=2022, trim="RWD", vin_prefix="5YJ3E1EA0NF",
        profile="tesla-3y", time_zone="America/Chicago", area=CHI, chemistry="lfp",
        full_new_kwh=60.5, full_start_kwh=60.0, full_now_kwh=59.3, buffer_kwh=2.5, full_range_new_km=438.0,
        rated_wh_per_km=132.0, ideal_factor=1.07, base_wh_per_km=138.0, mass_kg=1760, cda=0.51, crr=0.0090,
        charge_limit=100, home="p30", work="p31", home_ac=(230, 16, 3), dc_peak_kw=170, dc_knee=(0.10, 0.45),
        odo_start_km=31500.0, soc_start=64.0, tyre_rec_kpa=290, tyre_base=(291, 292, 290, 291),
        history_days=441, cooldown=0.012, asleep_kw=0.010, sentry_at_work=0.0, scheduled_charging=False,
        config={"trim": ("RWD", "can"), "drivetrain": ("RWD", "can"), "performance_package": (False, "can"),
                "pack_kwh_code": ("60", "can"), "wheel": ("18 in Aero", "can"), "paint": ("Pearl White", "owner"),
                "roof": ("Glass", "can"), "spoiler": ("None", "can"), "right_hand_drive": (False, "can"),
                "map_region": ("North America", "can"), "supercharging_access": ("Paid", "can")},
        updates=[(-341, "2025.44.25", "owner", "build_change"), (-17, None, None, "build_change")],
        first_version=("2025.32.6", "owner"),
        special={"road_trip": (-95, -90), "leak_start": -21},
        gaps=[
            (-402, 9.0, 18 * 60, "no_log", "moved"),
            (-299, 3.0, 180, "not_recorded", "parked"),
            (-210, 14.5, 95, "failed_check", "parked"),
            (-120, 22.0, 5, "dropped_frames", "parked"),
            (-61, 12.0, 30 * 60, "no_log", "moved"),
        ],
    )
    c3 = CarSpec(
        id="c3", n=3, name="Atlas", model="Model S", year=2017, trim="90D", vin_prefix="5YJSA1E20HF",
        profile="tesla-sx-legacy", time_zone="America/Los_Angeles", area=EB, chemistry="nca_nmc",
        full_new_kwh=87.0, full_start_kwh=80.2, full_now_kwh=77.4, buffer_kwh=4.0, full_range_new_km=473.0,
        rated_wh_per_km=172.0, ideal_factor=1.12, base_wh_per_km=185.0, mass_kg=2200, cda=0.56, crr=0.0100,
        charge_limit=90, home="p50", work="p51", home_ac=(240, 40, 1), dc_peak_kw=120, dc_knee=(0.10, 0.30),
        odo_start_km=98200.0, soc_start=68.0, tyre_rec_kpa=310, tyre_base=None,
        history_days=900, cooldown=0.015, asleep_kw=0.11, sentry_at_work=0.0, scheduled_charging=False,
        config={"trim": ("90D", "vin"), "drivetrain": ("AWD", "vin"), "performance_package": (False, "vin"),
                "pack_kwh_code": ("90", "teslamate"), "wheel": ("21 in Arachnid", "owner"), "paint": ("Midnight Silver", "teslamate"),
                "roof": (None, None), "spoiler": (None, None), "right_hand_drive": (False, "vin"),
                "map_region": ("North America", "teslamate"), "supercharging_access": ("Free", "owner")},
        updates=[(-860, "2024.20.9", "teslamate", "teslamate"), (-800, "2024.32.7", "teslamate", "teslamate"),
                 (-625, "2025.8.3", "teslamate", "teslamate"), (-511, "2025.20.6", "teslamate", "teslamate"),
                 (-308, "2025.45.8", "build_match", "build_change")],
        first_version=("2024.14.11", "teslamate"),
        special={"can_from": -398, "teslamate_until": -393, "silent_from_h": 55.0},
        gaps=[
            (-350, 10.0, 240, "no_log", "parked"),
            (-240, 16.0, 26 * 60, "no_log", "moved"),
            (-160, 2.0, 90, "dropped_frames", "parked"),
            (-70, 13.0, 60, "failed_check", "parked"),
        ],
    )
    return [c1, c2, c3]
