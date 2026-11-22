// Sentry Mate, "Logbook" UI pack: the car page's two reference panels (not a page of its own).
//
// Vehicle info: everything the server knows of the car's build (GET /cars/{car} `vehicle`), each fact with where it
// came from (the car's own CAN configuration, the VIN, the Tesla API, the owner, an import) and how sure it is;
// facts two inputs disagree on show both; a fact nothing says shows "unknown", never a guess. An admin can type a
// fact (PATCH /cars/{car} `config`, with the car's `version`), or forget a typed one.
//
// Car firmware: the fingerprints of their code that the four main modules broadcast on the vehicle bus
// (GET /cars/{car}/firmware), lowercase hex, with when each value was first seen and the values before it. The
// updates they reveal ("Tesla update installed") are on the Software page, which links here.

import { h, replace } from '../shared/dom.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { format, toMetric, unitLabel, convert } from '../shared/units.js';
import { formatDate, formatDateTime } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { icon, empty, errorView, errorWords } from '../core/ui.js';
import { field, form } from '../core/form.js';
import { openDialog } from '../core/dialog.js';

/* ------------------------------------------------------------------ vehicle info */

const SOURCE = {
  can: ['CAN', 'Read from the car’s own configuration on its CAN bus.'],
  vin: ['VIN', 'Worked out from the VIN’s first 11 characters (built-in tables, nothing looked up).'],
  tesla_api: ['Tesla API', 'What the Tesla API’s vehicle configuration says.'],
  owner: ['Typed by the owner', 'An admin typed this; it beats every other source.'],
  teslamate: ['Import', 'From a TeslaMate import’s car row.'],
  teslafi: ['Import', 'From a TeslaFi import.'],
  import: ['Import', 'From imported history.'],
};
const CONFIDENCE = { high: 'high confidence', medium: 'medium: several sources agree', low: 'low: one source, to be checked' };
const CHEM = { lfp: 'LFP (iron phosphate)', nca_nmc: 'NCA / NMC (nickel)' };

const yesNo = (v) => (v === true ? 'Yes' : v === false ? 'No' : String(v));
const kwh = (v) => {
  if (v == null) return null;
  if (typeof v === 'object') return v.min === v.max || v.max == null ? format(v.min, 'energy') : `${format(v.min, 'energy')} to ${format(v.max, 'energy')}`;
  return format(v, 'energy');
};
/**
 * The facts, in groups. Each: [field, name, show(value), edit] where edit says how an admin types it:
 * { key: the config key, kind: 'text' | 'whole' | 'number' | 'select' | 'yesno', options, unit, toApi(v), fromApi(v) }.
 */
