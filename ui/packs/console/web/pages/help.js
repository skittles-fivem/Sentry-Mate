// help: plain sans text for reading, a mono key list, the version and release notice, links, licences and
// notices, data credits, and short answers with a filter.
// API: GET about (versions and this build, licences, credits, links), release (the release check's last answer),
// notices (the release notice's notes link), settings (admins: release-notes links allowed, where maps and
// addresses come from).

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { head, paint, section, kv, link, btn, note } from '../core/ui.js';
import { KEYS } from '../core/keys.js';
import { tag, DASH } from '../core/fmt.js';
import { buildLabel, buildTitle, releaseStatus, changeLines, checkedAgo } from '../shared/release.js';
import { getNotices, onNotices } from '../core/notices.js';
import { downloadText } from '../core/secret.js';
import { toast } from '../core/toast.js';

export const id = 'help';
export const title = () => 'Help';

// what this pack ships, with the licence files next to them (served from the pack itself)
const BUNDLED = [
  ['JetBrains Mono', 'font for numbers, labels, tables', 'SIL Open Font License 1.1', '/fonts/OFL.txt'],
  ['IBM Plex Sans', 'font for help text', 'SIL Open Font License 1.1', '/fonts/OFL.txt'],
  ['MapLibre GL JS', 'draws the maps', 'BSD-3-Clause', '/vendor/maplibre-gl/LICENSE.txt'],
  ['uPlot', 'draws the charts', 'MIT', '/vendor/uplot/LICENSE.txt'],
];

// short answers in our own words: [question, answer, page, link words]
const QA = [
  ['Why is a car missing?', 'A car appears when its CAN Bridge uploads its first log. If it does not, check the upload token in the CAN Bridge app, and look for a key waiting to be tied to a car. A watch account sees only the cars an admin gave it.', 'settings-cars', 'settings/cars'],
  ['Why is the data a minute or more late?', 'The CAN Bridge sends finished logs, about a minute after the car records them. With no network in the car, logs wait on the device and arrive later; pages fill in by themselves when they do.', 'health', 'health'],
  ['What is the difference between asleep and unknown?', 'Asleep means the car went quiet and the CAN Bridge saw it. Unknown means no log covers that time. Unknown is never counted as asleep, and charts never draw a value across it.', 'timeline', 'timeline'],
  ['Does Sentry Mate wake the car?', 'No. It only listens to what the car sends anyway, so it never wakes the car, keeps it awake or sends it a command.', 'car', 'car overview'],
  ['Where do I change units, or rated against ideal range?', 'Each person picks units and the range once; every page then uses them. The server keeps metric values, so changing units never changes your data.', 'settings-display', 'settings/display'],
  ['How do I pick the dates a page shows?', 'The range on the right of the header line. It works in the car\'s own time zone and stays when you change page or car.', 'statistics', 'statistics'],
  ['Can I take my data out?', 'Yes. Tables have a csv link that uses your units, and drives can be saved as GPX tracks.', 'drives', 'drives'],
  ['Can I bring my TeslaMate history?', 'Yes. An admin imports a TeslaMate backup. Imported rows say imported from TeslaMate, and where they overlap with live data (SMO or the Tesla API) you choose which is shown.', 'import', 'import'],
  ['How do I move to another server?', 'Make a move code in settings/server and type it on the new server\'s first page, or download an export file and restore it there with the recovery key. No shell is needed.', 'settings-server', 'settings/server'],
];

const outside = (u) => /^https?:\/\//i.test(u || '');
const outLink = (href, text) => h('a', { class: 'lnk', href, target: '_blank', rel: 'noopener noreferrer external' }, text);
const hostOf = (u) => { try { return new URL(String(u).replace(/[{}]/g, '')).hostname || null; } catch { return null; } };

