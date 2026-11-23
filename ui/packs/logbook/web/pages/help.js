// Sentry Mate, "Logbook" UI pack: Help and about (/help).
//
// Version and build, API version and UI pack (GET /api/v1/about), the opt-in release check (GET /api/v1/release,
// and GET /api/v1/settings for whether release-notes links are allowed), licences and data credits (the
// server's, then the files this pack bundles), and short answers that each lead to the page they are about.

import { h } from '../shared/dom.js';
import { pageHead, icon, ago, flag } from '../core/ui.js';
import { PAGES } from '../core/pages.js';
import { parseUrl } from '../shared/state.js';
import { now } from '../shared/api.js';
import { buildLabel, buildTitle, releaseStatus, changeLines, checkedAgo } from '../shared/release.js';

export const id = 'help';
export const css = true;
export const title = () => 'Help and about';

/** Third-party files inside this pack, with their licences (the licence files sit next to them in web/). */
const BUNDLED = [
  ['MapLibre GL JS', 'draws the maps', 'BSD-3-Clause', '/vendor/maplibre-gl/LICENSE.txt'],
  ['uPlot', 'draws the charts', 'MIT', '/vendor/uplot/LICENSE.txt'],
  ['Inter', 'typeface for text and labels', 'SIL Open Font License 1.1', '/fonts/LICENSE-Inter.txt'],
  ['Charter', 'typeface for titles and figures', 'Bitstream Charter licence', '/fonts/LICENSE-Charter.txt'],
];

/**
 * Short answers. `admin` is the extra sentence when the fix is an admin's job: an admin reads `you`, a watch
 * account reads `watch`. `page` is where the answer leads (a car page uses the last car, else Fleet).
 */
const QA = [
  { id: 'q-missing', q: 'Why is a car missing?',
    a: 'A car appears once its CAN Bridge has uploaded its first log. A watch account also sees only the cars its admin gave it.',
    admin: { you: 'Check in Settings: Cars that the car has an upload token, that the token was used, and that no CAN Bridge key waits to be assigned or trusted.', watch: 'Ask an admin to check the car’s upload token and to add the car to your account.' },
    page: 'settings-cars' },
  { id: 'q-late', q: 'Why is the data late?',
    a: 'The CAN Bridge sends what it records about a minute later, and catches up when it has a connection again. Every live value shows its own age; nothing is hidden when it gets old.',
    page: 'car' },
  { id: 'q-unknown', q: 'Why is a span unknown, and how is that different from asleep?',
    a: 'Asleep means the car went quiet and the log says so: values are held across it. Unknown means no log covers the time: the CAN Bridge was off or had no connection, recording was paused, or a log failed its check. Charts never draw a line across unknown time. Data health lists every gap with its cause.',
    page: 'health' },
  { id: 'q-pairing', q: 'How does pairing work?',
    a: 'The CAN Bridge app signs every log with its own key. Sentry Mate checks the signature and the chain of logs before it uses them, so a missing log shows as a gap and a changed one is refused.',
    admin: { you: 'Make an upload token for the car in Settings: Cars, enter it in the CAN Bridge app, then trust the key it sends.', watch: 'An admin makes the upload token and trusts the key.' },
    page: 'settings-cars' },
  { id: 'q-estimated', q: 'Why is a figure marked estimated?',
    a: 'Figures from SMO (the car’s CAN bus) are measured: energy is volts times amps from the pack. A drive or charge only the Tesla API saw has its energy summed from the power Tesla reports every few seconds to a minute, and history imported from TeslaMate carries no such signals at all; both are marked estimated, and every row says where it came from (Data from SMO, Data from the Tesla API, Imported from TeslaMate).',
    page: 'import' },
  { id: 'q-units', q: 'How do I change units, range or the size of the text?',
    a: 'Use Aa in the masthead for the quick choices, or Settings: Display and units for all of them. Your choices are yours; an admin sets the defaults for new accounts.',
    page: 'settings-display' },
  { id: 'q-times', q: 'Which time zone are times in?',
    a: 'Every time and every day is in the car’s own time zone, shown next to the clock in the masthead. A page with several cars shows each row in its car’s zone.',
    page: 'fleet' },
  { id: 'q-csv', q: 'How do I get my data out?',
    a: 'Every table and chart has a Download CSV button. The file holds every row that matches the period and filters, not only the rows on screen, and its column names carry the units.',
    page: 'drives' },
  { id: 'q-car', q: 'Does Sentry Mate ever talk to the car or to Tesla?',
    a: 'Out of the box, no: it only reads what SMO (the CAN Bridge) recorded. An admin may add a Tesla account in Settings: Integrations; then the server also reads its cars from the Tesla API, as a second input. Even then it never wakes a car or sends it a command, and it asks nothing of other servers unless an admin turns that on.',
    page: 'settings-integrations' },
];

