// Car overview, the Vehicle info panel (#vehicle-info): everything the server knows of the car's build
// (GET /cars/{car} `vehicle`), each fact with where it came from (the car's CAN configuration, the VIN, the
// Tesla API, the owner, an import) and how sure it is. Two inputs that disagree are listed; a fact nothing says
// is "unknown", never guessed. An admin can type a value over any fact the server takes from the owner
// (PATCH /cars/{car} { version, config: { key: value } }; null forgets the typed value).

import { h, replace } from '../shared/dom.js';
import { format, convert, toMetric, unitLabel } from '../shared/units.js';
import { formatNumber } from '../shared/i18n.js';
import { icon } from '../core/icons.js';
import { panel, tag } from '../core/page.js';
import { field, form } from '../core/form.js';
import { openDialog } from '../core/dialog.js';
import { flash } from '../core/flash.js';
import { buildUrl } from '../shared/state.js';

const enc = encodeURIComponent;

// where a fact came from: [short word, the whole line]
export const FACT_SOURCE = {
  can: ['CAN', 'The car\'s own configuration, read from its CAN bus (SMO)'],
  vin: ['VIN', 'The VIN prefix, through Tesla\'s and the EPA\'s tables'],
  tesla_api: ['Tesla API', 'Tesla\'s own vehicle configuration, from the Tesla API'],
  owner: ['Owner', 'Typed by an admin here'],
  teslamate: ['Import', 'The car row of a TeslaMate import'],
  teslafi: ['Import', 'A TeslaFi import'],
};
const CONFIDENCE = {
  high: ['high', 'live', 'Tesla\'s filing or service manual, the EPA\'s data, the car\'s own configuration, or the owner'],
  medium: ['medium', 'plain', 'Several sources agree'],
  low: ['low', 'warm', 'One source only: worth checking'],
};

const yesNo = (v) => (v ? 'Yes' : 'No');
const kwh = (v) => (v == null ? null : v.min === v.max ? format(v.min, 'energy') : `${format(v.min, 'energy')} to ${format(v.max, 'energy')}`);
let regions = null;
const country = (c) => { try { regions ||= new Intl.DisplayNames(undefined, { type: 'region' }); return `${regions.of(c)} (${c})`; } catch { return c; } };

