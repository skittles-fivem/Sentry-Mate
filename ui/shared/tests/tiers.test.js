// Tests for tiers.js: tiered (block) prices split by the cycle's kWh, made-up numbers (up to 1,000 kWh at 0.16,
// 1,000 to 2,000 at 0.10, above 2,000 at 0.05), the words and the editor's checks.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as T from '../tiers.js';
import { setLocale } from '../i18n.js';
import { setUnits } from '../units.js';

const sp = (s) => String(s).replace(/[   ]/g, ' ');
const STEPS = [{ from_kwh: 0, per_kwh: 0.16 }, { from_kwh: 1000, per_kwh: 0.10 }, { from_kwh: 2000, per_kwh: 0.05 }];
const en = () => { setLocale('en-US'); setUnits({ distance: 'km', temperature: 'c', pressure: 'bar', range: 'rated' }); };
const round = (parts) => parts.map((p) => [p.step, Math.round(p.kwh * 1000) / 1000]);

test('a charge that crosses a step is split there', () => {
  assert.deepEqual(round(T.splitKwh(STEPS, 388, 650)), [[0, 612], [1, 38]]);
  // crossing two steps from 900: 100 + 1,000 + 100
  const p = T.splitKwh(STEPS, 900, 1200);
  assert.deepEqual(round(p), [[0, 100], [1, 1000], [2, 100]]);
  assert.ok(Math.abs(p.reduce((s, x) => s + x.cost, 0) - (16 + 100 + 5)) < 1e-9);
  assert.deepEqual([p[1].from_kwh, p[1].to_kwh, p[2].to_kwh], [1000, 2000, null]);
  // starting exactly on a step is in that step; nothing to split, nothing out
  assert.deepEqual(round(T.splitKwh(STEPS, 1000, 5)), [[1, 5]]);
  assert.deepEqual(T.splitKwh(STEPS, 0, 0), []);
  assert.deepEqual(T.splitKwh([], 0, 10), []);
});

test('the split reads as the charge page shows it', () => {
  en();
  assert.equal(sp(T.splitText(T.splitKwh(STEPS, 388, 650), 'USD')), '612 kWh at $0.16/kWh + 38 kWh at $0.10/kWh');
  assert.equal(T.stepParts({ parts: [{ rate: 'Night', from_kwh: null }, { rate: 'Step 1', from_kwh: 0 }] }).length, 1);
});

test('step ranges, cycle days and summaries in words', () => {
  en();
  assert.equal(sp(T.stepLabel(0, 1000)), 'Up to 1,000 kWh');
  assert.equal(sp(T.stepLabel(1000, 2000)), '1,000 to 2,000 kWh');
  assert.equal(sp(T.stepLabel(2000, null)), 'Above 2,000 kWh');
  assert.equal(T.stepLabel(0, null), 'Every kWh');
  assert.deepEqual(T.stepRange(STEPS, 1), { from: 1000, to: 2000 });
  assert.deepEqual(['1', '2', '3', '11', '21', '22', '28'].map((n) => T.ordinal(+n)), ['1st', '2nd', '3rd', '11th', '21st', '22nd', '28th']);
  assert.equal(T.cycleWords(1), 'Calendar month (from the 1st)');
  assert.equal(T.cycleWords(15), 'From the 15th of each month');
  assert.equal(T.cycleChoices().length, 28);
  assert.equal(sp(T.tiersSummary({ steps: STEPS }, 'USD')), '3 kWh steps from $0.16/kWh');
});

test('the worked example starts just under the second step, or at the household use', () => {
  const ex = T.workedExample({ steps: STEPS, base_kwh: 0 }, 1.5);
  assert.equal(ex.before, 980);
  assert.deepEqual(round(ex.parts), [[0, 20], [1, 30]]);
  assert.ok(Math.abs(ex.total - (20 * 0.16 + 30 * 0.10 + 1.5)) < 1e-9);
  const house = T.workedExample({ steps: STEPS, base_kwh: 1500 });
  assert.equal(house.before, 1500);
  assert.deepEqual(round(house.parts), [[1, 50]]);
});

test('the editor checks the steps the way the server does', () => {
  assert.deepEqual(T.tierErrors({ steps: STEPS, cycle_day: 1, base_kwh: 0 }), {});
  const e = T.tierErrors({ steps: [{ from_kwh: 0, per_kwh: null }, { from_kwh: 0, per_kwh: 11 }, { from_kwh: null, per_kwh: 0.1 }], cycle_day: 29, base_kwh: -1 });
  assert.deepEqual(Object.keys(e).sort(), ['price.tiers.base_kwh', 'price.tiers.cycle_day', 'price.tiers.steps.0.per_kwh',
    'price.tiers.steps.1.from_kwh', 'price.tiers.steps.1.per_kwh', 'price.tiers.steps.2.from_kwh'].sort());
  assert.ok(T.tierErrors({ steps: [] })['price.tiers.steps']);
  // the body always starts at 0 kWh and fills the defaults
  assert.deepEqual(T.tiersBody({ steps: [{ from_kwh: 5, per_kwh: 0.2 }], cycle_day: null, base_kwh: null }),
    { steps: [{ from_kwh: 0, per_kwh: 0.2 }], cycle_day: 1, base_kwh: 0 });
  assert.equal(T.copyTiers(null), null);
  assert.deepEqual(T.newTiers(0.3).steps[0], { from_kwh: 0, per_kwh: 0.3 });
});
