// Sentry Mate, "Pebble" UI pack: what the server knows of the car's build, on Car (/cars/{car}).
//
// GET /cars/{car} `vehicle` holds facts worked out offline from the VIN's first 11 characters, the car's own CAN
// configuration, the Tesla API, an import, and what an admin typed. Each fact is { value, source, confidence,
// note? }: the tiles show the value (or "Unknown"), where it came from (CAN, VIN, Tesla API, owner, import) and how
// sure it is (high, medium, low), and mark a fact two inputs disagree on with what each said. An admin corrects a
// fact in a sheet (PATCH /cars/{car} with the car's version and config: { key: value }; null forgets a typed one):
// a typed value beats every other source. A stale version reads the car again and says so.

import { h } from '../shared/dom.js';
import * as api from '../shared/api.js';
import { format } from '../shared/units.js';
import { formatNumber } from '../shared/i18n.js';
import { tile, chip, btn, note } from '../core/ui.js';
import { createSheet } from '../core/sheet.js';
import { field, form } from '../core/form.js';
import { sectionHead } from '../core/bits.js';
import { said } from '../core/settings.js';

const SOURCE = {
  can: ['CAN', 'teal', 'Read from the car’s own CAN configuration (SMO)'],
  vin: ['VIN', 'muted', 'Worked out from the VIN’s first 11 characters, with built-in tables'],
  tesla_api: ['Tesla API', 'sky', 'From the Tesla API’s vehicle configuration'],
  owner: ['Owner', 'coral', 'Typed by an admin: it beats every other source'],
  teslamate: ['Import', 'sun', 'From the car row of a TeslaMate import'],
  teslafi: ['Import', 'sun', 'From TeslaFi’s data'],
  import: ['Import', 'sun', 'From an import'],
};
const CONF = { high: 'high', medium: 'medium', low: 'low' };
const CONF_WHY = {
  high: 'High: Tesla’s filing or service manual, the EPA’s data, or the car’s own configuration.',
  medium: 'Medium: several sources agree.',
  low: 'Low: one source, to be checked.',
};

// [fact, label, config key an admin may type (null: not typed), kind of field]
const GROUPS = [
  ['The car', 'car', [
    ['model', 'Model', 'model', 'text'], ['model_year', 'Model year', 'model_year', 'year'], ['trim', 'Trim', 'trim', 'text'],
    ['body', 'Body', 'body', 'text'], ['platform', 'Platform', null], ['maker', 'Maker', null], ['plant', 'Plant', 'plant', 'text'],
    ['built_in', 'Built in', null], ['seats', 'Seats', 'seats', 'whole'], ['restraint', 'Restraints', 'restraint', 'text'],
  ]],
  ['Drive and battery', 'battery', [
    ['drive', 'Drive', 'drivetrain', 'drive'], ['motors', 'Motors', 'motors', 'whole'], ['performance', 'Performance', 'performance_package', 'bool'],
    ['battery_type', 'Battery (VIN)', 'battery_type', 'text'], ['chemistry', 'Chemistry', 'chemistry', 'chem'], ['pack', 'Pack', null],
    ['pack_code', 'Pack code', 'pack_kwh_code', 'text'], ['nominal_kwh', 'Pack energy, full', 'nominal_kwh', 'kwh'],
    ['usable_kwh', 'Usable energy', 'usable_kwh', 'kwh'], ['epa_range_km', 'EPA range when new', 'epa_range_km', 'km'],
  ]],
  ['Outside and options', 'star', [
    ['paint', 'Paint', 'paint', 'text'], ['wheel', 'Wheels', 'wheel', 'text'], ['wheel_in', 'Wheel size', 'wheel_in', 'inch'],
    ['roof', 'Roof', 'roof', 'text'], ['spoiler', 'Spoiler', 'spoiler', 'text'], ['steering', 'Steering wheel', 'right_hand_drive', 'steer'],
    ['region', 'Region', 'map_region', 'text'], ['supercharging_access', 'Supercharging', 'supercharging_access', 'text'],
  ]],
];

const regionName = (() => { try { const d = new Intl.DisplayNames(undefined, { type: 'region' }); return (c) => { try { return d.of(c) || c; } catch { return c; } }; } catch { return (c) => c; } })();
const yes = (b) => (b ? 'Yes' : 'No');

