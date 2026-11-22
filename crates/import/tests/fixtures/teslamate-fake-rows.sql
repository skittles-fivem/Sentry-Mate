-- Fake rows for the import tests, loaded into a database the official TeslaMate image made (see make-teslamate-dump.sh).
-- Everything here is made up: two cars with fake VINs (serial 000000 / 000001), coordinates around a made-up spot in
-- the middle of the Atlantic (12.3 N, 45.6 W), fake addresses, and a fake "token" that is not a real one. Rows are
-- written the way TeslaMate writes them (a drive is opened, gets positions, then is closed by an UPDATE), so the
-- dump has rows out of id order like a real one.

BEGIN;

INSERT INTO car_settings (id, suspend_min, suspend_after_idle_min, req_not_unlocked, free_supercharging, use_streaming_api, enabled, lfp_battery)
VALUES (1, 21, 15, false, false, true, true, false),
       (2, 21, 15, false, true, true, true, true);

INSERT INTO cars (id, eid, vid, model, efficiency, inserted_at, updated_at, vin, name, trim_badging, settings_id, exterior_color, spoiler_type, wheel_type, display_priority, marketing_name)
VALUES (1, 1000001, 2000001, '3', 0.153, '2024-01-01 00:00:00', '2024-01-01 00:00:00', '5YJ3E1EA0KF000000', 'Test Three', '74D', 1, 'MidnightSilver', 'None', 'Pinwheel18', 1, 'LR AWD'),
       (2, 1000002, 2000002, 'Y', 0.160, '2024-01-02 00:00:00', '2024-01-02 00:00:00', '7SAYGDEE0PF000001', 'Test Why', 'P74D', 2, NULL, NULL, NULL, 2, NULL);

INSERT INTO addresses (id, display_name, latitude, longitude, name, house_number, road, neighbourhood, city, county, postcode, state, state_district, country, raw, inserted_at, updated_at, osm_id, osm_type)
VALUES (1, '1, Made-up Road, Testville, Nowhere', 12.300000, -45.600000, NULL, '1', 'Made-up Road', NULL, 'Testville', NULL, '00000', NULL, NULL, 'Nowhere', '{"fake": true}', '2024-03-01 08:00:05', '2024-03-01 08:00:05', 1, 'node'),
       (2, E'Café "Test"\tTab\\Back\nslash, Second Street, Testville', 12.310000, -45.610000, E'Café "Test"\tTab\\Back\nslash', NULL, 'Second Street', NULL, 'Testville', NULL, NULL, NULL, NULL, 'Nowhere', NULL, '2024-03-01 08:10:05', '2024-03-01 08:10:05', 2, 'way'),
       (3, 'Testville Supercharger, Testville', 12.350000, -45.650000, 'Testville Supercharger', NULL, NULL, NULL, 'Testville', NULL, NULL, NULL, NULL, 'Nowhere', NULL, '2024-03-01 08:50:05', '2024-03-01 08:50:05', 3, 'node');

INSERT INTO geofences (id, name, latitude, longitude, radius, inserted_at, updated_at, cost_per_unit, session_fee, billing_type)
VALUES (1, 'Home', 12.300000, -45.600000, 25, '2024-01-01 00:00:00', '2024-01-01 00:00:00', 0.2500, NULL, 'per_kwh'),
       (2, 'Testville Supercharger', 12.350000, -45.650000, 100, '2024-01-01 00:00:00', '2024-01-01 00:00:00', 0.1000, 1.50, 'per_minute');

-- Car 1 drives from home to the cafe (drive 1); car 2 drives to the supercharger at the same time (drive 2), so
-- their positions interleave by id.
INSERT INTO drives (id, start_date, car_id, start_position_id, start_address_id, start_geofence_id)
VALUES (1, '2024-03-01 08:00:00.123456', 1, NULL, 1, 1),
       (2, '2024-03-01 08:00:30', 2, NULL, NULL, NULL);

INSERT INTO positions (id, date, latitude, longitude, speed, power, odometer, ideal_battery_range_km, battery_level, outside_temp, elevation, car_id, drive_id, inside_temp, est_battery_range_km, rated_battery_range_km, usable_battery_level, tpms_pressure_fl, tpms_pressure_fr, tpms_pressure_rl, tpms_pressure_rr)
SELECT 2 * i - 1,
       '2024-03-01 08:00:00.123456'::timestamp + make_interval(secs => 60 * i),
       12.300000 + 0.001 * i, -45.600000 - 0.001 * i,
       CASE WHEN i IN (0, 10) THEN 0 ELSE 40 + 5 * i END,
       CASE WHEN i = 10 THEN -12 ELSE 10 + i END,
       12000.0 + 0.85 * i, 400.00 - i, 80 - (i / 5), 12.5, 100 + i, 1, 1, 21.0,
       390.00 - i, 380.00 - 0.95 * i, 79 - (i / 5), 2.9, 2.9, 2.8, 2.8