const GROUPS = [
  ['The car', [
    ['maker', 'Maker', String, null],
    ['model', 'Model', String, { key: 'model', kind: 'text' }],
    ['model_year', 'Model year', String, { key: 'model_year', kind: 'whole', min: 2008, max: 2045 }],
    ['trim', 'Trim', String, { key: 'trim', kind: 'text' }],
    ['body', 'Body', String, { key: 'body', kind: 'text' }],
    ['platform', 'Platform', (v) => ({ legacy: 'Legacy', refresh: 'Refresh' }[v] || v), null],
    ['plant', 'Plant', String, { key: 'plant', kind: 'text' }],
    ['built_in', 'Built in', String, null],
    ['region', 'Region', String, { key: 'map_region', kind: 'text' }],
    ['steering', 'Steering', (v) => ({ left: 'Left-hand drive', right: 'Right-hand drive' }[v] || v),
      { key: 'right_hand_drive', kind: 'select', options: [['left', 'Left-hand drive'], ['right', 'Right-hand drive']], toApi: (v) => v === 'right', fromApi: (v) => v } ],
    ['seats', 'Seats', String, { key: 'seats', kind: 'whole', min: 1, max: 9 }],
    ['restraint', 'Restraints', String, { key: 'restraint', kind: 'text' }],
  ]],
  ['Battery and drive', [
    ['battery_type', 'Battery (VIN)', String, { key: 'battery_type', kind: 'text' }],
    ['chemistry', 'Chemistry', (v) => CHEM[v] || v, { key: 'chemistry', kind: 'select', options: [['lfp', CHEM.lfp], ['nca_nmc', CHEM.nca_nmc]] }],
    ['pack', 'Pack', String, null],
    ['pack_code', 'Pack code', String, { key: 'pack_kwh_code', kind: 'text' }],
    ['nominal_kwh', 'Pack energy, full', kwh, { key: 'nominal_kwh', kind: 'number', min: 5, max: 300, unit: 'kWh', fromApi: (v) => (v && typeof v === 'object' ? v.max ?? v.min : v) }],
    ['usable_kwh', 'Usable energy', kwh, { key: 'usable_kwh', kind: 'number', min: 5, max: 300, unit: 'kWh', fromApi: (v) => (v && typeof v === 'object' ? v.max ?? v.min : v) }],
    ['motors', 'Motors', String, { key: 'motors', kind: 'whole', min: 1, max: 4 }],
    ['drive', 'Drive', String, { key: 'drivetrain', kind: 'select', options: [['RWD', 'RWD (rear-wheel drive)'], ['AWD', 'AWD (all-wheel drive)']] }],
    ['performance', 'Performance', yesNo, { key: 'performance_package', kind: 'yesno' }],
    ['epa_range_km', 'EPA range when new', (v) => format(v, 'distance'), { key: 'epa_range_km', kind: 'number', dist: true }],
    ['supercharging_access', 'Supercharging', String, { key: 'supercharging_access', kind: 'text' }],
  ]],
  ['Looks', [
    ['paint', 'Paint', String, { key: 'paint', kind: 'text' }],
    ['wheel', 'Wheels', String, { key: 'wheel', kind: 'text' }],
    ['wheel_in', 'Wheel size', (v) => `${formatNumber(v)} in`, { key: 'wheel_in', kind: 'whole', min: 14, max: 24 }],
    ['roof', 'Roof', String, { key: 'roof', kind: 'text' }],
    ['spoiler', 'Spoiler', String, { key: 'spoiler', kind: 'text' }],
  ]],
];
const NAMES = Object.fromEntries(GROUPS.flatMap(([, list]) => list.map(([k, n]) => [k, n])));
const SHOW = Object.fromEntries(GROUPS.flatMap(([, list]) => list.map(([k, , f]) => [k, f])));
const showValue = (k, v) => { if (v == null) return null; try { return (SHOW[k] || String)(v); } catch { return String(v); } };