/** A fact's value in words (null when not known). */
function valueText(key, val) {
  if (val == null || val === '') return null;
  switch (key) {
    case 'chemistry': return val === 'lfp' ? 'LFP (iron phosphate)' : val === 'nca_nmc' ? 'NCA / NMC (nickel)' : String(val);
    case 'nominal_kwh': case 'usable_kwh': {
      if (typeof val === 'number') return format(val, 'energy');
      const a = val.min, b = val.max;
      if (a == null && b == null) return null;
      return a === b || b == null ? format(a, 'energy') : a == null ? format(b, 'energy') : `${format(a, 'energy')} to ${format(b, 'energy')}`;
    }
    case 'epa_range_km': return format(val, 'distance');
    case 'wheel_in': return `${formatNumber(val)} in`;
    case 'steering': return val === 'right' ? 'Right (right-hand drive)' : val === 'left' ? 'Left (left-hand drive)' : String(val);
    case 'built_in': return regionName(String(val));
    case 'performance': return typeof val === 'boolean' ? yes(val) : String(val);
    case 'platform': return val === 'legacy' ? 'Legacy' : val === 'refresh' ? 'Refresh (2021 on)' : String(val);
    case 'model_year': case 'seats': case 'motors': return typeof val === 'number' ? String(val) : String(val);
    default: return typeof val === 'boolean' ? yes(val) : typeof val === 'object' ? JSON.stringify(val) : String(val);
  }
}
const srcWords = (s) => SOURCE[s] || [String(s || '').replace(/_/g, ' '), 'muted', `From ${s}`];

/** The source mark of a fact: "CAN · high". */
function sourceMark(f) {
  if (!f || f.source == null) return null;
  const [word, tone, why] = srcWords(f.source);
  const c = CONF[f.confidence];
  return h('span', { class: ['vi-src', `tone-${tone}`, { 'is-low': f.confidence === 'low' }], title: `${why}.${c ? ` ${CONF_WHY[f.confidence]}` : ''}` },
    h('span', { class: 'src-dot', 'aria-hidden': 'true' }), h('span', null, word),
    c ? h('span', { class: 'vi-conf' }, `${c}`) : null,
    h('span', { class: 'sr-only' }, c ? ` source, ${c} confidence` : ' source'));
}

/**
 * The vehicle tiles: a section head, a tile per group of facts, and a tile of where they came from (the VIN prefix,
 * disagreements, codes the tables don't know, notes). onChanged() reads the page again after an admin's change.
 */
