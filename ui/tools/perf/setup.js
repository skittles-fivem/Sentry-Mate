#!/usr/bin/env node
// First-run setup of a freshly seeded server for measure.js: reads SETUP-CODE.txt from the keys folder and makes
// the first admin over HTTPS (POST /api/v1/setup). Does nothing when the server is set up already.
//   node setup.js --base https://127.0.0.1:10101 --keys DIR [--user owner] [--pass '...']
'use strict';
const { request } = require('playwright');
const fs = require('fs');
const path = require('path');

const a = {};
const v = process.argv.slice(2);
for (let i = 0; i < v.length; i += 2) a[v[i].replace(/^--/, '')] = v[i + 1];
const BASE = (a.base || 'https://127.0.0.1:10101').replace(/\/$/, '');
const USER = a.user || 'owner';
const PASS = a.pass || 'correct horse battery staple';

(async () => {
  const rq = await request.newContext({ ignoreHTTPSErrors: true, extraHTTPHeaders: { 'X-Sentry-Mate': '1' } });
  const st = await (await rq.get(BASE + '/api/v1/setup')).json();
  if (!st.needs_setup) { console.log('already set up:', st.state); return; }
  const code = fs.readFileSync(path.join(a.keys, 'SETUP-CODE.txt'), 'utf8').trim();
  const r = await rq.post(BASE + '/api/v1/setup', { data: { code, name: USER, password: PASS } });
  console.log('setup:', r.status(), r.ok() ? 'admin ' + USER + ' made' : await r.text());
  if (!r.ok()) process.exit(1);
  // wait for the engine to decode the seeded logs: the cars and some drives show
  for (let i = 0; i < 600; i++) {
    const cars = await (await rq.get(BASE + '/api/v1/cars')).json().catch(() => ({}));
    if (cars.items && cars.items.length) { console.log('cars:', cars.items.map((c) => c.id).join(', ')); break; }
    await new Promise((res) => setTimeout(res, 1000));
  }
  await rq.dispose();
})().catch((e) => { console.error(e); process.exit(1); });
