// Tests for live.js: a figure follows its events while the stream is open, reads its endpoint once at the start and
// after a reconnect, and polls only while the stream is down; rows bursts are folded; text is set only when changed.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { follow, onRows, jobOf, carOf, setText, patchFigures, liveWords } from '../live.js';
import * as F from '../fullrebuild.js';

function fakeTimers() {
  let now = 0, seq = 0;
  const due = new Map();
  return {
    setTimeout(f, ms) { const id = ++seq; due.set(id, { at: now + ms, f }); return id; },
    clearTimeout(id) { due.delete(id); },
    async advance(ms) {
      const end = now + ms;
      for (;;) {
        const next = [...due.entries()].sort((a, b) => a[1].at - b[1].at)[0];
        if (!next || next[1].at > end) break;
        now = next[1].at;
        due.delete(next[0]);
        next[1].f();
        await new Promise((r) => setImmediate(r));
      }
      now = end;
    },
    pending: () => due.size,
  };
}

function fakeApi(state = 'connecting') {
  const subs = new Set();
  const api = {
    state,
    events(onEvent, opts) {
      const sub = { onEvent, opts };
      subs.add(sub);
      opts.onStatus?.({ state: api.state });
      return () => subs.delete(sub);
    },
    setState(s) { api.state = s; for (const x of subs) x.opts.onStatus?.({ state: s }); },
    fire(type, data) { for (const x of subs) if (!x.opts.types || x.opts.types.includes(type)) x.onEvent({ type, data }); },
    subs,
  };
  return api;
}

const tick = () => new Promise((r) => setImmediate(r));

test('open stream: one read, then events only, no polling', async () => {
  const api = fakeApi('open');
  const t = fakeTimers();
  let loads = 0;
  const seen = [];
  const f = follow(api, {
    type: 'job', match: jobOf('full_rebuild'), pick: (d) => d.status, timers: t,
    load: async () => { loads++; return { state: 'running', logs: { done: 1, total: 10 } }; },
    onValue: (v, how) => seen.push([how, v.logs.done]), pollMs: 2000,
  });
  await tick();
  assert.equal(loads, 1, 'read once at the start');
  api.fire('job', { kind: 'compaction', id: '', status: {} });
  api.fire('job', { kind: 'full_rebuild', id: '', status: { state: 'running', logs: { done: 5, total: 10 } } });
  await t.advance(60000);
  assert.equal(loads, 1, 'no polling while the stream is open');
  assert.deepEqual(seen, [['load', 1], ['event', 5]]);
  assert.equal(f.live, true);
  f.stop();
  assert.equal(api.subs.size, 0);
});

test('a read started before an event applied does not overwrite it', async () => {
  const api = fakeApi('open');
  const t = fakeTimers();
  let finish;
  const seen = [];
  const f = follow(api, {
    type: 'job', timers: t, pollMs: null,
    load: () => new Promise((r) => { finish = r; }),
    onValue: (v, how) => seen.push([how, v.progress]),
  });
  api.fire('job', { progress: 90 });
  finish({ progress: 10 });
  await tick();
  assert.deepEqual(seen, [['event', 90]]);
  assert.equal(f.value.progress, 90);
  f.stop();
});

test('a read an event overtook is kept with the event put over it (overtaken), and read once', async () => {
  const api = fakeApi('open');
  const t = fakeTimers();
  let finish, loads = 0;
  const seen = [];
  const f = follow(api, {
    type: 'job', timers: t, pollMs: null,
    load: () => { loads++; return new Promise((r) => { finish = r; }); },
    overtaken: (read, ev) => ({ ...read, progress: ev.progress }),
    onValue: (v, how) => seen.push([how, v.progress, v.categories]),
  });
  api.fire('job', { progress: 90 });
  finish({ progress: 10, categories: 5 });
  await tick();
  assert.deepEqual(seen, [['event', 90, undefined], ['load', 90, 5]]);
  assert.equal(loads, 1);
  f.stop();
});

test('stream down: polls; back up: reads once to catch up and stops polling', async () => {
  const api = fakeApi('reconnecting');
  const t = fakeTimers();
  let loads = 0;
  const f = follow(api, { type: 'health', match: carOf('c1'), pick: (d) => d.health, timers: t,
    load: async () => { loads++; return { logs_waiting: loads }; }, pollMs: 1000 });
  await tick();
  assert.equal(loads, 1);
  await t.advance(3500);
  assert.equal(loads, 4, 'the fallback reads every second');
  api.setState('open');
  await tick();
  assert.equal(loads, 5, 'one read after the reconnect');
  await t.advance(10000);
  assert.equal(loads, 5, 'then none');
  api.fire('health', { car: 'c2', health: { logs_waiting: 99 } });
  assert.notEqual(f.value.logs_waiting, 99, 'another car is not this one');
  api.fire('health', { car: 'c1', health: { logs_waiting: 42 } });
  assert.equal(f.value.logs_waiting, 42);
  f.stop();
});

