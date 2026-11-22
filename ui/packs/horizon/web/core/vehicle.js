// Vehicle info: everything the server knows of the car's build (GET /cars/{car} `vehicle`), each fact with
// where it came from (the car's own CAN configuration, the VIN, the Tesla API, an import, or what an admin
// typed) and how sure it is. Two inputs that disagree are shown side by side; a code the tables do not know
// says so; a fact nobody told says unknown, never a guess. An admin can type a value over any fact the server
// takes (PATCH /cars/{car} with `version` and `config`), or forget a typed one (null).
//
//   vehiclePanel(ctx, car, { onSaved }) -> { el, set(car) }

import { h, replace } from '../shared/dom.js';
import { format, convert, toMetric, unitLabel } from '../shared/units.js';
import { formatNumber } from '../shared/i18n.js';
import { hud, btn, tag, note } from './ui.js';
import { field, form } from './form.js';
import { openDialog, toast } from './layer.js';
import { errorWords } from './states.js';
import { isAdmin } from './prefs.js';

const enc = encodeURIComponent;
export const FACT_SOURCE = Object.freeze({
  owner: ['Typed', 'org', 'Typed by an admin'],
  can: ['CAN', 'cyan', 'The car\'s own configuration on the CAN bus (SMO)'],
  vin: ['VIN', null, 'Worked out from the VIN with built-in tables'],
  tesla_api: ['Tesla API', 'violet', 'The Tesla API\'s vehicle configuration'],
  teslamate: ['Import', 'org', 'The car row of the imported TeslaMate history'],
  teslafi: ['Import', 'org', 'The imported TeslaFi history'],
  import: ['Import', 'org', 'Imported history'],
});
const CONF = { high: ['Sure', 'grn', 'Tesla\'s filing or service manual, the EPA\'s data, or the car\'s own configuration'], medium: ['Likely', 'cyan', 'Several sources agree'], low: ['To check', 'org', 'One source only: worth checking'] };
const CHEM = { lfp: 'LFP (iron phosphate)', nca_nmc: 'NCA or NMC (nickel)' };
const yesNo = (v) => (v ? 'Yes' : 'No');

// [field, label, how to show it, the config key an admin types it under, the kind of that input]
const GROUPS = [
  ['The car', [
    ['model', 'Model', null, 'model', 'text'], ['model_year', 'Model year', null, 'model_year', 'whole', [2008, 2045]],
    ['trim', 'Trim', null, 'trim', 'text'], ['maker', 'Maker'], ['plant', 'Plant', null, 'plant', 'text'], ['built_in', 'Built in'],
    ['platform', 'Platform', (v) => ({ legacy: 'Legacy (before the 2021 refresh)', refresh: 'Refresh (2021 on)' }[v] || v)],
    ['body', 'Body', null, 'body', 'text'], ['restraint', 'Restraints', null, 'restraint', 'text'], ['seats', 'Seats', null, 'seats', 'whole', [1, 9]],
  ]],
  ['Drive and battery', [
    ['drive', 'Drive', null, 'drivetrain', 'select', [['AWD', 'AWD (all-wheel drive)'], ['RWD', 'RWD (rear-wheel drive)']]],
    ['motors', 'Motors', null, 'motors', 'whole', [1, 4]], ['performance', 'Performance', yesNo, 'performance_package', 'bool'],
    ['battery_type', 'Battery (VIN)', null, 'battery_type', 'text'], ['chemistry', 'Chemistry', (v) => CHEM[v] || v, 'chemistry', 'select', [['lfp', CHEM.lfp], ['nca_nmc', CHEM.nca_nmc]]],
    ['pack', 'Pack'], ['pack_code', 'Pack code', null, 'pack_kwh_code', 'text'],
    ['nominal_kwh', 'Pack energy', 'kwh', 'nominal_kwh', 'number', [5, 300]], ['usable_kwh', 'Usable energy', 'kwh', 'usable_kwh', 'number', [5, 300]],
    ['epa_range_km', 'EPA range when new', 'range', 'epa_range_km', 'distance', [10, 2000]],
  ]],
  ['Outside and options', [
    ['wheel', 'Wheels', null, 'wheel', 'text'], ['wheel_in', 'Wheel size', (v) => `${formatNumber(v)} in`, 'wheel_in', 'whole', [14, 24]],
    ['paint', 'Paint', null, 'paint', 'text'], ['roof', 'Roof', null, 'roof', 'text'], ['spoiler', 'Spoiler', null, 'spoiler', 'text'],
    ['steering', 'Steering', (v) => (v === 'right' ? 'Right-hand drive' : v === 'left' ? 'Left-hand drive' : v), 'right_hand_drive', 'steer'],
    ['region', 'Map region', null, 'map_region', 'text'], ['supercharging_access', 'Supercharging', null, 'supercharging_access', 'text'],
  ]],
];

