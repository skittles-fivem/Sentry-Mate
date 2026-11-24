// Sentry Mate, "Pebble" UI pack: Help and about (/help).
//
// Plain readable tiles with headings: About (version, API version, this pack, whether a newer release is out,
// source code and documentation), Licences and notices (Sentry Mate's own, the server's parts, the files this pack
// bundles, data credits; /help#licence lands here), and one tile per common question with a button to the page it
// is about. Data: GET about (the build), release (the release check's answer), settings (the release-notes links rule).

import { h } from '../shared/dom.js';
import { tile, chip, icon, linkBtn } from '../core/ui.js';
import { sectionHead } from '../core/bits.js';
import { PAGES } from '../core/pages.js';
import { group, infoRow } from '../core/settings.js';
import * as api from '../shared/api.js';
import { buildLabel, buildTitle } from '../shared/release.js';
import { relState, relNewer } from '../core/release.js';

export const id = 'help';
export const css = true;
export const shape = [[2, 'h3'], [2, 'h3'], [2, 'h2'], [2, 'h2']];
export const title = () => 'Help and about';

/** Third-party files inside this pack; their licence files sit next to them. */
const BUNDLED = [
  ['MapLibre GL JS', 'Draws the maps', 'BSD-3-Clause', '/vendor/maplibre-gl/LICENSE.txt'],
  ['uPlot', 'Draws the charts', 'MIT', '/vendor/uplot/LICENSE.txt'],
  ['Manrope', 'The typeface', 'SIL Open Font License 1.1', '/fonts/LICENSE-Manrope.txt'],
  ['JetBrains Mono', 'Codes and logs', 'SIL Open Font License 1.1', '/fonts/LICENSE-JetBrainsMono.txt'],
];

/** Short answers; `you` is added for an admin, `watch` for a watch account. */
const QA = [
  { id: 'q-missing', icon: 'car', q: 'Why is a car missing?', page: 'settings-cars',
    a: 'A car appears once its CAN Bridge has uploaded its first log. A watch account sees only the cars an admin gave it.',
    you: 'Check in Settings: Cars that the car has an upload token, that it was used, and that no key waits to be tied to it.',
    watch: 'Ask an admin to check the car’s upload token and to add the car to your account.' },
  { id: 'q-late', icon: 'clock', q: 'Why is the data late?', page: 'car',
    a: 'The CAN Bridge sends what it records about a minute later, and catches up when it has a connection again. Every live value shows its own age.' },
  { id: 'q-unknown', icon: 'timeline', q: 'Unknown or asleep?', page: 'health',
    a: 'Asleep means the car went quiet and the log says so. Unknown means no log covers the time: the bridge was off or had no connection, or a log failed its check. Charts never draw a line across unknown time.' },
  { id: 'q-pairing', icon: 'key', q: 'How does pairing work?', page: 'settings-cars',
    a: 'The CAN Bridge signs every log with its own key. Sentry Mate checks the signature and the chain of logs before it uses them, so a missing log shows as a gap and a changed one is refused.',
    you: 'Make an upload token in Settings: Cars and type it in the CAN Bridge app.', watch: 'An admin makes the upload token.' },
  { id: 'q-estimated', icon: 'info', q: 'Why is a figure marked estimated?', page: 'import',
    a: 'Figures from SMO (the CAN Bridge’s logs) are measured. The Tesla API sends power only every few seconds to a minute, so a drive or charge it alone saw has its energy added up and marked estimated. History imported from TeslaMate carries no such signals either, so its energy and costs are worked out and marked estimated. Every row says where its data came from: SMO, the Tesla API or an import.' },
  { id: 'q-tesla', icon: 'plug', q: 'Do I need a Tesla account?', page: 'settings-integrations',
    a: 'No. SMO, the car’s CAN Bridge, is enough. A Tesla account is an optional second live input: an admin adds it under Settings: Integrations, and the server then reads its cars from Tesla’s Owner API too. It only reads; it never wakes a car or sends it a command.' },
  { id: 'q-units', icon: 'gauge', q: 'How do I change units, theme or text size?', page: 'settings-display',
    a: 'Settings: Display has the theme (Auto, Light, Dark), map colours and map style, units, rated or ideal range, the interface size and the UI pack. Your choices are yours alone.' },
  { id: 'q-maps', icon: 'map', q: 'Where do maps and addresses come from?', page: 'settings-maps',
    a: 'From this server by default: an offline map file and bundled place names, so nothing is looked up on another site. An admin can choose an online map (satellite pictures, for example), which maps people may switch to, and an online address service, for everyone.',
    you: 'Change them in Settings: Maps and addresses.', watch: 'An admin sets them; you pick your own map style in Settings: Display.' },
  { id: 'q-times', icon: 'calendar', q: 'Which time zone are times in?', page: 'fleet',
    a: 'Every time and day is in the car’s own time zone. A page with several cars shows each one in its own zone.' },
  { id: 'q-csv', icon: 'download', q: 'How do I get my data out?', page: 'drives',
    a: 'Lists have an Export button. The file holds every row of the period and filters, not only those on screen, with the units in the column names.' },
  { id: 'q-car', icon: 'shield', q: 'Does Sentry Mate talk to the car or to Tesla?', page: 'settings-integrations',
    a: 'No. It only reads what the CAN Bridge recorded. It never wakes the car or sends commands, and it asks no other server anything unless an admin turns that on (Settings: Integrations, Maps and addresses, Server).' },
  { id: 'q-move', icon: 'move', q: 'How do I move to another server?', page: 'settings-server',
    a: 'Start the new server, then make a move code on the old one under Settings: Server and type it on the new one’s first page. An export file and the recovery key do the same when the two cannot reach each other.',
    you: 'The move code and the export file ask for your password again.', watch: 'Only an admin can move the server.' },
];

