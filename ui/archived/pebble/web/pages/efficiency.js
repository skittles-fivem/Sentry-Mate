// Sentry Mate, "Pebble" UI pack: Efficiency (/cars/{car}/efficiency), how much energy driving takes, how the
// outside temperature and the speed change it, and the temperatures themselves.
//
// A pill row picks the shortest drive that counts. Then a big number tile of the average (net), the other headline
// figures, a bar tile of use by outside temperature (with its groups as rows), a bar tile of use by average speed
// (worked out here from the drives of the period), the energy factor, and the temperatures: lowest, average and
// highest with how much of the period was read, the temperatures over time and at each charge (one shared cursor),
// and each day outside as a small range bar.
// Data: GET /api/v1/cars/{car}/efficiency, /temperatures, /series, /drives, /charges.

import { h, holdScroll } from '../shared/dom.js';
import { format, convert, toMetric, unitLabel, getUnits } from '../shared/units.js';
import { formatDate, formatDayKey } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { tile, num, note } from '../core/ui.js';
import { emptyTile, ghostTile, plotGhost } from '../core/states.js';
import { timeChart, bars } from '../core/chart.js';
import { filterBar, pills, sectionHead, nums, statTile } from '../core/bits.js';
import { onRows } from '../shared/live.js';

export const id = 'efficiency';
export const css = true;
export const shape = [['full', 'h1'], [2, 'h2'], [2, 'h2'], [2, 'h3'], [2, 'h3']];
export const title = () => 'Efficiency';

const DAY = 864e5;
const n0 = (v) => (v == null ? '—' : formatNumber(v, { maximumFractionDigits: 0 }));
const pct = (v, d = 0) => format(v, 'percent', { digits: d });
const DIST = [1, 5, 10, 25];
const r3 = (v) => Math.round(v * 1000) / 1000;

