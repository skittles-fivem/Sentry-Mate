#!/usr/bin/env node
'use strict';
// A synthetic candump log that reaches every message of the Model 3/Y set (tests/parity.rs): pseudo-random payloads
// (xorshift32, fixed seed) for each message on its own bus, the same ids on the other bus (must not decode there),
// and the Screenmate's internal bus with 0x04F before and after a car bus's 0x04F. No car data.
//
//   node make-synthetic.js <Sentry-Manager checkout> > synthetic.candump
const path = require('path');
const { dbcFor } = require(path.join(process.argv[2], 'server/canDbc.js'));
const dbc = dbcFor('tesla-3y');
let x = 0x12345678;
const rnd = () => { x ^= x << 13; x >>>= 0; x ^= x >>> 17; x ^= x << 5; x >>>= 0; return x & 0xff; };
const hex = (n) => Array.from({ length: n }, () => rnd().toString(16).toUpperCase().padStart(2, '0')).join('');
let t = 1794099498000000;
const lines = ['# canbridge v1 synthetic'];
const frame = (iface, id, n) => { t += 137; lines.push(`(${Math.floor(t / 1e6)}.${String(t % 1e6).padStart(6, '0')}) ${iface} ${id.toString(16).toUpperCase().padStart(id > 0x7ff ? 8 : 3, '0')}#${hex(n)}`); };
const ids = [...dbc.messages.keys()].sort((a, b) => a - b);
for (let round = 0; round < 6; round++) {
  for (const id of ids) {
    const m = dbc.messages.get(id);
    const n = Math.min(8, m.size || 8);
    const own = m.bus === 'ChassisBus' ? 'can1' : 'can0';
    frame(own, id, n);
    if (round === 0) frame(own === 'can0' ? 'can1' : 'can0', id, n);
    if (round === 1) frame('can0', id, Math.max(0, n - 3)); // too short for some signals
  }
}
for (let i = 0; i < 20; i++) frame('can3', 0x04f, 8); // the Screenmate's GPS: decoded (no car bus sends it yet)
frame('can1', 0x04f, 8); // a car bus sends it
for (let i = 0; i < 5; i++) frame('can3', 0x04f, 8); // within 2.5 s: the car bus's wins
t += 3_000_000;
for (let i = 0; i < 5; i++) frame('can3', 0x04f, 8); // later: the internal one again
frame('can3', 0x3d9, 8); // never decoded on the internal bus
process.stdout.write(`${lines.join('\n')}\n`);