const outside = { target: '_blank', rel: 'external noopener noreferrer' };

export async function mount(ctx) {
  const admin = ctx.me && ctx.me.role === 'admin';
  const [about, release, settings] = await Promise.all([
    ctx.api.get('about', null, { maxAgeMs: 600000 }),
    ctx.api.get('release').catch(() => null),
    ctx.api.get('settings').catch(() => null),
  ]);
  const links = about.links || {};
  const pack = about.ui_pack || {};

  const row = (name, value, note, cover) => h('div', { class: 'about-row', cover },
    h('dt', null, name), h('dd', null, h('span', { class: 'about-value' }, value), note ? h('span', { class: 'about-note' }, note) : null));

  const packMismatch = pack.api != null && String(pack.api) !== String(about.api_version);
  const aboutList = h('dl', { class: 'about-list' },
    row('Sentry Mate', h('span', { class: 'mono', title: buildTitle(about) || null }, buildLabel(about) || '—'),
      about.dev_server ? 'The dev server: every car and figure is made up.' : 'The server this page talks to.', 'web.settings.about.version'),
    row('API version', h('span', { class: 'mono' }, String(about.api_version ?? '—')), 'The version of the web API the UI packs read.'),
    row('UI pack', `${pack.name || 'Logbook'} ${pack.version || ''}`.trim(),
      pack.api == null ? 'This pack names no API version.' : packMismatch
        ? h('span', { class: 'about-warn' }, `Made for API ${pack.api}, but the server speaks API ${about.api_version}: some parts may not work.`)
        : `Made for API ${pack.api}, the one this server speaks.`),
    row('Newer release', releaseValue(release), releaseNote(release, settings, admin, ctx), 'web.settings.about.update-available'),
    row('Source code', links.source ? h('a', { href: links.source, ...outside }, 'Sentry Mate’s source code') : 'Not named',
      links.source ? 'Opens on another site.' : 'This server gives no address for its source code.', 'web.settings.about.source-link'),
    row('Documentation', docsLink(links.docs), links.docs ? (links.docs.startsWith('/') ? 'Short answers further down this page.' : 'Opens on another site.') : 'This server gives no address for its documentation.', 'web.settings.about.docs-link'));

  const credits = about.data_credits || [];
  const licenceBlock = h('section', { class: 'help-block', id: 'licence', 'aria-labelledby': 'licence-title', cover: ['web.legal.license', 'web.legal.notice'] },
    h('h2', { class: 'label', id: 'licence-title' }, 'Licences and notices'),
    h('h3', { class: 'sublabel' }, 'Sentry Mate itself'),
    ownLicence(about.licence || {}, links),
    h('p', { class: 'note' }, 'It is built on these parts, each under its own licence:'),
    h('h3', { class: 'sublabel' }, 'In the server'),
    h('ul', { class: 'licence-list' },
      (about.licences || []).length ? about.licences.map((l) => h('li', null, h('span', null, l.name), h('span', { class: 'licence-name' }, l.licence || '—')))
        : h('li', { class: 'licence-none' }, 'The server lists no third-party parts.')),
    h('h3', { class: 'sublabel' }, 'In this UI pack'),
    h('ul', { class: 'licence-list' },
      BUNDLED.map(([n, what, l, href]) => h('li', null,
        h('span', null, n, h('span', { class: 'licence-what' }, ` · ${what}`)),
        h('a', { class: 'licence-name', href, target: '_blank', rel: 'noopener', title: `Read the ${n} licence` }, l)))),
    h('h3', { class: 'sublabel' }, 'Data credits'),
    h('ul', { class: 'licence-list' },
      credits.length ? credits.map((c) => h('li', null,
        h('span', null, c.name && c.text && !c.text.includes(c.name) ? [h('strong', null, c.name), ` · ${c.text}`] : (c.text || c.name)),
        h('span', { class: 'licence-name' }, c.licence || '—')))
        : h('li', { class: 'licence-none' }, 'No outside data in use.')));

  const qaBlock = h('section', { class: 'help-block help-qa', id: 'answers', 'aria-labelledby': 'qa-title' },
    h('h2', { class: 'label', id: 'qa-title' }, 'Questions and answers'),
    h('ol', { class: 'qa-index' }, QA.map((x) => h('li', null, h('a', { href: `#${x.id}` }, x.q)))),
    h('dl', { class: 'qa-list' }, QA.map((x) => {
      const href = ctx.link(x.page);
      const target = PAGES[parseUrl(href).page];
      return h('div', { class: 'qa', id: x.id },
        h('dt', null, x.q),
        h('dd', null,
          h('p', null, x.a, x.admin ? [' ', admin ? x.admin.you : x.admin.watch] : null),
          h('a', { class: 'link-arrow', href }, `Open ${target ? target.name : 'the page'}`, icon('arrowRight'))));
    })));

  ctx.main.append(
    pageHead({
      eyebrow: 'Set-up', title: 'Help and about', deck: ctx.info.purpose,
      aside: h('nav', { class: 'help-jump', 'aria-label': 'On this page' },
        h('a', { href: '#about' }, 'About'), h('a', { href: '#licence' }, 'Licences'), h('a', { href: '#answers' }, 'Answers')),
    }),
    h('div', { class: 'help-grid' },
      h('section', { class: 'help-block', id: 'about', 'aria-labelledby': 'about-title' },
        h('h2', { class: 'label', id: 'about-title' }, 'About'), aboutList),
      licenceBlock,
      qaBlock));

  // An address such as /help#licence (the footer's link) opens here from another page: go to that part.
  const hash = decodeURIComponent(location.hash.slice(1));
  if (hash) { const el = document.getElementById(hash); if (el && ctx.main.contains(el)) el.scrollIntoView({ block: 'start' }); }
  ctx.ready();
  return {};
}

