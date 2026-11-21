// Software (/cars/{car}/software): the version a car runs now, its update history in the period as a
// version table (version, installed at, days on it), and the version of every car.
// - A version is only ever what someone said: imported, typed by an admin, or the same module builds as an
//   update with a known version. An update seen only as changed module builds says "build changed".
// - GET /cars/{car}/updates (period, newest first, paged, summary in each page), PATCH .../updates/{id}
//   (admin: type or forget a version), GET /fleet/versions (every car, searched, paged).
// - Live: "rows" events with kind "updates" load the lists again.

import * as state from '../shared/state.js';
import { h, replace, cover } from '../shared/dom.js';
import { formatDate, formatTime, formatDuration, HOUR, DAY } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { head, section, kv, note, link, btn } from '../core/ui.js';
import { num, tag, dateTime, DASH } from '../core/fmt.js';
import { table } from '../core/table.js';
import { empty } from '../core/states.js';
import { field, form } from '../core/form.js';
import { openDialog } from '../core/dialog.js';
import { toast } from '../core/toast.js';
import { src, srcText } from '../core/source.js';

export const id = 'software';
export const title = () => 'Software';

const enc = encodeURIComponent;
const SOURCE = { teslamate: 'imported', tesla_api: 'tesla api', owner: 'typed', build_match: 'same build' };
const FOUND = { ota_status: 'update status', build_change: 'module builds', teslamate: 'imported', tesla_api: 'tesla api' };
const MODULES = { bms: 'bms', vcfront: 'front body', di_front: 'front motor', di_rear: 'rear motor', fc: 'fast charge' };
const mods = (list) => (list || []).map((m) => MODULES[m] || m).join(', ');
const none = (t) => h('span', { class: 'muted' }, t);
const VERSION_RX = /^\d{4}\.\d{1,2}(\.\d{1,3}){0,2}$/;

