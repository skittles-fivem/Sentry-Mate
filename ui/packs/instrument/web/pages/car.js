// Car overview (/cars/{car}): what the car is doing now, why it is awake, and the chosen period at a
// glance (default the last 24 hours). Live values come from GET /cars/{car}/now and then from the live
// event stream (`now` events, merged field by field); every value carries its own time. The period part
// (figures, battery chart, charging chart, activity strip) reloads when a drive, charge or parked spell
// starts or ends (`rows` events).

import * as state from '../shared/state.js';
import { tyreView, recommendedLine, wheelGuessLine, tyresTitle } from '../shared/tyres.js';
import { h, svg, replace, redraw, cover, cssVar, quantity, steady } from '../shared/dom.js';
import { format, getUnits, onUnitsChange, unitLabel, convert } from '../shared/units.js';
import { formatTime, formatDate, formatDateTime, zoneName, buckets } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { icon } from '../core/icons.js';
import { pageHead, panel, panelLink, metric, kv, csvButton } from '../core/page.js';
import { statePill, stateLabel, ago, agoEl, when, dur, val, TYRES } from '../core/fmt.js';
import { timelineRow } from './timeline.js';
import { createMap, carMarker, addRoute, padClear, mapFallback } from '../core/map.js';
import { SPOT_ZOOM } from '../shared/maptiles.js';
import { offstage, afterScroll } from '../core/stage.js';
import { loadUPlot, mountPlot, timeAxis, valueAxis, colors, areaFill, hexA } from '../core/chart.js';
import { activityStrip } from '../core/strip.js';
import { openCarSwitcher } from '../core/carswitch.js';
import { openPeriodPicker } from '../core/period.js';
import { rpx } from '../core/display.js';
import { downloadCsv, csvFilename, column, timeColumn } from '../shared/csv.js';
import { srcMark, srcText, inputsLine } from '../core/source.js';
import { dataNote } from '../shared/teslafi-data.js';
import { vehicleInfoPanel } from './vehicle-info.js';

export const id = 'car';
export const css = true;
export const title = () => 'Overview';

const clamp = (v, a, b) => Math.max(a, Math.min(b, v));
const pct = (v) => format(v, 'percent');

// why the car is awake, in words, with a line of help
const AWAKE = {
  sentry: ['Sentry mode', 'The cameras watch the car. It stays awake while Sentry is on.'],
  keep_climate_on: ['Keep climate on', 'Climate runs while the car is parked.'],
  keep_climate_dog: ['Dog mode', 'Climate keeps the cabin comfortable for a pet.'],
  keep_climate_camp: ['Camp mode', 'Climate, lights and power stay on for camping.'],
  user_present: ['Someone in the car', 'A seat or the key says a person is inside.'],
  preconditioning_cabin: ['Cabin preconditioning', 'The cabin is heated or cooled before a drive.'],
  preconditioning_battery: ['Battery preconditioning', 'The battery is warmed for driving or fast charging.'],
  charging: ['Charging', 'The car is charging.'],
  keep_warm: ['Battery kept warm', 'The battery heater keeps the pack warm.'],
  door_open: ['A door is open', 'An open door, trunk or frunk keeps the car awake.'],
  accessory_power: ['Accessory power', 'Power to the outlets is kept on.'],
};
const CHARGE_STATUS = { disconnected: 'Unplugged', no_power: 'Plugged in, no power', about_to_charge: 'Plugged in, waiting', charging: 'Charging', complete: 'Charge complete', stopped: 'Stopped' };
const LIMIT_BY = { pilot: 'the station', station: 'the station', cable: 'the cable', car: 'the car', screen: "the car's setting" };
const ACTIVITY = { driving: 'Driving', charging: 'Charging', online: 'Parked, awake', online_vehicle_bus_only: 'Awake, vehicle bus only', asleep: 'Asleep', unknown: 'Unknown (no data)', updating: 'Updating' };
const CLIMATE = { off: 'Off', on: 'On', precondition: 'Preconditioning', overheat_protect: 'Overheat protection' };
const KEEP = { off: 'Off', on: 'Keep climate', dog: 'Dog mode', camp: 'Camp mode' };

/* ------------------------------------------------------------------ values no CAN message carries */

// `now.off_can`: each value { value, at_ms, sources: [{ kind, label }], stale } or null, from the Tesla API or
// TeslaFi. Shown with its input's label and, once older than its limit, a stale mark with its age.
const OFF_SHORT = { tesla_api: 'Tesla API', teslafi_api: 'TeslaFi' };
const WINDOWS = [['driver_front', 'Driver front window'], ['passenger_front', 'Passenger front window'], ['driver_rear', 'Driver rear window'], ['passenger_rear', 'Passenger rear window']];
const SUNROOF = { closed: 'Closed', open: 'Open', vent: 'Vent', calibrating: 'Calibrating' };
function offSrc(pair) {
  const s = pair?.sources?.[0];
  if (!s) return null;
  return h('span', { class: ['off-src', pair.stale && 'is-stale'], title: `${s.label}; read ${ago(pair.at_ms)}${pair.stale ? '. Older than its limit, so it may have changed since.' : ''}` },
    h('i', { class: `src-m src-m--${s.kind}`, 'aria-hidden': 'true' }),
    h('span', { class: 'off-src-word' }, OFF_SHORT[s.kind] || s.label),
    pair.stale ? h('span', { class: 'off-stale' }, `stale · ${ago(pair.at_ms)}`) : null);
}
// a label, the value and where it came from; null when no input gives it (and `hide`), else a dash
function offRow(label, pair, text, { hide = false, cover: cv = null } = {}) {
  if (pair == null) return hide ? null : { label, value: h('span', { class: 'faint' }, '—'), src: null, cv };
  return { label, value: h('span', { class: pair.stale ? 'off-old' : null }, text(pair.value)), src: offSrc(pair), cv };
}

/* ------------------------------------------------------------------ live values */

// merge a `now` event (a delta) into the object we hold: { value, at_ms } pairs come whole
function merge(into, delta) {
  for (const [k, v] of Object.entries(delta || {})) {
    if (v && typeof v === 'object' && !Array.isArray(v) && !('value' in v) && into[k] && typeof into[k] === 'object' && !Array.isArray(into[k])) merge(into[k], v);
    else into[k] = v;
  }
  return into;
}

const at = (x) => (x && typeof x === 'object' && 'at_ms' in x ? x.at_ms : null);
// the input a live value came from ({ value, at_ms, sources }): a small mark for any input but SMO (the Tesla API),
// so a value the API sent is never mistaken for one measured on the car's bus. The live inputs line explains it.
const SHOW_VIA = false;
function via(pair) {
  const s = pair && typeof pair === 'object' ? pair.sources?.[0] : null;
  if (!s || s.kind === 'smo' || !SHOW_VIA) return null;
  return h('span', { class: 'src-via', title: s.label || 'Data from the Tesla API' }, h('i', { class: `src-m src-m--${s.kind}`, 'aria-hidden': 'true' }), h('span', null, s.kind === 'tesla_api' ? 'API' : s.kind === 'teslafi_api' ? 'TeslaFi' : s.kind === 'teslamate_local' ? 'TeslaMate' : s.label || s.kind));
}
// where a value came from is not shown beside it (the main view stays clean): its source is the value's hover title,
// and the Live inputs line names the inputs
const srcTitle = (pair) => { const x = pair && typeof pair === 'object' ? pair.sources?.[0] : null; return x && x.kind !== 'smo' ? (x.label || x.kind) : null; };
const withVia = (pair, content) => { const t = srcTitle(pair); return t ? h('span', { class: 'aged', title: t }, content) : content; };
// a value with its own age when it is older than the snapshot (a tyre read an hour ago), and its input
function aged(pair, content, newest) {
  const t = at(pair);
  if (t == null) return content;
  const old = newest != null && newest - t > 10 * 60000;
  return h('span', { class: 'aged', title: `Recorded ${ago(t)}${pair.sources?.[0]?.label ? `; ${pair.sources[0].label}` : ''}` }, content, via(pair), old ? h('span', { class: 'aged-note' }, ` · ${ago(t)}`) : null);
}

/* ------------------------------------------------------------------ the gauge */

function gauge(socPct, limit, tone) {
  const cx = 150, cy = 150, r = 118, a0 = 135, sweep = 270;
  const pt = (deg, rr) => { const a = (deg * Math.PI) / 180; return [cx + rr * Math.cos(a), cy + rr * Math.sin(a)]; };
  const arc = (from, to, rr) => {
    const [x1, y1] = pt(from, rr), [x2, y2] = pt(to, rr);
    return `M${x1.toFixed(2)} ${y1.toFixed(2)} A${rr} ${rr} 0 ${to - from > 180 ? 1 : 0} 1 ${x2.toFixed(2)} ${y2.toFixed(2)}`;
  };
  const at = (p) => a0 + (sweep * p) / 100;
  const parts = [
    // the everyday bands: under 20 %, 20 to 80 %, over 80 %
    svg('path', { class: 'gauge-band gauge-band--low', d: arc(at(0), at(20), r + 12) }),
    svg('path', { class: 'gauge-band gauge-band--mid', d: arc(at(20.6), at(79.4), r + 12) }),
    svg('path', { class: 'gauge-band gauge-band--high', d: arc(at(80), at(100), r + 12) }),
    svg('path', { class: 'gauge-track', d: arc(a0, a0 + sweep, r) }),
  ];
  for (let p = 0; p <= 100; p += 5) {
    const major = p % 25 === 0;
    const [x1, y1] = pt(at(p), r - (major ? 25 : 20)), [x2, y2] = pt(at(p), r - 13);
    parts.push(svg('line', { class: ['gauge-tick', major && 'gauge-tick--major'], x1: x1.toFixed(1), y1: y1.toFixed(1), x2: x2.toFixed(1), y2: y2.toFixed(1) }));
  }
  if (socPct != null) parts.push(svg('path', { class: 'gauge-fill', d: arc(a0, a0 + sweep, r), pathLength: '100', 'stroke-dasharray': `${clamp(socPct, 0, 100).toFixed(2)} 100` }));
  let label = null;
  if (limit != null) {
    const [x1, y1] = pt(at(limit), r - 10), [x2, y2] = pt(at(limit), r + 10), [lx, ly] = pt(at(limit), r + 26);
    parts.push(svg('line', { class: 'gauge-limit', x1: x1.toFixed(1), y1: y1.toFixed(1), x2: x2.toFixed(1), y2: y2.toFixed(1) }));
    label = h('span', { class: 'gauge-marks', 'aria-hidden': 'true' }, h('span', { class: 'gauge-limit-label', vars: { '--x': `${(lx / 3).toFixed(2)}%`, '--y': `${(ly / 3).toFixed(2)}%` } }, format(limit, 'percent')));
  }
  return [svg('svg', { class: 'gauge', viewBox: '0 0 300 300', 'aria-hidden': 'true', focusable: 'false' }, parts), label];
}