// [fact, label, words(value), edit] edit: { key, type: text|whole|number|select|distance, options, lo, hi }
const GROUPS = [
  ['Build', 'car', [
    ['model', 'Model', null, { key: 'model', type: 'text' }],
    ['model_year', 'Year', null, { key: 'model_year', type: 'whole', lo: 2008, hi: 2045 }],
    ['maker', 'Maker'],
    ['plant', 'Plant', null, { key: 'plant', type: 'text' }],
    ['built_in', 'Built in', country],
    ['platform', 'Platform', (v) => ({ legacy: 'Legacy (before the 2021 refresh)', refresh: 'Refresh (2021 on)' }[v] || v)],
    ['body', 'Body', null, { key: 'body', type: 'text' }],
    ['restraint', 'Restraints', null, { key: 'restraint', type: 'text' }],
    ['seats', 'Seats', (v) => formatNumber(v), { key: 'seats', type: 'whole', lo: 1, hi: 9 }],
  ]],
  ['Battery', 'battery', [
    ['chemistry', 'Chemistry', (v) => ({ lfp: 'LFP (iron phosphate)', nca_nmc: 'NCA / NMC (nickel)' }[v] || v), { key: 'chemistry', type: 'select', options: [['lfp', 'LFP (iron phosphate)'], ['nca_nmc', 'NCA / NMC (nickel)']] }],
    ['battery_type', 'Battery type (VIN)', null, { key: 'battery_type', type: 'text' }],
    ['pack', 'Pack'],
    ['pack_code', 'Pack code', null, { key: 'pack_kwh_code', type: 'text' }],
    ['nominal_kwh', 'Pack energy (full)', kwh, { key: 'nominal_kwh', type: 'number', lo: 5, hi: 300, unit: 'kWh', from: (v) => v?.min }],
    ['usable_kwh', 'Usable energy', kwh, { key: 'usable_kwh', type: 'number', lo: 5, hi: 300, unit: 'kWh', from: (v) => v?.min }],
    ['epa_range_km', 'EPA range when new', (v) => format(v, 'distance'), { key: 'epa_range_km', type: 'distance' }],
  ]],
  ['Drive', 'steer', [
    ['trim', 'Trim', null, { key: 'trim', type: 'text' }],
    ['drive', 'Drive', null, { key: 'drivetrain', type: 'select', options: [['RWD', 'RWD'], ['AWD', 'AWD']] }],
    ['motors', 'Motors', (v) => formatNumber(v), { key: 'motors', type: 'whole', lo: 1, hi: 4 }],
    ['performance', 'Performance', yesNo, { key: 'performance_package', type: 'select', options: [['true', 'Yes'], ['false', 'No']], to: (v) => v === 'true', from: (v) => (v == null ? '' : String(v)) }],
  ]],
  ['Look', 'car', [
    ['paint', 'Paint', null, { key: 'paint', type: 'text' }],
    ['wheel', 'Wheels', null, { key: 'wheel', type: 'text' }],
    ['wheel_in', 'Wheel size', (v) => `${formatNumber(v)} in`, { key: 'wheel_in', type: 'whole', lo: 14, hi: 24 }],
    ['roof', 'Roof', null, { key: 'roof', type: 'text' }],
    ['spoiler', 'Spoiler', null, { key: 'spoiler', type: 'text' }],
  ]],
  ['Market', 'globe', [
    ['steering', 'Steering', (v) => ({ left: 'Left-hand drive', right: 'Right-hand drive' }[v] || v), { key: 'right_hand_drive', type: 'select', options: [['false', 'Left-hand drive'], ['true', 'Right-hand drive']], to: (v) => v === 'true', from: (v) => (v == null ? '' : String(v === 'right')) }],
    ['region', 'Region', null, { key: 'map_region', type: 'text' }],
    ['supercharging_access', 'Supercharging', null, { key: 'supercharging_access', type: 'text' }],
  ]],
];
const LABEL = Object.fromEntries(GROUPS.flatMap(([, , rows]) => rows.map(([k, l]) => [k, l])));
const WORDS = Object.fromEntries(GROUPS.flatMap(([, , rows]) => rows.map(([k, , w]) => [k, w])));
const words = (k, v) => (v == null ? null : WORDS[k] ? WORDS[k](v) : typeof v === 'boolean' ? yesNo(v) : typeof v === 'object' ? JSON.stringify(v) : String(v));
const srcChip = (s) => (s ? h('span', { class: ['vi-src', `vi-src--${s}`], title: FACT_SOURCE[s]?.[1] || s }, FACT_SOURCE[s]?.[0] || s) : null);

/**
 * The panel. ctx: the car page's. Returns the panel element; it paints from ctx.car and reads the car again
 * after a change.
 */