const outside = { target: '_blank', rel: 'external noopener noreferrer' };
const val = (t, cls = null) => h('span', { class: ['hp-val', cls] }, t);

export async function mount(ctx) {
  const admin = ctx.admin;
  const [about, release, settings] = await Promise.all([
    ctx.api.get('about', null, { maxAgeMs: 600000 }),
    ctx.api.get('release').catch(() => null),
    ctx.api.get('settings').catch(() => null),
  ]);
  if (!ctx.alive()) return {};
  const links = about.links || {};
  const pack = about.ui_pack || {};
  const display = () => h('a', { class: 'place-link', href: `${ctx.link('settings-server')}#release-check` }, 'Settings: Server');

  /* ---------------------------------------------------------------- about */
  const mismatch = pack.api != null && String(pack.api) !== String(about.api_version);
  const st = release ? relState(release) : null;
  const status = release ? release.status : null;
  const isOff = !release || !release.enabled || status === 'off';
  let relSub;
  let relSide;
  let relTone = null;
  if (!release) {
    relSub = 'The server did not say how the release check went.';
    relSide = val('Not known');
  } else if (status === 'newer') {
    const n = release.newest || {};
    const notesUrl = n.compare_url || links.source || null;
    const allowed = !!(settings && settings.outside_links && settings.outside_links.release_notes);
    relTone = 'teal';
    relSub = [h('span', { class: 'hp-rel-text' }, `${st.text}.`), relNewer(release),
      h('span', { class: 'hp-rel-text' }, 'Your data stays. ', allowed && notesUrl ? [h('a', { class: 'place-link', href: notesUrl, ...outside }, 'What changed'), ' opens on another site.']
        : admin ? ['Release-notes links are off; turn them on in ', display(), '.'] : 'Release-notes links are off.')];
    relSide = chip('Newer build', { tone: 'teal' });
  } else if (status === 'up_to_date') {
    relSub = [st.ago ? `${st.ago[0].toUpperCase()}${st.ago.slice(1)}. ` : '', 'Asked once a day; nothing else is sent. ', admin ? ['Turn it off in ', display(), '.'] : null];
    relSide = val('Up to date');
  } else if (status === 'error') {
    relTone = 'sun';
    relSub = [`${st.text}.`, st.ago ? ` Last ${st.ago}.` : '', admin ? [' Try again in ', display(), '.'] : ''];
    relSide = val('Could not check');
  } else if (!isOff) {
    relSub = ['Asked once a day; nothing else is sent. ', admin ? ['See ', display(), '.'] : null];
    relSide = val(st.text);
  } else {
    relSub = ['The release check is off, so no other server is asked. ', admin ? ['Turn it on in ', display(), '.'] : 'An admin can turn it on.'];
    relSide = admin ? h('a', { class: 'tile-link', href: `${ctx.link('settings-server')}#release-check` }, 'Off', icon('chevronRight')) : val('Off');
  }

  const docs = links.docs;
  const aboutTile = tile({ w: 2, title: 'About', icon: 'info', id: 'about' },
    h('p', { class: 'hp-lede' }, 'Sentry Mate keeps your car’s history on your own server, from what the CAN Bridge records.'),
    group(null, [
      infoRow({ icon: 'server', tone: 'coral', name: 'Sentry Mate', sub: about.dev_server ? 'The dev server: every car and figure is made up.' : 'The server this page talks to.', side: h('span', { title: buildTitle(about) || null }, val(buildLabel(about, api.now()) || '—', 'hp-mono')), cover: 'web.settings.about.version', cls: 'hp-build' }),
      infoRow({ icon: 'chip', name: 'API version', sub: 'What the UI packs read.', side: val(String(about.api_version ?? '—'), 'hp-mono') }),
      infoRow({ icon: 'spark', name: `UI pack: ${pack.name || 'Pebble'}`, sub: pack.api == null ? 'It names no API version.' : mismatch ? `Made for API ${pack.api}, the server speaks ${about.api_version}: some parts may not work.` : `Made for API ${pack.api}, the one this server speaks.`, tone: mismatch ? 'sun' : null, side: val(pack.version || '—', 'hp-mono') }),
      infoRow({ icon: 'refresh', tone: relTone, name: 'Newer release', sub: relSub, cover: 'web.settings.about.update-available', cls: status === 'newer' ? 'hp-rel' : null, side: relSide }),
      infoRow({ icon: 'globe', name: 'Source code', sub: links.source ? 'Opens on another site.' : 'This server names no address for it.', cover: 'web.settings.about.source-link',
        side: links.source ? h('a', { class: 'tile-link', href: links.source, ...outside }, 'Open', icon('external')) : val('Not named') }),
      infoRow({ icon: 'book', name: 'Documentation', sub: docs ? (docs.startsWith('/') ? 'The answers further down this page.' : 'Opens on another site.') : 'This server names no address for it.', cover: 'web.settings.about.docs-link',
        side: !docs ? val('Not named') : docs.startsWith('/') ? h('a', { class: 'tile-link', href: docs === '/help' ? '#answers' : docs }, 'Read', icon('chevronRight')) : h('a', { class: 'tile-link', href: docs, ...outside }, 'Open', icon('external')) }),
    ]));

  /* ---------------------------------------------------------------- licences */
  const lic = about.licence || {};
  const own = lic.name
    ? [h('b', null, `${lic.name}. `), lic.url ? h('a', { class: 'place-link', href: lic.url, target: '_blank', rel: lic.url.startsWith('/') ? 'noopener' : outside.rel }, 'Read the full text')
      : links.source ? h('a', { class: 'place-link', href: links.source, ...outside }, 'Its text comes with the source code') : 'The server names no address for its text.']
    : [h('b', null, 'Not chosen yet. '), 'The project has not decided its licence, so there is no text to show yet.'];
  const credits = about.data_credits || [];
  const licTile = tile({ w: 2, title: 'Licences and notices', icon: 'book', id: 'licence', cover: ['web.legal.license', 'web.legal.notice'] },
    h('div', { class: 'hp-block' }, h('h3', { class: 'set-q' }, 'Sentry Mate itself'), h('p', { class: 'set-a' }, ...own)),
    group('In the server', (about.licences || []).length ? about.licences.map((l) => infoRow({ name: l.name, side: val(l.licence || '—') }))
      : [infoRow({ name: 'The server lists no third-party parts.' })]),
    group('In this UI pack', BUNDLED.map(([n, what, l, href]) => infoRow({ name: n, sub: what,
      side: h('a', { class: 'tile-link', href, target: '_blank', rel: 'noopener', title: `Read the ${n} licence` }, l) }))),
    group('Data credits', credits.length ? credits.map((c) => infoRow({ name: c.name || c.text, sub: c.name && c.text && c.text !== c.name ? c.text : null, side: val(c.licence || '—') }))
      : [infoRow({ name: 'No outside data in use.' })]));

  /* ---------------------------------------------------------------- answers */
  const answers = QA.map((x) => {
    const target = PAGES[x.page];
    return tile({ w: 2, title: x.q, icon: x.icon, id: x.id, cls: 'hp-qa' },
      h('p', { class: 'set-a' }, x.a, x.you ? [' ', admin ? x.you : x.watch] : null),
      h('div', { class: 'hp-go' }, linkBtn(`Open ${target ? target.title : 'the page'}`, ctx.link(x.page), { kind: 'soft', icon: 'arrowRight' })));
  });

  ctx.view.replaceChildren(aboutTile, licTile,
    Object.assign(sectionHead('Questions and answers', 'Short answers, each with the page it is about.'), { id: 'answers' }),
    ...answers);
  ctx.ready();
  return {};
}
