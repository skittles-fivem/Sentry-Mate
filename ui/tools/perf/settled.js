#!/usr/bin/env node
// Waits until the engine has decoded the seeded logs: the drive and charge counts of every car stop changing
// (checked every 20 s). Run it after setup.js, before measure.js.
//   node settled.js --base https://127.0.0.1:10101 [--user owner] [--pass '...']
'use strict';
const { request } = require('playwright');
const a = {};
const v = process.argv.slice(2);
for (let i = 0; i < v.length; i += 2) a[v[i].replace(/^--/, '')] = v[i + 1];
const BASE = (a.base || 'https://127.0.0.1:10101').replace(/\/$/, '');
(async () => {
  const rq = await request.newContext({ ignoreHTTPSErrors: true, extraHTTPHeaders: { 'X-Sentry-Mate': '1' } });
  const s = await rq.post(BASE + '/api/v1/session', { data: { name: a.user || 'owner', password: a.pass || 'correct horse battery staple' } });
  if (!s.ok()) throw new Error('sign-in ' + s.status());
  let prev = '';
  for (let i = 0; i < 180; i++) {
    const cars = (await (await rq.get(BASE + '/api/v1/cars')).json()).items || [];
    const out = [];
    for (const c of cars) {
      const d = await (await rq.get(`${BASE}/api/v1/cars/${c.id}/drives?from_ms=0&limit=1`)).json();
      const ch = await (await rq.get(`${BASE}/api/v1/cars/${c.id}/charges?from_ms=0&limit=500`)).json();
      out.push(`${c.id}: ${(d.summary && d.summary.count) ?? JSON.stringify(d.error)} drives, ${(ch.summary && ch.summary.count) ?? (ch.items || []).length} charges`);
    }
    const line = out.join(' | ');
    console.log(new Date().toISOString(), line);
    if (line === prev && cars.length) break;
    prev = line;
    await new Promise((r) => setTimeout(r, 20000));
  }
  await rq.dispose();
})().catch((e) => { console.error(e); process.exit(1); });