export async function mount(ctx) {
  const { car, tz } = ctx;
  const cid = car.id;
  const path = `cars/${enc(cid)}/updates`;
  const settings = await ctx.api.get('settings', null, { maxAgeMs: 300000 }).catch(() => null);
  if (!ctx.alive()) return;
  const notesOn = !!settings?.outside_links?.release_notes;
  const now = () => ctx.api.now();

  const hd = head(ctx);
  cover(hd, 'updates.section.car_header');
  const [periodBtn, carBtn] = hd.querySelectorAll('.ph-btn');
  if (periodBtn) cover(periodBtn, 'updates.filter.time_range');
  if (carBtn) cover(carBtn, 'updates.filter.car');

  const reloadBtn = btn('[reload]', () => reloadAll(true), { kind: 'line', cover: 'updates.control.refresh', title: 'load the figures and both lists again' });
  const nowSlot = h('div');

  function paintNow(sm, rows) {
    sm = sm || {};
    const installing = rows.find((r) => r.end_ms == null);
    const v = sm.current_version;
    const changed = sm.build_changed_since != null;
    replace(nowSlot, kv([
      ['version', changed ? [h('span', { class: 'tone--warn' }, 'not named yet'), none(v ? `  last named ${v}` : '')] : v ? [h('b', null, v), SOURCE[sm.current_version_source] ? [' ', tag(SOURCE[sm.current_version_source], 'muted')] : null] : none('not known')],
      ['since', changed ? dateTime(sm.build_changed_since, tz) : sm.current_version_since_ms != null ? dateTime(sm.current_version_since_ms, tz) : DASH],
      ['updates', h('span', { cover: 'updates.stat.count' }, formatNumber(sm.count ?? 0), none(' in this range'))],
      ['typical gap', h('span', { cover: 'updates.stat.median_gap' }, sm.median_gap_ms != null ? formatDuration(sm.median_gap_ms) : none('needs two updates')), { }],
      installing ? ['installing', [tag('running', 'warn'), ` since ${formatTime(installing.start_ms, tz)}`], { tone: 'warn' }] : null,
      ['state', changed ? [tag('warn', 'warn'), ' the module builds changed; nobody has said which version it is'] : [tag('ok', 'good'), ' the version is known']],
    ]),
    note('the car does not send its version text: a version is imported, typed by an admin, or taken from an update with the same builds. never guessed.'),
    h('p', { class: 'note', cover: 'projects.telegram-bot' }, 'a "software changed" event goes out on mqtt and the events list when an update ends: ',
      link(state.buildUrl('settings-integrations'), 'settings/integrations'), '. a pending update is not on the can bus.'));
  }

  const timelineHref = (r) => state.buildUrl('timeline', { car: cid, period: { from: r.start_ms - HOUR, to: (r.end_ms ?? now()) + HOUR } });
  const onVersion = (r) => ({ from: r.end_ms ?? r.start_ms, to: r.until_ms ?? now() });
  const daysOn = (r) => (r.end_ms == null ? null : ((r.until_ms ?? now()) - r.end_ms) / DAY);
  const notesHref = (v) => `https://www.notateslaapp.com/software-updates/version/${enc(v)}/release-notes`;

  const versionCell = (r) => h('span', null,
    r.version
      ? (notesOn && VERSION_RX.test(r.version)
        ? h('a', { class: 'lnk', href: notesHref(r.version), target: '_blank', rel: 'noopener noreferrer external', title: 'release notes on an outside site' }, r.version)
        : h('b', null, r.version))
      : h('span', { class: 'tone--warn' }, 'build changed'),
    r.version && SOURCE[r.version_source] ? [' ', tag(SOURCE[r.version_source], 'muted')] : null,
    ctx.admin && r.source === 'can' ? [' ', h('button', { class: 'btn btn--line tone--accent', type: 'button', onclick: () => editVersion(r) }, r.version ? '[edit]' : '[type it]')] : null);

  let painted = false;
  const updates = table(ctx, {
    title: 'versions', path, params: ctx.params, limit: 50, cover: 'updates.table', csvName: 'software updates',
    side: reloadBtn,
    note: 'newest first. days on it, charges and full range are what the car did from the end of the update to the next one (or now).',
    columns: [
      { key: 'version', label: 'version', ph: 'main', cover: 'updates.table.version', grow: true, render: versionCell },
      { key: 'start', label: 'installed at', ph: 'sub', cover: 'updates.table.date',
        render: (r) => h('a', { class: 'lnk-plain', href: timelineHref(r), title: 'the timeline around it' }, `${formatDate(r.start_ms, tz, { weekday: false })} ${formatTime(r.start_ms, tz)}`) },
      { key: 'took', label: 'took', num: true, cover: 'updates.table.duration',
        render: (r) => (r.end_ms == null ? [tag('running', 'warn'), ` ${formatDuration(r.duration_ms)}`] : formatDuration(r.duration_ms)) },
      { key: 'days', label: 'days on it', num: true, ph: 'num',
        render: (r) => (daysOn(r) == null ? none('installing') : h('span', { class: 'n' }, formatNumber(daysOn(r), { maximumFractionDigits: 0 }), h('span', { class: 'u' }, ' d'))) },
      { key: 'since', label: 'after prev', num: true, cover: 'updates.table.since_previous',
        render: (r) => (r.since_previous_ms == null ? none('first') : h('span', { class: 'n' }, formatNumber(r.since_previous_ms / DAY, { maximumFractionDigits: 0 }), h('span', { class: 'u' }, ' d'))) },
      { key: 'found', label: 'found by', render: (r) => FOUND[r.detected_by] || r.detected_by },
      { key: 'mods', label: 'changed modules', render: (r) => (r.changed_modules?.length ? mods(r.changed_modules) : none('none listed')) },
      { key: 'charges', label: 'charges', num: true, cover: 'updates.table.charge_count',
        render: (r) => (r.charges ? link(state.buildUrl('charges', { car: cid, period: onVersion(r) }), formatNumber(r.charges)) : formatNumber(r.charges ?? 0)) },
      { key: 'range', label: 'range 100%', num: true, cover: 'updates.table.avg_range', render: (r) => num(r.full_range_km, 'range') },
      { key: 'pack', label: 'full pack', num: true, render: (r) => (r.full_pack_kwh == null ? none('not measured') : num(r.full_pack_kwh, 'energy')) },
      { key: 'from', label: 'data', title: 'where the data came from: smo, api (the Tesla API), imp (imported)', render: (r) => src(r), value: (r) => srcText(r) },
    ],
    empty: () => empty('no updates in this range', ctx.period.preset === 'all' ? 'an update shows once the car installs one while recording, or with an imported history' : 'choose a longer range; all shows every update'),
    onPage: (pg) => { if (!ctx.alive()) return; painted = true; paintNow(pg.summary, updates.rows()); },
  });
  const notesLine = h('p', { class: 'note', cover: 'updates.link.release_notes' }, notesOn
    ? 'a known version links to its release notes on an outside site; nothing is sent until you open one.'
    : ['release-note links are off, so nothing leaves this server. ', ctx.admin ? 'turn them on in ' : 'an admin can turn them on in ', link(state.buildUrl('settings-display'), 'settings/display'), '.']);

  const search = h('input', { class: 'in in--search', type: 'search', placeholder: 'car or version', 'aria-label': 'Find a car or version', spellcheck: 'false' });
  const fleet = table(ctx, {
    title: 'every car', path: 'fleet/versions', params: {}, limit: 50, csvName: 'versions of every car',
    side: h('label', { class: 'filt filt--inline' }, h('span', { class: 'filt-p' }, 'find>'), search),
    columns: [
      { key: 'car', label: 'car', ph: 'main', grow: true, render: (r) => link(state.buildUrl('software', { car: r.car, period: state.current().period }), r.name || r.car) },
      { key: 'version', label: 'version now', ph: 'sub', render: (r) => (r.build_changed_since != null ? [h('span', { class: 'tone--warn' }, 'not named yet'), none(r.version ? ` last ${r.version}` : '')] : r.version || none('not known')) },
      { key: 'changed', label: 'changed since', render: (r) => (r.build_changed_since != null ? h('span', { class: 'tone--warn' }, dateTime(r.build_changed_since, tz)) : none('no change')) },
      { key: 'last', label: 'last update', num: true, ph: 'num', render: (r) => (r.last_update_ms != null ? formatDate(r.last_update_ms, tz, { weekday: false }) : none('none')) },
    ],
    empty: () => empty(search.value ? `no car or version holds "${search.value}"` : 'no cars yet'),
  });
  let searchT = 0;
  ctx.on(search, 'input', () => { clearTimeout(searchT); searchT = setTimeout(() => { if (ctx.alive()) fleet.reload({ search: search.value.trim() || undefined }); }, 250); });
  ctx.onCleanup(() => clearTimeout(searchT));

  replace(ctx.view, h('div', { class: 'page', dataset: { page: 'software' }, cover: 'shots.updates' },
    hd, section({ title: 'now' }, nowSlot), updates.el, notesLine, fleet.el));
  paintNow(null, []);

  async function editVersion(r) {
    const input = field({ label: 'version', name: 'version', value: r.version || '',
      hint: 'as the car screen shows it, e.g. 2026.38.4. empty forgets a typed version. cars on the same build get it too.',
      attrs: { pattern: '\\d{4}\\.\\d{1,2}(\\.\\d{1,3}){0,2}', autocomplete: 'off', spellcheck: 'false', size: '14' } });
    let dlg = null;
    const f = form({ fields: [input], submit: 'save', async onSubmit(v) {
      const s = String(v.version || '').trim();
      await ctx.api.patch(`${path}/${enc(r.id)}`, { version: s || null });
      toast(s ? `saved: this update installed ${s}` : 'the typed version is forgotten', { kind: 'ok' });
      dlg?.close();
      echoUntil = Date.now() + 5000;
      reloadAll(false);
    } });
    dlg = openDialog({ title: `update of ${formatDate(r.start_ms, tz, { weekday: false })}`, body: [note(r.changed_modules?.length ? `changed modules: ${mods(r.changed_modules)}` : 'no module list for this update'), f.el] });
    input.input.focus();
  }

  let echoUntil = 0;
  async function reloadAll(say) {
    reloadBtn.disabled = true;
    try { await Promise.all([updates.reload(), fleet.reload()]); if (say && ctx.alive()) toast('loaded again', { kind: 'info' }); } catch { /* the tables say why */ } finally { reloadBtn.disabled = false; }
  }
  ctx.events('rows', (ev) => {
    const d = ev?.data || {};
    if (!(d.kinds || []).includes('updates')) return;
    if (Date.now() < echoUntil) { echoUntil = 0; return; }
    if (d.car === cid) updates.reload(null, { quiet: true });
    fleet.reload(null, { quiet: true });
  });

  try { await Promise.all([updates.ready, fleet.ready]); } catch (e) { if (!painted) { ctx.fail(e); return; } }
  if (!ctx.alive()) return;
  ctx.ready();
}