FROM generate_series(1, 10) AS i;

INSERT INTO positions (id, date, latitude, longitude, speed, power, odometer, ideal_battery_range_km, battery_level, outside_temp, elevation, car_id, drive_id, inside_temp, est_battery_range_km, rated_battery_range_km, usable_battery_level, tpms_pressure_fl, tpms_pressure_fr, tpms_pressure_rl, tpms_pressure_rr)
SELECT 2 * i,
       '2024-03-01 08:00:30'::timestamp + make_interval(secs => 60 * i),
       12.340000 + 0.001 * i, -45.640000 - 0.001 * i,
       50, 20, 30000.0 + i, 200.00 - i, 30 - i, 11.0, NULL, 2, 2, NULL,
       NULL, 190.00 - i, NULL, NULL, NULL, NULL, NULL
FROM generate_series(1, 10) AS i;

UPDATE drives SET end_date = '2024-03-01 08:10:00.123456', outside_temp_avg = 12.5, speed_max = 85, power_max = 20,
       power_min = -12, start_ideal_range_km = 399.00, end_ideal_range_km = 390.00, start_km = 12000.85, end_km = 12008.5,
       distance = 7.65, duration_min = 9, inside_temp_avg = 21.0, end_address_id = 2, start_rated_range_km = 379.05,
       end_rated_range_km = 370.50, start_position_id = 1, end_position_id = 19, end_geofence_id = NULL, ascent = 9,
       descent = 0
 WHERE id = 1;

UPDATE drives SET end_date = '2024-03-01 08:10:30', outside_temp_avg = 11.0, speed_max = 50, power_max = 20,
       power_min = 20, start_km = 30001.0, end_km = 30010.0, distance = 9.0, duration_min = 9, end_address_id = 3,
       start_rated_range_km = 189.00, end_rated_range_km = 180.00, start_position_id = 2, end_position_id = 20,
       end_geofence_id = 2
 WHERE id = 2;

-- Car 2 charges at the supercharger (DC), car 1 at home overnight (AC). Car 2's second charge is still going.
INSERT INTO positions (id, date, latitude, longitude, speed, power, odometer, ideal_battery_range_km, battery_level, outside_temp, elevation, car_id, drive_id, rated_battery_range_km, usable_battery_level)
VALUES (21, '2024-03-01 09:00:00', 12.350000, -45.650000, NULL, -150, 30010.0, 190.00, 20, 10.5, 5, 2, NULL, 180.00, 20),
       (22, '2024-03-01 20:00:00', 12.300000, -45.600000, NULL, -11, 12008.5, 380.00, 76, 8.0, 110, 1, NULL, 360.00, 76),
       (23, '2024-03-03 10:00:00', 12.350000, -45.650000, NULL, -100, 30400.0, 150.00, 25, 9.0, 5, 2, NULL, 140.00, 25);

INSERT INTO charging_processes (id, start_date, car_id, position_id, address_id, geofence_id, start_battery_level, start_rated_range_km, start_ideal_range_km)
VALUES (1, '2024-03-01 09:00:00', 2, 21, 3, 2, 20, 180.00, 190.00),
       (2, '2024-03-01 20:00:00', 1, 22, 1, 1, 76, 360.00, 380.00);