function show(v, how) {
  if (v == null || v === '') return null;
  if (how === 'kwh') {
    const r = typeof v === 'object' ? v : { min: v, max: v };
    return r.min === r.max || r.max == null ? format(r.min, 'energy') : `${format(r.min, 'energy', { unit: false })} to ${format(r.max, 'energy')}`;
  }
  if (how === 'range') return format(v, 'distance');
  if (typeof how === 'function') return how(v);
  if (typeof v === 'boolean') return yesNo(v);
  return String(v);
}
const srcTag = (s) => {
  const x = FACT_SOURCE[s];
  return x ? h('span', { class: ['tag', x[1] && `tag--${x[1]}`, 'vi-src'], title: x[2] }, x[0]) : s ? tag(s) : null;
};

export function vehiclePanel(ctx, car, { onSaved, foot = null } = {}) {
  const body = h('div', { class: 'vi' });
  const side = h('span', { class: 'trk-count' });
  const el = hud({ title: 'Vehicle info', icon: 'car', id: 'vehicle', side, cls: 'vi-panel' }, body);
  let cur = car;

  function paint() {
    const v = cur.vehicle || {};
    const dis = new Map((v.disagreements || []).map((d) => [d.field, d.values || []]));
    const unk = new Map();
    for (const u of v.unknown_codes || []) { if (!unk.has(u.field)) unk.set(u.field, []); unk.get(u.field).push(u); }
    const admin = isAdmin();
    let known = 0, all = 0;
    const groups = GROUPS.map(([title, rows]) => h('section', { class: 'vi-group', 'aria-label': title },
      h('h3', { class: 'vi-head' }, title),
      h('dl', { class: 'vi-list' }, rows.map(([f, label, how, key, kind, extra]) => {
        const fact = v[f] || {};
        let lab = label;
        if (f === 'model_year' && v.year_kind === 'build_year') lab = 'Year built';
        const text = show(fact.value, how);
        all++; if (text != null) known++;
        const d = dis.get(f);
        const u = unk.get(f);
        const alt = f === 'trim' && text == null && v.trim_candidates?.length ? `could be ${v.trim_candidates.join(', ')}`
          : f === 'wheel_in' && text == null && v.wheel_options_in?.length ? `factory sizes ${v.wheel_options_in.map((x) => `${x} in`).join(', ')}` : null;
        const conf = CONF[fact.confidence];
        return h('div', { class: 'vi-row' },
          h('dt', { class: 'vi-k' }, lab),
          h('dd', { class: 'vi-v' },
            h('span', { class: ['vi-val', text == null && 'is-unknown'] }, text ?? 'Unknown'),
            text != null ? h('span', { class: 'vi-tags' }, srcTag(fact.source), conf ? h('span', { class: ['tag', `tag--${conf[1]}`], title: conf[2] }, conf[0]) : null) : null,
            fact.note ? h('span', { class: 'vi-note' }, fact.note) : null,
            alt ? h('span', { class: 'vi-note' }, alt) : null,
            u ? h('span', { class: 'vi-note warn' }, u.map((x) => `${x.at ? `${x.at}: ` : ''}code ${x.code} is not in the tables`).join('; '), ', so no value is given.') : null,
            d?.length > 1 ? h('span', { class: 'vi-dis' }, h('b', null, 'Disagree: '), d.map((x, i) => h('span', { class: 'vi-dis-i' }, i ? 'vs ' : '', `${show(x.value, how) ?? String(x.value)} `, srcTag(x.source), i ? null : ' (shown)'))) : null),
          admin && key ? h('dd', { class: 'vi-act' }, btn(fact.source === 'owner' ? 'Change' : 'Set', () => edit(f, lab, how, key, kind, extra, fact), { sm: true, kind: 'ghost', label: `${fact.source === 'owner' ? 'Change' : 'Set'} ${lab.toLowerCase()}` })) : null);
      }))));
    replace(side, `${formatNumber(known)} of ${formatNumber(all)} known`);
    replace(body,
      h('p', { class: 'small vi-intro' }, 'Worked out on this server from the VIN start ', h('code', { class: 'cd' }, v.vin_prefix || cur.vin_prefix || '—'), ', the car\'s own configuration on the CAN bus, the Tesla API and imports, with built-in tables: nothing is looked up outside. Where they differ, an admin\'s value wins, then the car\'s own, then the VIN\'s.'),
      h('div', { class: 'vi-groups' }, groups),
      v.notes?.length ? h('ul', { class: 'vi-notes' }, v.notes.map((n) => h('li', null, n))) : null,
      admin ? null : note('Only an admin can type a value over one of these.'),
      foot);
  }

  function edit(f, label, how, key, kind, extra, fact) {
    const typed = fact.source === 'owner';
    let input;
    const v0 = fact.value;
    if (kind === 'select') input = field({ label, name: 'value', type: 'select', value: v0 ?? '', options: [['', 'Choose…'], ...extra] });
    else if (kind === 'bool') input = field({ label, name: 'value', type: 'select', value: v0 == null ? '' : String(v0), options: [['', 'Choose…'], ['true', 'Yes'], ['false', 'No']] });
    else if (kind === 'steer') input = field({ label, name: 'value', type: 'select', value: v0 ?? '', options: [['', 'Choose…'], ['left', 'Left-hand drive'], ['right', 'Right-hand drive']] });
    else if (kind === 'distance') {
      const shown = v0 == null ? '' : String(Math.round(convert(v0, 'distance')));
      input = field({ label: `${label} (${unitLabel('distance')})`, name: 'value', type: 'number', value: shown, attrs: { min: '1', step: '1', inputmode: 'numeric' } });
    } else if (kind === 'number' || kind === 'whole') {
      const val = how === 'kwh' && v0 && typeof v0 === 'object' ? v0.min : v0;
      input = field({ label: how === 'kwh' ? `${label} (kWh)` : label, name: 'value', type: 'number', value: val ?? '', attrs: { min: String(extra[0]), max: String(extra[1]), step: kind === 'whole' ? '1' : '0.1', inputmode: kind === 'whole' ? 'numeric' : 'decimal' } });
    } else input = field({ label, name: 'value', value: v0 ?? '', attrs: { maxlength: '64', autocomplete: 'off' } });
    input.path = `config.${key}`;
    let d = null;
    const read = (raw) => {
      if (raw === '' || raw == null) return undefined;
      if (kind === 'bool') return raw === 'true';
      if (kind === 'steer') return raw === 'right';
      if (kind === 'distance') return Math.round(toMetric(Number(raw), 'distance') * 10) / 10;
      if (kind === 'number' || kind === 'whole') return Number(raw);
      return String(raw).trim() || undefined;
    };
    async function send(value) {
      try {
        const r = await ctx.api.patch(`cars/${enc(cur.id)}`, { version: cur.version, config: { [key]: value } });
        if (!ctx.alive()) return;
        set(r);
        toast(value === null ? `${label}: your value is forgotten; the car's own or the VIN's is shown again.` : `${label}: saved.`);
        d?.close();
        onSaved?.(r);
      } catch (e) {
        if (e?.status === 409 || e?.code === 'stale_version') {
          try { const fresh = await ctx.api.get(`cars/${enc(cur.id)}`); if (ctx.alive()) set(fresh); } catch { /* the message says it */ }
          throw Object.assign(e, { fields: null, message: 'Someone changed this car meanwhile. The panel shows their change now; look, then try again.' });
        }
        if (e?.fields) e.fields = Object.fromEntries(Object.entries(e.fields).map(([k, t]) => [k === `config.${key}` || k === key ? input.path : k, t]));
        throw e;
      }
    }
    const forget = typed ? btn('Forget my value', async (ev) => {
      const b = ev.currentTarget; b.disabled = true;
      try { await send(null); } catch (e) { fm.setMessage(e?.message || errorWords(e).text); } finally { b.disabled = false; }
    }, { kind: 'ghost', icon: 'x' }) : null;
    const fm = form({ fields: [input], submit: 'Save', secondary: forget, async onSubmit(vals) {
      const value = read(vals.value);
      if (value === undefined) { input.setError(typed ? 'Give a value, or press Forget my value.' : 'Give a value first.'); return; }
      await send(value);
    } });
    d = openDialog({ title: `${label} of ${cur.name}`, body: [
      h('p', { class: 'small' }, 'Now: ', h('b', null, show(fact.value, how) ?? 'unknown'), fact.source ? [' · ', FACT_SOURCE[fact.source]?.[2] || fact.source] : null, '.'),
      h('p', { class: 'small faint' }, 'A typed value beats every other source until you forget it.'), fm.el] });
    input.input.focus();
  }

  function set(c) { cur = c; Object.assign(car, c); paint(); }
  paint();
  return { el, set };
}