/* ------------------------------------------------------------------ the page */

// The first reads, made by mount() and warmed ahead by prefetch() (a link to this page under the pointer).
const nowRead = (ctx) => ctx.api.get(`cars/${encodeURIComponent(ctx.car.id)}/now`);
const settingsRead = (ctx) => ctx.api.get('settings', null, { maxAgeMs: 300000 });
const todayRead = (ctx) => {
  const dayStart = buckets(ctx.api.now() - 1, ctx.api.now(), 'day', ctx.tz)[0]?.start_ms;
  return ctx.api.page(`cars/${encodeURIComponent(ctx.car.id)}/timeline`, { from_ms: dayStart }, { limit: 30 });
};
// the period's figures, charts and strip: [statistics, series, activity, charges]
function windowReads(ctx) {
  const cid = ctx.car.id, p = ctx.period;
  const from = p.from_ms ?? (ctx.api.now() - 86400000), to = p.to_ms;
  const rk = getUnits().range === 'ideal' ? 'ideal_range_km' : 'range_km';
  const qs = { from_ms: from, to_ms: to };
  return Object.assign([
    ctx.api.get(`cars/${encodeURIComponent(cid)}/statistics`, { period: 'all', ...qs }),
    ctx.api.get(`cars/${encodeURIComponent(cid)}/series`, { fields: `soc_pct,usable_soc_pct,${rk}`, ...qs, max_points: 720 }),
    ctx.api.get(`cars/${encodeURIComponent(cid)}/activity`, qs),
    ctx.api.page(`cars/${encodeURIComponent(cid)}/charges`, { from_ms: from - 12 * 3600000, to_ms: to }, { limit: 12 }),
  ], { from, to });
}
// the charging chart: the charges that reach into the period, and the curves of the last 10 of them
const chargesIn = (items, from, to) => items.filter((c) => (c.end_ms ?? to) > from);
const curveReads = (ctx, charges) => [...charges].sort((a, b) => a.start_ms - b.start_ms).slice(-10)
  .map((c) => ctx.api.get(`cars/${encodeURIComponent(ctx.car.id)}/charges/${encodeURIComponent(c.id)}/curve`, { fields: 'power_kw,volts_v,amps_a,heater_kw,dc_volts_v,dc_amps_a', max_points: 400 }));
const trailRead = (ctx, driveId) => ctx.api.get(`cars/${encodeURIComponent(ctx.car.id)}/drives/${encodeURIComponent(driveId)}/track`, { simplify_m: 5 });
export async function prefetch(ctx) {
  const w = windowReads(ctx);
  const [n, , , , , , charges] = await Promise.all([nowRead(ctx), settingsRead(ctx), todayRead(ctx), ...w, Promise.resolve().then(loadUPlot).catch(() => null)]);
  await Promise.all([
    ...curveReads(ctx, chargesIn(charges?.items || [], w.from, w.to)),
    n?.state === 'driving' && n.drive?.drive_id ? trailRead(ctx, n.drive.drive_id) : null,
  ]);
}