INSERT INTO charges (id, date, battery_heater_on, battery_level, charge_energy_added, charger_actual_current, charger_phases, charger_pilot_current, charger_power, charger_voltage, fast_charger_present, conn_charge_cable, fast_charger_brand, fast_charger_type, ideal_battery_range_km, not_enough_power_to_heat, outside_temp, charging_process_id, battery_heater, battery_heater_no_power, rated_battery_range_km, usable_battery_level)
VALUES (1, '2024-03-01 09:00:30', false, 21, 0.50, 380, NULL, 500, 150, 395, true, 'IEC', 'Tesla', 'Tesla', 191.00, false, 10.5, 1, false, false, 181.00, 21),
       (2, '2024-03-01 09:10:00', false, 40, 14.00, 300, NULL, 500, 120, 400, true, 'IEC', 'Tesla', 'Tesla', 260.00, false, 10.5, 1, false, false, 245.00, 40),
       (3, '2024-03-01 20:00:30', false, 76, 0.10, 16, 3, 16, 11, 230, false, 'IEC', NULL, '<invalid>', 380.00, NULL, 8.0, 2, NULL, NULL, 360.00, 76),
       (4, '2024-03-01 09:29:30', false, 65, 30.25, 120, NULL, 500, 60, 410, true, 'IEC', 'Tesla', 'Tesla', 420.00, false, 10.0, 1, false, false, 400.00, 64),
       (5, '2024-03-02 01:59:30', false, 90, 40.00, 16, 3, 16, 11, 231, false, 'IEC', NULL, '<invalid>', 450.00, NULL, 6.0, 2, NULL, NULL, 430.00, 90);

UPDATE charging_processes SET end_date = '2024-03-01 09:30:00', charge_energy_added = 30.25, charge_energy_used = 31.10,
       end_battery_level = 65, end_rated_range_km = 400.00, end_ideal_range_km = 420.00, duration_min = 30,
       outside_temp_avg = 10.3, cost = 12.34
 WHERE id = 1;

UPDATE charging_processes SET end_date = '2024-03-02 02:00:00', charge_energy_added = 40.00, charge_energy_used = 44.50,
       end_battery_level = 90, end_rated_range_km = 430.00, end_ideal_range_km = 450.00, duration_min = 360,
       outside_temp_avg = 7.0
 WHERE id = 2;

INSERT INTO charging_processes (id, start_date, car_id, position_id, start_battery_level, start_rated_range_km)
VALUES (3, '2024-03-03 10:00:00', 2, 23, 25, 140.00);

INSERT INTO charges (id, date, battery_level, charge_energy_added, charger_actual_current, charger_power, charger_voltage, fast_charger_present, ideal_battery_range_km, charging_process_id, rated_battery_range_km)
VALUES (6, '2024-03-03 10:00:30', 26, 0.80, 250, 100, 400, true, 152.00, 3, 142.00);

-- A drive car 1 is still on when the backup is made: no end yet.
INSERT INTO drives (id, start_date, car_id, start_address_id)
VALUES (3, '2024-03-02 07:00:00', 1, 2);

INSERT INTO positions (id, date, latitude, longitude, speed, power, odometer, battery_level, outside_temp, car_id, drive_id, rated_battery_range_km)
VALUES (24, '2024-03-02 07:00:00', 12.310000, -45.610000, 20, 15, 12008.5, 90, 5.0, 1, 3, 430.00);

INSERT INTO states (id, state, start_date, end_date, car_id)
VALUES (1, 'online', '2024-03-01 07:55:00', '2024-03-01 08:15:00', 1),
       (2, 'online', '2024-03-01 07:58:00', '2024-03-01 09:40:00', 2),
       (3, 'asleep', '2024-03-01 08:15:00', '2024-03-01 19:55:00', 1),
       (4, 'offline', '2024-03-01 19:55:00', '2024-03-01 20:00:00', 1),
       (5, 'asleep', '2024-03-01 09:40:00', '2024-03-03 09:50:00', 2),
       (6, 'online', '2024-03-01 20:00:00', NULL, 1),
       (7, 'online', '2024-03-03 09:50:00', NULL, 2);

INSERT INTO updates (id, start_date, end_date, version, car_id)
VALUES (1, '2024-02-20 03:00:00', '2024-02-20 03:40:00', '2024.2.7 abc123def456', 1),
       (2, '2024-03-03 03:00:00', NULL, '2024.8.9 0123456789ab', 2);

-- Not a real token: a marker the importer must never read.
INSERT INTO private.tokens (id, inserted_at, updated_at, refresh, access)
VALUES (1, '2024-01-01 00:00:00', '2024-01-01 00:00:00', 'FAKE-NOT-A-TOKEN-refresh'::bytea, 'FAKE-NOT-A-TOKEN-access'::bytea);

SELECT setval('cars_id_seq', 2);
SELECT setval('car_settings_id_seq', 2);
SELECT setval('addresses_id_seq', 3);
SELECT setval('geofences_id_seq', 2);
SELECT setval('positions_id_seq', 24);
SELECT setval('trips_id_seq', 3);
SELECT setval('charging_processes_id_seq', 3);
SELECT setval('charges_id_seq', 6);
SELECT setval('states_id_seq', 7);
SELECT setval('updates_id_seq', 2);

COMMIT;
