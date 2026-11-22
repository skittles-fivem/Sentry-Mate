// The car overview's "## vehicle" block: everything the server knows of the car's build (GET /cars/{car} `vehicle`),
// each fact with where it came from (CAN, the VIN, the Tesla API, the owner, an import) and how sure it is.
// Unknown says unknown (never a guess); two inputs that disagree are listed; an admin can type a value over any of
// them, or forget a typed one (PATCH /cars/{car} { version, config: { key: value | null } }; 409 when the car changed
// meanwhile: the block shows the car as it is now and says so).

import { h, replace } from '../shared/dom.js';
import { format, convert, toMetric, unitLabel } from '../shared/units.js';
import { section, kv, note } from '../core/ui.js';
import { tag } from '../core/fmt.js';
import { field, form } from '../core/form.js';
import { openDialog } from '../core/dialog.js';
import { toast } from '../core/toast.js';

const enc = encodeURIComponent;
const SOURCE = { can: ['can', 'the car\'s own CAN configuration (SMO)'], vin: ['vin', 'the VIN and built-in tables'], tesla_api: ['api', 'the Tesla API'],
  owner: ['owner', 'typed by an admin'], teslamate: ['import', 'a TeslaMate import'], teslafi: ['import', 'a TeslaFi import'], import: ['import', 'an import'] };
const CONF = { high: ['sure', 'ok', 'Tesla\'s filing or service manual, the EPA\'s data, or the car\'s own configuration'], medium: ['likely', 'muted', 'several sources agree'], low: ['check', 'warn', 'one source only: to be checked'] };
const CHEM = { lfp: 'LFP (iron phosphate)', nca_nmc: 'NCA / NMC (nickel)' };

// [fact, label, how it is typed: config key and kind]
// kinds: text | whole lo hi | number lo hi (metric) | word [[v, l]] | bool | steering
const FACTS = [
  ['model', 'model', { key: 'model', kind: 'text' }],
  ['model_year', 'year', { key: 'model_year', kind: 'whole', lo: 2008, hi: 2045 }],
  ['trim', 'trim', { key: 'trim', kind: 'text' }],
  ['maker', 'maker', null],
  ['built_in', 'built in', null],
  ['plant', 'plant', { key: 'plant', kind: 'text' }],
  ['platform', 'platform', null],
  ['body', 'body', { key: 'body', kind: 'text' }],
  ['seats', 'seats', { key: 'seats', kind: 'whole', lo: 1, hi: 9 }],
  ['restraint', 'restraints', { key: 'restraint', kind: 'text' }],
  ['drive', 'drive', { key: 'drivetrain', kind: 'word', words: [['RWD', 'RWD'], ['AWD', 'AWD']] }],
  ['motors', 'motors', { key: 'motors', kind: 'whole', lo: 1, hi: 4 }],
  ['performance', 'performance', { key: 'performance_package', kind: 'bool' }],
  ['battery_type', 'battery (vin)', { key: 'battery_type', kind: 'text' }],
  ['chemistry', 'chemistry', { key: 'chemistry', kind: 'word', words: [['lfp', CHEM.lfp], ['nca_nmc', CHEM.nca_nmc]] }],
  ['pack', 'pack', null],
  ['pack_code', 'pack code', { key: 'pack_kwh_code', kind: 'text' }],
  ['nominal_kwh', 'pack energy', { key: 'nominal_kwh', kind: 'number', lo: 5, hi: 300, unit: 'energy' }],
  ['usable_kwh', 'usable energy', { key: 'usable_kwh', kind: 'number', lo: 5, hi: 300, unit: 'energy' }],
  ['epa_range_km', 'epa range', { key: 'epa_range_km', kind: 'number', lo: 10, hi: 2000, unit: 'distance' }],
  ['wheel', 'wheels', { key: 'wheel', kind: 'text' }],
  ['wheel_in', 'wheel size', { key: 'wheel_in', kind: 'whole', lo: 14, hi: 24 }],
  ['paint', 'paint', { key: 'paint', kind: 'text' }],
  ['roof', 'roof', { key: 'roof', kind: 'text' }],
  ['spoiler', 'spoiler', { key: 'spoiler', kind: 'text' }],
  ['steering', 'steering', { key: 'right_hand_drive', kind: 'steering' }],
  ['region', 'region', { key: 'map_region', kind: 'text' }],
  ['supercharging_access', 'supercharging', { key: 'supercharging_access', kind: 'text' }],
];
const LABEL = Object.fromEntries(FACTS.map(([k, l]) => [k, l]));
const muted = (t) => h('span', { class: 'muted' }, t);