export async function mount(ctx) {
  const { car, tz } = ctx;
  const cid = car.id;
  const root = h('div', { class: 'car-page', cover: ['shots.web-interface', 'shots.overview'] });
  replace(ctx.view, root);

  // GET now and the period's calls start together: the period paints when its answers are in, the live part when
  // /now is in, and grey blocks keep each part's place until then
  let now, settings = null;
  const nowP = nowRead(ctx);
  const settingsP = settingsRead(ctx).catch(() => null);
  nowP.catch(() => {}); // handled below

  const head = h('div');
  const inputsSlot = h('div', { class: 'now-inputs' });
  const banner = h('div');
  const gaugeSlot = h('section', { class: 'panel gauge-panel', 'aria-label': 'Battery and range' });
  const mapEl = h('div', { class: 'map', role: 'img', 'aria-label': `Map: where ${car.name} is` });
  const placeCard = h('div', { class: 'glass-card place-card' });
  const mapTools = h('div', { class: 'map-overlay map-overlay--bl map-tools' });
  const mapPanel = h('section', { class: 'panel map-panel', 'aria-label': 'Where the car is', cover: ['summary.map.position', 'summary.map.attribution', 'summary.map.zoom'] },
    mapEl, h('div', { class: 'map-overlay map-overlay--tl' }, placeCard), mapTools);
  const todaySlot = h('section', { class: 'panel today-tl', 'aria-label': "Today's timeline" });
  const rowLive = h('div', { class: 'now-row' });
  const rowMore = h('div', { class: 'now-row now-row--3' });
  const windowSlot = h('div', { class: 'car-window' });

  replace(root, head, inputsSlot, banner,
    h('div', { class: 'now-hero' }, gaugeSlot, mapPanel, todaySlot),
    h('div', { class: 'now-panels' }, rowLive, rowMore),
    windowSlot, vehicleInfoPanel(ctx));
  const wait = () => h('div', { class: 'skel skel--block', 'aria-hidden': 'true' });
  replace(head, h('div', { class: 'page-head now-head' }, h('div', { class: 'page-head-text' }, h('span', { class: 'eyebrow' }, 'Overview'), h('span', { class: 'skel skel--title', 'aria-hidden': 'true' }))));
  replace(gaugeSlot, wait());
  replace(todaySlot, wait());
  replace(rowLive, h('section', { class: 'panel panel-pad' }, wait()), h('section', { class: 'panel panel-pad' }, wait()));
  replace(windowSlot, h('div', { class: 'window-head' }, h('h2', { class: 'section-title' }, 'The period')), h('section', { class: 'panel panel-pad' }, wait()), h('section', { class: 'panel panel-pad' }, wait()));

  /* ---------- header, banner */
  // The parts below are drawn again every 2 s while live values come in. redraw() leaves every node that looks
  // the same on the page, so the control that has the keyboard focus (Switch car, Timeline, Change order, Full
  // screen, a link in a panel) is still there after an update. Their listeners read `now` when they run.
  function paintHead() {
    const n = now;
    const vin = car.vin_prefix;
    const trim = car.config?.trim;
    const sold = state.isSold(car);
    redraw(head, pageHead({
      cls: 'now-head',
      eyebrow: 'Overview',
      title: h('span', { cover: 'summary.header.name' }, car.name || `Unnamed ${car.model || 'car'}`),
      titleExtra: sold
        ? h('span', { cover: 'summary.stat.status' }, h('span', { class: 'tag tag--plain' }, car.sold_date ? `Sold on ${car.sold_date}` : 'Sold'))
        : h('span', { cover: 'summary.stat.status' }, statePill(n.state, { lg: true, stale: n.stale })),
      sub: [
        h('span', { cover: 'summary.header.model', title: 'Model and year come from the VIN prefix.' }, [car.model, car.year].filter(Boolean).join(' · ') || 'Model not known yet'),
        trim?.value ? h('span', { cover: 'summary.header.trim', title: `Trim: ${trim.value}, from ${trim.source === 'can' ? "the car's own configuration message" : trim.source === 'owner' ? 'the owner' : trim.source === 'vin' ? 'the VIN prefix' : trim.source === 'tesla_api' ? 'the Tesla API' : 'the TeslaMate import'}` }, trim.value, h('span', { class: 'trim-src' }, { can: 'from SMO', vin: 'from the VIN', owner: 'typed by the owner', teslamate: 'imported from TeslaMate', tesla_api: 'from the Tesla API' }[trim.source] || 'source not known')) : h('span', { cover: 'summary.header.trim' }, 'Trim not known yet'),
        vin ? h('span', null, 'VIN ', h('span', { class: 'key-chip', title: 'Only the VIN prefix is stored; the serial number never is.' }, vin)) : null,
        h('span', { cover: 'summary.stat.status' }, `${sold || n.stale ? 'Last known: ' : ''}${stateLabel(n.state)} since ${n.since_ms ? when(n.since_ms, tz) : '—'}`, n.since_ms ? h('span', { class: 'faint' }, ` (${dur(Math.max(0, ctx.api.now() - n.since_ms))})`) : null),
      ],
      actions: [
        h('button', { class: 'btn', type: 'button', onclick: openCarSwitcher, cover: 'overview.filter.car', title: 'Show another car on this page' }, icon('car'), 'Switch car'),
        h('a', { class: 'btn', href: ctx.link('timeline') }, icon('timeline'), 'Timeline'),
      ],
    }));
  }

  // the live inputs (SMO, the Tesla API) with each one's newest value, and what the car's history comes from
  function paintInputs() {
    const ins = now.inputs || [];
    const api = ins.some((i) => i.kind === 'tesla_api');
    // TeslaFi's data is stamped with TeslaFi's own time: old while the connection works means TeslaFi has had nothing
    // new from the car (its problem) or its times are read in the wrong zone (the account's setting)
    const dn = dataNote(ins, now.state, ctx.api.now());
    redraw(inputsSlot,
      ins.length ? inputsLine(ins, ago) : h('span', { class: 'src-inputs' }, h('span', { class: 'src-inputs-label' }, 'Live inputs'), h('span', { class: 'faint' }, 'none: no SMO log and no Tesla API reading yet')),
      ctx.admin ? panelLink(`${state.buildUrl('settings-cars', { car: cid })}#data-sources`, 'Change order', { class: 'panel-link now-inputs-order', title: 'Which input wins when two cover the same time: Settings: Cars, Data sources' }) : null,
      api ? h('span', { class: 'now-inputs-key' }, 'A value marked ', h('span', { class: 'src-via' }, h('i', { class: 'src-m src-m--tesla_api' }), h('span', null, 'API')), ' came from the Tesla API; the rest from SMO.') : null,
      car.sources?.length ? h('span', { class: 'now-inputs-hist' }, h('span', { class: 'faint' }, 'History: '), srcMark(car)) : null,
      dn ? h('p', { class: 'note warn now-inputs-data', role: 'status', dataset: { kind: dn.kind } }, icon('warn', 'ico--sm'), ' ', dn.text) : null);
  }

  function paintBanner() {
    const n = now;
    const midDrive = n.state === 'driving' && n.stale;
    // no data: no sample at all (an unknown state with fresh values is not "no data")
    const none = !n.at_ms;
    const show = n.stale || none;
    redraw(banner, h('div', {
      class: ['stale-banner', none && 'stale-banner--offline'], role: 'status', hidden: !show,
      cover: ['summary.state.drive_offline', 'summary.data.restored_values'],
    }, icon(none ? 'question' : 'clock'),
    h('p', null,
      h('b', null, midDrive ? 'Data stopped during a drive. ' : none ? 'No data for now. ' : 'Old data. '),
      `The newest values are from ${n.at_ms ? formatDateTime(n.at_ms, tz) : 'a while ago'} (${n.at_ms ? ago(n.at_ms) : '—'}). `,
      midDrive ? 'The drive is shown up to there and is finished when later logs arrive. ' : '',
      'Values below are from then; one recorded earlier still shows its own age.')));
  }

  /* ---------- the gauge panel */
  function paintGauge() {
    const n = now, c = n.charge || {};
    const soc = val(n.soc_pct), usable = val(n.usable_soc_pct);
    const ideal = getUnits().range === 'ideal';
    const rangePair = ideal ? n.ideal_range_km : n.range_km;
    const limit = val(c.limit_pct);
    const charging = n.state === 'charging';
    const tone = n.stale || n.state === 'unknown' ? 'stale' : charging ? 'charging' : soc != null && soc < 20 ? 'low' : 'live';
    const full = val(rangePair) != null && soc ? (val(rangePair) / soc) * 100 : null;
    const pills = [];
    if (n.state === 'driving') {
      pills.push(h('span', { class: ['pill pill--lg pill--live', n.stale && 'is-stale'] }, icon('steer'), `Driving · ${format(val(n.speed_kph), 'speed')}`));
      if (val(n.gear)) pills.push(h('span', { class: 'pill pill--lg pill--plain' }, `Gear ${val(n.gear)}`));
    } else if (charging) {
      pills.push(h('span', { class: 'pill pill--lg pill--warm' }, icon('bolt'), `Charging · ${format(val(c.power_kw), 'power')}${val(c.kind) ? ` ${String(val(c.kind)).toUpperCase()}` : ''}`));
    } else {
      const plugged = val(c.plugged);
      pills.push(h('span', { class: ['pill pill--lg', plugged ? 'pill--warm' : 'pill--plain', n.stale && 'is-stale'] }, icon('plug'), plugged ? 'Plugged in' : plugged === false ? 'Unplugged' : 'Plug not known'));
      if (val(c.status) === 'complete') pills.push(h('span', { class: 'pill pill--lg pill--live' }, icon('check'), 'Charge complete'));
    }
    if (val(n.flags?.reduced_range)) pills.push(h('span', { class: 'pill pill--lg pill--warm', title: 'Part of the charge is locked by the cold battery, so the real range is lower.' }, icon('thermo'), 'Reduced range: cold'));
    const [g, label] = gauge(soc, limit, tone);
    redraw(gaugeSlot,
      h('div', { class: 'panel-head' }, h('h2', { class: 'panel-title' }, icon('battery'), 'Battery'),
        h('span', { class: 'faint' }, n.state === 'driving' ? `Driving since ${formatTime(n.since_ms, tz)}` : `${stateLabel(n.state)} since ${when(n.since_ms, tz)}`)),
      h('div', { class: 'gauge-main' },
        h('div', {
          class: ['gauge-wrap', `gauge--${tone}`], role: 'img', cover: ['overview.stat.battery_level', 'nr.dash.battery-level', 'nr.dash.battery-level.bands', 'summary.stat.soc'],
          'aria-label': `Battery ${pct(soc)}${usable != null && Math.abs(usable - soc) >= 0.5 ? `, usable ${pct(usable)}` : ''}, ${format(val(rangePair), 'range')} ${ideal ? 'ideal' : 'rated'} range${limit != null ? `, charge limit ${pct(limit)}` : ''}. Bands: under 20 %, 20 to 80 %, over 80 %.`,
        }, g, label,
        h('div', { class: 'gauge-center' },
          h('div', { class: 'gauge-soc' }, soc != null ? format(soc, 'percent', { unit: false }) : '—', h('sup', null, '%')),
          usable != null && soc != null && Math.abs(usable - soc) >= 0.5 ? h('div', { class: 'gauge-caption gauge-usable', title: 'The cold battery locks part of the charge; this much can be used now.' }, `${pct(usable)} usable`, via(n.usable_soc_pct)) : null,
          h('div', { class: 'gauge-range', cover: ['summary.stat.range_preferred', 'overview.stat.range', 'nr.dash.ideal-range'] }, aged(rangePair, format(val(rangePair), 'range'), n.at_ms)),
          h('div', { class: 'gauge-caption' }, `${ideal ? 'ideal' : 'rated'} range${n.stale ? ', last known' : ''}`))),
        h('div', { class: 'gauge-status' }, pills)),
      h('div', { class: 'gauge-stats' },
        metric('Charge limit', limit == null ? null : withVia(c.limit_pct, quantity(limit, 'percent')), null, { size: 'sm', cover: ['summary.stat.charge_limit', 'nr.dash.charge-limit'], why: val(c.plugged) ? 'Not sent yet' : 'Not plugged in' }),
        metric('In the pack', val(n.energy_remaining_kwh), 'energy', { size: 'sm', explain: 'energy remaining' }),
        metric('Full charge', full, 'range', { size: 'sm', explain: 'range at 100 %' })),
      h('div', { class: 'gauge-est', cover: ['summary.stat.range_estimated', 'nr.dash.est-range'] },
        h('span', null, "Sentry Mate's estimate: "), h('b', null, format(val(n.est_range_km), 'range')),
        h('span', { class: 'faint' }, n.est_basis === 'wh_per_km_30d' ? ' · energy left over the last 30 days’ consumption' : ' · from energy left and recent consumption')));
  }

  /* ---------- the map */
  let map = null, marker = null, markerKey = null, trailShown = false;
  function paintPlace() {
    const n = now, loc = n.location || {};
    const plat = val(loc.lat), plon = val(loc.lon);
    const known = plat != null && plon != null;
    const placeName = loc.place?.name || loc.address?.short || (n.state === 'driving' ? 'On the road' : known ? 'Not a saved place' : 'No position yet');
    const area = [loc.address?.locality, loc.address?.region].filter(Boolean).join(', ');
    const detail = loc.place ? loc.address?.short : area || (known && !loc.address?.short ? `${plat.toFixed(5)}, ${plon.toFixed(5)}` : '');
    const c = n.charge || {};
    const rows = [];
    if (n.state === 'driving') {
      rows.push(['Started', formatTime(n.drive?.start_ms ?? n.since_ms, tz)], ['This drive', `${format(n.drive?.distance_km, 'distance')} · ${dur((n.at_ms || 0) - (n.drive?.start_ms ?? n.since_ms))}`]);
    } else if (n.state === 'charging') {
      rows.push(['Charging since', formatTime(n.since_ms, tz)]);
      if (c.finish_ms) rows.push([`Reaches ${pct(val(c.limit_pct))}`, `about ${when(c.finish_ms, tz)}`]);
    } else {
      rows.push(['Here since', when(n.since_ms, tz)], ['Charge port', CHARGE_STATUS[val(c.status)] || '—']);
    }
    rows.push(['Accuracy', format(val(loc.accuracy_m), 'length')], ['Position', aged(loc.lat, agoEl(at(loc.lat), tz), null)]);
    redraw(placeCard,
      h('div', { class: 'place-card-top' }, h('span', { class: ['place-icon', n.state === 'charging' && 'place-icon--warm', (n.stale || n.state === 'unknown') && 'place-icon--muted'] }, icon(n.state === 'driving' ? 'route' : loc.place ? 'home' : 'pin')),
        h('div', null, h('div', { class: 'place-name' }, placeName), detail ? h('div', { class: 'place-detail' }, detail) : null)),
      h('dl', { class: 'place-card-rows' }, rows.map(([k, v]) => [h('dt', null, k), h('dd', null, v)])));
    const lat = val(loc.lat), lon = val(loc.lon);
    const outside = !!settings?.outside_links?.maps;
    redraw(mapTools,
      h('button', { class: 'btn btn--sm map-btn', type: 'button', cover: 'summary.map.fullscreen', onclick: () => toggleFull() }, icon('monitor', 'ico--sm'), document.fullscreenElement ? 'Back to the page' : 'Full screen'),
      outside && lat != null
        ? h('a', { class: 'btn btn--sm map-btn', href: `https://www.openstreetmap.org/?mlat=${lat.toFixed(5)}&mlon=${lon.toFixed(5)}#map=17/${lat.toFixed(5)}/${lon.toFixed(5)}`, target: '_blank', rel: 'noopener noreferrer external', cover: 'summary.map.external_link' }, icon('ext', 'ico--sm'), 'Open in OpenStreetMap')
        : h('span', { class: 'map-chip', cover: 'summary.map.external_link', title: 'Links to outside map sites are off. An admin can turn them on in Settings: Maps and addresses.' }, icon('lock', 'ico--sm'), 'Outside map links off'));
  }
  function toggleFull() {
    if (document.fullscreenElement) document.exitFullscreen?.();
    else mapPanel.requestFullscreen?.().catch(() => {});
  }
  ctx.on(document, 'fullscreenchange', () => { paintPlace(); map?.map.resize(); });

  async function mountMap() {
    const loc = now.location || {};
    const lat = val(loc.lat), lon = val(loc.lon);
    if (lat == null || lon == null) { mapEl.append(mapFallback('No position recorded yet.')); return; }
    try {
      map = await createMap(mapEl, ctx, { center: [lon, lat], zoom: SPOT_ZOOM, lazy: true, afterReady: true });
      if (!map) return;
      await map.ready;
      if (!ctx.alive()) return;
      placeMarker();
    } catch (e) {
      console.warn(e);
      replace(mapEl, mapFallback());
    }
  }
  // A map drawn again on a 4K screen takes longer than a frame (far longer without a graphics card), and a car
  // that is driving moves its marker every 2 s: while the page is being scrolled, and while the map is out of
  // view, the move waits; it is made once the scroll has rested with the map in view.
  const SCROLL_REST_MS = 250;
  let scrolledAt = -Infinity, restT = 0, markerWaits = false, mapSeen = true;
  const scrolling = () => performance.now() - scrolledAt < SCROLL_REST_MS;
  function markerRest() {
    restT = 0;
    if (!ctx.alive() || !markerWaits) return;
    if (scrolling()) { restT = setTimeout(markerRest, SCROLL_REST_MS); return; }
    if (mapSeen) placeMarker();
  }
  ctx.on(window, 'scroll', () => {
    scrolledAt = performance.now();
    if (markerWaits && !restT) restT = setTimeout(markerRest, SCROLL_REST_MS);
  }, { capture: true, passive: true });
  if (typeof IntersectionObserver === 'function') {
    const seen = new IntersectionObserver((es) => {
      mapSeen = es[es.length - 1].isIntersecting;
      if (mapSeen && markerWaits && !restT) restT = setTimeout(markerRest, 0);
    });
    seen.observe(mapEl);
    ctx.onCleanup(() => seen.disconnect());
  }
  ctx.onCleanup(() => clearTimeout(restT));

  function placeMarker() {
    if (!map) return;
    if (marker && (scrolling() || !mapSeen)) {
      markerWaits = true;
      if (!restT && mapSeen) restT = setTimeout(markerRest, SCROLL_REST_MS);
      return;
    }
    markerWaits = false;
    const n = now, loc = n.location || {};
    const lat = val(loc.lat), lon = val(loc.lon);
    if (lat == null) return;
    const tone = n.stale || n.state === 'unknown' ? 'muted' : n.state === 'charging' ? 'warm' : n.state === 'asleep' ? 'asleep' : 'live';
    const heading = n.state === 'driving' ? val(loc.heading_deg) : null;
    // The marker sits in the middle of what the place card leaves of the map. Where that middle is under the
    // tools at the bottom (the full screen button, the outside link or its "off" chip) or the map's own switch and
    // credit, as on a phone, the frame's bottom edge rises until the whole marker is above them.
    const pad = () => {
      const p = { ...padClear(mapEl, placeCard, { left: 4, top: 30, right: 0, bottom: 0, beside: true }), top: rpx(30) };
      const m = mapEl.getBoundingClientRect();
      if (!m.width || !m.height) return p;
      const reach = (marker?.getElement().offsetWidth || rpx(24)) / 2 + rpx(8);
      const x = m.left + (p.left + m.width - p.right) / 2;
      let y = m.top + (p.top + m.height - p.bottom) / 2;
      const over = [...mapTools.children, mapEl.querySelector('.maplibregl-ctrl-bottom-right')]
        .map((e) => e?.getBoundingClientRect()).filter((r) => r && r.width > 0 && r.height > 0).sort((a, b) => b.top - a.top);
      for (const r of over) {
        if (x + reach > r.left && x - reach < r.right && y + reach > r.top && y - reach < r.bottom) y = r.top - reach;
      }
      y = Math.max(y, m.top + Math.min(p.top + reach, m.height / 2));
      p.bottom = Math.max(p.bottom, Math.round(m.height + p.top - 2 * (y - m.top)));
      return p;
    };
    // live values come every few seconds: the map is drawn again only when the car moved, turned or changed
    // state (a map drawn again on a 4K screen takes longer than a frame and holds a scroll up)
    const key = JSON.stringify([lon, lat, tone, heading, pad()]);
    if (key === markerKey && marker) return;
    markerKey = key;
    marker?.remove();
    marker = carMarker(map, [lon, lat], { tone, heading });
    map.frame(() => map.map.jumpTo({ center: [lon, lat], padding: pad() }));
    if (n.state === 'driving' && n.drive?.drive_id && !trailShown) {
      trailShown = true;
      trailRead(ctx, n.drive.drive_id).then((t) => {
        if (!ctx.alive() || !map) return;
        const coords = (t.features || []).flatMap((f) => f.geometry.coordinates.map((c) => [c[0], c[1]]));
        if (coords.length > 1) addRoute(map, coords, 'trail', { thin: true });
      }).catch(() => {});
    }
  }

  /* ---------- the live panels */
  function paintLive() {
    const n = now, c = n.charge || {}, f = n.flags || {};
    const newest = n.at_ms;
    // temperatures: one plain 0-40 °C scale, the normal range shaded, set points beside
    const scale = (cv, band) => {
      const p = (x) => clamp(((x - 0) / 40) * 100, 0, 100);
      return h('span', { class: 'temp-scale', 'aria-hidden': 'true' },
        band ? h('i', { class: 'temp-band', vars: { '--x': `${p(band[0]).toFixed(1)}%`, '--w': `${(p(band[1]) - p(band[0])).toFixed(1)}%` } }) : null,
        cv != null ? h('i', { class: 'temp-tick', vars: { '--x': `${p(cv).toFixed(1)}%` } }) : null);
    };
    const setp = (pair) => { const v = val(pair); return v == null ? null : typeof v === 'string' ? v : format(v, 'temperature'); };
    const temps = panel({ title: 'Temperatures', icon: 'thermo', side: h('span', { class: 'faint' }, agoEl(newest, tz)) },
      h('div', { class: 'temp-rows' },
        [['Outside', n.outside_c, null, ['summary.stat.outside_temp', 'overview.stat.outside_temp']],
          ['Cabin', n.cabin_c, [18, 24], ['summary.stat.inside_temp', 'overview.stat.inside_temp']],
          ['Battery', n.battery_c, [15, 35], null]].map(([l, pair, band, cv]) => {
          const row = h('div', { class: 'temp-row' }, h('span', { class: 'temp-label' }, l), h('span', { class: 'temp-val' }, aged(pair, quantity(val(pair), 'temperature', { why: val(pair) == null ? (l === 'Cabin' ? 'Not sent by this car' : 'No reading') : null }), newest)), scale(val(pair), band));
          if (cv) cover(row, cv);
          return row;
        })),
      h('div', { class: 'temp-axis', 'aria-hidden': 'true' }, h('span', null, format(0, 'temperature', { digits: 0 })), h('span', { class: 'temp-axis-key' }, h('i'), 'normal range'), h('span', null, format(40, 'temperature', { digits: 0 }))),
      setp(n.setpoint_left_c) == null && setp(n.setpoint_right_c) == null
        ? h('p', { class: 'setpoints', cover: 'overview.stat.driver_temp' }, `Climate ${(CLIMATE[val(f.climate)] || 'not known').toLowerCase()}`, h('span', { class: 'faint' }, val(f.climate) === 'off' ? ' · the car sends no set point while climate is off' : ' · no set point from this car'))
        : h('p', { class: 'setpoints', cover: 'overview.stat.driver_temp' }, 'Climate set to ', h('b', null, setp(n.setpoint_left_c) ?? '—'), ' left, ', h('b', null, setp(n.setpoint_right_c) ?? '—'), ' right', h('span', { class: 'faint' }, ` · climate ${(CLIMATE[val(f.climate)] || '—').toLowerCase()}`)));

    // tyres: all four at all times, with the recommended pressure (the car's, typed, or the door placard's).
    // Flagged at 10 % under it, as the server judges: a cold morning takes a few percent off every tyre, a slow
    // leak goes well past that (a TPMS warning starts at about 20 to 25 % under)
    const tv = tyreView(n);
    const tyres = tv.kpa;
    const tyre = ([key, label], i, right) => {
      const v = tyres[i];
      const isLow = tv.low[i] === true;
      return h('div', { class: ['tire', right && 'tire--r', isLow && 'is-low'] },
        h('span', { class: 'tire-pos' }, label),
        h('span', { class: 'tire-val' }, aged(n.tyres_kpa?.[i], quantity(v, 'pressure', { why: v == null ? 'No reading' : null }), newest)),
        isLow ? h('span', { class: 'tire-note' }, `${format(tv.against[i] - v, 'pressure')} under`) : null);
    };
    const wheel = (i) => (tv.low[i] === true ? 'wheel is-low' : 'wheel');
    // the placard's size only a guess: say so, and where an admin sets it (the Vehicle info panel below)
    const wheelGuess = (now) => {
      const g = wheelGuessLine(now);
      return g ? [' ', g.said, ctx.admin ? [' ', g.ask, ' ', h('a', { href: '#vehicle-info' }, g.action), '.'] : null] : null;
    };
    const tyrePanel = panel({ title: 'Tyres', icon: 'tire', cover: 'summary.flag.tire_pressure_warning', side: h('span', { class: 'faint' }, agoEl(at(n.tyres_kpa?.[0]) ?? newest, tz)) },
      h('div', { class: 'tires' },
        tyre(TYRES[0], 0, false),
        svg('svg', { class: 'car-top', viewBox: '0 0 64 118', 'aria-hidden': 'true', focusable: 'false' },
          svg('rect', { class: wheel(0), x: 2, y: 17, width: 9, height: 22, rx: 3 }), svg('rect', { class: wheel(1), x: 53, y: 17, width: 9, height: 22, rx: 3 }),
          svg('rect', { class: wheel(2), x: 2, y: 79, width: 9, height: 22, rx: 3 }), svg('rect', { class: wheel(3), x: 53, y: 79, width: 9, height: 22, rx: 3 }),
          svg('rect', { class: 'body', x: 9, y: 3, width: 46, height: 112, rx: 19 }),
          svg('path', { class: 'glass', d: 'M15.5 36 Q32 28 48.5 36 L45.5 52 Q32 48 18.5 52 Z' }),
          svg('path', { class: 'glass', d: 'M18.5 86 Q32 90 45.5 86 L47.5 98 Q32 103 16.5 98 Z' })),
        tyre(TYRES[1], 1, true), tyre(TYRES[2], 2, false), tyre(TYRES[3], 3, true)),
      h('p', { class: 'note tire-rec', title: tyresTitle(n) || null }, recommendedLine(n), tv.front != null ? wheelGuess(n) : ' No tyre is flagged.'));

    // the car: 12 V, odometer, software, data age
    const sw = n.software || {};
    const srcWord = { teslamate: 'imported from TeslaMate', teslafi: 'imported from TeslaFi', teslamate_local: 'read live from local TeslaMate', teslafi_api: 'read live from TeslaFi',
      tesla_api: 'read from the Tesla API', owner: 'typed by the owner', build_match: 'matched by its module builds' }[sw.version_source] || null;
    const srcShort = { teslamate: 'TeslaMate import', teslafi: 'TeslaFi import', teslamate_local: 'local TeslaMate', teslafi_api: 'TeslaFi',
      tesla_api: 'the Tesla API', owner: 'typed by the owner', build_match: 'module builds' }[sw.version_source] || null;
    const swFrom = sw.version
      ? [srcShort, sw.version_since_ms ? `since ${formatDate(sw.version_since_ms, tz, { weekday: false })}` : null].filter(Boolean).join(', ')
      : 'not on the CAN bus; TeslaMate, TeslaFi, the Tesla API, an import or you can give it (Software)';
    const speedV = val(n.speed_kph), powerV = val(n.power_kw);
    const vehicle = panel({ title: 'Vehicle', icon: 'car', cover: 'nr.dash.panel.car-status' },
      kv([
        ['Speed', aged(n.speed_kph, format(speedV, 'speed'), newest), { icon: 'steer', cover: 'summary.stat.speed', title: n.state === 'driving' ? 'Speed the car shows' : 'Standing still' }],
        ['Power', aged(n.power_kw, h('span', null, format(powerV, 'power'), powerV != null && powerV < -0.05 ? h('span', { class: 'faint' }, n.state === 'driving' || n.driving ? ' regen' : ' into the pack') : null), newest), { icon: 'bolt', title: 'Power out of the battery; below zero is regen or charging.' }],
        ['12 V battery', aged(n.lv_v, format(val(n.lv_v), 'voltage'), newest), { icon: 'battery' }],
        ['Odometer', h('a', { class: 'link', href: ctx.link('statistics'), title: 'Distance over time on Statistics' }, format(val(n.odometer_km), 'odometer')), { icon: 'odo', cover: ['summary.stat.odometer', 'overview.stat.odometer', 'overview.link.mileage'] }],
        ['Software', h('a', { class: 'link', href: ctx.link('software'), title: sw.version ? `Version ${srcWord || 'from another input'}${sw.version_since_ms ? `, since ${formatDate(sw.version_since_ms, tz, { weekday: false })}` : ''} (the car sends no version text on CAN)` : `Not known: ${swFrom}` }, sw.version || 'not known'), { icon: 'chip', cover: ['summary.stat.version', 'overview.stat.firmware', 'overview.link.updates'] }],
        ['Firmware', h('span', { class: 'car-fw-links' },
          h('a', { class: 'link', href: `${ctx.link('software')}#firmware`, title: 'The fingerprints of the modules\' firmware, as the car sends them' }, 'Module fingerprints'),
          h('a', { class: 'link', href: `${ctx.link('software')}#firmware-history`, title: 'Every fingerprint each module has had, with the version it came with, over time' }, 'History')), { icon: 'chip' }],
        sw.changed_since_ms ? ['Software changed', h('span', { class: 'warn' }, `since ${when(sw.changed_since_ms, tz)}`), { icon: 'info', cover: 'summary.flag.software_update', title: "The car's module builds changed; the version text is not known yet." }]
          : ['Software changed', h('span', { class: 'faint' }, 'no change seen'), { icon: 'info', cover: 'summary.flag.software_update', title: "Shows when the car's module builds change after an update." }],
        ['Newest value', agoEl(n.at_ms, tz), { icon: 'live', cover: ['nr.dash.last-update', 'summary.data.live_push'], title: `Recorded ${n.at_ms ? formatDateTime(n.at_ms, tz) : '—'}. Logs arrive about ${n.delay_s ? Math.round(n.delay_s / 60) || 1 : 1} min after the car records them; this page updates by itself.` }],
        ['Log received', agoEl(n.received_ms, tz), { icon: 'upload' }],
      ]),
      h('p', { class: 'note' }, `Logs arrive about ${n.delay_s ? (n.delay_s < 120 ? `${Math.round(n.delay_s)} s` : dur(n.delay_s * 1000)) : '75 s'} after the car records them, and this page updates by itself; a value older than that shows its own age. `,
        h('a', { class: 'link', href: ctx.link('health'), cover: 'summary.data.prefs_snapshot' }, 'Every signal'), ` · values in your units (${unitLabel('distance')}, ${unitLabel('temperature')}, ${unitLabel('pressure')}); they change at once when you change your units.`));

    // why the car is awake
    const buses = Object.fromEntries((n.buses || []).map((b) => [b.bus, b.awake]));
    const reasons = (n.awake_reasons || []).filter((r) => AWAKE[r]);
    // a sold car, or one that sent nothing for a while, is described as it was when it last sent (never "now")
    const old = state.isSold(car) || n.stale;
    const awakeText = n.state === 'asleep' ? 'Asleep: both buses are quiet.' : n.state === 'driving' ? 'Driving.' : n.state === 'unknown' ? 'Not known: no log for now.'
      : buses.vehicle && !buses.chassis ? 'Parked, kept awake: only the vehicle bus is awake (the chassis bus sleeps).' : 'Awake.';
    const why = panel({ title: old ? 'Why the car was awake' : 'Why the car is awake', icon: 'info', cover: 'web.settings.car.sleep-blockers' },
      h('p', { class: 'why-state' }, old && n.state !== 'unknown' ? `At its last log: ${awakeText[0].toLowerCase()}${awakeText.slice(1)}` : awakeText),
      reasons.length ? h('ul', { class: 'why-list' }, reasons.map((r) => h('li', null, h('b', null, AWAKE[r][0]), h('span', null, AWAKE[r][1]))))
        : n.state === 'parked' ? h('p', { class: 'note' }, old ? 'Nothing the car reported then kept it awake.' : 'Nothing the car reports keeps it awake; it should fall asleep soon.') : null,
      h('p', { class: 'note' }, 'Read from the car’s own messages. Sentry Mate only listens, so it never keeps the car awake.'));

    // status flags: each door and lid by name; a pair keeps its label and value together and wraps whole
    const flagPair = ([k, v]) => h('div', { class: 'flag' }, h('dt', null, k), h('dd', null, v));
    const yes = (pair, on = 'Open', off = 'Closed', warn = true) => { const v = val(pair); return v == null ? '—' : v ? (warn ? h('span', { class: 'warn' }, on) : on) : off; };
    const doors = f.doors || {};
    const anyDoor = ['fl', 'fr', 'rl', 'rr'].some((k) => val(doors[k])) || val(f.trunk) || val(f.frunk);
    const flags = panel({ title: 'Doors and modes', icon: 'lock', cover: ['summary.flag.doors_open'] },
      h('dl', { class: 'flag-grid' },
        [['Front left door', yes(doors.fl)], ['Front right door', yes(doors.fr)], ['Rear left door', yes(doors.rl)], ['Rear right door', yes(doors.rr)],
          ['Trunk', yes(f.trunk)], ['Frunk', yes(f.frunk)], ['Liftgate', val(f.liftgate_deg) ? h('span', { class: 'warn' }, `open ${format(val(f.liftgate_deg), 'degrees')}`) : val(f.liftgate_deg) === 0 ? 'Closed' : '—'],
          ['Charge port', yes(f.charge_port_open)],
          [h('span', { cover: 'summary.flag.plugged_in' }, 'Cable'), val(c.plugged) == null ? '—' : val(c.plugged) ? h('span', { class: 'warn' }, 'Plugged in') : 'Unplugged'],
          [h('span', { cover: 'summary.flag.reduced_range', title: 'Part of the charge is locked by a cold battery' }, 'Cold battery'), val(f.reduced_range) == null ? '—' : val(f.reduced_range) ? h('span', { class: 'warn' }, 'Range reduced') : 'No'],
        ].map(flagPair)),
      h('dl', { class: 'flag-grid flag-grid--modes' },
        [[h('span', { cover: 'summary.flag.sentry_mode' }, 'Sentry'), withVia(f.sentry, yes(f.sentry, 'On', 'Off'))],
          [h('span', { cover: 'summary.flag.sentry_recording', title: 'A guess: the screen and cameras woke while Sentry was on. Not a confirmed event.' }, 'Sentry event (guess)'), val(f.sentry_event_guess) ? h('span', { class: 'warn' }, 'Likely') : 'No'],
          [h('span', { cover: 'summary.flag.preconditioning' }, 'Climate'), withVia(f.climate, CLIMATE[val(f.climate)] || '—')],
          ['Battery preconditioning', withVia(f.battery_preconditioning, yes(f.battery_preconditioning, 'On', 'Off'))],
          [h('span', { cover: 'summary.flag.dog_mode' }, 'Keep climate'), withVia(f.keep_climate, KEEP[val(f.keep_climate)] || '—')],
          ['Battery kept warm', yes(f.keep_warm, 'On', 'Off', false)],
          [h('span', { cover: 'summary.flag.driver_present' }, 'Someone in the car'), withVia(f.user_present, val(f.user_present) == null ? '—' : val(f.user_present) ? 'Yes' : 'No')],
          [h('span', { cover: ['summary.flag.service_mode', 'summary.state.in_service'] }, 'Service mode'), val(f.service_mode) ? h('span', { class: 'warn' }, now.location?.place ? `On, at ${now.location.place.name}` : 'On') : val(f.service_mode) === false ? 'Off' : '—'],
          ['Accessory power', yes(f.accessory_power, 'On', 'Off')],
          ['Centre display', yes(f.display_on, 'On', 'Off', false)],
          ['Parking brake', val(f.parking_brake) == null ? '—' : val(f.parking_brake) ? 'On' : 'Off'],
        ].map(flagPair)),
      h('p', { class: 'note' }, anyDoor ? h('b', null, 'Something is open. ') : null, 'Doors, trunk, frunk and liftgate are read on the CAN bus. Locks and windows are not on it: they come from the Tesla API or TeslaFi, below, when the car has one.'));

    // what no CAN message carries: from the Tesla API or TeslaFi, each value with its input and age
    const o = n.off_can || {};
    const w = o.windows || {}, rt = o.route || {};
    const openWord = (v) => (v ? h('span', { class: 'warn' }, 'Open') : 'Closed');
    const update = o.update_available == null ? null : {
      ...o.update_available,
      value: o.update_installing?.value ? `Installing${val(o.update_version) ? ` ${val(o.update_version)}` : ''}`
        : o.update_available.value ? [val(o.update_version) || 'An update', ' waiting', val(o.update_download_pct) != null ? h('span', { class: 'faint' }, ` · ${pct(val(o.update_download_pct))} downloaded`) : null]
          : 'None waiting',
    };
    const routeText = rt.destination ? [h('b', null, val(rt.destination)),
      h('span', { class: 'faint' }, [val(rt.km) != null ? ` · ${format(val(rt.km), 'distance')}` : '', val(rt.minutes) != null ? ` · ${dur(val(rt.minutes) * 60000)} left` : '', val(rt.traffic_min) ? ` · ${dur(val(rt.traffic_min) * 60000)} lost to traffic` : ''].join(''))] : null;
    const offRows = [
      offRow('Locks', o.locked, (v) => (v ? 'Locked' : h('span', { class: 'warn' }, 'Unlocked')), { cover: 'summary.flag.lock' }),
      offRow('Windows', o.windows_open, (v) => (v ? h('span', { class: 'warn' }, WINDOWS.filter(([k]) => val(w[k])).map(([, l]) => l.replace(/ window$/, '')).join(', ') || 'One is open') : 'All closed'), { cover: 'summary.flag.windows_open' }),
      ...WINDOWS.map(([k, l]) => offRow(l, w[k], openWord, { hide: true })),
      offRow('Sunroof', o.sunroof_state, (v) => [SUNROOF[v] || v, val(o.sunroof_pct) ? h('span', { class: 'faint' }, ` · ${pct(val(o.sunroof_pct))} open`) : null], { hide: true }),
      offRow('Version reported', o.version, (v) => v),
      offRow('Software update', update, (v) => v),
      offRow('Scheduled charge start', o.scheduled_charge_start_ms, (v) => when(v, tz)),
      offRow('Scheduled departure', o.scheduled_departure_ms, (v) => when(v, tz)),
      routeText ? offRow('Route', rt.destination, () => routeText) : null,
    ].filter(Boolean);
    const offLabels = [...new Set(Object.values({ ...o, ...w, ...rt }).filter((x) => x && x.sources).map((x) => x.sources[0].label))];
    const anyOff = offLabels.length > 0;
    const offPanel = panel({ title: 'Locks, windows and software', icon: 'lock', cls: 'off-panel', side: h('span', { class: 'faint' }, anyOff ? offLabels.join(' · ') : 'Not on the CAN bus') },
      anyOff ? h('dl', { class: 'off-grid' }, offRows.map((r) => {
        const el = h('div', { class: 'off-pair' }, h('dt', null, r.label), h('dd', null, r.value, r.src));
        if (r.cv) cover(el, r.cv);
        return el;
      })) : h('p', { class: 'note', cover: ['summary.flag.lock', 'summary.flag.windows_open'] }, 'No input gives these for this car now. No CAN message carries the locks, windows, sunroof, software version, waiting update, charge schedule or route; the Tesla API or TeslaFi can (', h('a', { class: 'link', href: state.buildUrl('settings-integrations', { car: cid }) }, 'Settings: Integrations'), ').'),
      anyOff ? h('p', { class: 'note' }, 'Read by the car’s other inputs, never guessed (a lock request is not a lock state). A value marked stale is older than its limit (an hour for locks and windows, 10 min for a route) and may have changed.') : null);

    // charging now
    const chg = n.charge || {};
    const charging = n.state === 'charging';
    const plugged = val(chg.plugged);
    const usualStart = scheduleFor === n.location?.place?.id ? scheduleHint : null;
    const chargePanel = panel({ title: 'Charging now', icon: 'bolt', cover: ['nr.dash.panel.charge-status', 'summary.stat.scheduled_charging'], cls: charging ? 'is-charging' : null,
      side: chg.charge_id ? panelLink(state.buildUrl('charge', { car: cid, charge: chg.charge_id }), 'Open the charge') : panelLink(ctx.link('charges'), 'Charges') },
    h('p', { class: 'charge-word' }, h('b', null, CHARGE_STATUS[val(chg.status)] || (plugged ? 'Plugged in' : 'Not plugged in')),
      val(chg.kind) ? h('span', { class: 'tag tag--warm' }, String(val(chg.kind)).toUpperCase()) : null,
      !charging && plugged && usualStart ? h('span', { class: 'faint' }, ` · usually starts at ${usualStart} here`) : null),
    h('div', { class: 'charge-grid' },
      metric('Power', val(chg.power_kw) == null ? null : withVia(chg.power_kw, quantity(val(chg.power_kw), 'power')), null, { size: 'sm', why: charging ? 'No reading' : 'Not charging', cover: ['summary.stat.charger_power', 'nr.dash.charging-power', 'overview.stat.charging_power'] }),
      metric('Volts', val(chg.volts_v), 'voltage', { size: 'sm', why: charging ? 'No reading' : 'Not charging', cover: 'overview.stat.ac_voltage', explain: val(chg.kind) === 'dc' ? "the charger's" : null }),
      metric('Amps', val(chg.amps_a), 'current', { size: 'sm', why: charging ? 'No reading' : 'Not charging', cover: 'nr.dash.charging-current', explain: val(chg.phases) ? `${val(chg.phases)} ${val(chg.phases) === 1 ? 'phase' : 'phases'}${val(chg.pilot_a) ? `, station allows ${format(val(chg.pilot_a), 'current')}` : ''}${LIMIT_BY[val(chg.limit_source)] && !['pilot', 'station'].includes(val(chg.limit_source)) ? `; held by ${LIMIT_BY[val(chg.limit_source)]}` : ''}` : null }),
      metric('Added', val(chg.added_kwh) == null ? null : withVia(chg.added_kwh, quantity(val(chg.added_kwh), 'energy')), null, { size: 'sm', cover: 'summary.stat.energy_added', why: 'No charge under way' }),
      metric('Charge limit', val(chg.limit_pct) == null ? null : withVia(chg.limit_pct, quantity(val(chg.limit_pct), 'percent')), null, { size: 'sm', cover: 'summary.stat.charge_limit', explain: val(chg.limit_pct) != null ? 'set on the car' : null, why: plugged ? 'Not sent yet' : 'Not plugged in' }),
      metric('Still to add', val(chg.to_full_kwh), 'energy', { size: 'sm', explain: val(chg.limit_pct) != null ? `to ${pct(val(chg.limit_pct))}` : null, why: plugged ? 'Not known yet' : 'Not plugged in' }),
      metric('Time to full', val(chg.to_full_min) != null ? withVia(chg.to_full_min, quantity(val(chg.to_full_min) * 60000, 'duration')) : null, null, { size: 'sm', cover: ['summary.stat.remaining_time', 'nr.dash.time-to-full'], why: charging ? 'Not known yet' : 'No charge under way' }),
      metric('Done at', chg.finish_ms ? when(chg.finish_ms, tz) : null, null, { size: 'sm', cover: 'summary.stat.expected_finish', explain: chg.finish_ms ? `${formatDate(chg.finish_ms, tz, { weekday: false })}, ${zoneName(chg.finish_ms, tz)} (the car's time)` : null, why: charging ? 'Not known yet' : 'No charge under way' })));

    redraw(rowLive, temps, tyrePanel, vehicle, why);
    redraw(rowMore, chargePanel, flags, offPanel);
  }

  // "usually starts at HH:MM here": the median start time of past charges at this place
  let scheduleHint = null, scheduleFor = null, learning = false;
  async function learnSchedule() {
    const pid = now.location?.place?.id;
    if (!pid || !val(now.charge?.plugged) || now.state === 'charging' || learning) return;
    if (scheduleHint && scheduleFor === pid) return;
    learning = true;
    try {
      const pg = await ctx.api.page(`cars/${encodeURIComponent(cid)}/charges`, { places: pid }, { limit: 20 });
      const mins = pg.items.map((c) => { const d = new Date(c.start_ms); const p = new Intl.DateTimeFormat('en-GB', { timeZone: tz, hour: '2-digit', minute: '2-digit', hourCycle: 'h23' }).format(d).split(':'); return +p[0] * 60 + +p[1]; }).sort((a, b) => a - b);
      if (mins.length < 3 || !ctx.alive()) return;
      const m = mins[Math.floor(mins.length / 2)];
      const base = new Date(Date.UTC(2026, 0, 5, Math.floor(m / 60), m % 60));
      if (now.location?.place?.id !== pid) return;
      scheduleHint = new Intl.DateTimeFormat(undefined, { timeZone: 'UTC', hour: 'numeric', minute: '2-digit' }).format(base);
      scheduleFor = pid;
      paintLive();
    } catch { /* a hint only */ } finally { learning = false; }
  }

  /* ---------- today, newest first (beside the map on large screens) */
  async function paintToday() {
    try {
      const pg = await todayRead(ctx);
      if (!ctx.alive()) return;
      redraw(todaySlot,
        h('div', { class: 'panel-head' }, h('h2', { class: 'panel-title' }, icon('timeline'), 'Today, newest first'), panelLink(state.buildUrl('timeline', { car: cid, period: { preset: '24h' } }), 'Timeline')),
        h('div', { class: 'today-tl-body' }, h('div', { class: 'tl-list today-tl-list' }, pg.items.length ? pg.items.map((r) => timelineRow(r, { car: cid, tz, compact: true })) : h('p', { class: 'note today-empty' }, 'Nothing yet today.'))));
    } catch { replace(todaySlot); }
  }

  /* ---------- the period: figures, charts, strip */
  const sync = `car-${cid}`;
  async function paintWindow() {
    const p = ctx.period;
    const from = p.from_ms ?? (ctx.api.now() - 86400000), to = p.to_ms;
    const rk = getUnits().range === 'ideal' ? 'ideal_range_km' : 'range_km';
    const qs = { from_ms: from, to_ms: to };
    const ideal = rk === 'ideal_range_km';
    const reads = windowReads(ctx);
    // a read that failed is said as that, never shown as "there is none"
    let chargesErr = null;
    const [stats, series, act, charges] = await Promise.all([reads[0].catch(() => null), reads[1], reads[2], reads[3].catch((e) => { chargesErr = e; return { items: [] }; })]);
    if (!ctx.alive()) return;
    const row = stats?.items?.[0] || {};
    // a period with no data is not a period the car stood still: say which it was
    const rec = row.completeness?.recorded_pct;
    const noStats = !stats || !stats.items?.length;
    const noData = !noStats && rec === 0;
    const blank = noStats ? 'Could not be loaded' : noData ? 'No data for this period' : null;
    const partNote = !blank && rec != null && rec < 100 ? ` · ${formatNumber(rec, { maximumFractionDigits: rec < 10 ? 1 : 0 })} % of the period recorded` : '';
    const fig = (v) => (blank ? null : v);

    const periodBtn = h('button', { class: 'btn btn--sm', type: 'button', onclick: (e) => openPeriodPicker(e.currentTarget), cover: 'overview.control.time_range', title: 'Choose the period (shared by every page)' }, icon('calendar', 'ico--sm'), p.preset === 'custom' ? 'Custom period' : p.label, icon('chev-d', 'ico--sm'));
    const summary = h('div', { class: 'window-figures' },
      metric('Distance driven', fig(row.distance_logged_km), 'distance', { cover: 'overview.stat.distance_logged', why: blank, explain: blank ? (noStats ? 'The period figures did not load; the charts below may still show data.' : 'Nothing was logged in this period, so it is not known whether the car moved.') : `${row.drives ?? 0} ${row.drives === 1 ? 'drive' : 'drives'} logged in the period${partNote}` }),
      metric('Net consumption', fig(row.net_wh_per_km), 'efficiency', { cover: 'overview.stat.consumption_net', explain: blank ? null : `Energy the pack gave while driving, regen taken off, per distance.${partNote}`, why: blank || 'No drive in the period' }),
      metric('Gross consumption', fig(row.gross_wh_per_km), 'efficiency', { cover: 'overview.stat.consumption_gross', explain: blank ? null : `Everything used, parked time included, from the pack’s own counters, per distance.${partNote}`, why: blank || 'No drive in the period' }),
      metric('Energy added', fig(row.energy_added_kwh), 'energy', { why: blank, explain: blank ? null : `${row.charges ?? 0} ${row.charges === 1 ? 'charge' : 'charges'}, from the pack’s counters${partNote}` }));

    const battEl = h('div', { class: 'chart chart--batt' });
    const battLegend = h('div', { class: 'legend', role: 'group', 'aria-label': 'Battery chart series' });
    const chgEl = h('div', { class: 'chart chart--chg' });
    const supEl = h('div', { class: 'chart chart--sup' });
    const chgLegend = h('div', { class: 'legend', role: 'group', 'aria-label': 'Charging chart series' });
    const curveNote = h('p', { class: 'note' });

    // drawn off screen and swapped in whole, charts included: a live update never empties the period first
    await offstage(windowSlot, async (slot) => {
    replace(slot,
      h('div', { class: 'window-head', cover: ['overview.control.refresh'] },
        h('h2', { class: 'section-title' }, 'The period'), periodBtn,
        h('span', { class: 'faint' }, 'Updates by itself when a drive, charge or parked spell starts or ends.')),
      panel({ title: 'Summary', icon: 'stats' }, summary),
      panel({ title: 'Battery', icon: 'battery', cover: ['overview.chart.charge_level', 'overview.control.shared_crosshair', 'overview.chart.legend_toggle'],
        side: h('span', { class: 'row' }, srcMark(series, { short: true }), csvButton({ onClick: () => downloadCsv(csvFilename(car.name, 'battery', p.preset), series.t_ms.map((t, i) => ({ t, soc: series.values.soc_pct?.[i], usable: series.values.usable_soc_pct?.[i], range: series.values[rk]?.[i], state: series.state?.[i] })), [timeColumn('Time', 't', tz), column('Battery', 'soc', 'percent'), column('Usable', 'usable', 'percent'), column(ideal ? 'Ideal range' : 'Rated range', 'range', 'range'), column('State', 'state')]) }),
          h('a', { class: 'panel-link', href: ctx.link('battery'), cover: 'overview.link.charge_level' }, 'Battery', icon('chev-r', 'ico--sm'))) },
      battLegend, battEl, h('p', { class: 'note' }, 'Shaded blue: asleep (the battery holds). Grey: unknown, no log; the lines break there.', series.sources?.length > 1 ? ` Points: ${series.source_label}. Where SMO has data its seconds are used; the Tesla API only fills what SMO lacks.` : '')),
      panel({ title: 'Charging', icon: 'bolt', cover: ['overview.chart.charging_details.power', 'overview.chart.charging_details.energy', 'overview.chart.charging_details.ac_current', 'overview.chart.charging_details.ac_voltage', 'overview.chart.charging_details.battery_heater'],
        side: h('span', { class: 'row' }, csvButton({ onClick: () => downloadCsv(csvFilename(car.name, 'charging', p.preset), chgRows.filter((r) => r.power != null), [timeColumn('Time', 't', tz), column('Charge', 'charge'), column('Kind', 'kind'), column('Power', 'power', 'power'), column('Battery heating', 'heater', 'power'), column('Energy added', 'energy', 'energy'), column('Volts', 'volts', 'voltage'), column('Amps', 'amps', 'current')]) }),
          h('a', { class: 'panel-link', href: ctx.link('charges'), cover: 'overview.link.charging_stats' }, 'Charges', icon('chev-r', 'ico--sm'))) },
      chgLegend, chgEl, supEl, curveNote),
      panel({ title: 'What the car did', icon: 'timeline', cover: 'overview.chart.states',
        side: h('span', { class: 'row' }, csvButton({ onClick: () => downloadCsv(csvFilename(car.name, 'activity', p.preset), (act.items || []).map((a) => ({ ...a, state: ACTIVITY[a.kind] || a.kind, minutes: a.end_ms != null ? Math.round((a.end_ms - a.start_ms) / 60000) : null })), [timeColumn('Start', 'start_ms', tz), timeColumn('End', 'end_ms', tz), column('State', 'state'), column('Minutes', 'minutes'), { label: 'Data from', value: (a) => srcText(a) }]) }),
          h('a', { class: 'panel-link', href: ctx.link('timeline'), cover: 'overview.link.states' }, 'Timeline', icon('chev-r', 'ico--sm'))) },
      activityStrip(act.items || [], from, to, { tz, now: ctx.api.now(), size: 'lg' })));

    await loadUPlot();
    if (!ctx.alive()) return;
    batteryChart(battEl, battLegend, series, rk, from, to);
    await chargingChart(chgEl, supEl, chgLegend, curveNote, chargesIn(charges.items, from, to), from, to, chargesErr);
    });
  }

  // a legend whose entries show and hide their series, with the value at the cursor: textsAt(i) gives the
  // values at point i, and each value sits in a box as wide as the widest of them, so the chips keep their size
  function legendChips(host, entries, plots, count, textsAt) {
    const all = Array.from({ length: count }, (_, i) => textsAt(i));
    const chips = entries.map((e, i) => {
      const b = h('button', { class: 'legend-chip', type: 'button', 'aria-pressed': 'true', dataset: { series: e.key }, title: 'Show or hide this line' },
        h('i', { class: 'legend-swatch', vars: { '--c': `var(${e.color})` } }), h('span', null, e.label),
        steady(h('b', { class: 'legend-val' }, e.idle || ''), [e.idle, '—', ...all.map((t) => t[i])]));
      b.addEventListener('click', () => {
        const on = b.getAttribute('aria-pressed') !== 'true';
        b.setAttribute('aria-pressed', String(on));
        const u = plots[e.plot]?.u;
        if (u) u.setSeries(e.index, { show: on });
      });
      return b;
    });
    replace(host, chips);
    return {
      show(i) { const texts = i == null ? null : textsAt(i); chips.forEach((c, k) => { c.querySelector('.legend-val').textContent = i == null ? (entries[k].idle || '') : (texts[k] ?? '—'); }); },
    };
  }

  // a value axis whose numbers carry their unit (100 %, 150 km, 7 kW)
  const unitAxis = (scale, unit, { size = 60, digits = 0, side = 3 } = {}) => ({
    ...valueAxis({ size, digits }), scale, side,
    values: (u, v) => v.map((n) => (n == null ? '' : `${formatNumber(n, { maximumFractionDigits: digits })}${unit === '%' ? ' %' : `\u00a0${unit}`}`)),
    ...(side === 1 ? { grid: { show: false } } : {}),
  });
  const span = (a, kind) => { const f = a.filter((v) => v != null); return f.length ? `${format(Math.min(...f), kind)} – ${format(Math.max(...f), kind)}` : 'no data'; };

  // asleep and unknown buckets drawn behind the lines
  const stateBands = (states, xs) => (u) => {
    if (!states) return;
    const c = u.ctx, { top, height } = u.bbox;
    const asleep = cssVar('--st-asleep'), unknown = cssVar('--st-unknown');
    c.save();
    for (let i = 0; i < xs.length; i++) {
      const s = states[i];
      if (s !== 'asleep' && s !== 'unknown') continue;
      const x0 = u.valToPos(xs[i], 'x', true), x1 = u.valToPos(xs[i + 1] ?? xs[i] + (xs[i] - (xs[i - 1] ?? xs[i] - 60)), 'x', true);
      c.fillStyle = hexA(s === 'asleep' ? asleep : unknown, s === 'asleep' ? 0.13 : 0.22);
      c.fillRect(x0, top, Math.max(1, x1 - x0), height);
    }
    c.restore();
  };

  const plots = {};
  let chgRows = [];
  function batteryChart(el, legendHost, series, rk, from, to) {
    const xs = series.t_ms.map((t) => t / 1000);
    // asleep is not unknown: the battery holds while the car sleeps, so its line goes on through asleep
    // buckets; it breaks only where nothing is known
    const carry = (a) => { let last = null; return (a || []).map((v, i) => { if (v != null) { last = v; return v; } return series.state?.[i] === 'asleep' ? last : (last = null); }); };
    const soc = carry(series.values.soc_pct), usable = carry(series.values.usable_soc_pct), rangeKm = carry(series.values[rk]);
    const range = rangeKm.map((v) => (v == null ? null : convert(v, 'range')));
    const mm = (a) => { const f = a.filter((v) => v != null); return f.length ? [Math.min(...f), Math.max(...f)] : [null, null]; };
    const [s0, s1] = mm(soc), [r0, r1] = mm(range);
    const entries = [
      { key: 'soc', label: 'Battery', color: '--c-soc', plot: 'batt', index: 1, idle: s0 != null ? `${pct(s0)} – ${pct(s1)}` : 'no data' },
      { key: 'usable', label: 'Usable', color: '--c-power', plot: 'batt', index: 2, idle: span(usable, 'percent') },
      { key: 'range', label: rk === 'ideal_range_km' ? 'Ideal range' : 'Rated range', color: '--live', plot: 'batt', index: 3, idle: r0 != null ? `${formatNumber(r0, { maximumFractionDigits: 0 })} – ${formatNumber(r1, { maximumFractionDigits: 0 })} ${unitLabel('range')}` : 'no data' },
    ];
    const leg = legendChips(legendHost, entries, plots, xs.length, (i) => [pct(soc[i]), pct(usable[i]), format(rangeKm[i], 'range')]);
    const make = (width, height) => {
      const c = colors();
      return {
        width, height, padding: [rpx(10), rpx(4), 0, 0], legend: { show: false },
        cursor: { sync: { key: sync, setSeries: false }, drag: { x: false, y: false }, points: { size: rpx(8), width: rpx(2), stroke: c.cursor } },
        scales: { x: { time: true, min: from / 1000, max: to / 1000 }, pct: { range: [0, 100] }, dist: { auto: true, range: (u, a, b) => [0, Math.max(10, (b || 0) * 1.1)] } },
        axes: [timeAxis(tz, { spanMs: to - from }), unitAxis('pct', '%', { size: 52 }), unitAxis('dist', unitLabel('range'), { size: 66, side: 1 })],
        series: [{},
          { label: 'Battery', scale: 'pct', stroke: cssVar('--c-soc'), width: rpx(2), fill: areaFill(cssVar('--c-soc'), 0.2, 0.01), points: { show: false } },
          { label: 'Usable', scale: 'pct', stroke: cssVar('--c-power'), width: rpx(1.5), dash: [rpx(4), rpx(3)], points: { show: false } },
          { label: 'Range', scale: 'dist', stroke: c.live, width: rpx(1.5), points: { show: false } }],
        hooks: {
          drawClear: [stateBands(series.state, xs)],
          setCursor: [(u) => { const i = u.cursor.idx; leg.show(i == null || u.cursor.left < 0 ? null : i); }],
        },
      };
    };
    plots.batt = mountPlot(el, make, [xs, soc, usable, range], ctx, null, 'Battery level, usable level and range over the period');
  }

  async function chargingChart(el, supEl, legendHost, noteEl, charges, from, to, failed = null) {
    // the curves of the charges in the period, on one time axis, broken between sessions
    const sorted = [...charges].sort((a, b) => a.start_ms - b.start_ms).slice(-10);
    let curveErr = null;
    const curves = await Promise.all(curveReads(ctx, charges).map((p) => p.catch((e) => { curveErr = e; return null; })));
    if (!ctx.alive()) return;
    const T = [], P = [], E = [], V = [], A = [], HT = [];
    let addedInWin = 0;
    chgRows = [];
    curves.forEach((cv, k) => {
      if (!cv || !cv.t_ms?.length) return;
      const dc = cv.kind === 'dc';
      // the curve's power added up gives the shape; it is scaled to end at the charge's own figure
      // (energy_added_kwh, from the pack's counters), so chart, legend, CSV and Summary agree
      let raw = 0;
      cv.t_ms.forEach((t, i) => { if (i) raw += ((cv.values.power_kw[i - 1] || 0) * (t - cv.t_ms[i - 1])) / 3600000; });
      const own = sorted[k].energy_added_kwh;
      const k2 = own != null && raw > 0 ? own / raw : 1;
      let kwh = 0, before = 0, inWin = null;
      cv.t_ms.forEach((t, i) => {
        if (i) kwh += (((cv.values.power_kw[i - 1] || 0) * (t - cv.t_ms[i - 1])) / 3600000) * k2;
        if (t < from) before = kwh;
        if (t >= from && t <= to) inWin = kwh;
        if (t < from || t > to) return;
        T.push(t / 1000); P.push(cv.values.power_kw[i]); E.push(kwh); HT.push(cv.values.heater_kw?.[i] ?? null);
        V.push(dc ? cv.values.dc_volts_v?.[i] ?? null : cv.values.volts_v[i]); A.push(dc ? cv.values.dc_amps_a?.[i] ?? null : cv.values.amps_a[i]);
        chgRows.push({ t, charge: sorted[k].id, kind: dc ? 'DC' : 'AC', power: P[P.length - 1], heater: HT[HT.length - 1], energy: kwh, volts: V[V.length - 1], amps: A[A.length - 1] });
      });
      if (inWin != null) addedInWin += inWin - before;
      const last = T[T.length - 1];
      if (last != null) { T.push(last + 1); P.push(null); E.push(null); HT.push(null); V.push(null); A.push(null); }
    });
    for (const x of [el, supEl, legendHost]) x.hidden = !T.length;
    noteEl.textContent = !T.length && (failed || curveErr) ? `The ${failed ? 'charges' : 'charging curves'} of this period could not be loaded: ${(failed || curveErr).message}`
      : !T.length ? 'No charging in this period. Choose a longer period in the period picker to see past charges here.' : `${sorted.length} ${sorted.length === 1 ? 'charge' : 'charges'} in the period${charges.length > 10 ? ' (the 10 newest)' : ''}. For a DC charge, volts and amps are the charger’s own. Battery heating is the heater’s power.`;
    if (!T.length) return;
    const maxOf = (a) => Math.max(0, ...a.filter((v) => v != null));
    const entries = [
      { key: 'power', label: 'Charging power', color: '--c-charge', plot: 'chg', index: 1, idle: T.length ? `peak ${format(maxOf(P), 'power')}` : '' },
      { key: 'heater', label: 'Battery heating', color: '--danger', plot: 'chg', index: 2, idle: T.length ? `peak ${format(maxOf(HT), 'power')}` : '' },
      { key: 'energy', label: 'Energy added', color: '--c-soc', plot: 'chg', index: 3, idle: `${format(addedInWin, 'energy')}${sorted.length > 1 ? ' in all' : ''}` },
      { key: 'volts', label: 'Volts', color: '--c-power', plot: 'sup', index: 1, idle: span(V, 'voltage') },
      { key: 'amps', label: 'Amps', color: '--live', plot: 'sup', index: 2, idle: span(A, 'current') },
    ];
    const leg = legendChips(legendHost, entries, plots, T.length, (i) => [format(P[i], 'power'), format(HT[i], 'power'), format(E[i], 'energy'), format(V[i], 'voltage'), format(A[i], 'current')]);
    const css = cssVar;
    const xs = T.length ? T : [from / 1000, to / 1000];
    const empty = (n) => (T.length ? n : [null, null]);
    const cursor = (u) => { const i = u.cursor.idx; leg.show(i == null || u.cursor.left < 0 ? null : i); };
    const base = (width, height, showX) => {
      const c = colors();
      return {
        width, height, padding: [rpx(10), rpx(4), showX ? 0 : rpx(4), 0], legend: { show: false },
        cursor: { sync: { key: sync, setSeries: false }, drag: { x: false, y: false }, points: { size: rpx(7), width: rpx(2), stroke: c.cursor } },
        hooks: { setCursor: [cursor] },
      };
    };
    plots.chg = mountPlot(el, (width, height) => ({
      ...base(width, height, false),
      scales: { x: { time: true, min: from / 1000, max: to / 1000 }, kw: { range: (u, a, b) => [0, Math.max(5, (b || 0) * 1.12)] }, kwh: { range: (u, a, b) => [0, Math.max(1, (b || 0) * 1.12)] } },
      axes: [timeAxis(tz, { show: false, spanMs: to - from }), unitAxis('kw', unitLabel('power'), { size: 58 }), unitAxis('kwh', unitLabel('energy'), { size: 70, digits: 1, side: 1 })],
      series: [{},
        { label: 'Power', scale: 'kw', stroke: css('--c-charge'), width: rpx(2), fill: areaFill(css('--c-charge'), 0.22, 0.01), points: { show: false } },
        { label: 'Heating', scale: 'kw', stroke: css('--danger'), width: rpx(1.5), points: { show: false } },
        { label: 'Energy', scale: 'kwh', stroke: css('--c-soc'), width: rpx(1.5), dash: [rpx(4), rpx(3)], points: { show: false } }],
    }), [xs, empty(P), empty(HT), empty(E)], ctx, null, 'Charging power, heating power and energy added during the charge');
    plots.sup = mountPlot(supEl, (width, height) => ({
      ...base(width, height, true),
      scales: { x: { time: true, min: from / 1000, max: to / 1000 }, v: { auto: true }, a: { range: (u, a, b) => [0, Math.max(5, (b || 0) * 1.15)] } },
      axes: [timeAxis(tz, { spanMs: to - from }), unitAxis('v', unitLabel('voltage'), { size: 58 }), unitAxis('a', unitLabel('current'), { size: 70, side: 1 })],
      series: [{},
        { label: 'Volts', scale: 'v', stroke: css('--c-power'), width: rpx(1.5), points: { show: false } },
        { label: 'Amps', scale: 'a', stroke: css('--live'), width: rpx(1.5), points: { show: false } }],
    }), [xs, empty(V), empty(A)], ctx, null, 'Supercharger volts and amps during the charge');
  }

  /* ---------- first draw, then live */
  const windowP = paintWindow().catch((e) => {
    if (!ctx.alive()) return;
    replace(windowSlot, panel({ title: 'The period', icon: 'stats' }, h('p', { class: 'note' }, `The charts could not be loaded: ${e.message}`)));
  });
  try {
    [now, settings] = await Promise.all([nowP, settingsP]);
  } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;
  paintHead(); paintInputs(); paintBanner(); paintGauge(); paintPlace(); paintLive();
  ctx.setTitle('Overview');
  const todayP = paintToday();
  await Promise.all([windowP, todayP]);
  if (!ctx.alive()) return;
  ctx.ready();
  // the map (MapLibre's setup is the largest piece of work on this page) is made after ready, once it is in view,
  // in its box of a fixed size with its overlays already there: nothing on the page moves when it comes
  mountMap();
  learnSchedule();

  // live values: merge `now` deltas, draw again at most every 2 s
  let pend = 0;
  ctx.events('now', (ev) => {
    if (ev.data?.car !== cid || !ev.data.now) return;
    const plugKey = () => `${val(now.charge?.plugged)}|${now.location?.place?.id ?? ''}|${now.state}`;
    const before = plugKey();
    merge(now, ev.data.now);
    // plugged in, unplugged or moved: learn the usual start time for the place it is at now
    if (plugKey() !== before && !(scheduleHint && scheduleFor === now.location?.place?.id)) learnSchedule();
    if (pend) return;
    // (while the page scrolls the panels wait for it to rest: drawn all at once they took tens of ms of a frame)
    pend = setTimeout(() => afterScroll(() => { pend = 0; if (!ctx.alive()) return; paintHead(); paintInputs(); paintBanner(); paintGauge(); paintPlace(); paintLive(); placeMarker(); }), 2000);
  });
  let rowsT = 0;
  ctx.events('rows', (ev) => {
    if (ev.data?.car !== cid) return;
    if (rowsT) return;   // (words that keep coming, as in a rebuild, never put it off)
    rowsT = setTimeout(() => afterScroll(() => { rowsT = 0; if (ctx.alive()) { paintWindow().catch(() => {}); paintToday(); } }), 1500);
  });
  ctx.onCleanup(() => { clearTimeout(pend); clearTimeout(rowsT); });
  ctx.onCleanup(onUnitsChange(() => { paintGauge(); paintPlace(); paintLive(); paintWindow().catch(() => {}); }));
}