export async function mount(ctx) {
  const needCar = !(ctx.state.car ?? state.lastCar());
  let [about, notices, settings, cars, rel] = await Promise.all([
    ctx.api.get('about', null, { maxAgeMs: 600000 }),
    ctx.api.get('notices', null, { maxAgeMs: 5000 }).then((r) => r.items).catch(() => getNotices()),
    ctx.admin ? ctx.api.get('settings').catch(() => null) : null,
    // the frame has just read the same list (for the car link): its answer is reused
    needCar ? ctx.api.get('cars', { sort: 'name', limit: 200 }, { maxAgeMs: 15000 }).then((r) => r.items || []).catch(() => null) : null,
    ctx.api.get('release').catch(() => null),
  ]);
  if (!ctx.alive()) return;
  const pack = about.ui_pack || {};
  const mismatch = pack.api != null && about.api_version != null && String(pack.api) !== String(about.api_version);

  const release = h('div', { class: 'rel-help' });
  const paintRelease = (list) => {
    const now = ctx.api.now();
    const n = (list || []).find((x) => x.code === 'release_available');
    const to = ctx.admin ? [' ', link(`${state.buildUrl('settings-display')}#release-check`, 'settings/display')] : null;
    const when = rel ? checkedAgo(rel, now) : '';
    const ago = when ? h('span', { class: 'muted' }, ` (${when})`) : null;
    const st = rel ? releaseStatus(rel, now) : null;
    if (!rel) replace(release, h('span', { class: 'muted' }, 'could not read the release check'));
    else if (rel.status === 'newer') {
      // the notes link only while an admin allows release-notes links (settings.outside_links.release_notes)
      const notes = settings?.outside_links?.release_notes ? (n?.detail?.notes_url || rel.newest?.compare_url) : null;
      const lines = changeLines(rel.newest);
      replace(release,
        h('p', null, tag('new', 'accent'), ' ', st.text, ago, outside(notes) ? [' ', outLink(notes, 'release notes')] : null),
        lines.length ? h('ul', { class: 'rel-titles' }, lines.map((x) => h('li', null, x))) : null,
        rel.pull ? h('p', null, h('span', { class: 'muted' }, 'to update, on the server: '), h('code', { class: 'mono rel-pull' }, rel.pull)) : null,
        h('p', { class: 'muted' }, 'Your data stays when you update the container.', to));
    } else if (rel.status === 'up_to_date') replace(release, tag('ok', 'ok'), ' up to date', ago, '.', to);
    else if (rel.status === 'error') replace(release, tag('err', 'bad'), ' ', st.text, ago, to);
    else if (rel.status === 'never' || rel.status === 'checking') replace(release, tag('wait', 'muted'), ' ', st.text.toLowerCase(), ': the release check is on; its first answer shows here.', to);
    else if (ctx.admin) replace(release, tag('off', 'muted'), ' not checked: the release check is off, so Sentry Mate never asks the internet. turn it on in', to);
    else replace(release, tag('off', 'muted'), ' off');
  };
  paintRelease(notices);
  ctx.onCleanup(onNotices(async (list) => {
    rel = await ctx.api.get('release').catch(() => rel);
    if (ctx.alive()) paintRelease(list);
  }));

  const docs = about.links?.docs, source = about.links?.source;
  const aboutSec = section({ title: 'about', id: 'about' }, kv([
    ['sentry mate', [h('span', { title: buildTitle(about) || null }, buildLabel(about, ctx.api.now()) || DASH), about.dev_server ? [' ', tag('dev server, made-up data', 'warn')] : null], { cover: 'web.settings.about.version' }],
    ['api', `/api/v${about.api_version ?? '?'}`],
    ['ui pack', [`${pack.name || DASH} ${pack.version || ''}`.trim(), mismatch ? [' ', tag('warn', 'warn'), ` made for api ${pack.api}; this server speaks ${about.api_version}`] : null]],
    ['newer release', release, { cover: 'web.settings.about.update-available' }],
    ['source code', source ? outLink(source, source.replace(/^https?:\/\//i, '')) : h('span', { class: 'muted' }, 'not published yet'), { cover: 'web.settings.about.source-link' }],
    ['documentation', outside(docs) ? outLink(docs, docs.replace(/^https?:\/\//i, '')) : [link('#qa', 'answers below'), h('span', { class: 'muted' }, source ? '; guides are in the docs folder of the source' : '')], { cover: 'web.settings.about.docs-link' }],
  ]));

  const keys = section({ title: 'keys', id: 'keys' },
    h('dl', { class: 'keylist help-keys' }, KEYS.map(([k, what]) => h('div', { class: 'keylist-row', dataset: { row: '' } }, h('dt', { class: 'kbd-line' }, k), h('dd', null, what)))),
    note('Keys are off while a field has focus. Every key has a mouse or tap way too: the link row under the command bar, the header buttons, the rows themselves.'));

  const read = section({ title: 'how it works' }, h('div', { class: 'prose' },
    h('p', null, 'Sentry Mate keeps what your car says about itself. A small logger in the car, the CAN Bridge, records the messages on the car\'s own wiring and uploads them here, signed, about once a minute. Sentry Mate needs no Tesla account and never sends the car anything.'),
    h('p', null, 'Every page is built from those logs: drives, charges, the battery, efficiency and the rest. When a log is missing, the time shows as unknown, never as a guess. Numbers use the units you chose and days are the car\'s own days, in its time zone.'),
    h('p', null, 'The command bar at the top shows where you are as a path; each part is a link. Press Ctrl K or / anywhere to jump to any page, car, drive, charge or setting by typing a few letters.')));

  const own = about.licence;
  const allText = () => {
    const out = [`Sentry Mate ${about.version || ''}`.trim(), `UI pack: ${pack.name || '-'} ${pack.version || ''}`.trim(), '',
      own?.name ? `Sentry Mate is licensed under ${own.name}.` : 'The licence of Sentry Mate itself is not published with this version yet.', '', 'Parts shipped with this UI pack:',
      ...BUNDLED.map(([n, w, l, f]) => `- ${n} (${w}): ${l}. Full text: ${f}`)];
    if ((about.licences || []).length) out.push('', 'Parts used by the server:', ...about.licences.map((l) => `- ${l.name}: ${l.licence}`));
    if ((about.data_credits || []).length) out.push('', 'Data credits:', ...about.data_credits.map((c) => `- ${c.name}: ${c.text}${c.licence ? ` (${c.licence})` : ''}`));
    return out.join('\r\n') + '\r\n';
  };
  const lic = section({ title: 'licences and notices', id: 'licences', side: btn('download notices', () => { downloadText('sentry-mate-notices.txt', allText()); toast('Notices saved as a text file.'); }, { cover: 'web.legal.notice' }) },
    h('ul', { class: 'ln-list', cover: 'web.legal.license' },
      h('li', null, tag('self', 'accent'), h('span', { class: 'ln-list-t' }, 'Sentry Mate: ', own?.name ? [own.name, own.url ? [' ', link(own.url, 'full text', { target: '_blank', rel: 'noopener' })] : null] : h('span', { class: 'muted' }, 'its own licence is not published with this version yet'))),
      BUNDLED.map(([n, w, l, f]) => h('li', null, tag('pack', 'muted'), h('span', { class: 'ln-list-t' }, `${n}: ${l}, ${w}. `, link(f, 'licence text', { target: '_blank', rel: 'noopener' })))),
      (about.licences || []).map((l) => h('li', null, tag('server', 'muted'), h('span', { class: 'ln-list-t' }, `${l.name}: ${l.licence}, decodes the car's messages on the server`)))),
    note('Each part keeps its own licence. The full texts open as plain files from this server, never from another site.'));

  const credits = about.data_credits || [];
  const lookup = !settings ? 'Where maps and addresses come from is set by an admin in settings/display.'
    : settings.tiles?.mode !== 'online' && settings.geocode?.mode !== 'online' ? 'Maps and addresses come from data stored on this server; nothing is looked up on another site.'
      : `${settings.tiles?.mode === 'online' ? `Map tiles come from ${hostOf(settings.tiles.online_url) || 'an outside tile service'}` : 'Map tiles come from this server'}; ${settings.geocode?.mode === 'online' ? `addresses are looked up at ${hostOf(settings.geocode.url) || 'an outside service'}` : 'addresses come from this server'}.`;
  const creditSec = section({ title: 'data credits' },
    credits.length ? h('ul', { class: 'ln-list' }, credits.map((c) => h('li', null, tag('data', 'muted'), h('span', { class: 'ln-list-t' }, `${c.name}: ${c.text}${c.licence ? ` (${c.licence})` : ''}`))))
      : note('This server uses no outside data that needs a credit.'),
    note(lookup));

  const carLink = (page, words) => {
    let href = state.linkTo(page);
    if (href === state.buildUrl('fleet') && page !== 'fleet') {
      const pick = cars && cars.length ? state.preferredCar(cars) : null;
      if (pick) href = state.linkTo(page, { car: pick });
      else return link(href, `choose a car, then open ${words}`);
    }
    return link(href, `open ${words}`);
  };
  const items = QA.map(([q, a, page, words]) => ({ el: h('div', { dataset: { row: '' } }, h('dt', null, q), h('dd', null, a, ' ', carLink(page, words))), hay: `${q} ${a} ${words}`.toLowerCase() }));
  const none = h('p', { class: 'empty-line', hidden: true }, '-- no answer has those words --');
  const count = h('span', { class: 'muted' }, `${items.length} answers`);
  const filter = h('input', { class: 'in', type: 'search', placeholder: 'find an answer', 'aria-label': 'Find an answer' });
  filter.addEventListener('input', () => {
    const w = filter.value.toLowerCase().split(/\s+/).filter(Boolean);
    let n = 0;
    for (const it of items) { const ok = w.every((x) => it.hay.includes(x)); it.el.hidden = !ok; if (ok) n++; }
    none.hidden = n > 0;
    count.textContent = w.length ? `${n} of ${items.length} answers` : `${items.length} answers`;
  });
  const qa = section({ title: 'answers', id: 'qa', side: count },
    h('div', { class: 'filt' }, h('span', { class: 'filt-p' }, 'find> '), filter),
    h('dl', { class: 'qa prose' }, items.map((i) => i.el)), none);

  paint(ctx, head(ctx), read, keys, aboutSec, qa, lic, creditSec);
  ctx.ready();
}