test('initial: the value the page has just read is the first one, not read again', async () => {
  for (const state of ['open', 'connecting']) {
    const api = fakeApi(state);
    const t = fakeTimers();
    let loads = 0;
    const seen = [];
    const f = follow(api, { type: 'job', timers: t, load: async () => { loads++; return { n: loads }; }, initial: { n: 0 },
      onValue: (v, how) => seen.push([how, v.n]), pollMs: 1000 });
    await tick();
    assert.equal(loads, 0, `${state}: no read at the start`);
    assert.deepEqual(f.value, { n: 0 });
    assert.deepEqual(seen, [], `${state}: the page painted it already`);
    if (state === 'connecting') {
      await t.advance(1000);
      assert.equal(loads, 1, 'the fallback reads go on as before');
      api.setState('open');
      await tick();
      assert.equal(loads, 2, 'and the read after the stream comes up');
    }
    f.stop();
  }
});

test('status updates while still down keep the planned read', async () => {
  const api = fakeApi('connecting');
  const t = fakeTimers();
  let loads = 0;
  follow(api, { type: 'health', timers: t, load: async () => { loads++; return {}; }, pollMs: 5000 });
  await tick();
  for (let i = 0; i < 6; i++) { await t.advance(2000); api.setState(i % 2 ? 'connecting' : 'reconnecting'); }
  assert.equal(loads, 3, 'a read every 5 s however often the stream retries');
});

test('pollMs as a function: slower when idle, none when null', async () => {
  const api = fakeApi('closed');
  const t = fakeTimers();
  let loads = 0, state = 'running';
  follow(api, { type: 'job', timers: t, load: async () => { loads++; return { state }; },
    pollMs: (s) => (s?.state === 'running' ? 2000 : null) });
  await tick();
  await t.advance(4100);
  assert.equal(loads, 3);
  state = 'done';
  await t.advance(2000);
  assert.equal(loads, 4);
  await t.advance(60000);
  assert.equal(loads, 4, 'nothing to poll for once done');
});

test('rows bursts are folded into one read', async () => {
  const api = fakeApi('open');
  const t = fakeTimers();
  const runs = [];
  onRows(api, { car: 'c1', kinds: ['drives'], waitMs: 1000, run: (d) => runs.push(d.kinds), timers: t });
  api.fire('rows', { car: 'c1', kinds: ['drives'] });
  api.fire('rows', { car: 'c2', kinds: ['drives'] });
  api.fire('rows', { car: 'c1', kinds: ['charges'] });
  await t.advance(500);
  api.fire('rows', { car: 'c1', kinds: ['states', 'drives'] });
  await t.advance(999);
  assert.equal(runs.length, 0);
  await t.advance(1);
  assert.deepEqual(runs, [['states', 'drives']]);
});

test('text is set only when it changed', () => {
  const el = { textContent: '1 of 2', writes: 0 };
  const proxy = new Proxy(el, { set(o, k, v) { if (k === 'textContent') o.writes++; o[k] = v; return true; } });
  assert.equal(setText(proxy, '1 of 2'), false);
  assert.equal(setText(proxy, '2 of 2'), true);
  assert.equal(el.writes, 1);
  const a = { textContent: 'x', getAttribute: () => 'a' };
  const b = { textContent: 'y', getAttribute: () => 'b' };
  assert.equal(patchFigures({ querySelectorAll: () => [a, b] }, { a: 'x', b: 'z' }), 1);
  assert.equal(b.textContent, 'z');
  assert.equal(liveWords({ state: 'open' }).tone, 'live');
  assert.equal(liveWords({ state: 'reconnecting' }).tone, 'wait');
});

test('the rebuild panel from the screenshot: the bar follows the logs, days and data fill in, speed and time left show', () => {
  const NOW = Date.UTC(2026, 10, 22, 12);
  // what the server sent before the fix: no bytes of the decoded logs, no days yet
  const old = { state: 'running', phase: 'decoding', cars_total: 1, car: 'c1', logs: { done: 634, total: 20494 },
    days: { done: 0, total: 0 }, bytes: { done: 0, total: 134e6 }, rate_bytes_per_s: null, eta_ms: null,
    last_progress_ms: NOW - 1000, errors: [] };
  assert.equal(F.percent(old), 3, 'the logs, not the bytes, give the percent');
  assert.equal(F.fraction(old), 0.03);
  const by = (s) => Object.fromEntries(F.lines(s, NOW).map((l) => [l.key, l.text]));
  assert.equal(by(old).days, 'Counted once the logs are decoded', 'never blank while running');
  assert.equal(by(old).rate, 'Working out the speed');
  // what it sends now
  const now = { ...old, percent: 3, days: { done: 0, total: 412 }, bytes: { done: 4.1e6, total: 134e6 },
    rate_bytes_per_s: 410e3, rate_logs_per_s: 63.4, eta_ms: 313000 };
  assert.equal(F.percent(now), 3);
  assert.equal(by(now).logs, '634 of 20,494');
  assert.equal(by(now).days, '0 of 412');
  assert.equal(by(now).bytes, '4.1 MB of 134 MB');
  assert.equal(by(now).rate, '410 kB/s · 63 logs/s · about 5 min left');
  const days = { ...now, phase: 'days', percent: undefined, days: { done: 103, total: 412 }, rate_bytes_per_s: null,
    rate_logs_per_s: null, rate_days_per_s: 2.5, eta_ms: 124000 };
  assert.equal(F.percent(days), 25, 'then the days give it');
  assert.equal(by(days).rate, '2.5 days/s · about 2 min left');
});