/** A fact's value as text in the page's words and units (null value: null). */
export function factText(k, v) {
  if (v == null) return null;
  if (k === 'nominal_kwh' || k === 'usable_kwh') {
    if (typeof v !== 'object') return format(v, 'energy');
    return v.min === v.max ? format(v.min, 'energy') : `${format(v.min, 'energy')} .. ${format(v.max, 'energy')}`;
  }
  if (k === 'epa_range_km') return format(v, 'distance');
  if (k === 'chemistry') return CHEM[v] || String(v);
  if (k === 'wheel_in') return `${v} in`;
  if (typeof v === 'boolean') return v ? 'yes' : 'no';
  return String(v);
}
const srcTag = (s) => (s ? h('span', { class: 'veh-src', title: `from ${(SOURCE[s] || [s, s])[1]}` }, tag((SOURCE[s] || [s])[0], s === 'owner' ? 'warn' : 'muted')) : null);
const confTag = (c) => (c ? h('span', { class: 'veh-conf', title: (CONF[c] || [c, '', c])[2] }, tag((CONF[c] || [c, 'muted'])[0], (CONF[c] || [c, 'muted'])[1])) : null);

/**
 * The block. getCar(): the car as the page has it; setCar(car): after a save (the page keeps the new copy).
 * Returns { el, paint() }.
 */