export function vehicleTiles(ctx, car, { onChanged = null } = {}) {
  const vh = car.vehicle || {};
  const dis = new Map((vh.disagreements || []).map((d) => [d.field, d.values || []]));
  const unknown = vh.unknown_codes || [];
  const yearLabel = vh.year_kind === 'build_year' ? 'Year built' : 'Model year';
  const sheet = createSheet({ title: 'Correct a value' });
  ctx.onCleanup(() => sheet.el.remove());

  function row([key, label0, cfgKey, kind]) {
    const label = key === 'model_year' ? yearLabel : label0;
    const f = vh[key] || { value: null, source: null, confidence: null };
    const text = valueText(key, f.value);
    const d = dis.get(key);
    const known = text != null;
    const edit = ctx.admin && cfgKey ? btn(`Correct ${label.toLowerCase()}`, { kind: 'ghost', icon: 'edit', iconOnly: true, cls: 'btn-sm vi-edit', onclick: (e) => correct({ key, label, cfgKey, kind, f }, e.currentTarget) }) : null;
    return h('li', { class: ['vi-row', { 'is-unknown': !known, 'is-differ': !!d }] },
      h('div', { class: 'vi-line' },
        h('span', { class: 'vi-k' }, label),
        h('span', { class: 'vi-v' }, h('span', { class: 'vi-val' }, known ? text : 'Unknown'), sourceMark(f), d ? chip('Inputs differ', { tone: 'sun', icon: 'warning' }) : null),
        edit || (ctx.admin ? h('span', { class: 'vi-edit-gap', 'aria-hidden': 'true' }) : null)),
      f.note ? h('p', { class: 'vi-note' }, f.note) : null,
      d ? h('ul', { class: 'vi-differ' }, d.map((x, i) => h('li', null, h('span', { class: 'vi-differ-v' }, valueText(key, x.value) ?? 'nothing'), ' ', h('span', { class: 'vi-differ-s' }, `said ${srcWords(x.source)[0]}`, i === 0 ? ', shown' : '')))) : null);
  }

  const tiles = GROUPS.map(([name, ic, rows]) => tile({ w: 2, title: name, icon: ic, cls: 'vi-tile', id: rows.some(([k]) => k === 'wheel_in') ? 'vi-wheels' : null }, h('ul', { class: 'vi-list' }, rows.map(row))));

  const extra = [];
  if (vh.trim_candidates && vh.trim_candidates.length) extra.push(['Could be', vh.trim_candidates.join(', ')]);
  if (vh.epa_name) extra.push(['EPA entry', vh.epa_name]);
  if (vh.wheel_options_in && vh.wheel_options_in.length) extra.push(['Factory wheel sizes', vh.wheel_options_in.map((x) => `${x} in`).join(', ')]);
  const whereT = tile({ w: 2, title: 'Where these come from', icon: 'info', cls: 'vi-where' },
    h('dl', { class: 'facts' },
      h('div', { class: 'fact' }, h('dt', null, 'VIN prefix'), h('dd', null, vh.vin_prefix ? h('code', { class: 'fw-hex' }, vh.vin_prefix) : car.vin_prefix || 'Not known')),
      extra.map(([k, x]) => h('div', { class: 'fact' }, h('dt', null, k), h('dd', null, x))),
      h('div', { class: 'fact' }, h('dt', null, 'Inputs that disagree'), h('dd', null, dis.size ? `${formatNumber(dis.size)}: marked above` : 'None'))),
    unknown.length ? said('warn', `Codes the tables do not know (no value is given for them, never a guess): ${unknown.map((u) => `${u.field} (${u.at ? String(u.at).replace(/^vin:/, 'VIN position ') : u.source === 'vin' ? 'VIN' : u.source === 'can' ? 'CAN' : u.source}: code ${u.code})`).join('; ')}.`) : null,
    (vh.notes || []).length ? h('ul', { class: 'set-list' }, vh.notes.map((n) => h('li', null, n))) : null,
    h('div', { class: 'vi-legend' },
      ['owner', 'can', 'tesla_api', 'vin', 'teslamate'].map((s) => h('span', { class: ['vi-src', `tone-${srcWords(s)[1]}`], title: srcWords(s)[2] }, h('span', { class: 'src-dot', 'aria-hidden': 'true' }), h('span', null, srcWords(s)[0])))),
    note('Each value says where it came from, and how sure that is: high (Tesla’s filing, the EPA’s data, the car’s own configuration), medium (several sources agree), low (one source, to be checked). What an admin types comes first, then the car’s CAN configuration, then the VIN; an import last. Nothing is looked up online.'),
    note(ctx.admin ? 'Press the pencil on a row to correct it; “Forget” goes back to what the inputs say.' : 'An admin can correct a value.'));

  /* ---------- an admin's correction */
  function control(kind, f, cfgKey) {
    const cur = f.value;
    const sel = (opts, v) => field({ label: 'Value', name: 'value', type: 'select', value: v ?? '', options: [['', 'Choose…'], ...opts] });
    switch (kind) {
      case 'bool': return sel([['true', 'Yes'], ['false', 'No']], cur == null ? '' : String(!!cur));
      case 'steer': return sel([['false', 'Left (left-hand drive)'], ['true', 'Right (right-hand drive)']], cur == null ? '' : String(cur === 'right'));
      case 'chem': return sel([['lfp', 'LFP (iron phosphate)'], ['nca_nmc', 'NCA / NMC (nickel)']], cur);
      case 'drive': return sel([['RWD', 'RWD (rear wheels)'], ['AWD', 'AWD (all wheels)']], cur);
      case 'year': return field({ label: 'Value', name: 'value', type: 'number', value: cur, hint: '2008 to 2045.', attrs: { min: '2008', max: '2045', step: '1', inputmode: 'numeric' } });
      case 'whole': return field({ label: 'Value', name: 'value', type: 'number', value: cur, hint: cfgKey === 'seats' ? '1 to 9.' : '1 to 4.', attrs: { min: '1', max: cfgKey === 'seats' ? '9' : '4', step: '1', inputmode: 'numeric' } });
      case 'inch': return field({ label: 'Value (inches)', name: 'value', type: 'number', value: cur, hint: '14 to 24.', attrs: { min: '14', max: '24', step: '1', inputmode: 'numeric' } });
      case 'kwh': return field({ label: 'Value (kWh)', name: 'value', type: 'number', value: cur && typeof cur === 'object' ? cur.max ?? cur.min : cur, hint: '5 to 300 kWh.', attrs: { min: '5', max: '300', step: '0.1', inputmode: 'decimal' } });
      case 'km': return field({ label: 'Value (km)', name: 'value', type: 'number', value: cur, hint: 'In kilometres, as the EPA’s rating converted: 10 to 2000.', attrs: { min: '10', max: '2000', step: '1', inputmode: 'decimal' } });
      default: return field({ label: 'Value', name: 'value', value: cur == null ? '' : String(cur), attrs: { maxlength: '64', autocomplete: 'off', spellcheck: 'false' } });
    }
  }
  const parse = (kind, raw) => {
    if (raw == null || raw === '') return undefined;
    if (kind === 'bool' || kind === 'steer') return raw === 'true';
    if (['year', 'whole', 'inch', 'kwh', 'km'].includes(kind)) return typeof raw === 'number' ? raw : Number(raw);
    return String(raw).trim() || undefined;
  };
  function correct({ key, label, cfgKey, kind, f }, opener) {
    const fld = control(kind, f, cfgKey);
    const typed = f.source === 'owner';
    async function send(value) {
      try {
        await ctx.api.patch(api.apiPath`cars/${car.id}`, { version: car.version, config: { [cfgKey]: value } });
      } catch (e) {
        if (e && e.code === 'stale_version') {
          api.invalidate('');
          sheet.close();
          ctx.toast('Someone else changed this car a moment ago, so nothing was saved. The page now shows their change: make yours again if it is still needed.', { kind: 'info', next: true });
          onChanged?.();
          return false;
        }
        if (e && e.code === 'not_allowed') throw new Error('Only an admin can correct the car’s facts.');
        if (e && e.fields) throw Object.assign(new Error(Object.values(e.fields).join(' ')), { fields: { value: Object.values(e.fields).join(' ') } });
        throw e;
      }
      api.invalidate('');
      sheet.close();
      ctx.toast(value === null ? `${label}: back to what the inputs say.` : `${label} saved.`, { next: true });
      onChanged?.();
      return true;
    }
    const forget = typed ? btn('Forget my value', { kind: 'soft', icon: 'reset', onclick: async (e) => {
      const b = e.currentTarget; b.disabled = true;
      try { await send(null); } catch (err) { ctx.toast(`Not saved: ${err.message}`, { kind: 'error' }); } finally { b.disabled = false; }
    } }) : null;
    const others = f.source && !typed ? `Now ${valueText(key, f.value) ?? 'unknown'}, from ${srcWords(f.source)[0]}.` : typed ? `Now ${valueText(key, f.value)}, typed by an admin.` : 'Not known yet.';
    sheet.titleEl.textContent = `Correct ${label.toLowerCase()}`;
    sheet.body.replaceChildren(
      h('p', { class: 'sheet-text' }, `${others} A typed value beats every other source, on every page. `),
      form({
        fields: [fld], submitLabel: 'Save', secondary: forget,
        async onsubmit(vals) {
          const value = parse(kind, vals.value);
          if (value === undefined || (typeof value === 'number' && !Number.isFinite(value))) throw Object.assign(new Error('Type a value first.'), { fields: { value: typed ? 'Type a value, or press Forget my value.' : 'Type a value.' } });
          await send(value);
        },
      }));
    sheet.open(opener);
  }

  return [sectionHead('Vehicle info', 'What is known of how this car was built, and where each fact came from.'), ...tiles, whereT];
}
