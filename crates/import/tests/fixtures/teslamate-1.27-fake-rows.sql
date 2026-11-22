-- Fake rows for an older TeslaMate (1.27, 2023): its tokens table is still in the public schema, and it has no
-- ascent/descent, no LFP setting and no private schema. One made-up car (fake VIN, serial 000002), coordinates around
-- the same made-up spot in the Atlantic, a fake "token" that is not a real one.

BEGIN;

INSERT INTO car_settings (id, suspend_min, suspend_after_idle_min, req_not_unlocked, free_supercharging, use_streaming_api)
VALUES (1, 21, 15, false, true, true);

INSERT INTO cars (id, eid, vid, model, efficiency, inserted_at, updated_at, vin, name, trim_badging, settings_id, exterior_color, wheel_type, display_priority, marketing_name)
VALUES (1, 1000003, 2000003, 'S', 0.190, '2023-01-01 00:00:00', '2023-01-01 00:00:00', '5YJSA1E26HF000002', 'Old Ess', '90D', 1, 'Red', 'Slipstream19', 1, NULL);

INSERT INTO addresses (id, display_name, latitude, longitude, name, road, city, country, inserted_at, updated_at, osm_id, osm_type)
VALUES (1, 'Made-up Road, Testville', 12.300000, -45.600000, NULL, 'Made-up Road', 'Testville', 'Nowhere', '2023-05-01 10:00:00', '2023-05-01 10:00:00', 1, 'way');

INSERT INTO geofences (id, name, latitude, longitude, radius, inserted_at, updated_at, cost_per_unit, session_fee, billing_type)
VALUES (1, 'Home', 12.300000, -45.600000, 3000, '2023-01-01 00:00:00', '2023-01-01 00:00:00', 0.3000, NULL, 'per_kwh');

INSERT INTO drives (id, start_date, car_id, start_address_id)
VALUES (1, '2023-05-01 10:00:00', 1, 1);

INSERT INTO positions (id, date, latitude, longitude, speed, power, odometer, ideal_battery_range_km, battery_level, outside_temp, elevation, car_id, drive_id, inside_temp, est_battery_range_km, rated_battery_range_km, usable_battery_level, tpms_pressure_fl, tpms_pressure_fr, tpms_pressure_rl, tpms_pressure_rr)
VALUES (1, '2023-05-01 10:00:00.5', 12.300000, -45.600000, 0, 5, 50000.0, 320.00, 70, 18.0, 100, 1, 1, 22.0, 300.00, 310.00, 69, 2.9, 2.9, 2.9, 2.9),
       (2, '2023-05-01 10:05:00.5', 12.302000, -45.602000, 60, 25, 50004.2, 316.00, 69, 18.5, 104, 1, 1, 22.0, 296.00, 306.00, 68, 2.9, 2.9, 2.9, 2.9);

UPDATE drives SET end_date = '2023-05-01 10:05:00.5', speed_max = 60, power_max = 25, power_min = 5, start_km = 50000.0,
       end_km = 50004.2, distance = 4.2, duration_min = 5, start_rated_range_km = 310.00, end_rated_range_km = 306.00,
       start_position_id = 1, end_position_id = 2
 WHERE id = 1;

INSERT INTO states (id, state, start_date, end_date, car_id)
VALUES (1, 'online', '2023-05-01 09:55:00', '2023-05-01 10:10:00', 1),
       (2, 'asleep', '2023-05-01 10:10:00', NULL, 1);

INSERT INTO updates (id, start_date, end_date, version, car_id)
VALUES (1, '2023-04-01 02:00:00', '2023-04-01 02:30:00', '2023.12.1 0123456789ab', 1);

-- Not a real token: a marker the importer must never read (TeslaMate 1.27 keeps tokens in public.tokens).
INSERT INTO tokens (id, inserted_at, updated_at, refresh, access)
VALUES (1, '2023-01-01 00:00:00', '2023-01-01 00:00:00', 'FAKE-NOT-A-TOKEN-refresh'::bytea, 'FAKE-NOT-A-TOKEN-access'::bytea);

COMMIT;