/** GET /release's status line: "A newer build is ready: …", "Up to date", "Could not check: …", "Off". */
function releaseValue(r) {
  if (!r) return 'Not known';
  const st = releaseStatus(r, now());
  if (r.status === 'newer') return h('span', { class: 'about-new' }, flag('New'), ' ', st.text);
  if (r.status === 'error') return h('span', { class: 'about-warn' }, st.text);
  return st.text;
}

function releaseNote(r, settings, admin, ctx) {
  const where = () => h('a', { href: ctx.link('settings-display') }, 'Settings: Display and units');
  if (!r) return 'The server did not say whether the release check is on.';
  const ago = checkedAgo(r, now());
  if (r.status === 'off' || !r.enabled) {
    return ['The release check is off, so Sentry Mate asks no other server. ', admin ? ['Turn it on in ', where(), '.'] : 'An admin can turn it on.'];
  }
  const turnOff = admin ? h('span', null, 'Turn the release check off in ', where(), '.') : null;
  if (r.status === 'newer') {
    const notesUrl = (r.newest && r.newest.compare_url) || null;
    const allowed = !!(settings && settings.outside_links && settings.outside_links.release_notes);
    let notes;
    if (!settings) notes = 'The server did not say whether release-notes links are allowed, so none is shown.';
    else if (!allowed) notes = ['Release-notes links are off, so the changes are not linked. ', admin ? ['Turn them on in ', where(), '.'] : 'An admin can turn them on.'];
    else if (!notesUrl) notes = 'The server gives no address for the changes.';
    else notes = [h('a', { href: notesUrl, ...outside }, 'What changed'), ' opens on another site.'];
    const lines = changeLines(r.newest);
    return h('span', { class: 'release-note' },
      lines.length ? h('ul', { class: 'release-titles' }, lines.map((x) => h('li', null, x))) : null,
      r.pull ? h('span', null, 'To update, run on the server: ', h('code', { class: 'mono release-pull' }, r.pull), ' Your data stays.') : null,
      h('span', null, notes),
      ago ? h('span', null, `Last ${ago}.`) : null,
      turnOff);
  }
  const first = r.status === 'up_to_date' ? 'This is the newest published build.'
    : r.status === 'error' ? 'The check tries again tomorrow.'
      : r.status === 'checking' ? 'Asking GitHub now.' : 'The first check runs soon.';
  return h('span', { class: 'release-note' },
    h('span', null, first, ago ? ` Last ${ago}.` : ''),
    h('span', null, 'The release check asks GitHub once a day for the newest build and sends nothing about this server.'),
    turnOff);
}

/** Sentry Mate's own licence (GET /api/v1/about `licence`), and where its full text is, if anywhere. */
function ownLicence(licence, links) {
  const text = licence.url
    ? h('a', { href: licence.url, target: '_blank', rel: licence.url.startsWith('/') ? 'noopener' : outside.rel }, 'Read the full text')
    : links.source ? h('a', { href: links.source, ...outside }, 'Its text comes with the source code') : null;
  if (!licence.name) {
    return h('p', { class: 'note own-licence' }, h('strong', null, 'Licence: not chosen yet. '),
      'The project has not decided under which licence it is published, so there is no licence text to show here yet.');
  }
  return h('p', { class: 'note own-licence' }, h('strong', null, `Licence: ${licence.name}. `),
    text || 'This server gives no address for its full text, and no source code address, so the text cannot be shown here.');
}

function docsLink(docs) {
  if (!docs) return 'Not named';
  if (docs.startsWith('/')) return h('a', { href: docs === '/help' ? '#answers' : docs }, 'These help pages');
  return h('a', { href: docs, ...outside }, 'The documentation');
}