/** The Vehicle info panel: { el, setCar(car) }, drawn from the car the page already has. */
export function vehiclePanel(ctx, car) {
  const admin = !!(ctx.me && ctx.me.role === 'admin');
  let cur = car;
  const body = h('div', { class: 'vi-body' });
  const el = h('section', { class: 'block car-vehicle', id: 'car-vehicle', 'aria-labelledby': 'car-vehicle-title' },
    h('div', { class: 'block-head' },
      h('h2', { class: 'label', id: 'car-vehicle-title' }, 'Vehicle info', h('span', { class: 'label-note' }, 'how the car was built, and how each fact is known'))),
    body);

  function draw() {
    const v = cur.vehicle || {};
    const dis = new Map((v.disagreements || []).map((d) => [d.field, d.values || []]));
    const unknownCodes = v.unknown_codes || [];
    replace(body,
      h('p', { class: 'note vi-lead' }, 'Worked out on this server from the VIN’s first 11 characters', v.vin_prefix ? [' (', h('span', { class: 'mono' }, v.vin_prefix), ')'] : '',
        ', the car’s own configuration on CAN, the Tesla API and imports. A fact nothing tells says “unknown”: it is never guessed.',
        v.year_kind === 'build_year' ? ' This car’s 10th VIN character is the calendar year it was built, not a model year.' : '',
        admin ? ' An admin can type a fact to correct it; a typed fact beats every other source.' : ''),
      h('div', { class: 'vi-groups' }, GROUPS.map(([gname, list]) => h('div', { class: 'vi-group' },
        h('h3', { class: 'vi-group-name' }, gname),
        h('dl', { class: 'vi-facts' }, list.map(([k, name, , edit]) => factRow(k, name === 'Model year' && v.year_kind === 'build_year' ? 'Year built' : name, v[k], dis.get(k), edit)))))),
      (v.trim_candidates || []).length ? h('p', { class: 'note' }, 'The inputs do not settle on one trim. It can be: ', v.trim_candidates.join('; '), '.') : null,
      v.epa_name ? h('p', { class: 'note' }, 'EPA listing: ', v.epa_name, v.epa_id ? h('span', { class: 'mono' }, ` (${v.epa_id})`) : null, '.') : null,
      (v.wheel_options_in || []).length ? h('p', { class: 'note' }, `Factory wheel sizes for this car: ${v.wheel_options_in.map((x) => `${x} in`).join(', ')}.`) : null,
      dis.size ? h('div', { class: 'vi-disagree', role: 'note' },
        h('p', { class: 'vi-disagree-title' }, icon('warning'), `${dis.size === 1 ? 'One fact' : `${formatNumber(dis.size)} facts`} two inputs disagree on`),
        h('ul', null, [...dis].map(([k, vals]) => h('li', null, h('strong', null, NAMES[k] || k.replace(/_/g, ' ')), ': ',
          vals.map((x, i) => [i ? ', ' : '', showValue(k, x.value) ?? 'nothing', h('span', { class: 'vi-faint' }, ` (${(SOURCE[x.source] || [x.source])[0]}${i === 0 ? ', shown' : ''})`)]))))) : null,
      unknownCodes.length ? h('div', { class: 'vi-unknown-codes' },
        h('p', { class: 'vi-faint' }, 'Codes the built-in tables do not know (no value is given for them):'),
        h('ul', null, unknownCodes.map((c) => h('li', null, NAMES[c.field] || c.field, ': code ', h('span', { class: 'mono' }, String(c.code)), h('span', { class: 'vi-faint' }, ` at ${c.at} (${(SOURCE[c.source] || [c.source])[0]})`))))) : null,
      (v.notes || []).length ? h('ul', { class: 'vi-notes' }, v.notes.map((t) => h('li', null, t))) : null);
  }

  function factRow(k, name, f, disagree, edit) {
    const value = f && f.value != null ? showValue(k, f.value) : null;
    const src = f && f.source ? SOURCE[f.source] || [f.source, ''] : null;
    return h('div', { class: ['vi-fact', { 'is-unknown': value == null, 'is-owner': f && f.source === 'owner', 'is-disputed': !!disagree }] },
      h('dt', null, name),
      h('dd', null,
        h('span', { class: 'vi-value' }, value ?? 'unknown'),
        value != null && src ? h('span', { class: 'vi-meta' },
          h('span', { class: ['vi-src', `vi-src-${f.source}`], title: src[1] }, src[0]),
          f.confidence ? h('span', { class: `vi-conf vi-conf-${f.confidence}`, title: CONFIDENCE[f.confidence] || f.confidence }, f.confidence) : null) : null,
        f && f.note ? h('span', { class: 'vi-note' }, f.note) : null,
        disagree && disagree.length > 1 ? h('span', { class: 'vi-note is-warn' }, 'Also said: ',
          disagree.slice(1).map((x, i) => [i ? ', ' : '', showValue(k, x.value) ?? 'nothing', ` (${(SOURCE[x.source] || [x.source])[0]})`])) : null,
        admin && edit ? h('button', { type: 'button', class: 'vi-edit', 'aria-label': `${f && f.source === 'owner' ? 'Change or forget' : 'Type'} ${name.toLowerCase()}`, onclick: (e) => editFact(k, name, f, edit, e.currentTarget) },
          f && f.source === 'owner' ? 'Change' : 'Correct') : null));
  }

  async function editFact(k, name, f, edit, opener) {
    const typed = f && f.source === 'owner';
    const now = f && f.value != null ? (edit.fromApi ? edit.fromApi(f.value) : f.value) : null;
    const distUnit = edit.dist ? unitLabel('distance') : null;
    const shownNow = edit.dist && now != null ? Math.round(convert(now, 'distance')) : now;
    let control;
    if (edit.kind === 'select' || edit.kind === 'yesno') {
      const opts = edit.kind === 'yesno' ? [['true', 'Yes'], ['false', 'No']] : edit.options;
      control = field({ label: name, name: 'v', type: 'select', value: now == null ? opts[0][0] : String(now), options: opts });
    } else {
      control = field({
        label: `${name}${edit.unit ? ` (${edit.unit})` : distUnit ? ` (${distUnit})` : ''}`, name: 'v', type: edit.kind === 'text' ? 'text' : 'number',
        value: shownNow ?? '', hint: edit.kind === 'whole' ? `A whole number${edit.min != null ? ` from ${edit.min} to ${edit.max}` : ''}.` : edit.kind === 'number' && edit.min != null ? `A number from ${edit.min} to ${edit.max}.` : 'As you would like it shown, up to 100 characters.',
        attrs: { autocomplete: 'off', spellcheck: 'false', required: true, step: edit.kind === 'whole' ? '1' : 'any', maxlength: edit.kind === 'text' ? '100' : null },
      });
    }
    let closeIt = null;
    const send = async (value) => {
      try {
        const ans = await ctx.api.patch(api.apiPath`cars/${cur.id}`, { version: cur.version, config: { [edit.key]: value } });
        if (!ctx.alive()) return;
        cur = ans && ans.vehicle ? ans : await ctx.api.get(api.apiPath`cars/${cur.id}`);
        ctx.flash(value == null ? `Forgotten: ${name.toLowerCase()} is worked out from the car again.` : `Saved: ${name.toLowerCase()} is now what you typed.`);
        if (closeIt) closeIt('done');
        draw();
      } catch (e) {
        if (api.isAbort(e)) throw e;
        if (e && (e.code === 'stale_version' || e.status === 409)) {
          cur = await ctx.api.get(api.apiPath`cars/${cur.id}`).catch(() => cur);
          draw();
          throw new Error('Someone changed this car meanwhile; its facts are shown as they are now. Make your change again if it is still needed.');
        }
        if (e && e.fields) throw Object.assign(new Error(errorWords(e).text), { fields: { v: Object.values(e.fields).join(' ') } });
        throw e;
      }
    };
    const f2 = form({
      fields: [control], submitLabel: 'Save',
      onsubmit: async (vals) => {
        let raw = vals.v;
        if (edit.kind === 'text') { raw = String(raw || '').trim(); if (!raw) throw Object.assign(new Error('Type a value.'), { fields: { v: 'Type a value, or use Forget to go back to what the car says.' } }); }
        else if (edit.kind === 'yesno') raw = raw === 'true';
        else if (edit.kind === 'whole' || edit.kind === 'number') {
          const n = Number(raw);
          if (raw === '' || !Number.isFinite(n) || (edit.kind === 'whole' && !Number.isInteger(n))) throw Object.assign(new Error('Not a number.'), { fields: { v: edit.kind === 'whole' ? 'A whole number.' : 'A number.' } });
          raw = edit.dist ? Math.round(toMetric(n, 'distance') * 10) / 10 : n;
        }
        await send(edit.toApi ? edit.toApi(raw) : raw);
      },
    });
    await openDialog({
      title: typed ? `Change ${name.toLowerCase()}` : `Correct ${name.toLowerCase()}`,
      body: (close) => {
        closeIt = close;
        return [
          h('p', { class: 'dialog-text' }, f && f.value != null
            ? ['Now: ', showValue(k, f.value), ' (', (SOURCE[f.source] || [f.source])[0], f.confidence ? `, ${f.confidence} confidence` : '', '). ']
            : 'Nothing tells this now. ',
          'What you type is shown everywhere for this car and beats every other source.'),
          f2,
          typed ? h('p', { class: 'vi-forget' }, h('button', {
            type: 'button', class: 'button button-quiet button-small',
            onclick: async (e) => { e.currentTarget.disabled = true; try { await send(null); } catch (err) { if (!api.isAbort(err)) ctx.flash(`Not forgotten: ${err.message}`, { kind: 'error' }); e.target.disabled = false; } },
          }, 'Forget what was typed')) : null,
        ];
      },
      actions: [], signal: ctx.signal, initialFocus: 'input, select',
    });
    if (ctx.alive() && opener && !document.contains(opener)) {
      const again = [...body.querySelectorAll('.vi-edit')].find((b) => b.getAttribute('aria-label')?.endsWith(name.toLowerCase()));
      again?.focus();
    }
  }

  draw();
  return { el, setCar: (c) => { cur = c; draw(); } };
}

