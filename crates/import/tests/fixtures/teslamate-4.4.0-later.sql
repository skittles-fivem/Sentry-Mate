-- PostgreSQL database dump
\restrict FOJXmbgCsfBPBEyTTSIft8DFnMhGIOCsWdvk5RrmyGtRzB0lP55aEXuG98uhAHp
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
-- Data for Name: tokens; Type: TABLE DATA; Schema: private; Owner: teslamate
COPY private.tokens (id, inserted_at, updated_at, refresh, access) FROM stdin;
1	2024-01-01 00:00:00	2024-01-01 00:00:00	\\x46414b452d4e4f542d412d544f4b454e2d72656672657368	\\x46414b452d4e4f542d412d544f4b454e2d616363657373
\.
-- Data for Name: addresses; Type: TABLE DATA; Schema: public; Owner: teslamate
COPY public.addresses (id, display_name, latitude, longitude, name, house_number, road, neighbourhood, city, county, postcode, state, state_district, country, raw, inserted_at, updated_at, osm_id, osm_type) FROM stdin;
1	1, Made-up Road, Testville, Nowhere	12.300000	-45.600000	\N	1	Made-up Road	\N	Testville	\N	00000	\N	\N	Nowhere	{"fake": true}	2024-03-01 08:00:05	2024-03-01 08:00:05	1	node
2	Café "Test"\tTab\\Back\nslash, Second Street, Testville	12.310000	-45.610000	Café "Test"\tTab\\Back\nslash	\N	Second Street	\N	Testville	\N	\N	\N	\N	Nowhere	\N	2024-03-01 08:10:05	2024-03-01 08:10:05	2	way
3	Testville Supercharger, Testville	12.350000	-45.650000	Testville Supercharger	\N	\N	\N	Testville	\N	\N	\N	\N	Nowhere	\N	2024-03-01 08:50:05	2024-03-01 08:50:05	3	node
4	Test Works, Testville	12.330000	-45.630000	Test Works	\N	Factory Lane	\N	Testville	\N	\N	\N	\N	Nowhere	\N	2024-03-04 12:15:05	2024-03-04 12:15:05	4	way
\.
-- Data for Name: car_settings; Type: TABLE DATA; Schema: public; Owner: teslamate
COPY public.car_settings (id, suspend_min, suspend_after_idle_min, req_not_unlocked, free_supercharging, use_streaming_api, enabled, lfp_battery) FROM stdin;
1	21	15	f	f	t	t	f
2	21	15	f	t	t	t	t
3	21	15	f	f	t	t	f
\.
-- Data for Name: cars; Type: TABLE DATA; Schema: public; Owner: teslamate
COPY public.cars (id, eid, vid, model, efficiency, inserted_at, updated_at, vin, name, trim_badging, settings_id, exterior_color, spoiler_type, wheel_type, display_priority, marketing_name) FROM stdin;
1	1000001	2000001	3	0.153	2024-01-01 00:00:00	2024-01-01 00:00:00	5YJ3E1EA0KF000000	Test Three	74D	1	MidnightSilver	None	Pinwheel18	1	LR AWD
2	1000002	2000002	Y	0.16	2024-01-02 00:00:00	2024-01-02 00:00:00	7SAYGDEE0PF000001	Test Why	P74D	2	\N	\N	\N	2	\N
3	1000003	2000003	S	0.19	2024-03-04 00:00:00	2024-03-04 00:00:00	5YJSA1E26HF000002	Test Ess	90D	3	\N	\N	\N	3	\N
\.
-- Data for Name: charges; Type: TABLE DATA; Schema: public; Owner: teslamate
COPY public.charges (id, date, battery_heater_on, battery_level, charge_energy_added, charger_actual_current, charger_phases, charger_pilot_current, charger_power, charger_voltage, fast_charger_present, conn_charge_cable, fast_charger_brand, fast_charger_type, ideal_battery_range_km, not_enough_power_to_heat, outside_temp, charging_process_id, battery_heater, battery_heater_no_power, rated_battery_range_km, usable_battery_level) FROM stdin;
1	2024-03-01 09:00:30	f	21	0.50	380	\N	500	150	395	t	IEC	Tesla	Tesla	191.00	f	10.5	1	f	f	181.00	21
2	2024-03-01 09:10:00	f	40	14.00	300	\N	500	120	400	t	IEC	Tesla	Tesla	260.00	f	10.5	1	f	f	245.00	40
3	2024-03-01 20:00:30	f	76	0.10	16	3	16	11	230	f	IEC	\N	<invalid>	380.00	\N	8.0	2	\N	\N	360.00	76
4	2024-03-01 09:29:30	f	65	30.25	120	\N	500	60	410	t	IEC	Tesla	Tesla	420.00	f	10.0	1	f	f	400.00	64
5	2024-03-02 01:59:30	f	90	40.00	16	3	16	11	231	f	IEC	\N	<invalid>	450.00	\N	6.0	2	\N	\N	430.00	90
6	2024-03-03 10:00:30	\N	26	0.80	250	\N	\N	100	400	t	\N	\N	\N	152.00	\N	\N	3	\N	\N	142.00	\N
7	2024-03-03 10:20:00	\N	60	25.50	150	\N	\N	70	405	t	\N	\N	\N	300.00	\N	\N	3	\N	\N	285.00	\N
\.
-- Data for Name: charging_processes; Type: TABLE DATA; Schema: public; Owner: teslamate
COPY public.charging_processes (id, start_date, end_date, charge_energy_added, start_ideal_range_km, end_ideal_range_km, start_battery_level, end_battery_level, duration_min, outside_temp_avg, car_id, position_id, address_id, start_rated_range_km, end_rated_range_km, geofence_id, charge_energy_used, cost) FROM stdin;
1	2024-03-01 09:00:00	2024-03-01 09:30:00	30.25	190.00	420.00	20	65	30	10.3	2	21	3	180.00	400.00	2	31.10	12.34
2	2024-03-01 20:00:00	2024-03-02 02:00:00	40.00	380.00	450.00	76	90	360	7.0	1	22	1	360.00	430.00	1	44.50	\N
3	2024-03-03 10:00:00	2024-03-03 10:20:30	25.50	\N	\N	25	60	20	\N	2	23	\N	140.00	285.00	\N	26.40	9.99
\.
-- Data for Name: drives; Type: TABLE DATA; Schema: public; Owner: teslamate
COPY public.drives (id, start_date, end_date, outside_temp_avg, speed_max, power_max, power_min, start_ideal_range_km, end_ideal_range_km, start_km, end_km, distance, duration_min, car_id, inside_temp_avg, start_address_id, end_address_id, start_rated_range_km, end_rated_range_km, start_position_id, end_position_id, start_geofence_id, end_geofence_id, ascent, descent) FROM stdin;
1	2024-03-01 08:00:00.123456	2024-03-01 08:10:00.123456	12.5	85	20	-12	399.00	390.00	12000.85	12008.5	7.65	9	1	21.0	1	2	379.05	370.50	1	19	1	\N	9	0
2	2024-03-01 08:00:30	2024-03-01 08:10:30	11.0	50	20	20	\N	\N	30001	30010	9	9	2	\N	\N	3	189.00	180.00	2	20	\N	2	\N	\N
3	2024-03-02 07:00:00	2024-03-02 07:10:00	\N	60	30	2	\N	\N	12008.5	12015	6.5	10	1	\N	2	1	430.00	421.00	24	26	\N	1	\N	\N
4	2024-03-04 12:00:00	2024-03-04 12:15:00	\N	70	40	-10	\N	\N	30400	30412	12	15	2	\N	\N	4	285.00	270.00	27	28	\N	\N	\N	\N
\.
-- Data for Name: geofences; Type: TABLE DATA; Schema: public; Owner: teslamate
COPY public.geofences (id, name, latitude, longitude, radius, inserted_at, updated_at, cost_per_unit, session_fee, billing_type) FROM stdin;
2	Testville Supercharger	12.350000	-45.650000	100	2024-01-01 00:00:00	2024-01-01 00:00:00	0.1000	1.50	per_minute
3	Work	12.330000	-45.630000	150	2024-03-04 00:00:00	2024-03-04 00:00:00	\N	\N	per_kwh
1	Home	12.300000	-45.600000	30	2024-01-01 00:00:00	2024-03-04 00:00:00	0.2500	\N	per_kwh
\.
-- Data for Name: import_file_checkpoints; Type: TABLE DATA; Schema: public; Owner: teslamate
COPY public.import_file_checkpoints (id, run_id, file_name, file_fingerprint, completed_at, inserted_at, updated_at) FROM stdin;
\.
-- Data for Name: import_rejections; Type: TABLE DATA; Schema: public; Owner: teslamate
COPY public.import_rejections (id, run_id, file_name, file_fingerprint, "row", reason, fields, inserted_at, updated_at) FROM stdin;
\.
-- Data for Name: import_runs; Type: TABLE DATA; Schema: public; Owner: teslamate
COPY public.import_runs (id, source_key, status, timezone, date_limit, date_limit_captured, car_id, inserted_at, updated_at) FROM stdin;
\.
-- Data for Name: positions; Type: TABLE DATA; Schema: public; Owner: teslamate
COPY public.positions (id, date, latitude, longitude, speed, power, odometer, ideal_battery_range_km, battery_level, outside_temp, elevation, fan_status, driver_temp_setting, passenger_temp_setting, is_climate_on, is_rear_defroster_on, is_front_defroster_on, car_id, drive_id, inside_temp, battery_heater, battery_heater_on, battery_heater_no_power, est_battery_range_km, rated_battery_range_km, usable_battery_level, tpms_pressure_fl, tpms_pressure_fr, tpms_pressure_rl, tpms_pressure_rr) FROM stdin;
1	2024-03-01 08:01:00.123456	12.301000	-45.601000	45	11	12000.85	399.00	80	12.5	101	\N	\N	\N	\N	\N	\N	1	1	21.0	\N	\N	\N	389.00	379.05	79	2.9	2.9	2.8	2.8
3	2024-03-01 08:02:00.123456	12.302000	-45.602000	50	12	12001.7	398.00	80	12.5	102	\N	\N	\N	\N	\N	\N	1	1	21.0	\N	\N	\N	388.00	378.10	79	2.9	2.9	2.8	2.8
5	2024-03-01 08:03:00.123456	12.303000	-45.603000	55	13	12002.55	397.00	80	12.5	103	\N	\N	\N	\N	\N	\N	1	1	21.0	\N	\N	\N	387.00	377.15	79	2.9	2.9	2.8	2.8
7	2024-03-01 08:04:00.123456	12.304000	-45.604000	60	14	12003.4	396.00	80	12.5	104	\N	\N	\N	\N	\N	\N	1	1	21.0	\N	\N	\N	386.00	376.20	79	2.9	2.9	2.8	2.8
9	2024-03-01 08:05:00.123456	12.305000	-45.605000	65	15	12004.25	395.00	79	12.5	105	\N	\N	\N	\N	\N	\N	1	1	21.0	\N	\N	\N	385.00	375.25	78	2.9	2.9	2.8	2.8
11	2024-03-01 08:06:00.123456	12.306000	-45.606000	70	16	12005.1	394.00	79	12.5	106	\N	\N	\N	\N	\N	\N	1	1	21.0	\N	\N	\N	384.00	374.30	78	2.9	2.9	2.8	2.8
13	2024-03-01 08:07:00.123456	12.307000	-45.607000	75	17	12005.95	393.00	79	12.5	107	\N	\N	\N	\N	\N	\N	1	1	21.0	\N	\N	\N	383.00	373.35	78	2.9	2.9	2.8	2.8
15	2024-03-01 08:08:00.123456	12.308000	-45.608000	80	18	12006.8	392.00	79	12.5	108	\N	\N	\N	\N	\N	\N	1	1	21.0	\N	\N	\N	382.00	372.40	78	2.9	2.9	2.8	2.8
17	2024-03-01 08:09:00.123456	12.309000	-45.609000	85	19	12007.65	391.00	79	12.5	109	\N	\N	\N	\N	\N	\N	1	1	21.0	\N	\N	\N	381.00	371.45	78	2.9	2.9	2.8	2.8
19	2024-03-01 08:10:00.123456	12.310000	-45.610000	0	-12	12008.5	390.00	78	12.5	110	\N	\N	\N	\N	\N	\N	1	1	21.0	\N	\N	\N	380.00	370.50	77	2.9	2.9	2.8	2.8
2	2024-03-01 08:01:30	12.341000	-45.641000	50	20	30001	199.00	29	11.0	\N	\N	\N	\N	\N	\N	\N	2	2	\N	\N	\N	\N	\N	189.00	\N	\N	\N	\N	\N
4	2024-03-01 08:02:30	12.342000	-45.642000	50	20	30002	198.00	28	11.0	\N	\N	\N	\N	\N	\N	\N	2	2	\N	\N	\N	\N	\N	188.00	\N	\N	\N	\N	\N
6	2024-03-01 08:03:30	12.343000	-45.643000	50	20	30003	197.00	27	11.0	\N	\N	\N	\N	\N	\N	\N	2	2	\N	\N	\N	\N	\N	187.00	\N	\N	\N	\N	\N
8	2024-03-01 08:04:30	12.344000	-45.644000	50	20	30004	196.00	26	11.0	\N	\N	\N	\N	\N	\N	\N	2	2	\N	\N	\N	\N	\N	186.00	\N	\N	\N	\N	\N
10	2024-03-01 08:05:30	12.345000	-45.645000	50	20	30005	195.00	25	11.0	\N	\N	\N	\N	\N	\N	\N	2	2	\N	\N	\N	\N	\N	185.00	\N	\N	\N	\N	\N
12	2024-03-01 08:06:30	12.346000	-45.646000	50	20	30006	194.00	24	11.0	\N	\N	\N	\N	\N	\N	\N	2	2	\N	\N	\N	\N	\N	184.00	\N	\N	\N	\N	\N
14	2024-03-01 08:07:30	12.347000	-45.647000	50	20	30007	193.00	23	11.0	\N	\N	\N	\N	\N	\N	\N	2	2	\N	\N	\N	\N	\N	183.00	\N	\N	\N	\N	\N
16	2024-03-01 08:08:30	12.348000	-45.648000	50	20	30008	192.00	22	11.0	\N	\N	\N	\N	\N	\N	\N	2	2	\N	\N	\N	\N	\N	182.00	\N	\N	\N	\N	\N
18	2024-03-01 08:09:30	12.349000	-45.649000	50	20	30009	191.00	21	11.0	\N	\N	\N	\N	\N	\N	\N	2	2	\N	\N	\N	\N	\N	181.00	\N	\N	\N	\N	\N
20	2024-03-01 08:10:30	12.350000	-45.650000	50	20	30010	190.00	20	11.0	\N	\N	\N	\N	\N	\N	\N	2	2	\N	\N	\N	\N	\N	180.00	\N	\N	\N	\N	\N
21	2024-03-01 09:00:00	12.350000	-45.650000	\N	-150	30010	190.00	20	10.5	5	\N	\N	\N	\N	\N	\N	2	\N	\N	\N	\N	\N	\N	180.00	20	\N	\N	\N	\N
22	2024-03-01 20:00:00	12.300000	-45.600000	\N	-11	12008.5	380.00	76	8.0	110	\N	\N	\N	\N	\N	\N	1	\N	\N	\N	\N	\N	\N	360.00	76	\N	\N	\N	\N
23	2024-03-03 10:00:00	12.350000	-45.650000	\N	-100	30400	150.00	25	9.0	5	\N	\N	\N	\N	\N	\N	2	\N	\N	\N	\N	\N	\N	140.00	25	\N	\N	\N	\N
24	2024-03-02 07:00:00	12.310000	-45.610000	20	15	12008.5	\N	90	5.0	\N	\N	\N	\N	\N	\N	\N	1	3	\N	\N	\N	\N	\N	430.00	\N	\N	\N	\N	\N
25	2024-03-02 07:05:00	12.305000	-45.605000	60	30	12012	\N	89	5.5	\N	\N	\N	\N	\N	\N	\N	1	3	\N	\N	\N	\N	\N	425.00	\N	\N	\N	\N	\N
26	2024-03-02 07:10:00	12.300000	-45.600000	0	2	12015	\N	88	6.0	\N	\N	\N	\N	\N	\N	\N	1	3	\N	\N	\N	\N	\N	421.00	\N	\N	\N	\N	\N
27	2024-03-04 12:00:00	12.350000	-45.650000	10	5	30400	\N	60	\N	\N	\N	\N	\N	\N	\N	\N	2	4	\N	\N	\N	\N	\N	285.00	\N	\N	\N	\N	\N
28	2024-03-04 12:15:00	12.330000	-45.630000	0	0	30412	\N	55	\N	\N	\N	\N	\N	\N	\N	\N	2	4	\N	\N	\N	\N	\N	270.00	\N	\N	\N	\N	\N
29	2024-03-05 09:00:00	12.300000	-45.600000	\N	\N	\N	\N	70	\N	\N	\N	\N	\N	\N	\N	\N	3	\N	\N	\N	\N	\N	\N	300.00	\N	\N	\N	\N	\N
\.
-- Data for Name: schema_migrations; Type: TABLE DATA; Schema: public; Owner: teslamate
COPY public.schema_migrations (version, inserted_at) FROM stdin;
20190330150000	2026-11-16 16:52:32
20190330160000	2026-11-16 16:52:32
20190330170000	2026-11-16 16:52:32
20190330180000	2026-11-16 16:52:32
20190330190000	2026-11-16 16:52:32
20190330200000	2026-11-16 16:52:32
20190408203117	2026-11-16 16:52:32
20190415103933	2026-11-16 16:52:32
20190415115227	2026-11-16 16:52:32
20190415130006	2026-11-16 16:52:32
20190415130705	2026-11-16 16:52:32
20190415192200	2026-11-16 16:52:32
20190416125429	2026-11-16 16:52:32
20190525125700	2026-11-16 16:52:32
20190717184003	2026-11-16 16:52:32
20190729142656	2026-11-16 16:52:32
20190729181314	2026-11-16 16:52:32
20190730101523	2026-11-16 16:52:32
20190731154452	2026-11-16 16:52:32
20190805092941	2026-11-16 16:52:32
20190810105216	2026-11-16 16:52:32
20190810131321	2026-11-16 16:52:32
20190810151901	2026-11-16 16:52:32
20190812191616	2026-11-16 16:52:32
20190813184320	2026-11-16 16:52:32
20190814152810	2026-11-16 16:52:32
20190816165713	2026-11-16 16:52:32
20190816200723	2026-11-16 16:52:32
20190821143938	2026-11-16 16:52:32
20190821155748	2026-11-16 16:52:32
20190823173437	2026-11-16 16:52:32
20190826142828	2026-11-16 16:52:32
20190828094708	2026-11-16 16:52:32
20190828104902	2026-11-16 16:52:32
20190828122529	2026-11-16 16:52:32
20190828150058	2026-11-16 16:52:32
20190903151524	2026-11-16 16:52:32
20190913165850	2026-11-16 16:52:32
20190913175011	2026-11-16 16:52:32
20190913175543	2026-11-16 16:52:32
20190925152807	2026-11-16 16:52:32
20190925161034	2026-11-16 16:52:32
20190925182253	2026-11-16 16:52:32
20190928155641	2026-11-16 16:52:32
20191003130650	2026-11-16 16:52:32
20191003132415	2026-11-16 16:52:32
20191007105010	2026-11-16 16:52:32
20191008191431	2026-11-16 16:52:32
20191017003836	2026-11-16 16:52:32
20191020130234	2026-11-16 16:52:32
20191026144449	2026-11-16 16:52:32
20191026145925	2026-11-16 16:52:32
20191026185642	2026-11-16 16:52:32
20191117042320	2026-11-16 16:52:32
20191117143038	2026-11-16 16:52:32
20191117171307	2026-11-16 16:52:32
20191119162847	2026-11-16 16:52:32
20191212215130	2026-11-16 16:52:32
20191212230527	2026-11-16 16:52:32
20200103073606	2026-11-16 16:52:32
20200116190926	2026-11-16 16:52:32
20200120130125	2026-11-16 16:52:32
20200120142602	2026-11-16 16:52:32
20200203120311	2026-11-16 16:52:32
20200203180529	2026-11-16 16:52:32
20200212001245	2026-11-16 16:52:32
20200216121330	2026-11-16 16:52:32
20200302100654	2026-11-16 16:52:32
20200306130218	2026-11-16 16:52:32
20200306133847	2026-11-16 16:52:32
20200318164021	2026-11-16 16:52:32
20200320140020	2026-11-16 16:52:32
20200401170940	2026-11-16 16:52:32
20200401171402	2026-11-16 16:52:32
20200401171923	2026-11-16 16:52:32
20200410112005	2026-11-16 16:52:33
20200502140646	2026-11-16 16:52:33
20200528163852	2026-11-16 16:52:33
20200528173223	2026-11-16 16:52:33
20200528175158	2026-11-16 16:52:33
20200709165119	2026-11-16 16:52:33
20210130174838	2026-11-16 16:52:33
20210812173700	2026-11-16 16:52:33
20210831153305	2026-11-16 16:52:33
20211022103654	2026-11-16 16:52:33
20220123131732	2026-11-16 16:52:33
20220422132017	2026-11-16 16:52:33
20220617170400	2026-11-16 16:52:33
20220718085412	2026-11-16 16:52:33
20230417225712	2026-11-16 16:52:33
20240603152807	2026-11-16 16:52:33
20240627021414	2026-11-16 16:52:33
20240915193446	2026-11-16 16:52:33
20240929084639	2026-11-16 16:52:33
20250407155134	2026-11-16 16:52:33
20250613133700	2026-11-16 16:52:33
20250924215353	2026-11-16 16:52:33
20251207212310	2026-11-16 16:52:33
20251225150000	2026-11-16 16:52:33
20260411070212	2026-11-16 16:52:33
20260715081000	2026-11-16 16:52:33
20260716110000	2026-11-16 16:52:33
20260718160000	2026-11-16 16:52:33
20260807090000	2026-11-16 16:52:33
20260808090000	2026-11-16 16:52:33
20261009120000	2026-11-16 16:52:33
20261014120000	2026-11-16 16:52:33
\.
-- Data for Name: settings; Type: TABLE DATA; Schema: public; Owner: teslamate
COPY public.settings (id, inserted_at, updated_at, unit_of_length, unit_of_temperature, preferred_range, base_url, grafana_url, language, unit_of_pressure, theme_mode) FROM stdin;
1	2026-11-16 16:52:33	2026-11-16 16:52:33	km	C	rated	\N	\N	en	bar	system
\.
-- Data for Name: states; Type: TABLE DATA; Schema: public; Owner: teslamate
COPY public.states (id, state, start_date, end_date, car_id) FROM stdin;
1	online	2024-03-01 07:55:00	2024-03-01 08:15:00	1
2	online	2024-03-01 07:58:00	2024-03-01 09:40:00	2
3	asleep	2024-03-01 08:15:00	2024-03-01 19:55:00	1
4	offline	2024-03-01 19:55:00	2024-03-01 20:00:00	1
5	asleep	2024-03-01 09:40:00	2024-03-03 09:50:00	2
6	online	2024-03-01 20:00:00	2024-03-02 07:20:00	1
7	online	2024-03-03 09:50:00	2024-03-04 12:30:00	2
8	asleep	2024-03-02 07:20:00	\N	1
9	asleep	2024-03-04 12:30:00	\N	2
10	online	2024-03-05 08:55:00	2024-03-05 09:05:00	3
\.
-- Data for Name: updates; Type: TABLE DATA; Schema: public; Owner: teslamate
COPY public.updates (id, start_date, end_date, version, car_id) FROM stdin;
1	2024-02-20 03:00:00	2024-02-20 03:40:00	2024.2.7 abc123def456	1
2	2024-03-03 03:00:00	2024-03-03 03:45:00	2024.8.9 0123456789ab	2
\.
SELECT pg_catalog.setval('private.tokens_id_seq', 1, false);
SELECT pg_catalog.setval('public.addresses_id_seq', 4, true);
SELECT pg_catalog.setval('public.car_settings_id_seq', 3, true);
SELECT pg_catalog.setval('public.cars_id_seq', 3, true);
SELECT pg_catalog.setval('public.charges_id_seq', 7, true);
SELECT pg_catalog.setval('public.charging_processes_id_seq', 3, true);
SELECT pg_catalog.setval('public.geofences_id_seq', 3, true);
SELECT pg_catalog.setval('public.import_file_checkpoints_id_seq', 1, false);
SELECT pg_catalog.setval('public.import_rejections_id_seq', 1, false);
SELECT pg_catalog.setval('public.import_runs_id_seq', 1, false);
SELECT pg_catalog.setval('public.positions_id_seq', 29, true);
SELECT pg_catalog.setval('public.settings_id_seq', 1, true);
SELECT pg_catalog.setval('public.states_id_seq', 10, true);
SELECT pg_catalog.setval('public.trips_id_seq', 4, true);
SELECT pg_catalog.setval('public.updates_id_seq', 2, true);
-- PostgreSQL database dump complete
\unrestrict FOJXmbgCsfBPBEyTTSIft8DFnMhGIOCsWdvk5RrmyGtRzB0lP55aEXuG98uhAHp