export async function mount(ctx) {
  const car = ctx.car, tz = ctx.tz, period = ctx.period;
  const base = api.apiPath`cars/${car.id}`;
  const units = getUnits();
  const fahr = units.temperature === 'f';
  const rangeWord = units.range === 'ideal' ? 'ideal' : 'rated';
  const dUnit = unitLabel('distance');
  const tUnit = unitLabel('temperature');
  const groupC = fahr ? r3(toMetric(10, 'temperature_delta')) : 5;
  let minD = DIST.includes(Number(ctx.query.min_d)) ? Number(ctx.query.min_d) : 1;
  const minKm = () => r3(toMetric(minD, 'distance'));
  const effParams = () => ({ ...ctx.params, min_distance_km: minKm(), temp_group_c: groupC });

  // Every call starts at once and each part paints when its own answers are in; grey tiles keep their room until
  // then. The drives (for use by speed) come with the efficiency tiles, so those are all there at once.
  const drivesOf = () => ctx.api.list(`${base}/drives`, { ...ctx.params, min_distance_km: minKm(), sort: 'start', dir: 'desc' }, { limit: 200 }).all({ max: 3000 }).catch(() => []);
  const effP = ctx.api.get(`${base}/efficiency`, effParams());
  const tempsP = ctx.api.get(`${base}/temperatures`, ctx.params).catch((e) => ({ error: e }));
  const drivesP = drivesOf();
  let temps = {};
  const now = api.now();
  let fromMs = period.from_ms ?? now - 30 * DAY;
  const toMs = Math.min(period.to_ms ?? now, now);

  /* ------------------------------------------------------------ the filter row */
  const distPills = pills({ label: 'Shortest drive that counts', value: String(minD), cover: 'ef.filter.min_distance',
    options: DIST.map((v) => [String(v), v === 1 ? `Every drive (${v} ${dUnit}+)` : `${v} ${dUnit} or more`]),
    onchange: (v) => { minD = Number(v); ctx.setQuery({ min_d: minD === 1 ? null : String(minD) }); refresh(); } });
  const bar = filterBar(ctx, { carCover: ['ef.section.per_car', 'tp.filter.car'], periodCover: ['ef.filter.time_range', 'tp.filter.time_range'] }, distPills);

  const effBox = h('div', { class: 'contents' });
  const tempHead = sectionHead('Temperatures', 'What the car read outside, in the cabin and in the pack. Point at one time chart and the other follows.', { cover: ['tp.sections', 'tp.control.shared_crosshair'] });
  const sumT = tile({ w: 2, title: 'Lowest, average, highest', icon: 'thermo', cls: 'tp-sum', cover: 'shots.temperatures' });
  const histT = tile({ w: 'full', title: 'Over time', icon: 'timeline', cls: 'chart-tile' });
  const chgT = tile({ w: 2, title: 'Outside at each charge', icon: 'bolt', cls: 'chart-tile' });
  const dayT = tile({ w: 2, title: 'Each day outside', icon: 'calendar', cls: 'chart-tile', cover: ['tp.chart.daily_envelope', 'tp.chart.daily_coverage'] });
  effBox.append(ghostTile(2, 'h3'), ghostTile(2, 'h2'), ghostTile(2, 'h3'), ghostTile(2, 'h3'));
  sumT.append(plotGhost({ short: true }));
  for (const t of [histT, chgT, dayT]) t.append(plotGhost({ short: t !== histT }));
  const unghost = (t) => t.querySelector(':scope > .plot-ghost')?.remove();
  ctx.view.replaceChildren(bar, effBox, tempHead, sumT, histT, chgT, dayT);

  /* ------------------------------------------------------------ efficiency */
  async function refresh() {
    const [d, drives] = await Promise.all([ctx.api.get(`${base}/efficiency`, effParams()).catch((e) => ({ error: e })), drivesOf()]);
    if (!ctx.alive()) return;
    // what is on screen stays where it is while the parts above it are drawn again (shared/dom.js)
    holdScroll(ctx.view);
    paintEff(d, drives);
    ctx.ready();
  }

  function paintEff(d, drives) {
    if (d.error) { effBox.replaceChildren(emptyTile({ icon: 'leaf', title: 'Efficiency could not be read', text: 'Try again in a moment.' })); return; }
    const minText = format(minKm(), 'distance', { digits: 0 });
    const why = d.drives ? 'too little distance' : `no drive of ${minText} or more`;
    const all = d.all_drives || { drives: d.drives, distance_km: d.distance_km, energy_kwh: d.energy_kwh };
    const used = (all.energy_kwh || 0) + (d.parked_kwh || 0);
    const parkedShare = used > 0 ? (100 * (d.parked_kwh || 0)) / used : null;

    const avgT = tile({ w: 2, title: 'Average use', icon: 'leaf', cls: 'ef-avg', cover: 'shots.efficiency' },
      h('div', { class: 'ef-avg-top' },
        h('div', { class: 'ef-avg-main', cover: 'ef.stat.consumption_net' }, h('span', { class: 'hero-k' }, 'Net, driving only'), num(d.net_wh_per_km, 'efficiency', { size: 'xl', why })),
        nums([
          ['Gross, with parked use', num(d.gross_wh_per_km, 'efficiency', { size: 'm', why }), 'ef.stat.consumption_gross'],
          ['Logged distance', num(d.distance_km, 'distance', { size: 'm' }), 'ef.stat.logged_distance'],
          ['Used while parked', num(parkedShare, 'percent', { size: 'm', why: 'nothing used' })],
          ['Drives counted', h('span', { class: 'num num-m' }, h('span', { class: 'num-n' }, n0(d.drives)))],
        ])),
      d.estimated_drives ? note(`${n0(d.estimated_drives)} of the ${n0(d.drives)} drives have estimated energy: imported drives work it out from range, and drives only the Tesla API saw add up its power readings. Neither is measured.`) : null);
    const helpT = tile({ w: 2, title: 'What these numbers mean', icon: 'info', cls: 'ef-help' },
      h('p', { class: 'note', cover: 'ef.help.headline_stats' },
        `Net is the energy the pack gave while driving (braking energy already taken off) over the distance, for drives of ${minText} or more. Gross is what owning the car takes: every drive of the period plus all energy used parked (climate, Sentry, standby), so the pill above does not change it.`),
      h('p', { class: 'note', cover: 'faq.no-consumption' },
        'Nothing to wait for: use is measured by the pack from the first drive, with no charges needed to set it up. A dash means no drive in the period was long enough; pick a longer period or every drive.'));

    /* by outside temperature */
    const groups = (d.by_temperature || []).slice().sort((a, b) => a.temp_c - b.temp_c);
    const fac = rangeWord === 'ideal' ? d.factor?.ideal_wh_per_km : d.factor?.rated_wh_per_km;
    const ofRange = (g) => (fac && g.wh_per_km ? (fac / g.wh_per_km) * 100 : g.range_efficiency_pct ?? null);
    const gLabel = (g) => {
      const a = convert(g.temp_c, 'temperature'), b = convert(g.temp_to_c ?? g.temp_c + groupC, 'temperature');
      return `${formatNumber(Math.round(a))} to ${formatNumber(Math.round(b))}`;
    };
    const tempT = tile({ w: 2, title: 'By outside temperature', icon: 'thermo', cls: ['chart-tile', 'ef-bars-tile'], cover: 'ef.table.temperature' });
    if (!groups.length) tempT.append(emptyTile({ icon: 'thermo', title: 'No drives to group', text: d.drives ? 'None of the drives has an outside temperature.' : `No drive of ${minText} or more in this period.`, w: 2 }));
    else {
      tempT.append(
        h('p', { class: 'chart-note' }, `${unitLabel('efficiency')} per group of ${fahr ? 10 : 5} ${tUnit}. Lower is better.`),
        bars(groups.map((g) => ({ label: gLabel(g), value: g.wh_per_km || 0, text: n0(g.wh_per_km == null ? null : convert(g.wh_per_km, 'efficiency')), title: `${gLabel(g)} ${tUnit}: ${format(g.wh_per_km, 'efficiency')}` })), { tone: 'c1', label: 'Use by outside temperature' }),
        h('div', { class: 'ef-rows', role: 'table', 'aria-label': 'Use by outside temperature' },
          h('div', { class: 'ef-row ef-row-head', role: 'row' },
            h('span', { role: 'columnheader', cover: 'ef.temp.col.temperature' }, `Outside (${tUnit})`),
            h('span', { role: 'columnheader', cover: 'ef.temp.col.consumption' }, 'Use'),
            h('span', { role: 'columnheader', cover: 'ef.temp.col.efficiency', title: `Distance driven per ${unitLabel('range')} of ${rangeWord} range used` }, `Of ${rangeWord}`),
            h('span', { role: 'columnheader', cover: 'ef.temp.col.distance' }, 'Distance'),
            h('span', { role: 'columnheader', cover: 'ef.temp.col.speed' }, 'Speed')),
          groups.slice().reverse().map((g) => h('div', { class: 'ef-row', role: 'row' },
            h('span', { role: 'cell' }, h('a', { href: state.buildUrl('drives', { car: car.id, period: ctx.period.preset === 'custom' ? { from: fromMs, to: toMs } : { preset: ctx.period.preset } }), title: `${n0(g.drives)} drives` }, gLabel(g))),
            h('span', { role: 'cell' }, format(g.wh_per_km, 'efficiency')),
            h('span', { role: 'cell', class: ofRange(g) != null && ofRange(g) < 80 ? 'is-low' : null }, pct(ofRange(g))),
            h('span', { role: 'cell' }, format(g.distance_km, 'distance')),
            h('span', { role: 'cell' }, format(g.avg_kph, 'speed'))))),
        ...(d.no_temperature?.drives ? [note(`${n0(d.no_temperature.drives)} drives (${format(d.no_temperature.distance_km, 'distance')}) had no outside temperature: in the figures above, in no group.`)] : []));
    }

    /* by average speed, from the drives */
    const speedT = tile({ w: 2, title: 'By average speed', icon: 'gauge', cls: ['chart-tile', 'ef-bars-tile'] });
    const step = 20; // in the person's speed unit
    const bins = new Map();
    for (const x of drives) {
      if (x.speed_avg_kph == null || !x.distance_km || x.energy_kwh == null) continue;
      const k = Math.min(6, Math.floor(convert(x.speed_avg_kph, 'speed') / step));
      const b = bins.get(k) || { k, kwh: 0, km: 0, n: 0 };
      b.kwh += x.energy_kwh; b.km += x.distance_km; b.n += 1;
      bins.set(k, b);
    }
    const sb = [...bins.values()].sort((a, b) => a.k - b.k).map((b) => ({ ...b, wh: b.km > 0 ? (b.kwh * 1000) / b.km : null }));
    const sLabel = (b) => (b.k >= 6 ? `${step * 6}+` : `${step * b.k} to ${step * (b.k + 1)}`);
    if (!sb.length) speedT.append(emptyTile({ icon: 'gauge', title: 'No drives this period', text: `Drives of ${minText} or more with energy show here.`, w: 2 }));
    else {
      speedT.append(
        h('p', { class: 'chart-note' }, `${unitLabel('efficiency')} by the drive’s average speed (${unitLabel('speed')}), stops included.`),
        bars(sb.map((b) => ({ label: sLabel(b), value: b.wh || 0, text: n0(b.wh == null ? null : convert(b.wh, 'efficiency')), title: `${sLabel(b)} ${unitLabel('speed')}: ${format(b.wh, 'efficiency')}, ${b.n} drives` })), { tone: 'c2', label: 'Use by average speed' }),
        h('ul', { class: 'ef-speed-key' }, sb.map((b) => h('li', null, h('b', null, `${sLabel(b)} ${unitLabel('speed')}`), `${n0(b.n)} ${b.n === 1 ? 'drive' : 'drives'} · ${format(b.km, 'distance')}`))),
        note('Worked out here from each drive’s energy and distance. Fast drives use more: air drag grows with speed.'));
    }

    /* the energy factor */
    const f = d.factor || {};
    const owner = f.source === 'owner';
    const facT = tile({ w: 2, title: 'Energy factor', icon: 'battery', cls: 'ef-factor' },
      h('div', { cover: 'ef.stat.current_factor' }, h('span', { class: 'hero-k' }, `Per ${unitLabel('range')} of ${rangeWord} range`), num(fac, 'efficiency', { size: 'l', why: 'not worked out yet' })),
      h('ul', { class: 'ef-fac' },
        h('li', { cover: 'ef.table.derived_rated', class: rangeWord === 'rated' ? 'is-mine' : null },
          h('span', null, 'Rated'), h('b', { cover: 'ef.rated.col.efficiency' }, format(f.rated_wh_per_km, 'efficiency')),
          h('span', { class: 'ef-fac-n', cover: 'ef.rated.col.count' }, owner ? 'typed by the owner' : `${n0(f.readings)} readings`)),
        h('li', { cover: 'ef.table.derived_ideal', class: rangeWord === 'ideal' ? 'is-mine' : null },
          h('span', null, 'Ideal'), h('b', { cover: 'ef.ideal.col.efficiency' }, format(f.ideal_wh_per_km, 'efficiency')),
          h('span', { class: 'ef-fac-n', cover: 'ef.ideal.col.count' }, `${n0(f.readings)} readings`))),
      note(owner ? 'Typed by the owner in car settings; it replaces the worked-out value.' : `What one ${unitLabel('range')} of range on the car’s screen stands for: energy and range read in the same second, over the last 14 days with readings. Only a cross-check; use above is measured.`));
    effBox.replaceChildren(avgT, helpT, tempT, speedT, facT,
      statTile({ label: 'Energy used driving', value: d.energy_kwh, kind: 'energy', icon: 'bolt', sub: `plus ${format(d.parked_kwh, 'energy')} parked` }),
      statTile({ label: 'Drives without energy', value: d.without_energy ?? 0, kind: 'count', icon: 'route', sub: 'left out of the use figures' }));
  }

  /* ------------------------------------------------------------ temperatures: the summary */
  function paintSummary() {
    unghost(sumT);
    if (temps.error) { sumT.append(note('The temperatures could not be read. Reload the page to try again.')); return; }
    const miss = temps.missing_ms || {};
    const span = Math.max(1, toMs - fromMs);
    const col = (key, name, cv, why) => {
      const t = temps[key] || {};
      const none = t.avg_c == null;
      const cov = h('span', { class: 'cov-fill' });
      if (t.coverage_pct > 0) cov.style.setProperty('--v', String(Math.min(1, t.coverage_pct / 100))); else cov.hidden = true;
      return h('div', { class: ['tp-col', `tp-${key}`] },
        h('h3', { class: 'tp-name' }, h('span', { class: 'tp-sw', 'aria-hidden': 'true' }), name),
        h('div', { cover: cv.avg || null }, h('span', { class: 'hero-k' }, 'Average'), num(t.avg_c, 'temperature', { size: 'l', why: none ? why : 'no reading' })),
        h('div', { class: 'tp-lohi' },
          h('div', { cover: cv.min || null }, h('span', { class: 'hero-k' }, 'Lowest'), num(t.min_c, 'temperature', { size: 's', why: none ? '' : 'no reading' })),
          h('div', { cover: cv.max || null }, h('span', { class: 'hero-k' }, 'Highest'), num(t.max_c, 'temperature', { size: 's', why: none ? '' : 'no reading' }))),
        h('div', { class: 'tp-cov', cover: cv.cov || null, title: 'Share of the period with readings: the car reads only while awake' },
          h('span', { class: 'cov', 'aria-hidden': 'true' }, cov), h('span', { class: 'tp-cov-t' }, `${none ? '0 %' : pct(t.coverage_pct, 1)} read`)));
    };
    sumT.append(
      h('div', { class: 'tp-cols' },
        col('outside', 'Outside', { min: 'tp.outside.min', avg: 'tp.outside.avg', max: 'tp.outside.max', cov: 'tp.outside.coverage' }, 'no reading'),
        col('cabin', 'Cabin', { min: 'tp.inside.min', avg: 'tp.inside.avg', max: 'tp.inside.max', cov: 'tp.inside.coverage' }, 'not sent'),
        col('battery', 'Battery', {}, 'no reading')),
      h('p', { class: 'tile-line' }, `Asleep ${pct((100 * (miss.asleep || 0)) / span, 1)} of the period, unknown ${pct((100 * (miss.unknown || 0)) / span, 1)}.`),
      h('p', { class: 'note', cover: 'tp.summary.help' }, 'Lowest and highest are single readings. The average is the mean of each day’s mean, so one long drive does not outweigh quiet days. A low read share means the figures describe only the times the car was in use.'));
  }

  /* ------------------------------------------------------------ temperatures over time */
  const sync = `tp-${car.id}-${Date.now()}`;
  async function paintHistory() {
    const d = await ctx.api.get(`${base}/series`, { fields: 'outside_c,cabin_c,battery_c,setpoint_left_c,climate', ...ctx.params, from_ms: fromMs, to_ms: toMs, max_points: 800 }).catch((e) => ({ error: e }));
    if (!ctx.alive()) return;
    unghost(histT);
    const ts = d.t_ms || [], v = d.values || {};
    const climate = v.climate || [];
    const set = (v.setpoint_left_c || []).map((x, i) => (climate[i] === 'on' || climate[i] === true ? x : null));
    const any = ['outside_c', 'cabin_c', 'battery_c'].some((k) => (v[k] || []).some((x) => x != null));
    if (d.error || !ts.length || !any) {
      histT.append(h('div', { cover: ['tp.chart.outside', 'tp.chart.inside', 'tp.chart.setpoint'] }, emptyTile({ icon: 'thermo', title: 'No temperature readings in this period', text: 'The car was asleep or not logged the whole time. Pick a longer period.' })));
      return;
    }
    await timeChart(ctx, histT, { tz, t_ms: ts, sync, states: d.state, label: 'Temperatures over time', series: [
      { key: 'out', name: 'Outside', kind: 'temperature', values: v.outside_c, tone: 'c1', axis: 'left', signed: true, cover: 'tp.chart.outside' },
      { key: 'cab', name: 'Cabin', kind: 'temperature', values: v.cabin_c, tone: 'c2', dashed: true, axis: 'left', signed: true, cover: 'tp.chart.inside' },
      { key: 'bat', name: 'Battery', kind: 'temperature', values: v.battery_c, tone: 'c3', axis: 'left', signed: true },
      { key: 'set', name: 'Set point, climate on', kind: 'temperature', values: set, tone: 'c4', axis: 'left', signed: true, cover: 'tp.chart.setpoint' },
    ] });
    if (!ctx.alive()) return;
    histT.append(h('p', { class: 'chart-note' }, 'Grey shading: the car was asleep and read nothing. The set point shows only while climate was on. Drag to zoom, double-click to go back.'));
  }

  /* ------------------------------------------------------------ outside at each charge */
  async function paintCharges() {
    const items = await ctx.api.list(`${base}/charges`, { ...ctx.params, sort: 'start', dir: 'asc' }, { limit: 200 }).all({ max: 3000 }).catch(() => []);
    if (!ctx.alive()) return;
    unghost(chgT);
    const list = items.filter((c) => c.start_ms != null && c.outside_avg_c != null).sort((a, b) => a.start_ms - b.start_ms);
    if (!list.length) {
      chgT.append(h('div', { cover: 'tp.chart.charging_outside' }, emptyTile({ icon: 'bolt', title: items.length ? 'No charge has an outside temperature' : 'No charges this period', text: items.length ? 'The car sent no outside temperature while charging.' : 'Charges that start in the period show here.', w: 2 })));
      return;
    }
    const sv = list.map((c) => c.outside_avg_c);
    const mean = sv.reduce((s, x) => s + x, 0) / sv.length;
    await timeChart(ctx, chgT, { tz, t_ms: list.map((c) => c.start_ms), sync, short: true, label: 'Outside temperature at each charge', cover: 'tp.chart.charging_outside', series: [
      { key: 'o', name: 'Outside at the charge', kind: 'temperature', values: sv, tone: 'c3', points: true, signed: true },
    ] });
    if (!ctx.alive()) return;
    chgT.append(h('p', { class: 'chart-note' }, `${n0(list.length)} charges · lowest ${format(Math.min(...sv), 'temperature')} · average ${format(mean, 'temperature')} · highest ${format(Math.max(...sv), 'temperature')}. Cold charges are slower and spend energy warming the pack.`));
  }

  /* ------------------------------------------------------------ each day outside */
  function paintDaily() {
    unghost(dayT);
    const days = (temps.daily || []);
    const help = h('p', { class: 'note', cover: 'tp.chart.daily_envelope_help' }, 'The thin line runs from the day’s lowest to its highest reading; the box holds the middle 70 % of readings. The bar under each day is how much of it the car was awake: trust a day with a short bar less.');
    if (!days.some((x) => x.max_c != null)) {
      dayT.append(emptyTile({ icon: 'calendar', title: 'No day with an outside reading', text: 'The car was asleep or not logged the whole period.', w: 2 }), help);
      return;
    }
    const vals = days.flatMap((x) => [x.min_c, x.max_c]).filter((x) => x != null).map((x) => convert(x, 'temperature'));
    const lo = Math.floor(Math.min(...vals) / 5) * 5, hi = Math.ceil(Math.max(...vals) / 5) * 5 || lo + 5;
    const y = (c) => (c == null ? null : (convert(c, 'temperature') - lo) / Math.max(1, hi - lo));
    const read = h('p', { class: 'tile-line env-read' }, 'Point at a day for its figures.');
    const cols = days.map((x) => {
      const has = x.max_c != null;
      const el = h('span', { class: ['env-day', { 'is-empty': !has }], tabindex: has ? '0' : null, role: has ? 'img' : null,
        'aria-label': has ? `${formatDayKey(x.date)}: ${format(x.min_c, 'temperature')} to ${format(x.max_c, 'temperature')}` : null },
      h('span', { class: 'env-plot' }, has ? h('span', { class: 'env-whisk' }) : null, has && x.p15_c != null ? h('span', { class: 'env-box' }) : null),
      h('span', { class: 'env-cov' }, h('span', { class: 'env-cov-fill' })));
      if (has) {
        el.style.setProperty('--lo', y(x.min_c).toFixed(4)); el.style.setProperty('--hi', y(x.max_c).toFixed(4));
        if (x.p15_c != null) { el.style.setProperty('--blo', y(x.p15_c).toFixed(4)); el.style.setProperty('--bhi', y(x.p85_c ?? x.p15_c).toFixed(4)); }
        const words = `${formatDayKey(x.date)}: ${format(x.min_c, 'temperature')} to ${format(x.max_c, 'temperature')}, middle ${format(x.p15_c, 'temperature')} to ${format(x.p85_c, 'temperature')}, awake ${pct(x.coverage_pct)}`;
        const showIt = () => { read.textContent = words; };
        el.addEventListener('mouseenter', showIt); el.addEventListener('focus', showIt);
      }
      const cf = el.querySelector('.env-cov-fill');
      if (x.coverage_pct > 0) cf.style.setProperty('--v', String(Math.min(1, x.coverage_pct / 100))); else cf.remove();
      return el;
    });
    const first = days[0], last = days[days.length - 1];
    dayT.append(
      h('div', { class: 'env' },
        h('div', { class: 'env-axis', 'aria-hidden': 'true' }, h('span', null, `${formatNumber(hi)}°`), h('span', null, `${formatNumber(lo)}°`)),
        h('div', { class: 'env-days', role: 'group', 'aria-label': 'Each day: lowest to highest outside temperature' }, cols)),
      h('div', { class: 'env-x', 'aria-hidden': 'true' }, h('span', null, formatDate(first.t_ms ?? Date.parse(first.date), tz, { weekday: false })), h('span', null, formatDate(last.t_ms ?? Date.parse(last.date), tz, { weekday: false }))),
      read, help);
  }

  // the temperatures: the summary, each day, and the readings over time (an All time period starts at their first day)
  const paintTemps = async () => {
    temps = await tempsP;
    if (!ctx.alive()) return;
    if (period.from_ms == null) fromMs = temps.daily?.[0]?.t_ms ?? fromMs;
    paintSummary();
    paintDaily();
    await paintHistory();
  };
  // GET efficiency failing fails the page (as before)
  const paintFirst = async () => { const [eff, drives0] = await Promise.all([effP, drivesP]); if (ctx.alive()) paintEff(eff, drives0); };
  await Promise.all([paintFirst(), paintTemps(), paintCharges()]);
  if (!ctx.alive()) return {};
  ctx.ready();
  // a drive of the car started or ended (once a burst of `rows` has passed): the efficiency read again
  onRows(ctx.api, { car: car.id, waitMs: 2000, run: () => { if (ctx.alive()) refresh().catch(() => {}); } });
  return {};
}