/* ------------------------------------------------------------------ car firmware */

const MODULE_NAME = { vcfront: 'Front body controller', bms: 'Battery management', dir: 'Rear drive unit', dif: 'Front drive unit' };

/** The Car firmware panel: { el, load() }: load() draws it, or its error with Try again. */
export function firmwarePanel(ctx, car) {
  const tz = ctx.tz;
  const body = h('div', { class: 'fw-body' });
  const el = h('section', { class: 'block car-firmware', id: 'car-firmware', tabindex: '-1', 'aria-labelledby': 'car-firmware-title' },
    h('div', { class: 'block-head' },
      h('h2', { class: 'label', id: 'car-firmware-title' }, 'Car firmware', h('span', { class: 'label-note' }, 'fingerprints the modules broadcast on CAN'))),
    body);
  const swHref = `${state.buildUrl('software', { car: car.id })}#sw-fp`;

  async function load() {
    let fw;
    try {
      fw = await ctx.api.get(api.apiPath`cars/${car.id}/firmware`);
    } catch (e) {
      if (api.isAbort(e)) throw e;
      replace(body, errorView(e, { retry: () => load().catch(() => {}), eyebrow: 'Car firmware', title: 'The firmware fingerprints could not be loaded.', home: false }));
      return;
    }
    if (!ctx.alive()) return;
    const mods = fw.modules || [];
    const any = mods.some((m) => (m.fields || []).length);
    const day = (ms) => formatDate(ms, tz, { now: api.now(), year: true, weekday: false });
    replace(body,
      h('p', { class: 'note fw-lead' }, 'Tesla’s version number is not on CAN. Instead each main module broadcasts fingerprints of its code (checksums and build hashes, here as lowercase hex); when they change, a Tesla update was installed. ',
        h('a', { href: swHref }, fw.updates ? `${formatNumber(fw.updates)} ${fw.updates === 1 ? 'update' : 'updates'} found this way, on Software` : 'Updates found this way are on Software'), '.'),
      h('p', { class: 'fw-facts' },
        h('span', null, 'Last seen ', h('strong', null, fw.updated_ms ? formatDateTime(fw.updated_ms, tz, { now: api.now() }) : 'never')),
        fw.bridge_hints && fw.bridge_hints.count ? h('span', null, `The CAN Bridge noted a firmware change ${formatNumber(fw.bridge_hints.count)} ${fw.bridge_hints.count === 1 ? 'time' : 'times'}${fw.bridge_hints.last_ms ? `, last ${day(fw.bridge_hints.last_ms)}` : ''} (a hint only)`) : null),
      any ? null : empty({ title: 'No fingerprint in this car’s logs yet.', text: 'They show once the car’s logs carry the modules’ info messages; some older cars send none.' }),
      h('div', { class: 'fw-modules' }, mods.map((m) => h('div', { class: 'fw-module' },
        h('h3', { class: 'fw-module-name' }, m.name || MODULE_NAME[m.id] || m.id,
          m.can_id != null ? h('span', { class: 'fw-can mono' }, `0x${Number(m.can_id).toString(16).toUpperCase()}`) : null),
        (m.fields || []).length
          ? h('dl', { class: 'fw-fields' }, m.fields.map((f) => {
            const before = (f.history || []).filter((x) => x.hex !== f.hex);
            return h('div', { class: 'fw-field' },
              h('dt', { class: 'mono' }, f.field),
              h('dd', null,
                h('span', { class: 'fw-hex mono' }, f.hex),
                h('span', { class: 'fw-seen' }, `since ${day(f.first_seen_ms)}`),
                before.length ? h('span', { class: 'fw-before' }, 'before: ', before.slice(-2).reverse().map((x, i) => [i ? ', ' : '', h('span', { class: 'mono' }, x.hex), ` to ${day(x.last_seen_ms)}`]),
                  before.length > 2 ? ` and ${formatNumber(before.length - 2)} more` : '') : null));
          }))
          : h('p', { class: 'fw-none' }, 'No fingerprint from this module in the logs.')))));
  }
  return { el, load };
}

