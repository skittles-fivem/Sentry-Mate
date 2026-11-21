// Sentry Mate, "Logbook" UI pack: Trip (/cars/{car}/trip?from=&to=).
//
// Any stretch of time as a trip: where the car went, what it cost and how it charged. The trip is the
// shared period (the masthead picker); this page adds shortcuts (the last three drives, one day, a week or
// a month, a saved trip), saving and naming trips, and a second car to compare. Data:
// GET /api/v1/cars/{car}/trip (figures, time split, energy, cost, map markers), /tracks (one line per
// drive), /activity (the state strip), /series (battery %, range, elevation and position over the window),
// /cars/{car}/drives and /charges (the two tables, paged, with CSV), /trips (saved trips: POST, PATCH,
// DELETE), /cars (the car to compare) and the second car's /trip.
//
// Zooming a chart picks a stretch; "Make this the trip" turns it into the window, so the map, the figures
// and both tables follow. Asleep and not recorded time are kept apart everywhere.

import { h, svg, replace } from '../shared/dom.js';
import { format, convert, unitLabel, rangeField, rangeLabel, formatMoneyList } from '../shared/units.js';
import { formatTime, formatDate, formatDuration, dayKey, dayRange, startOf, addPeriod, addDays, periodLabel } from '../shared/time.js';
import { formatNumber, tn } from '../shared/i18n.js';
import { downloadCsv, csvFilename, column, timeColumn } from '../shared/csv.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { pageHead, q, figure, icon, empty, errorView, loadingBlock, flag, button, sep } from '../core/ui.js';
import { dataTable } from '../core/table.js';
import { makeMap, boundsOf, coordsText } from '../core/map.js';
import { stackedCharts } from '../core/chart.js';
import { activityStrip, placeLink, STRIP_NAMES } from '../core/journal.js';
import { sourceMark, sourceLabel, isImported, teslaOnly, ESTIMATED_WHY } from '../core/sources.js';
import { openDialog, confirmDialog } from '../core/dialog.js';
import { zoneLabel } from '../core/format.js';
import { remount } from '../core/router.js';

export const id = 'trip';
export const css = true;
export const title = () => 'Trip';

const SERIES_FIELDS = 'soc_pct,range_km,ideal_range_km,elevation_m,lat,lon';
const TIME_PARTS = [
  ['driving', 'Driving', 'from the first to the last position of each drive'],
  ['charging_ac', 'AC charging', 'charging on a wall box or public AC'],
  ['charging_dc', 'DC fast charging', 'Superchargers and other DC chargers'],
  ['parked_awake', 'Parked, awake', 'off the charger with the car awake (Sentry, climate, someone in it)'],
  ['asleep', 'Parked, asleep', 'the car was quiet and using next to nothing'],
  ['not_recorded', 'Not recorded', 'no log, a paused recording or a log that failed its check; never counted as sleep'],
];
const COST_SOURCE = { tariff: 'place price', manual: 'typed in', free_supercharging: 'free Supercharging', teslamate: 'imported' };
// Whole seconds first, then minutes: the same rounding as formatDuration, so the two lines of a cell agree.
const h_mm = (ms) => { const m = Math.round(Math.round((ms || 0) / 1000) / 60); return `${Math.floor(m / 60)}:${String(m % 60).padStart(2, '0')}`; };
const has = (list) => (list || []).some((v) => v != null);

