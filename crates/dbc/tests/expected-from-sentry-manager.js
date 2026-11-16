#!/usr/bin/env node
'use strict';
// Makes the expected decoding for tests/parity.rs with Sentry Six Manager's own decoder, so the Rust decoder is
// checked against it value for value:
//
//   node expected-from-sentry-manager.js <Sentry-Manager checkout> <candump text file> [sidecar.json] > expected.json
//
// Per series: [count, first, last, min, max, sum] of every decoded value (before any rounding), with the same bus
// rules as server/canIndex.js (named buses only, the internal bus for 0x04F's GPS while no car bus sends it).
const fs = require('fs');
const path = require('path');
const [sm, file, sideFile] = process.argv.slice(2);
const { dbcFor, decodeFrame } = require(path.join(sm, 'server/canDbc.js'));
const { busMapOf, internalBusesOf, INTERNAL_BUS_MESSAGES, INTERNAL_YIELD_MS } = require(path.join(sm, 'server/canIndex.js'));
const side = sideFile ? JSON.parse(fs.readFileSync(sideFile, 'utf8')) : null;
const dbc = dbcFor('tesla-3y');
const busOf = busMapOf(side, dbc.buses);
const internal = internalBusesOf(side);
const LINE = /^\((\d+)\.(\d+)\)\s+([A-Za-z][A-Za-z0-9_]{0,15})\s+([0-9A-Fa-f]{1,8})#([0-9A-Fa-f]*)\s*$/;
const out = {};
const carSent = new Map();
for (const line of fs.readFileSync(file, 'latin1').split('\n')) {
  const m = LINE.exec(line);
  if (!m) continue;
  const t = Number(m[1]) * 1000 + Number(m[2]) / 1000;
  const iface = m[3];
  const id = parseInt(m[4], 16);
  const msg = dbc.messages.get(id);
  if (!msg) continue;
  const want = busOf[iface];
  let only = null;
  if (want) {
    if (msg.bus !== want) continue;
    if (INTERNAL_BUS_MESSAGES.has(id)) carSent.set(id, t);
  } else {
    only = INTERNAL_BUS_MESSAGES.get(id);
    if (!only || !internal.includes(iface)) continue;
    const car = carSent.get(id);
    if (car !== undefined && Math.abs(t - car) < INTERNAL_YIELD_MS) continue;
  }
  for (const [sig, v] of decodeFrame(msg, Buffer.from(m[5], 'hex'))) {
    const nm = sig.series || sig.name;
    if (only && !only.has(nm)) continue;
    const s = out[nm];
    if (!s) out[nm] = [1, v, v, v, v, v];
    else { s[0]++; s[2] = v; if (v < s[3]) s[3] = v; if (v > s[4]) s[4] = v; s[5] += v; }
  }
}
process.stdout.write(JSON.stringify(Object.fromEntries(Object.entries(out).sort())));