export function vehicleSection(ctx, { getCar, setCar }) {
  const side = h('span', { class: 'muted' });
  const body = h('div');
  const el = section({ title: 'vehicle', id: 'vehicle', side }, body);

  function paint() {
    const car = getCar();
    const v = car.vehicle || {};
    const dis = new Map((v.disagreements || []).map((d) => [d.field, d]));
    const unknownCodes = v.unknown_codes || [];
    const known = FACTS.filter(([k]) => v[k]?.value != null).length;
    replace(side, `${known} of ${FACTS.length} known`);
    const row = ([k, label, edit]) => {
      const f = v[k] || {};
      const text = factText(k, f.value);
      const d = dis.get(k);
      const yearWord = k === 'model_year' && v.year_kind === 'build_year' ? ' (build year)' : '';
      const val = text == null
        ? [muted('unknown'), unknownCodes.some((u) => u.field === k) ? muted(' (a code the tables do not know)') : null]
        : [h('span', { class: 'veh-v' }, text + yearWord), ' ', srcTag(f.source), ' ', confTag(f.confidence), d ? [' ', tag('disagree', 'warn')] : null];
      const btnEl = ctx.admin && edit ? h('button', { class: 'btn btn--line tone--accent veh-edit', type: 'button', title: `type the ${label} yourself`, 'aria-label': `${f.source === 'owner' ? 'Change' : 'Type'} the ${label}`, onclick: () => editFact(k, label, edit, f) }, f.source === 'owner' ? '[change]' : '[type]') : null;
      return [label, [val, btnEl ? [' ', btnEl] : null, f.note ? h('span', { class: 'veh-note muted' }, f.note) : null], { tone: d ? 'warn' : null }];
    };
    replace(body,
      kv([
        ['vin prefix', v.vin_prefix ? [v.vin_prefix, muted(' only the prefix is kept, never the serial')] : muted('unknown')],
        ...FACTS.map(row),
      ]),
      (v.disagreements || []).length ? h('div', null, h('p', { class: 'sub-title' }, 'inputs that disagree'),
        h('ul', { class: 'ln-list' }, v.disagreements.map((d) => h('li', null, tag('disagree', 'warn'), h('span', { class: 'ln-list-t' },
          h('b', null, LABEL[d.field] || d.field), ': ',
          (d.values || []).map((x, i) => [i ? muted(' vs ') : null, factText(d.field, x.value) ?? 'unknown', ' ', srcTag(x.source), i === 0 ? muted(' (shown)') : null])))))) : null,
      unknownCodes.length ? h('div', null, h('p', { class: 'sub-title' }, 'codes the tables do not know'),
        h('ul', { class: 'ln-list' }, unknownCodes.map((u) => h('li', null, tag('unknown', 'muted'), h('span', { class: 'ln-list-t' },
          h('b', null, LABEL[u.field] || u.field), `: ${u.at || ''} says ${u.code}`, ' ', srcTag(u.source), muted(' no value is given for it, never a guess')))))) : null,
      (v.trim_candidates || []).length ? note(`trim could be: ${v.trim_candidates.join(', ')} (the inputs do not settle on one)`) : null,
      (v.wheel_options_in || []).length ? note(`factory wheel sizes: ${v.wheel_options_in.map((x) => `${x} in`).join(', ')}`) : null,
      v.epa_name ? note(`EPA: ${v.epa_name}${v.epa_id ? ` (id ${v.epa_id})` : ''}`) : null,
      (v.notes || []).map((t) => note(t)),
      note('Where each fact came from: ', srcTag('can'), ' the car\'s own CAN configuration, ', srcTag('vin'), ' the VIN prefix and built-in tables, ', srcTag('tesla_api'), ' the Tesla API, ',
        srcTag('teslamate'), ' an import, ', srcTag('owner'), ' typed by an admin (beats every other source). How sure: ', confTag('high'), ' ', confTag('medium'), ' ', confTag('low'), '.',
        ctx.admin ? '' : ' An admin can type a value over any of them.'));
  }

  function editFact(k, label, edit, f) {
    const owner = f.source === 'owner';
    const metric = edit.kind === 'number' && edit.unit === 'distance';
    const shownNow = f.value == null ? '' : edit.kind === 'number' ? (typeof f.value === 'object' ? (f.value.min === f.value.max ? f.value.min : '') : f.value)
      : edit.kind === 'bool' ? String(f.value) : edit.kind === 'steering' ? String(f.value === 'right') : String(f.value);
    const start = metric && shownNow !== '' ? Math.round(convert(shownNow, 'distance') * 10) / 10 : shownNow;
    const unit = edit.kind === 'number' ? (edit.unit === 'distance' ? unitLabel('distance') : 'kWh') : null;
    const opts = edit.kind === 'word' ? [['', 'not typed: use the inputs'], ...edit.words]
      : edit.kind === 'bool' ? [['', 'not typed: use the inputs'], ['true', 'yes'], ['false', 'no']]
        : edit.kind === 'steering' ? [['', 'not typed: use the inputs'], ['false', 'left-hand drive'], ['true', 'right-hand drive']] : null;
    const fl = field({ label: unit ? `${label} (${unit})` : label, name: 'value', path: `config.${edit.key}`, type: opts ? 'select' : edit.kind === 'text' ? 'text' : 'number', options: opts,
      value: owner ? String(start) : '', hint: owner ? 'Empty forgets the typed value: the inputs\' own is shown again.' : `Now: ${factText(k, f.value) ?? 'unknown'}${f.source ? ` (from ${(SOURCE[f.source] || [f.source, f.source])[1]})` : ''}. What you type beats every other source; empty leaves it to them.`,
      attrs: edit.kind === 'text' ? { maxlength: '100', autocomplete: 'off' } : edit.kind === 'whole' ? { min: String(edit.lo), max: String(edit.hi), step: '1', inputmode: 'numeric' }
        : edit.kind === 'number' ? { step: 'any', inputmode: 'decimal' } : {} });
    let dlg = null;
    const parse = (t) => {
      const s = String(t ?? '').trim();
      if (!s) return null;
      if (edit.kind === 'bool' || edit.kind === 'steering') return s === 'true';
      if (edit.kind === 'whole' || edit.kind === 'number') {
        const n = Number(s.replace(',', '.'));
        if (!Number.isFinite(n)) throw Object.assign(new Error('a number'), { fields: { [`config.${edit.key}`]: 'Type a number, or leave it empty.' } });
        const m = metric ? toMetric(n, 'distance') : n;
        if (edit.kind === 'whole' && !Number.isInteger(n)) throw Object.assign(new Error('whole'), { fields: { [`config.${edit.key}`]: `A whole number from ${edit.lo} to ${edit.hi}.` } });
        return metric ? Math.round(m * 10) / 10 : m;
      }
      return s;
    };
    const frm = form({
      fields: [fl], submit: 'save', secondary: h('button', { class: 'btn', type: 'button', onclick: () => dlg.close() }, 'cancel'),
      async onSubmit(vals) {
        const value = parse(vals.value);
        if (value == null && !owner) { dlg.close(); return; }
        const car = getCar();
        let fresh;
        try { fresh = await ctx.api.patch(`cars/${enc(car.id)}`, { version: car.version, config: { [edit.key]: value } }); } catch (e) {
          if (e?.status === 409) {
            const now = await ctx.api.get(`cars/${enc(car.id)}`).catch(() => null);
            if (now && ctx.alive()) { setCar(now); paint(); }
            throw Object.assign(new Error('Someone changed this car a moment ago. The page shows it as it is now; save again if you still want this.'), { code: 'stale' });
          }
          throw e;
        }
        if (!ctx.alive()) return;
        setCar(fresh); paint();
        dlg.close();
        toast(value == null ? `The typed ${label} is forgotten: the inputs' own is shown.` : `${label}: ${factText(k, k === 'nominal_kwh' || k === 'usable_kwh' ? { min: value, max: value } : k === 'steering' ? (value ? 'right' : 'left') : value) ?? value}, typed by you.`, { kind: 'ok' });
      },
    });
    dlg = openDialog({ title: `${label} of ${getCar().name || 'the car'}`, body: [note('Typed values are kept apart from what the inputs say, and win over them.'), frm.el] });
    fl.input.focus();
  }

  paint();
  return { el, paint };
}