export async function mount(ctx) {
  const car = ctx.car, tz = ctx.tz, now = api.now();
  const allTime = !(ctx.period && ctx.period.from_ms != null);
  const from = allTime ? 0 : ctx.period.from_ms;
  const to = Math.min(ctx.period && ctx.period.to_ms != null ? ctx.period.to_ms : now, now);
  const win = { from_ms: from, to_ms: to };
  const vs = ctx.query.vs && ctx.query.vs !== car.id ? String(ctx.query.vs) : null;
  const tripPath = (cid) => api.apiPath`cars/${cid}/trip`;
  const link = (page, extra = {}) => state.buildUrl(page, { car: car.id, period: ctx.state.period, ...extra });
  const asTrip = (a, b) => state.buildUrl('trip', { car: car.id, period: { from: a, to: b }, query: vs ? { vs } : null });

  // "Everything recorded" has no start of its own: the strip, the charts, their CSV and the zoom start at the
  // car's first recorded moment (the start of its first activity segment, else its first drive), never 1970.
  let first = from;
  const actP = ctx.api.get(api.apiPath`cars/${car.id}/activity`, win);
  const firstP = !allTime ? Promise.resolve(from) : actP.catch(() => null).then(async (a) => {
    const seg = a && (a.items || []).find((x) => x.start_ms != null);
    if (seg) return seg.start_ms;
    try {
      const d = await ctx.api.get(api.apiPath`cars/${car.id}/drives`, { limit: 1, sort: 'start', dir: 'asc' });
      if (d.items && d.items.length) return d.items[0].start_ms;
    } catch { /* fall back below */ }
    return Math.max(0, to - 86400000);
  }).then((x) => { first = Math.min(x, to - 60000); return first; });
  // Everything the page draws, at once; a part that fails shows its own error.
  const [tripR, actR, seriesR, savedR, carsR, lastR, vsR] = await Promise.allSettled([
    ctx.api.get(tripPath(car.id), win),
    actP.then((a) => firstP.then((f) => (allTime && a ? { ...a, from_ms: f } : a))),
    firstP.then((f) => ctx.api.get(api.apiPath`cars/${car.id}/series`, { from_ms: f, to_ms: to, fields: SERIES_FIELDS, max_points: 1500 })),
    ctx.api.get('trips', { car: car.id, limit: 100 }),
    ctx.api.get('cars', { sort: 'name', limit: 100 }),
    ctx.api.get(api.apiPath`cars/${car.id}/drives`, { limit: 3, sort: 'start', dir: 'desc' }),
    vs ? ctx.api.get(tripPath(vs), win) : Promise.resolve(null),
  ]);
  if (!ctx.alive()) return {};
  if (tripR.status === 'rejected') throw tripR.reason;
  const t = tripR.value;
  let saved = savedR.status === 'fulfilled' ? savedR.value.items || [] : [];
  const current = () => saved.find((x) => x.from_ms === from && x.to_ms === (ctx.period && ctx.period.to_ms));
  const nDrives = (t.drives || []).length, nCharges = (t.charges || []).length;
  const windowText = from
    ? `${formatDate(from, tz, { now })}, ${formatTime(from, tz)} to ${formatDate(to, tz, { now })}, ${formatTime(to, tz)}`
    : `Everything recorded, up to ${formatDate(to, tz, { now })}, ${formatTime(to, tz)}`;

  /* ------------------------------------------------------------ head: the trip's name and window */
  const headTitle = h('span');
  const headAside = h('div', { class: 'trip-head-actions' });
  const drawHead = () => {
    const c = current();
    replace(headTitle, c ? c.name : periodLabel(ctx.period, tz, now) || 'Trip');
    ctx.title(c ? c.name : 'Trip');
    replace(headAside, ctx.me ? button(c ? 'Rename this trip' : 'Save this trip', { iconName: c ? 'settings' : 'star', onclick: () => saveDialog(c) }) : null);
  };
  const head = pageHead({
    eyebrow: car.name,
    title: headTitle,
    deck: [windowText, sep(), zoneLabel(tz), sep(), `${tn(nDrives, '{n} drive', '{n} drives')}, ${tn(nCharges, '{n} charge', '{n} charges')}`],
    aside: headAside,
    cls: 'trip-head',
  });
  drawHead();

  /* ------------------------------------------------------------ window and cars */
  const lastThree = lastR.status === 'fulfilled' ? lastR.value.items || [] : [];
  const today = dayKey(now, tz);
  const dayIn = h('input', { class: 'field-input trip-day', type: 'date', id: 'trip-day', value: dayKey(first || now, tz), max: today });
  dayIn.addEventListener('change', () => {
    if (!/^\d{4}-\d{2}-\d{2}$/.test(dayIn.value)) return;
    const r = dayRange(dayIn.value, dayIn.value, tz);
    ctx.go(asTrip(r.from, Math.min(r.to, now)));
  });
  const weekStart = startOf(now, 'week', tz), monthStart = startOf(now, 'month', tz), lastMonth = addPeriod(monthStart, 'month', -1, tz);
  const yKey = addDays(today, -1);
  const yesterday = dayRange(yKey, yKey, tz);
  const shortcut = (text, a, b, opts = {}) => h('li', null, h('a', { class: ['shortcut', { 'is-on': a === from && Math.abs((b ?? now) - to) < 60000 }], href: asTrip(a, b), cover: opts.cover || null, title: opts.title || null }, text));
  const third = lastThree.length ? lastThree[lastThree.length - 1] : null;
  const shortcuts = h('ul', { class: 'shortcut-list' },
    third ? shortcut(lastThree.length === 3 ? 'The last three drives' : `The last ${tn(lastThree.length, '{n} drive', '{n} drives')}`, third.start_ms, null, {
      cover: 'trip.link.last_three_drives', title: `From the start of the drive at ${formatDate(third.start_ms, tz, { now })}, ${formatTime(third.start_ms, tz)} up to now`,
    }) : h('li', { class: 'shortcut-none', cover: 'trip.link.last_three_drives' }, 'The last three drives: no drive recorded yet'),
    shortcut('Today', startOf(now, 'day', tz), null),
    shortcut('Yesterday', yesterday.from, yesterday.to),
    shortcut('This week', weekStart, null, { title: 'From Monday, as on Statistics' }),
    shortcut('This month', monthStart, null),
    shortcut('Last month', lastMonth, monthStart));

  const savedList = h('ul', { class: 'saved-list' });
  const drawSaved = () => {
    const c = current();
    replace(savedList, saved.length ? saved.map((s) => h('li', { class: ['saved-row', { 'is-on': c && c.id === s.id }] },
      h('a', { class: 'saved-name', href: asTrip(s.from_ms, s.to_ms) }, s.name),
      h('span', { class: 'saved-when' }, `${formatDate(s.from_ms, tz, { now })} to ${formatDate(s.to_ms - 1, tz, { now })}`),
      h('span', { class: 'saved-actions' },
        button('Rename', { cls: 'button-small', onclick: () => saveDialog(s), title: `Rename ${s.name}` }),
        button('Remove', { cls: 'button-small', kind: 'danger', onclick: () => removeSaved(s), title: `Remove ${s.name}` }))))
      : h('li', { class: 'saved-none' }, savedR.status === 'rejected' ? 'Saved trips could not be loaded.' : 'No saved trip for this car yet. Pick a window and press “Save this trip”.'));
  };
  drawSaved();

  async function saveDialog(existing) {
    const nameIn = h('input', { class: 'field-input', id: 'trip-name', maxlength: '80', required: true, value: existing ? existing.name : '', placeholder: 'For example: Lake weekend' });
    const err = h('p', { class: 'field-error', role: 'alert', hidden: true });
    const body = h('div', { class: 'trip-save' },
      h('p', { class: 'note' }, existing ? `${formatDate(existing.from_ms, tz, { now })}, ${formatTime(existing.from_ms, tz)} to ${formatDate(existing.to_ms, tz, { now })}, ${formatTime(existing.to_ms, tz)}.` : `${windowText}. Saved trips are yours: other accounts do not see them.`),
      h('div', { class: 'field' }, h('label', { class: 'field-label', for: 'trip-name' }, 'Name'), nameIn, h('p', { class: 'field-hint' }, '1 to 80 characters.')), err);
    const result = await openDialog({
      title: existing ? 'Rename the trip' : 'Save this trip', body, initialFocus: '#trip-name', signal: ctx.signal,
      actions: [{ label: 'Cancel', value: null }, {
        label: existing ? 'Rename' : 'Save', kind: 'primary', value: 'ok', onclick: async () => {
          const name = nameIn.value.trim();
          if (!name) { err.textContent = 'Give the trip a name.'; err.hidden = false; return false; }
          try {
            if (existing) {
              const got = await ctx.api.patch(api.apiPath`trips/${existing.id}`, { name });
              saved = saved.map((s) => (s.id === got.id ? got : s));
            } else {
              const got = await ctx.api.post('trips', { car: car.id, from_ms: from, to_ms: ctx.period.to_ms ?? to, name });
              saved = [got, ...saved];
            }
            return true;
          } catch (e) {
            err.textContent = (e && e.fields && (e.fields.name || e.fields.to_ms || e.fields.from_ms)) || (e && e.message) || 'Not saved.';
            err.hidden = false;
            return false;
          }
        },
      }],
    });
    if (result !== 'ok' || !ctx.alive()) return;
    ctx.flash(existing ? 'Trip renamed.' : 'Trip saved.');
    drawHead(); drawSaved();
  }
  async function removeSaved(s) {
    if (!(await confirmDialog({ title: `Remove ${s.name}?`, text: 'Only the saved name and window go; the drives and charges stay as they are.', yes: 'Remove', danger: true }))) return;
    try {
      await ctx.api.del(api.apiPath`trips/${s.id}`);
      saved = saved.filter((x) => x.id !== s.id);
      ctx.flash('Saved trip removed.');
      drawHead(); drawSaved();
    } catch (e) { ctx.flash(`Not removed: ${(e && e.message) || 'the server said no'}`, { kind: 'error' }); }
  }

  // A second car to compare, from the cars this account sees (searchable when there are many).
  const cars = carsR.status === 'fulfilled' ? (carsR.value.items || []).filter((c) => c.id !== car.id) : [];
  const vsSel = h('select', { class: 'field-select', id: 'trip-vs' });
  const fillCars = (list) => replace(vsSel, h('option', { value: '' }, 'No second car'),
    list.filter((c) => c.id !== car.id).map((c) => h('option', { value: c.id, selected: c.id === vs }, c.name)),
    vs && !list.some((c) => c.id === vs) ? h('option', { value: vs, selected: true }, vsR.status === 'fulfilled' && vsR.value ? `Car ${vs}` : vs) : null);
  fillCars(cars);
  vsSel.addEventListener('change', () => ctx.go(state.buildUrl('trip', { car: car.id, period: ctx.state.period, query: { vs: vsSel.value || null } })));
  let carSearch = null;
  if (carsR.status === 'fulfilled' && carsR.value.next) {
    let timer = 0;
    carSearch = h('input', { class: 'field-input', type: 'search', id: 'trip-vs-q', placeholder: 'Find a car by name or model', autocomplete: 'off' });
    carSearch.addEventListener('input', () => {
      clearTimeout(timer);
      timer = setTimeout(async () => {
        try { const r = await ctx.api.get('cars', { sort: 'name', limit: 100, search: carSearch.value.trim() || undefined }); fillCars(r.items || []); } catch { /* keep the list */ }
      }, 250);
    });
    ctx.onCleanup(() => clearTimeout(timer));
  }
  const carField = cars.length || vs
    ? h('div', { class: 'field trip-vs-field' }, h('label', { class: 'field-label', for: 'trip-vs' }, 'Compare with'), carSearch, vsSel,
      h('p', { class: 'field-hint' }, 'The same window for another car, side by side under the figures.'))
    : h('p', { class: 'note' }, 'Only this car is on your account, so there is no second car to compare.');

  const windowBlock = h('section', { class: 'block trip-window', 'aria-labelledby': 'tw-title', cover: ['trip.filter.time_range', 'trip.control.per_car'] },
    h('div', { class: 'block-head' }, h('h2', { class: 'label', id: 'tw-title' }, 'Window and cars', h('span', { class: 'label-note' }, 'change the window with the period picker above, or here'))),
    h('div', { class: 'tw-grid' },
      h('div', { class: 'tw-part' }, h('h3', { class: 'tw-label' }, 'Shortcuts'), shortcuts,
        h('div', { class: 'field tw-day' }, h('label', { class: 'field-label', for: 'trip-day' }, 'One day'), dayIn),
        h('p', { class: 'field-hint' }, 'Every row of Statistics, Drives and the Timeline also opens as a trip.')),
      h('div', { class: 'tw-part' }, h('h3', { class: 'tw-label' }, 'Saved trips'), savedList),
      h('div', { class: 'tw-part' }, h('h3', { class: 'tw-label' }, 'Second car'), carField)),
    h('p', { class: 'note tw-rule' }, 'A drive belongs to the trip when it starts in the window; a charge when it starts in the window. Times are in the car’s zone.'));

  /* ------------------------------------------------------------ figures: every blank one says why */
  const noDrive = nDrives ? null : 'no drive in this window';
  const parkedKwh = t.energy_kwh.gross != null && t.energy_kwh.drive_net != null ? t.energy_kwh.gross - t.energy_kwh.drive_net : null;
  const money = (list, why) => (list && list.length ? h('span', { class: 'q' }, formatMoneyList(list)) : q(null, 'money', { why }));
  const per100 = (list) => (list || []).map((x) => format({ amount: x.amount, currency: x.currency }, 'money_per_distance')).join(' + ');
  const costWhy = !nCharges ? 'no charge in this window' : t.cost.priced ? null : 'no charge has a price';
  const figs = h('div', { class: 'figs trip-figs', cover: ['trip.stat.mileage', 'trip.stat.speed_moving', 'trip.stat.speed_incl_dc', 'trip.stat.consumption_net', 'trip.stat.consumption_gross', 'trip.stat.charging_cost', 'trip.stat.cost_per_100', 'trip.empty_states'] },
    figure('Distance', q(t.distance.odometer_km, 'distance', { why: noDrive || 'odometer not read' }),
      t.distance.odometer_km != null ? ['by the odometer; the drives add up to ', format(t.distance.drives_km, 'distance')] : nDrives ? ['the drives add up to ', format(t.distance.drives_km, 'distance')] : null),
    figure('Average speed, moving', q(t.speed_kph.moving, 'speed', { why: noDrive || 'no driving time' }), 'distance of the drives ÷ their driving time'),
    figure('Average speed, with fast charging', q(t.speed_kph.incl_dc, 'speed', { why: noDrive || 'no driving time' }),
      t.time_ms.charging_dc ? `${formatDuration(t.time_ms.charging_dc)} of DC charging counted as travel time` : 'no DC charging, so the same as moving'),
    figure('Consumption, net', q(t.wh_per_km.net, 'efficiency', { why: noDrive || 'too short to be reliable' }), 'energy the drives used ÷ their distance (regen already off)'),
    figure('Consumption, gross', q(t.wh_per_km.gross, 'efficiency', { why: noDrive || 'too short to be reliable' }),
      parkedKwh != null && nDrives ? ['the drives plus ', format(parkedKwh, 'energy'), ' used parked (standby, Sentry, climate) ÷ distance; same method for any window'] : 'driving and parked use ÷ distance'),
    figure('Charging cost', money(t.cost.by_currency, costWhy),
      nCharges ? `${formatNumber(t.cost.priced)} of ${tn(nCharges, '{n} charge', '{n} charges')} priced${t.cost.unpriced ? `; ${formatNumber(t.cost.unpriced)} without a price, set it in the table below` : ''}; currencies never added together` : null),
    figure('Cost per distance', (t.cost.per_100km || []).length ? h('span', { class: 'q' }, per100(t.cost.per_100km)) : q(null, 'money_per_distance', { why: costWhy || noDrive || 'too short a distance' }),
      (t.cost.per_100km || []).length ? 'charging cost of the window ÷ distance driven' : null));

  // Where the time went: one bar, every part with its share and an explanation.
  const totalMs = TIME_PARTS.reduce((a, [k]) => a + (t.time_ms[k] || 0), 0);
  const timeCsv = button('Download CSV', { iconName: 'download', cls: 'button-small', onclick: () => downloadCsv(csvFilename(car.name, 'trip', dayKey(first || now, tz), 'time spent'), TIME_PARTS, [
    { label: 'Part', value: (p) => p[1] }, { label: 'Minutes', value: (p) => Math.round(((t.time_ms[p[0]] || 0) / 60000) * 10) / 10 },
    { label: 'Share (%)', value: (p) => (totalMs ? Math.round(((t.time_ms[p[0]] || 0) / totalMs) * 1000) / 10 : null) },
  ]) });
  const timeBlock = h('section', { class: 'trip-part trip-time', 'aria-labelledby': 'tt-title', cover: 'trip.chart.time_spent' },
    h('div', { class: 'part-head' }, h('h2', { class: 'label', id: 'tt-title' }, 'Where the time went', h('span', { class: 'label-note' }, formatDuration(totalMs))), totalMs ? timeCsv : null),
    totalMs ? [
      h('div', { class: 'ts-bar', role: 'img', 'aria-label': TIME_PARTS.filter(([k]) => t.time_ms[k]).map(([k, n]) => `${n} ${formatDuration(t.time_ms[k])}`).join(', ') },
        TIME_PARTS.filter(([k]) => t.time_ms[k] > 0).map(([k, n]) => h('span', { class: ['ts-seg', `ts-${k}`], title: `${n}: ${formatDuration(t.time_ms[k])}`, vars: { '--w': `${((t.time_ms[k] / totalMs) * 100).toFixed(3)}%` } }))),
      h('ul', { class: 'ts-list' }, TIME_PARTS.map(([k, n, why]) => h('li', { class: ['ts-row', { 'is-zero': !t.time_ms[k] }] },
        h('span', { class: ['ts-swatch', `ts-${k}`], 'aria-hidden': 'true' }),
        h('span', { class: 'ts-name' }, n, h('span', { class: 'ts-why' }, why)),
        h('span', { class: 'ts-value' }, formatDuration(t.time_ms[k] || 0), h('span', { class: 'ts-share' }, format(totalMs ? ((t.time_ms[k] || 0) / totalMs) * 100 : 0, 'percent', { digits: 0 }))))))]
      : h('p', { class: 'note' }, 'Nothing was recorded in this window.'));

  // Energy used against energy put back.
  const E = [
    ['gross', 'Used in all', t.energy_kwh.gross, 'driving and parked, from the pack'],
    ['drive_net', 'Used driving', t.energy_kwh.drive_net, 'net, regen taken off'],
    ['added_ac', 'Added on AC', t.energy_kwh.added_ac, 'into the pack'],
    ['added_dc', 'Added on DC', t.energy_kwh.added_dc, 'into the pack'],
  ];
  const eMax = Math.max(0.001, ...E.map((e) => e[2] || 0));
  const energyCsv = button('Download CSV', { iconName: 'download', cls: 'button-small', onclick: () => downloadCsv(csvFilename(car.name, 'trip', dayKey(first || now, tz), 'energy'), E, [
    { label: 'Figure', value: (e) => e[1] }, column('Energy', (e) => e[2], 'energy', { digits: 3 }), { label: 'How', value: (e) => e[3] },
  ]) });
  const energyBlock = h('section', { class: 'trip-part trip-energy', 'aria-labelledby': 'te-title', cover: 'trip.chart.energy' },
    h('div', { class: 'part-head' }, h('h2', { class: 'label', id: 'te-title' }, 'Energy used and added', h('span', { class: 'label-note' }, unitLabel('energy'))), energyCsv),
    h('ul', { class: 'en-list' }, E.map(([k, n, v, how]) => h('li', { class: 'en-row' },
      h('span', { class: 'en-name' }, n, h('span', { class: 'en-why' }, how)),
      h('span', { class: 'en-bar', 'aria-hidden': 'true' }, h('span', { class: ['en-fill', `en-${k}`], vars: { '--w': `${(((v || 0) / eMax) * 100).toFixed(2)}%` } })),
      h('span', { class: 'en-value' }, q(v, 'energy'))))),
    h('p', { class: 'note' }, 'Measured at the pack (volts × amps and its energy counters). Imported history is estimated from range.'));

  /* ------------------------------------------------------------ the second car, side by side */
  let compareBlock = null;
  if (vs) {
    const other = vsR.status === 'fulfilled' ? vsR.value : null;
    const otherName = (carsR.status === 'fulfilled' && (carsR.value.items || []).find((c) => c.id === vs) || {}).name || vs;
    if (!other) {
      compareBlock = h('section', { class: 'block trip-compare' }, h('div', { class: 'block-head' }, h('h2', { class: 'label' }, `Compared with ${otherName}`)),
        errorView(vsR.reason, { eyebrow: 'Could not load the second car', retry: () => remount() }));
    } else {
      const R = [
        ['Distance', (x) => x.distance.odometer_km, 'distance'], ['Drives', (x) => (x.drives || []).length, 'count'], ['Charges', (x) => (x.charges || []).length, 'count'],
        ['Driving time', (x) => x.time_ms.driving, 'duration'], ['Asleep', (x) => x.time_ms.asleep, 'duration'], ['Not recorded', (x) => x.time_ms.not_recorded, 'duration'],
        ['Average speed, moving', (x) => x.speed_kph.moving, 'speed'], ['Consumption, net', (x) => x.wh_per_km.net, 'efficiency'], ['Consumption, gross', (x) => x.wh_per_km.gross, 'efficiency'],
        ['Energy used in all', (x) => x.energy_kwh.gross, 'energy'], ['Added on AC', (x) => x.energy_kwh.added_ac, 'energy'], ['Added on DC', (x) => x.energy_kwh.added_dc, 'energy'],
        ['Charging cost', (x) => x.cost.by_currency, 'money'],
      ];
      const cell = (get, kind, x) => { const v = get(x); return kind === 'duration' ? formatDuration(v || 0) : kind === 'money' ? money(v, 'none priced') : kind === 'count' ? formatNumber(v) : q(v, kind); };
      const csvVal = (get, kind, x) => { const v = get(x); return kind === 'money' ? formatMoneyList(v) : kind === 'duration' ? Math.round((v || 0) / 60000) : kind === 'count' ? v : v == null ? null : Math.round(convert(v, kind) * 100) / 100; };
      const unitOf = (kind) => (kind === 'duration' ? 'min' : kind === 'money' || kind === 'count' ? '' : unitLabel(kind));
      const tbl = h('table', { class: 'ledger-table compare-table' },
        h('thead', null, h('tr', null, h('th', { scope: 'col' }, 'Figure'), h('th', { scope: 'col', class: 'num' }, car.name), h('th', { scope: 'col', class: 'num' }, otherName))),
        h('tbody', null, R.map(([n, get, kind]) => h('tr', null, h('th', { scope: 'row' }, n), h('td', { class: 'num' }, cell(get, kind, t)), h('td', { class: 'num' }, cell(get, kind, other))))));
      compareBlock = h('section', { class: 'block trip-compare', 'aria-labelledby': 'tc-title' },
        h('div', { class: 'block-head' }, h('h2', { class: 'label', id: 'tc-title' }, `Compared with ${otherName}`, h('span', { class: 'label-note' }, 'the same window, in this car’s zone')),
          h('div', { class: 'block-actions' },
            h('a', { class: 'button button-quiet button-small', href: state.buildUrl('trip', { car: vs, period: ctx.state.period, query: { vs: car.id } }) }, `Open ${otherName}’s trip`, icon('arrowRight')),
            button('Download CSV', { iconName: 'download', cls: 'button-small', onclick: () => downloadCsv(csvFilename(car.name, otherName, 'trip', dayKey(first || now, tz)), R, [
              { label: 'Figure', value: (r) => r[0] }, { label: 'Unit', value: (r) => unitOf(r[2]) },
              { label: car.name, value: (r) => csvVal(r[1], r[2], t) }, { label: otherName, value: (r) => csvVal(r[1], r[2], other) }]) }))),
        h('div', { class: 'ledger-scroll' }, tbl));
    }
  }

  /* ------------------------------------------------------------ the route map */
  const mapBox = h('div', { class: 'map map-trip', role: 'region', 'aria-label': 'Map of the trip' });
  const mapKey = h('ul', { class: 'trip-map-key' },
    [['pin-start', 'Start'], ['pin-end', 'End'], ['pin-charge', 'Charge'], ['pin-overnight', 'Overnight stop away from home']].map(([c, n]) =>
      h('li', null, h('span', { class: ['pin-swatch', c], 'aria-hidden': 'true' }), n)));
  const mapNote = h('p', { class: 'note map-foot' });
  const mapCol = h('section', { class: 'trip-map', 'aria-labelledby': 'tm-title', cover: 'trip.map.route' },
    h('h2', { class: 'label', id: 'tm-title' }, 'Where the car went', h('span', { class: 'label-note' }, 'one line per drive')), mapBox, mapKey, mapNote);

  /* ------------------------------------------------------------ activity strip */
  const act = actR.status === 'fulfilled' ? actR.value : null;
  const stripCsv = act ? button('Download CSV', { iconName: 'download', cls: 'button-small', onclick: () => downloadCsv(csvFilename(car.name, 'trip', dayKey(first || now, tz), 'states'), act.items || [], [
    { label: 'State', value: (s) => STRIP_NAMES[s.kind] || s.kind }, timeColumn('Start', 'start_ms', tz), timeColumn('End', 'end_ms', tz),
    { label: 'Minutes', value: (s) => Math.round((((s.end_ms ?? now) - s.start_ms) / 60000) * 10) / 10 }, { label: 'Data from', value: (s) => sourceLabel(s) },
  ]) }) : null;
  const stripBlock = h('section', { class: 'block trip-strip', 'aria-labelledby': 'ts-title', cover: 'trip.chart.states' },
    h('div', { class: 'block-head' }, h('h2', { class: 'label', id: 'ts-title' }, 'What the car was doing', h('span', { class: 'label-note' }, 'pick a drive or a charge to open it')), stripCsv),
    act ? activityStrip(act, { tz, now, link: (s) => (s.ref && s.ref.drive ? link('drive', { drive: s.ref.drive }) : s.ref && s.ref.charge ? link('charge', { charge: s.ref.charge }) : null), label: `What ${car.name} was doing during the trip, coloured by state.` })
      : errorView(actR.reason, { eyebrow: 'Could not load the strip', retry: () => remount() }));

  /* ------------------------------------------------------------ battery, range and elevation */
  const s = seriesR.status === 'fulfilled' ? seriesR.value : null;
  const charts = h('div', { class: 'charts trip-charts', cover: ['trip.chart.battery.soc', 'trip.chart.battery.range', 'trip.chart.elevation'] });
  const zoomBar = h('div', { class: 'zoom-bar', 'aria-live': 'polite', cover: 'trip.control.zoom' },
    h('p', { class: 'zoom-idle' }, 'Point at a chart to read all three at that moment and see where the car was. Drag across a chart to pick a stretch; you can then make it the trip.'));
  const readout = h('p', { class: 'readout trip-readout' });
  const seriesCsv = button('Download CSV', { iconName: 'download', cls: 'button-small', disabled: !s || !s.t_ms.length });
  const chartBlock = h('section', { class: 'block trip-battery', 'aria-labelledby': 'tb-title' },
    h('div', { class: 'block-head' }, h('h2', { class: 'label', id: 'tb-title' }, 'Battery and elevation',
      h('span', { class: 'label-note' }, s && s.bucket_ms > 1000 ? `one value per ${formatDuration(s.bucket_ms)}, lowest and highest kept` : 'over the trip', s && s.t_ms && s.t_ms.length && ((s.sources && s.sources.length) || s.source_label) ? `; ${sourceLabel(s).replace(/^D/, 'd').replace(/^I/, 'i')}` : '')), h('div', { class: 'block-actions' }, seriesCsv)),
    readout, zoomBar, charts);

  /* ------------------------------------------------------------ the drives and the charges */
  const placeCell = (p, still = false) => {
    if (!p) return '—';
    const href = still ? null : placeLink(p);
    const name = p.label || (p.address && p.address.short) || 'an unnamed spot';
    const sub = p.place && p.address && p.address.short ? h('span', { class: 'cell-sub' }, p.address.short) : null;
    return [href ? h('a', { class: 'cell-main place-link', href, title: p.place ? `Edit ${p.place.name} in Places` : 'Name this spot in Places' }, name) : h('span', { class: 'cell-main' }, name), sub];
  };
  const pct = (v) => q(v, 'percent', { digits: 0 });
  const consumptionWhy = (d) => (d.end_ms == null ? 'still driving' : d.distance_km != null && d.distance_km < 1 ? 'too short to be reliable' : d.energy_kwh == null ? 'no pack reading' : 'not reliable');
  const drives = dataTable(ctx, {
    path: api.apiPath`cars/${car.id}/drives`, params: { ...win }, limit: 100, sort: { key: 'start', dir: 'asc' }, itemName: ['drive', 'drives'],
    label: 'Drives of the trip', cls: 'trip-drives',
    caption: 'In the order driven. Open a drive for its route and charts; a place opens in Places.',
    cover: ['trip.drives.table'],
    empty: () => empty({ title: 'No drive started in this window.', text: 'Pick a longer window with the period picker, or one of the shortcuts above.' }),
    columns: [
      { key: 'start', label: 'Started', sort: 'start', th: true, firstDir: 'asc', cover: ['trip.drives.date', 'trip.drives.link.details'],
        cell: (d) => [h('a', { class: 'cell-main', href: link('drive', { drive: d.id }), title: 'Open the drive' }, formatTime(d.start_ms, tz)),
          h('span', { class: 'cell-sub' }, formatDate(d.start_ms, tz, { now })), isImported(d) ? flag('Imported', { kind: 'estimated', title: `${sourceLabel(d)}: energy estimated from range` }) : null, sourceMark(d, { block: true, short: true })] },
      { key: 'from', label: 'From', cover: ['trip.drives.start', 'trip.drives.link.geofence_start'], cell: (d) => placeCell(d.start) },
      { key: 'to', label: 'To', cover: ['trip.drives.destination', 'trip.drives.link.geofence_end'], cell: (d) => placeCell(d.end, d.end_ms == null) },
      { key: 'duration', label: 'Duration', sort: 'duration', num: true, cover: ['trip.drives.duration', 'trip.drives.link.duration_hint'], hint: 'Hours and minutes; the line under it is the same in minutes',
        cell: (d) => h('span', { title: `${h_mm(d.duration_ms)} (hours:minutes)` }, h('span', { class: 'cell-main' }, formatDuration(d.duration_ms)), h('span', { class: 'cell-sub' }, `${h_mm(d.duration_ms)} h:mm`)) },
      { key: 'distance', label: 'Distance', sort: 'distance', num: true, cover: 'trip.drives.distance', cell: (d) => q(d.distance_km, 'distance', { why: 'not read' }) },
      { key: 'soc_start', label: 'Battery at start', num: true, cover: 'trip.drives.soc_start', cell: (d) => pct(d.start_soc_pct) },
      { key: 'soc_end', label: 'Battery at end', num: true, cover: 'trip.drives.soc_end', cell: (d) => (d.end_ms == null ? [pct(d.end_soc_pct), h('span', { class: 'cell-sub' }, 'now')] : pct(d.end_soc_pct)) },
      { key: 'wh', label: 'Consumption', sort: 'wh_per_km', num: true, cover: 'trip.drives.consumption', hint: 'Net energy ÷ distance; blank with the reason when it would be unreliable',
        cell: (d) => (d.wh_per_km != null ? [q(d.wh_per_km, 'efficiency'), d.energy_basis === 'estimated' ? h('span', { class: 'cell-sub', title: teslaOnly(d) ? ESTIMATED_WHY : null }, 'estimated') : null] : q(null, 'efficiency', { why: consumptionWhy(d) })) },
    ],
  });

  const admin = ctx.me && ctx.me.role === 'admin';
  const powerClass = (kw) => (kw == null ? null : kw < 20 ? 'pw-slow' : kw < 55 ? 'pw-mid' : 'pw-fast');
  const rangeAdded = (c) => {
    if (rangeField('range_km') === 'range_km') return c.range_added_km;
    const a = c[rangeField('start_range_km')], b = c[rangeField('end_range_km')];
    return a != null && b != null ? b - a : c.range_added_km;
  };
  const costCell = (c) => {
    const text = c.cost ? format({ amount: c.cost.amount, currency: c.cost.currency }, 'money') : 'No price';
    const sub = c.cost ? COST_SOURCE[c.cost.source] || c.cost.source : c.place ? 'the place has no price' : 'not a saved place';
    const href = link('charge', { charge: c.id, query: { edit: 'cost' } });
    return [admin ? h('a', { class: ['cell-main', 'cost-link', { 'is-missing': !c.cost }], href, title: c.cost ? 'Change the cost of this charge' : 'Set the cost of this charge' }, text, h('span', { class: 'cost-edit' }, c.cost ? 'change' : 'set'))
      : h('a', { class: ['cell-main', 'cost-link', { 'is-missing': !c.cost }], href: link('charge', { charge: c.id }), title: 'Open the charge to see how its cost was worked out' }, text),
    h('span', { class: 'cell-sub' }, sub), admin ? null : h('span', { class: 'cell-sub cost-ro' }, 'an admin can edit it')];
  };
  const drawnCell = (c) => (c.energy_grid_kwh != null ? [q(c.energy_grid_kwh, 'energy'), h('span', { class: 'cell-sub' }, 'grid, metered')]
    : c.dc_inlet_kwh != null ? [q(c.dc_inlet_kwh, 'energy'), h('span', { class: 'cell-sub' }, 'charger, at the inlet')]
      : q(null, 'energy', { why: c.kind === 'dc' ? 'not sent by the charger' : 'not measured' }));
  const charges = dataTable(ctx, {
    path: api.apiPath`cars/${car.id}/charges`, params: { ...win }, limit: 100, sort: { key: 'start', dir: 'asc' }, itemName: ['charge', 'charges'],
    label: 'Charges of the trip', cls: 'trip-charges',
    caption: admin ? 'In order. Open a charge for its curve; the cost opens its cost editor.' : 'In order. Open a charge for its curve and how its cost was worked out.',
    cover: ['trip.charges.table', 'trip.charges.link.cost'],
    empty: () => empty({ title: 'No charge started in this window.', text: 'The car did not charge during the trip, or a charge started before it; pick a wider window to include it.' }),
    columns: [
      { key: 'start', label: 'Started', sort: 'start', th: true, firstDir: 'asc', cover: ['trip.charges.date', 'trip.charges.link.details'],
        cell: (c) => [h('a', { class: 'cell-main', href: link('charge', { charge: c.id }), title: 'Open the charge' }, formatTime(c.start_ms, tz)),
          h('span', { class: 'cell-sub' }, `${formatDate(c.start_ms, tz, { now })}, ${(c.kind || '').toUpperCase() || 'kind not known'}`), sourceMark(c, { block: true, short: true })] },
      { key: 'place', label: 'Place', cover: ['trip.charges.location', 'trip.charges.link.geofence'],
        cell: (c) => placeCell({ ...c.position, place: c.place, address: c.address, label: (c.place && c.place.name) || (c.address && c.address.short) }) },
      { key: 'duration', label: 'Charging time', sort: 'duration', num: true, cover: 'trip.charges.duration',
        cell: (c) => [h('span', { class: 'cell-main' }, c.end_ms == null ? `${formatDuration(c.charging_ms)} so far` : formatDuration(c.charging_ms)), c.plugged_ms && c.plugged_ms - c.charging_ms > 60000 ? h('span', { class: 'cell-sub' }, `plugged in ${formatDuration(c.plugged_ms)}`) : null] },
      { key: 'cost', label: 'Cost', sort: 'cost', num: true, cover: 'trip.charges.cost', cell: costCell },
      { key: 'added', label: 'Added', sort: 'energy', num: true, cover: 'trip.charges.energy_added', hint: 'Energy stored in the pack', cell: (c) => [q(c.energy_added_kwh, 'energy', { why: 'not measured' }), h('span', { class: 'cell-sub' }, 'into the pack')] },
      { key: 'drawn', label: 'Drawn', cls: 'c-opt', num: true, cover: 'trip.charges.energy_used', hint: 'Energy taken from the charger, losses included', cell: drawnCell },
      { key: 'power', label: 'Average power', cls: 'c-opt2', num: true, cover: 'trip.charges.power_avg', hint: 'Energy added ÷ charging time; slow under 20 kW, fast from 55 kW',
        cell: (c) => h('span', { class: ['pw', powerClass(c.power_avg_kw)] }, q(c.power_avg_kw, 'power', { why: 'too short' })) },
      { key: 'range', label: `${rangeLabel()} added`, cls: 'c-opt', num: true, cover: 'trip.charges.range_added', cell: (c) => q(rangeAdded(c), 'distance', { why: 'not read' }) },
      { key: 'soc_start', label: 'Battery at start', num: true, cover: 'trip.charges.soc_start', cell: (c) => pct(c.start_soc_pct) },
      { key: 'soc_end', label: 'Battery at end', num: true, cover: 'trip.charges.soc_end', cell: (c) => (c.end_ms == null ? [pct(c.end_soc_pct), h('span', { class: 'cell-sub' }, 'now')] : pct(c.end_soc_pct)) },
    ],
  });

  /* ------------------------------------------------------------ layout */
  const liveNote = h('p', { class: 'trip-live', hidden: true, role: 'status' });
  ctx.main.append(head, liveNote, windowBlock,
    h('div', { class: 'trip-body', cover: 'shots.trip' },
      h('div', { class: 'trip-top' }, mapCol,
        h('div', { class: 'trip-side' }, h('h2', { class: 'label' }, 'The trip in figures'), figs, timeBlock, energyBlock)),
      compareBlock, stripBlock, chartBlock,
      h('section', { class: 'block', 'aria-labelledby': 'td-title' }, h('div', { class: 'block-head' }, h('h2', { class: 'label', id: 'td-title' }, 'Drives')), drives.el),
      h('section', { class: 'block', 'aria-labelledby': 'tch-title' }, h('div', { class: 'block-head' }, h('h2', { class: 'label', id: 'tch-title' }, 'Charges')), charges.el)));

  // A window that runs up to now: say when a drive or charge starts or ends (pages never poll).
  if (to >= now - 60000) {
    ctx.events(['rows'], (ev) => {
      const e = ev.data || {};
      if (e.car !== car.id) return;
      replace(liveNote, 'The car has new drives or charges in this window. ', h('button', { type: 'button', class: 'button button-quiet button-small', onclick: () => remount() }, icon('reset'), 'Show them'));
      liveNote.hidden = false;
    });
  }

  const tablesLoaded = Promise.all([drives.load(), charges.load()]);

  /* ------------------------------------------------------------ the map: drives, then the markers */
  let tracks = [];
  let tracksErr = null;
  if (nDrives) {
    try {
      const r = await ctx.api.get('tracks', { cars: car.id, ...win, max_points: Math.min(200000, Math.max(4000, nDrives * 400)) });
      tracks = (r.drives || []).map((d) => d.lon.map((x, i) => [x, d.lat[i]]).filter((c) => c[0] != null && c[1] != null)).filter((c) => c.length > 1);
    } catch (err) { if (ctx.signal.aborted) return {}; tracksErr = err; }
  }
  const markers = (t.markers || []).filter((m) => m.lat != null && m.lon != null);
  const allPts = [...tracks.flat(), ...markers.map((m) => [m.lon, m.lat])];
  let m = null, dot = null, dotEl = null;
  if (allPts.length) {
    m = await makeMap(ctx, mapBox, { bounds: boundsOf(allPts), padding: 48, maxZoom: 15, fullscreen: true });
    if (!ctx.alive()) return {};
    if (m) {
      await m.loaded;
      if (!ctx.alive()) return {};
      m.map.addSource('drives', { type: 'geojson', data: { type: 'FeatureCollection', features: tracks.map((c) => ({ type: 'Feature', properties: {}, geometry: { type: 'LineString', coordinates: c } })) } });
      m.addLine({ id: 'drives-casing', source: 'drives', paint: { 'line-opacity': 0.9 } }, 7, '--paper');
      m.addLine({ id: 'drives-line', source: 'drives' }, 3.2, '--accent');
      // Markers at the same spot (within about 100 m) share one pin that names them all.
      const groups = new Map();
      for (const mk of markers) {
        const k = `${mk.lat.toFixed(3)},${mk.lon.toFixed(3)}`;
        if (!groups.has(k)) groups.set(k, []);
        groups.get(k).push(mk);
      }
      const labelled = groups.size <= 8;
      const driving = to >= now - 60000 && lastThree.length > 0 && lastThree[0].end_ms == null;
      const day = (ms) => formatDate(ms, tz, { now, weekday: false });
      const RANK = ['start', 'end', 'charge', 'overnight'];
      for (const list of groups.values()) {
        const kinds = new Set(list.map((x) => x.kind));
        const words = [];
        if (kinds.has('start')) words.push('Start');
        if (kinds.has('end')) words.push(driving ? 'Now' : 'End');
        const ch = list.filter((x) => x.kind === 'charge'), nt = list.filter((x) => x.kind === 'overnight');
        if (labelled && ch.length) words.push(ch.length === 1 ? `Charge, ${day(ch[0].t_ms)}` : `${ch.length} charges, ${day(ch[0].t_ms)} to ${day(ch[ch.length - 1].t_ms)}`);
        if (labelled && nt.length) words.push(nt.length === 1 ? `Night, ${day(nt[0].t_ms)}` : `${nt.length} nights`);
        const top = RANK.find((k) => kinds.has(k));
        m.addPin([list[0].lon, list[0].lat], words.join(' · ') || null, `pin-${top === 'overnight' ? 'overnight' : top}`);
      }
      dotEl = h('div', { class: 'car-dot is-hidden' });
      dot = new m.ml.Marker({ element: dotEl, anchor: 'center' }).setLngLat(allPts[0]).addTo(m.map);
    }
  } else {
    mapBox.classList.add('is-empty');
    replace(mapBox, h('p', { class: 'map-note' }, 'No drive and no charge in this window, so there is nothing to draw.'));
  }
  const counts = { charge: markers.filter((x) => x.kind === 'charge').length, overnight: markers.filter((x) => x.kind === 'overnight').length };
  replace(mapNote, tracksErr ? 'The routes could not be loaded; the pins still show the start, the end and the stops. '
    : nDrives && !tracks.length ? 'The drives of this window carry no positions (imported history keeps none). ' : null,
  nDrives ? `${tn(tracks.length, '{n} route', '{n} routes')}, ${tn(counts.charge, '{n} charge', '{n} charges')}, ${tn(counts.overnight, '{n} night away', '{n} nights away')}. ` : null,
  markers.length && markers[0].kind === 'start' ? `Started at ${coordsText([markers[0].lon, markers[0].lat])}.` : null);

  /* ------------------------------------------------------------ charts on one time axis */
  if (s && s.t_ms.length > 1) {
    const V = s.values;
    const rf = rangeField('range_km');
    const rangeVals = V[rf] && has(V[rf]) ? V[rf] : V.range_km;
    const at = (f, i) => (i != null && V[f] ? V[f][i] : null);
    const showReadout = (i) => {
      if (i == null) { replace(readout, h('span', { class: 'readout-idle' }, `${formatNumber(s.t_ms.length)} readings over the trip`)); return; }
      const st = s.state ? s.state[i] : null;
      // Asleep: the value is held, as the charts draw it; take the last earlier reading, never across unknown time.
      const read = (vals) => {
        const v = vals ? vals[i] : null;
        if (v != null || !vals) return { v, why: null };
        if (st !== 'asleep') return { v: null, why: 'not recorded' };
        for (let j = i - 1; j >= 0; j--) {
          if (s.state && s.state[j] === 'unknown') return { v: null, why: 'asleep since before a gap' };
          if (vals[j] != null) return { v: vals[j], why: 'held while asleep' };
        }
        return { v: null, why: 'asleep since before this window' };
      };
      const part = (name, vals, kind, opts) => {
        const r = read(vals);
        return h('span', null, `${name} `, h('b', null, format(r.v, kind, opts)), r.why ? h('span', { class: 'readout-held' }, ` ${r.why}`) : null);
      };
      replace(readout, h('span', { class: 'readout-time' }, `${formatDate(s.t_ms[i], tz, { now, weekday: false })}, ${formatTime(s.t_ms[i], tz)}`),
        st ? h('span', null, st === 'unknown' ? 'not recorded' : st) : null,
        part('Battery', V.soc_pct, 'percent', { digits: 1 }),
        part(rangeLabel(), rangeVals, 'range'),
        part('Elevation', V.elevation_m, 'length'));
    };
    showReadout(null);
    const onHover = (i) => {
      showReadout(i);
      if (!dot) return;
      const lat = at('lat', i), lon = at('lon', i);
      if (lat == null || lon == null) { dotEl.classList.add('is-hidden'); return; }
      dotEl.classList.remove('is-hidden');
      dot.setLngLat([lon, lat]);
    };
    let plotted = null;
    const onZoom = (a, b) => {
      const full = a <= s.t_ms[0] + 1000 && b >= s.t_ms[s.t_ms.length - 1] - 1000;
      if (full) {
        replace(zoomBar, h('p', { class: 'zoom-idle' }, 'Point at a chart to read all three at that moment and see where the car was. Drag across a chart to pick a stretch; you can then make it the trip.'));
        return;
      }
      const a2 = Math.max(first, Math.round(a / 60000) * 60000), b2 = Math.min(to, Math.round(b / 60000) * 60000);
      replace(zoomBar, h('p', { class: 'zoom-text' }, 'Picked ', h('b', null, `${formatDate(a2, tz, { now, weekday: false })}, ${formatTime(a2, tz)}`), ' to ', h('b', null, `${formatDate(b2, tz, { now, weekday: false })}, ${formatTime(b2, tz)}`), ` (${formatDuration(b2 - a2)}).`),
        h('a', { class: 'button button-small', href: asTrip(a2, b2) }, 'Make this the trip', icon('arrowRight')),
        button('Show the whole trip', { iconName: 'reset', cls: 'button-small', onclick: () => plotted && plotted.resetZoom() }));
    };
    const rows = [
      { key: 'soc', name: 'Battery', kind: 'percent', values: V.soc_pct, colour: '--accent', fill: '--chart-wash', floor: 0, ticks: 4, note: 'charge level' },
      { key: 'range', name: rangeLabel(), kind: 'range', values: rangeVals, colour: '--ink-2', ticks: 4, note: 'as the car shows it' },
      { key: 'elev', name: 'Elevation', kind: 'length', values: V.elevation_m, colour: '--ink-3', fill: '--chart-sunk', ticks: 4, note: has(V.elevation_m) ? 'the car’s own reading' : 'no elevation recorded' },
    ];
    plotted = await stackedCharts(ctx, charts, { tz, t_ms: s.t_ms, states: s.state, rows, onHover, onZoom });
    if (!ctx.alive()) return {};
    charts.append(h('p', { class: 'note chart-foot' }, 'Lavender: asleep, the value is held (nothing changes while the car sleeps). Red: not recorded, the line breaks and nothing is carried across.'));
    seriesCsv.addEventListener('click', () => downloadCsv(csvFilename(car.name, 'trip', dayKey(first || now, tz), 'battery and elevation'), s.t_ms.map((x, i) => ({ x, i })), [
      timeColumn('Time', 'x', tz), { label: 'State', value: (r) => (s.state ? s.state[r.i] : null) },
      column('Battery', (r) => at('soc_pct', r.i), 'percent', { digits: 2 }), column(rangeLabel(), (r) => (rangeVals ? rangeVals[r.i] : null), 'range', { digits: 2 }),
      column('Elevation', (r) => at('elevation_m', r.i), 'length', { digits: 1 }),
      { label: 'Latitude', value: (r) => at('lat', r.i) }, { label: 'Longitude', value: (r) => at('lon', r.i) },
    ]));
  } else if (seriesR.status === 'rejected') {
    replace(charts, errorView(seriesR.reason, { eyebrow: 'Readings', retry: () => remount() }));
  } else {
    replace(charts, empty({ title: 'No readings in this window.', text: 'The car sent nothing, or the history here is imported (which keeps only totals). The Data health page shows whether logs arrived.' }));
  }

  await tablesLoaded;
  if (m) await Promise.race([m.ready, new Promise((r) => setTimeout(r, 5000))]);
  if (!ctx.alive()) return {};
  ctx.ready();
  return {};
}
