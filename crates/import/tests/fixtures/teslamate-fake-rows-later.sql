-- More fake rows, applied after teslamate-fake-rows.sql, for the "newer backup" fixture: what was still open is
-- closed, new drives and charges arrive, a third car is added, and a geofence is edited. Made up like the first file.

BEGIN;

-- Car 1 finishes the drive it was on.
INSERT INTO positions (id, date, latitude, longitude, speed, power, odometer, battery_level, outside_temp, car_id, drive_id, rated_battery_range_km)
VALUES (25, '2024-03-02 07:05:00', 12.305000, -45.605000, 60, 30, 12012.0, 89, 5.5, 1, 3, 425.00),
       (26, '2024-03-02 07:10:00', 12.300000, -45.600000, 0, 2, 12015.0, 88, 6.0, 1, 3, 421.00);

UPDATE drives SET end_date = '2024-03-02 07:10:00', speed_max = 60, power_max = 30, power_min = 2, start_km = 12008.5,
       end_km = 12015.0, distance = 6.5, duration_min = 10, end_address_id = 1, start_rated_range_km = 430.00,
       end_rated_range_km = 421.00, start_position_id = 24, end_position_id = 26, end_geofence_id = 1
 WHERE id = 3;

-- Car 2's charge ends.
INSERT INTO charges (id, date, battery_level, charge_energy_added, charger_actual_current, charger_power, charger_voltage, fast_charger_present, ideal_battery_range_km, charging_process_id, rated_battery_range_km)
VALUES (7, '2024-03-03 10:20:00', 60, 25.50, 150, 70, 405, true, 300.00, 3, 285.00);

UPDATE charging_processes SET end_date = '2024-03-03 10:20:30', charge_energy_added = 25.50, charge_energy_used = 26.40,
       end_battery_level = 60, end_rated_range_km = 285.00, duration_min = 20, cost = 9.99
 WHERE id = 3;

-- A new drive of car 2, to a new address.
INSERT INTO addresses (id, display_name, latitude, longitude, name, road, city, country, inserted_at, updated_at, osm_id, osm_type)
VALUES (4, 'Test Works, Testville', 12.330000, -45.630000, 'Test Works', 'Factory Lane', 'Testville', 'Nowhere', '2024-03-04 12:15:05', '2024-03-04 12:15:05', 4, 'way');

INSERT INTO drives (id, start_date, car_id)
VALUES (4, '2024-03-04 12:00:00', 2);

INSERT INTO positions (id, date, latitude, longitude, speed, power, odometer, battery_level, car_id, drive_id, rated_battery_range_km)
VALUES (27, '2024-03-04 12:00:00', 12.350000, -45.650000, 10, 5, 30400.0, 60, 2, 4, 285.00),
       (28, '2024-03-04 12:15:00', 12.330000, -45.630000, 0, 0, 30412.0, 55, 2, 4, 270.00);

UPDATE drives SET end_date = '2024-03-04 12:15:00', speed_max = 70, power_max = 40, power_min = -10, start_km = 30400.0,
       end_km = 30412.0, distance = 12.0, duration_min = 15, start_rated_range_km = 285.00, end_rated_range_km = 270.00,
       start_position_id = 27, end_position_id = 28, end_address_id = 4
 WHERE id = 4;

INSERT INTO geofences (id, name, latitude, longitude, radius, inserted_at, updated_at, cost_per_unit, billing_type)
VALUES (3, 'Work', 12.330000, -45.630000, 150, '2024-03-04 00:00:00', '2024-03-04 00:00:00', NULL, 'per_kwh');

UPDATE geofences SET radius = 30, updated_at = '2024-03-04 00:00:00' WHERE id = 1;

UPDATE states SET end_date = '2024-03-02 07:20:00' WHERE id = 6;
UPDATE states SET end_date = '2024-03-04 12:30:00' WHERE id = 7;
INSERT INTO states (id, state, start_date, end_date, car_id)
VALUES (8, 'asleep', '2024-03-02 07:20:00', NULL, 1),
       (9, 'asleep', '2024-03-04 12:30:00', NULL, 2);

UPDATE updates SET end_date = '2024-03-03 03:45:00' WHERE id = 2;

-- A third car joins.
INSERT INTO car_settings (id) VALUES (3);
INSERT INTO cars (id, eid, vid, model, efficiency, inserted_at, updated_at, vin, name, trim_badging, settings_id, display_priority)
VALUES (3, 1000003, 2000003, 'S', 0.190, '2024-03-04 00:00:00', '2024-03-04 00:00:00', '5YJSA1E26HF000002', 'Test Ess', '90D', 3, 3);

INSERT INTO positions (id, date, latitude, longitude, battery_level, car_id, rated_battery_range_km)
VALUES (29, '2024-03-05 09:00:00', 12.300000, -45.600000, 70, 3, 300.00);

INSERT INTO states (id, state, start_date, end_date, car_id)
VALUES (10, 'online', '2024-03-05 08:55:00', '2024-03-05 09:05:00', 3);

SELECT setval('car_settings_id_seq', 3);
SELECT setval('cars_id_seq', 3);
SELECT setval('addresses_id_seq', 4);
SELECT setval('geofences_id_seq', 3);
SELECT setval('positions_id_seq', 29);
SELECT setval('trips_id_seq', 4);
SELECT setval('charges_id_seq', 7);
SELECT setval('states_id_seq', 10);

COMMIT;
