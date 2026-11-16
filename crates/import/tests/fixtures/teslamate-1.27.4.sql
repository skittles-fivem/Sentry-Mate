-- PostgreSQL database dump
\restrict luI4y51R0xckTvxM62Qf1RhiImOreGcraEDp6urDbkDUMKvvoBUZB0blnQZfUTg
-- Dumped from database version 16.16 (Debian 16.16-1.pgdg13+1)
-- Dumped by pg_dump version 16.16 (Debian 16.16-1.pgdg13+1)
SET statement_timeout = 0;
SET lock_timeout = 0;
SET idle_in_transaction_session_timeout = 0;
SET client_encoding = 'UTF8';
SET standard_conforming_strings = on;
SELECT pg_catalog.set_config('search_path', '', false);
SET check_function_bodies = false;
SET xmloption = content;
SET client_min_messages = warning;
SET row_security = off;
SET default_tablespace = '';
SET default_table_access_method = heap;
-- Data for Name: addresses; Type: TABLE DATA; Schema: public; Owner: teslamate
COPY public.addresses (id, display_name, latitude, longitude, name, house_number, road, neighbourhood, city, county, postcode, state, state_district, country, raw, inserted_at, updated_at, osm_id, osm_type) FROM stdin;
1	Made-up Road, Testville	12.300000	-45.600000	\N	\N	Made-up Road	\N	Testville	\N	\N	\N	\N	Nowhere	\N	2023-05-01 10:00:00	2023-05-01 10:00:00	1	way
\.
-- Data for Name: car_settings; Type: TABLE DATA; Schema: public; Owner: teslamate
COPY public.car_settings (id, suspend_min, suspend_after_idle_min, req_not_unlocked, free_supercharging, use_streaming_api) FROM stdin;
1	21	15	f	t	t
\.
-- Data for Name: cars; Type: TABLE DATA; Schema: public; Owner: teslamate
COPY public.cars (id, eid, vid, model, efficiency, inserted_at, updated_at, vin, name, trim_badging, settings_id, exterior_color, spoiler_type, wheel_type, display_priority, marketing_name) FROM stdin;
1	1000003	2000003	S	0.19	2023-01-01 00:00:00	2023-01-01 00:00:00	5YJSA1E26HF000002	Old Ess	90D	1	Red	\N	Slipstream19	1	\N
\.
-- Data for Name: charges; Type: TABLE DATA; Schema: public; Owner: teslamate
COPY public.charges (id, date, battery_heater_on, battery_level, charge_energy_added, charger_actual_current, charger_phases, charger_pilot_current, charger_power, charger_voltage, fast_charger_present, conn_charge_cable, fast_charger_brand, fast_charger_type, ideal_battery_range_km, not_enough_power_to_heat, outside_temp, charging_process_id, battery_heater, battery_heater_no_power, rated_battery_range_km, usable_battery_level) FROM stdin;
\.
-- Data for Name: charging_processes; Type: TABLE DATA; Schema: public; Owner: teslamate
COPY public.charging_processes (id, start_date, end_date, charge_energy_added, start_ideal_range_km, end_ideal_range_km, start_battery_level, end_battery_level, duration_min, outside_temp_avg, car_id, position_id, address_id, start_rated_range_km, end_rated_range_km, geofence_id, charge_energy_used, cost) FROM stdin;
\.
-- Data for Name: drives; Type: TABLE DATA; Schema: public; Owner: teslamate
COPY public.drives (id, start_date, end_date, outside_temp_avg, speed_max, power_max, power_min, start_ideal_range_km, end_ideal_range_km, start_km, end_km, distance, duration_min, car_id, inside_temp_avg, start_address_id, end_address_id, start_rated_range_km, end_rated_range_km, start_position_id, end_position_id, start_geofence_id, end_geofence_id) FROM stdin;
1	2023-05-01 10:00:00	2023-05-01 10:05:00.5	\N	60	25	5	\N	\N	50000	50004.2	4.2	5	1	\N	1	\N	310.00	306.00	1	2	\N	\N
\.
-- Data for Name: geofences; Type: TABLE DATA; Schema: public; Owner: teslamate
COPY public.geofences (id, name, latitude, longitude, radius, inserted_at, updated_at, cost_per_unit, session_fee, billing_type) FROM stdin;
1	Home	12.300000	-45.600000	3000	2023-01-01 00:00:00	2023-01-01 00:00:00	0.3000	\N	per_kwh
\.
-- Data for Name: positions; Type: TABLE DATA; Schema: public; Owner: teslamate
COPY public.positions (id, date, latitude, longitude, speed, power, odometer, ideal_battery_range_km, battery_level, outside_temp, elevation, fan_status, driver_temp_setting, passenger_temp_setting, is_climate_on, is_rear_defroster_on, is_front_defroster_on, car_id, drive_id, inside_temp, battery_heater, battery_heater_on, battery_heater_no_power, est_battery_range_km, rated_battery_range_km, usable_battery_level, tpms_pressure_fl, tpms_pressure_fr, tpms_pressure_rl, tpms_pressure_rr) FROM stdin;
1	2023-05-01 10:00:00.5	12.300000	-45.600000	0	5	50000	320.00	70	18.0	100	\N	\N	\N	\N	\N	\N	1	1	22.0	\N	\N	\N	300.00	310.00	69	2.9	2.9	2.9	2.9
2	2023-05-01 10:05:00.5	12.302000	-45.602000	60	25	50004.2	316.00	69	18.5	104	\N	\N	\N	\N	\N	\N	1	1	22.0	\N	\N	\N	296.00	306.00	68	2.9	2.9	2.9	2.9
\.
-- Data for Name: schema_migrations; Type: TABLE DATA; Schema: public; Owner: teslamate
COPY public.schema_migrations (version, inserted_at) FROM stdin;
20190330150000	2026-11-16 16:52:54
20190330160000	2026-11-16 16:52:54
20190330170000	2026-11-16 16:52:54
20190330180000	2026-11-16 16:52:54
20190330190000	2026-11-16 16:52:54
20190330200000	2026-11-16 16:52:54
20190408203117	2026-11-16 16:52:54
20190415103933	2026-11-16 16:52:54
20190415115227	2026-11-16 16:52:54
20190415130006	2026-11-16 16:52:54
20190415130705	2026-11-16 16:52:54
20190415192200	2026-11-16 16:52:54
20190416125429	2026-11-16 16:52:54
20190525125700	2026-11-16 16:52:54
20190717184003	2026-11-16 16:52:54
20190729142656	2026-11-16 16:52:54
20190729181314	2026-11-16 16:52:54
20190730101523	2026-11-16 16:52:54
20190731154452	2026-11-16 16:52:54
20190805092941	2026-11-16 16:52:54
20190810105216	2026-11-16 16:52:55
20190810131321	2026-11-16 16:52:55
20190810151901	2026-11-16 16:52:55
20190812191616	2026-11-16 16:52:55
20190813184320	2026-11-16 16:52:55
20190814152810	2026-11-16 16:52:55
20190816165713	2026-11-16 16:52:55
20190816200723	2026-11-16 16:52:55
20190821143938	2026-11-16 16:52:55
20190821155748	2026-11-16 16:52:55
20190823173437	2026-11-16 16:52:55
20190826142828	2026-11-16 16:52:55
20190828094708	2026-11-16 16:52:55
20190828104902	2026-11-16 16:52:55
20190828122529	2026-11-16 16:52:55
20190828150058	2026-11-16 16:52:55
20190903151524	2026-11-16 16:52:55
20190913165850	2026-11-16 16:52:55
20190913175011	2026-11-16 16:52:55
20190913175543	2026-11-16 16:52:55
20190925152807	2026-11-16 16:52:55
20190925161034	2026-11-16 16:52:55
20190925182253	2026-11-16 16:52:55
20190928155641	2026-11-16 16:52:55
20191003130650	2026-11-16 16:52:55
20191003132415	2026-11-16 16:52:55
20191007105010	2026-11-16 16:52:55
20191008191431	2026-11-16 16:52:55
20191017003836	2026-11-16 16:52:55
20191020130234	2026-11-16 16:52:55
20191026144449	2026-11-16 16:52:55
20191026145925	2026-11-16 16:52:55
20191026185642	2026-11-16 16:52:55
20191117042320	2026-11-16 16:52:55
20191117143038	2026-11-16 16:52:55
20191117171307	2026-11-16 16:52:55
20191119162847	2026-11-16 16:52:55
20191212215130	2026-11-16 16:52:55
20191212230527	2026-11-16 16:52:55
20200103073606	2026-11-16 16:52:55
20200116190926	2026-11-16 16:52:55
20200120130125	2026-11-16 16:52:55
20200120142602	2026-11-16 16:52:55
20200203120311	2026-11-16 16:52:55
20200203180529	2026-11-16 16:52:55
20200212001245	2026-11-16 16:52:55
20200216121330	2026-11-16 16:52:55
20200302100654	2026-11-16 16:52:55
20200306130218	2026-11-16 16:52:55
20200306133847	2026-11-16 16:52:55
20200318164021	2026-11-16 16:52:55
20200320140020	2026-11-16 16:52:55
20200401170940	2026-11-16 16:52:55
20200401171402	2026-11-16 16:52:55
20200401171923	2026-11-16 16:52:55
20200410112005	2026-11-16 16:52:55
20200502140646	2026-11-16 16:52:55
20200528163852	2026-11-16 16:52:55
20200528173223	2026-11-16 16:52:55
20200528175158	2026-11-16 16:52:55
20200709165119	2026-11-16 16:52:55
20210130174838	2026-11-16 16:52:55
20210812173700	2026-11-16 16:52:55
20210831153305	2026-11-16 16:52:55
20211022103654	2026-11-16 16:52:55
20220123131732	2026-11-16 16:52:55
20220422132017	2026-11-16 16:52:55
20220617170400	2026-11-16 16:52:55
20220718085412	2026-11-16 16:52:55
20230417225712	2026-11-16 16:52:55
\.
-- Data for Name: settings; Type: TABLE DATA; Schema: public; Owner: teslamate
COPY public.settings (id, inserted_at, updated_at, unit_of_length, unit_of_temperature, preferred_range, base_url, grafana_url, language, unit_of_pressure) FROM stdin;
1	2026-11-16 16:52:55	2026-11-16 16:52:55	km	C	rated	\N	\N	en	bar
\.
-- Data for Name: states; Type: TABLE DATA; Schema: public; Owner: teslamate
COPY public.states (id, state, start_date, end_date, car_id) FROM stdin;
1	online	2023-05-01 09:55:00	2023-05-01 10:10:00	1
2	asleep	2023-05-01 10:10:00	\N	1
\.
-- Data for Name: tokens; Type: TABLE DATA; Schema: public; Owner: teslamate
COPY public.tokens (id, inserted_at, updated_at, refresh, access) FROM stdin;
1	2023-01-01 00:00:00	2023-01-01 00:00:00	\\x46414b452d4e4f542d412d544f4b454e2d72656672657368	\\x46414b452d4e4f542d412d544f4b454e2d616363657373
\.
-- Data for Name: updates; Type: TABLE DATA; Schema: public; Owner: teslamate
COPY public.updates (id, start_date, end_date, version, car_id) FROM stdin;
1	2023-04-01 02:00:00	2023-04-01 02:30:00	2023.12.1 0123456789ab	1
\.
SELECT pg_catalog.setval('public.addresses_id_seq', 1, false);
SELECT pg_catalog.setval('public.car_settings_id_seq', 1, false);
SELECT pg_catalog.setval('public.cars_id_seq', 1, false);
SELECT pg_catalog.setval('public.charges_id_seq', 1, false);
SELECT pg_catalog.setval('public.charging_processes_id_seq', 1, false);
SELECT pg_catalog.setval('public.geofences_id_seq', 1, false);
SELECT pg_catalog.setval('public.positions_id_seq', 1, false);
SELECT pg_catalog.setval('public.settings_id_seq', 1, true);
SELECT pg_catalog.setval('public.states_id_seq', 1, false);
SELECT pg_catalog.setval('public.tokens_id_seq', 1, false);
SELECT pg_catalog.setval('public.trips_id_seq', 1, false);
SELECT pg_catalog.setval('public.updates_id_seq', 1, false);
-- PostgreSQL database dump complete
\unrestrict luI4y51R0xckTvxM62Qf1RhiImOreGcraEDp6urDbkDUMKvvoBUZB0blnQZfUTg