export function vehicleInfoPanel(ctx) {
  const cid = ctx.car.id;
  let car = ctx.car;
  const body = h('div', { class: 'vi-body' });
  const el = panel({ title: 'Vehicle info', icon: 'car', cls: 'vi', id: 'vehicle-info',
    side: h('span', { class: 'vi-fw-links' },
      h('a', { class: 'panel-link', href: `${buildUrl('software', { car: cid })}#firmware` }, 'Car firmware', icon('chev-r', 'ico--sm')),
      h('a', { class: 'panel-link', href: `${buildUrl('software', { car: cid })}#firmware-history`, title: 'Every fingerprint each module has had, over time' }, 'Firmware history', icon('chev-r', 'ico--sm'))) }, body);

  function paint() {
    const v = car.vehicle || {};
    const dis = v.disagreements || [];
    const disOf = new Map(dis.map((d) => [d.field, d]));
    const unknownCodes = v.unknown_codes || [];
    replace(body,
      h('p', { class: 'note vi-intro' }, 'What the server knows of this car\'s build, worked out here from the VIN prefix, the car\'s own configuration on its CAN bus, the Tesla API, an import and what an admin typed. Nothing is looked up elsewhere. Each fact says where it came from and how sure it is; a fact nothing tells stays unknown.',
        ctx.admin ? ' Change one to type the right value: it beats every other source.' : ' Only an admin can correct a fact.'),
      dis.length ? h('div', { class: 'vi-alert', role: 'note' }, icon('warn', 'ico--sm'), h('div', null,
        h('b', null, dis.length === 1 ? 'Two inputs disagree. ' : `Inputs disagree on ${dis.length} facts. `),
        h('ul', { class: 'vi-dis-list' }, dis.map((d) => h('li', null, h('b', null, `${LABEL[d.field] || d.field}: `),
          (d.values || []).map((x, i) => [i ? ', ' : '', `${words(d.field, x.value)} (${FACT_SOURCE[x.source]?.[0] || x.source}${i === 0 ? ', shown' : ''})`])))))) : null,
      h('div', { class: 'vi-groups' }, GROUPS.map(([title, ic, rows]) => h('section', { class: 'vi-group', 'aria-label': title },
        h('h3', { class: 'vi-group-title' }, icon(ic, 'ico--sm'), title),
        h('dl', { class: 'vi-list' }, rows.map(([k, label, , edit]) => factRow(k, label, v[k], edit, disOf.get(k))))))),
      h('div', { class: 'vi-foot' },
        h('p', { class: 'vi-meta' }, h('span', { class: 'faint' }, 'VIN prefix '), h('span', { class: 'key-chip' }, v.vin_prefix || car.vin_prefix || '—'),
          v.year_kind === 'build_year' ? h('span', { class: 'faint' }, ' · the year is the calendar year built (Shanghai and Berlin cars)') : null),
        v.trim_candidates?.length ? h('p', { class: 'vi-meta' }, h('span', { class: 'faint' }, 'The trim can be one of: '), v.trim_candidates.join(', ')) : null,
        v.epa_name ? h('p', { class: 'vi-meta' }, h('span', { class: 'faint' }, 'EPA listing: '), v.epa_name) : null,
        v.wheel_options_in?.length ? h('p', { class: 'vi-meta' }, h('span', { class: 'faint' }, 'Factory wheel sizes: '), v.wheel_options_in.map((x) => `${x} in`).join(', ')) : null,
        unknownCodes.length ? h('p', { class: 'vi-meta' }, h('span', { class: 'faint' }, 'Codes the tables do not know (no value given for them): '),
          unknownCodes.map((u, i) => [i ? '; ' : '', `${LABEL[u.field] || u.field}: ${u.code} at ${u.at} (${FACT_SOURCE[u.source]?.[0] || u.source})`])) : null,
        (v.notes || []).map((n) => h('p', { class: 'vi-meta' }, n)),
        h('p', { class: 'vi-key' }, h('span', { class: 'faint' }, 'Sources: '), Object.entries(FACT_SOURCE).filter(([k]) => k !== 'teslafi').map(([k, [w, l]]) => h('span', { class: 'vi-key-item' }, srcChip(k), ` ${l.replace(/^The /, 'the ')}`)))));
  }

  function factRow(k, label, f, edit, dis) {
    const has = f && f.value != null;
    const conf = CONFIDENCE[f?.confidence];
    const btn = ctx.admin && edit ? h('button', { class: 'btn btn--ghost btn--sm vi-edit', type: 'button', 'aria-label': `Change ${label}`, title: `Type the right ${label.toLowerCase()}`, onclick: () => change(k, label, f, edit) }, icon('settings', 'ico--sm'), 'Change') : null;
    return h('div', { class: ['vi-row', dis && 'has-dis'] },
      h('dt', { class: 'vi-label' }, label),
      h('dd', { class: 'vi-val' },
        h('span', { class: ['vi-value', !has && 'is-unknown'] }, has ? words(k, f.value) : 'unknown'),
        h('span', { class: 'vi-how' },
          has ? srcChip(f.source) : h('span', { class: 'faint' }, 'nothing says it'),
          has && conf ? h('span', { class: ['vi-conf', `vi-conf--${f.confidence}`], title: conf[2] }, `${conf[0]} confidence`) : null,
          dis ? tag('disagree', 'warm') : null),
        f?.note ? h('span', { class: 'vi-note' }, f.note) : null),
      btn);
  }

  async function change(k, label, f, edit) {
    const cur = f?.value ?? null;
    const typed = f?.source === 'owner';
    let fld;
    const start = edit.from ? edit.from(cur) : cur;
    if (edit.type === 'select') {
      fld = field({ label, name: 'v', path: `config.${edit.key}`, type: 'select', value: start ?? '', options: [['', 'Choose…'], ...edit.options], required: true });
    } else if (edit.type === 'text') {
      fld = field({ label, name: 'v', path: `config.${edit.key}`, value: start ?? '', required: true, attrs: { maxlength: '100', autocomplete: 'off' } });
    } else {
      const dist = edit.type === 'distance';
      const shown = dist && start != null ? Math.round(convert(start, 'distance')) : start;
      const lo = dist ? 10 : edit.lo, hi = dist ? 2000 : edit.hi;
      fld = field({ label: `${label} (${dist ? unitLabel('distance') : edit.unit || 'number'})`, name: 'v', path: `config.${edit.key}`, type: 'number', value: shown ?? '', required: true,
        attrs: { step: edit.type === 'whole' ? '1' : 'any', min: dist ? null : String(lo), max: dist ? null : String(hi), inputmode: edit.type === 'whole' ? 'numeric' : 'decimal' } });
    }
    let done = false;
    const save = async (value) => {
      try {
        car = await ctx.api.patch(`cars/${enc(cid)}`, { version: car.version, config: { [edit.key]: value } });
      } catch (e) {
        if (e?.code === 'stale_version' || e?.status === 409) {
          car = await ctx.api.get(`cars/${enc(cid)}`).catch(() => car);
          paint();
          e.message = 'Someone else changed this car a moment ago, so nothing was saved. The panel now shows their change: make yours again if it is still needed.';
        }
        throw e;
      }
      if (!ctx.alive()) return;
      done = true;
      paint();
      flash(value == null ? `${label}: the typed value is forgotten.` : `${label} saved. It beats every other source.`);
      fl.el.closest('dialog')?.close();
    };
    const forget = typed ? h('button', { class: 'btn btn--ghost', type: 'button', onclick: async () => { fl.busy(true); try { await save(null); } catch (e) { fl.setMessage(e?.message || 'That did not work.'); } finally { fl.busy(false); } } }, 'Forget the typed value') : null;
    const fl = form({
      fields: [fld], submit: 'Save', secondary: forget,
      async onSubmit(vals) {
        let x = vals.v;
        if (edit.type === 'text') { x = String(x).trim(); if (!x) { fld.setError('Type a value, or use Forget.'); return; } }
        else if (edit.type === 'select') { if (x === '') { fld.setError('Choose one.'); return; } if (edit.to) x = edit.to(x); }
        else if (edit.type === 'distance') { x = toMetric(x, 'distance'); x = Math.round(x * 10) / 10; if (!(x >= 10 && x <= 2000)) { fld.setError(`From ${format(10, 'distance')} to ${format(2000, 'distance')}.`); return; } }
        else if (edit.type === 'whole' && !Number.isInteger(x)) { fld.setError('A whole number.'); return; }
        await save(x);
      },
    });
    const now = f?.value != null ? `Now ${words(k, f.value)}, ${FACT_SOURCE[f.source]?.[1]?.replace(/^The /, 'the ').replace(/^Typed/, 'typed') || 'source not known'}.` : 'Now unknown: no source says it.';
    await openDialog({ title: `${label} of ${car.name}`, body: [h('p', { class: 'note' }, now, ' A value you type beats every other source until you forget it.'), fl.el], actions: [] });
    return done;
  }

  paint();
  return el;
}
