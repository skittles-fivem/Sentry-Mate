// Help and about (/help): versions, the opt-in release notice, links, licences and notices, data credits,
// and short answers to common questions.
// GET /api/v1/about for the versions, licences and credits; GET /api/v1/notices for the release notice (only
// there when the owner turned the release check on); GET /api/v1/settings (admins only) to say whether the
// release check is on.

import * as state from '../shared/state.js';
import { h, replace, on } from '../shared/dom.js';
import { downloadCsv, csvFilename } from '../shared/csv.js';
import { icon } from '../core/icons.js';
import { pageHead, panel, kv, pill, csvButton, appLink } from '../core/page.js';
import { getNotices, onNotices } from '../core/notices.js';
import { date } from '../core/fmt.js';
import { flash } from '../core/flash.js';

export const id = 'help';
export const css = true;

// The third-party parts this pack ships, with their licence files (served from the pack itself).
const BUNDLED = [
  { name: 'MapLibre GL JS', what: 'Draws the maps', licence: 'BSD-3-Clause', href: '/vendor/maplibre-gl/LICENSE.txt' },
  { name: 'uPlot', what: 'Draws the charts', licence: 'MIT', href: '/vendor/uplot/LICENSE.txt' },
  { name: 'Inter', what: 'Text font', licence: 'SIL Open Font License 1.1', href: '/fonts/LICENSE-Inter.txt' },
  { name: 'JetBrains Mono', what: 'Font for figures and codes', licence: 'SIL Open Font License 1.1', href: '/fonts/LICENSE-JetBrainsMono.txt' },
];

// Short answers in our own words. [question, answer, page, link text, words that also find it]
const QA = [
  ['Why is a car missing?', 'A car appears when its CAN Bridge uploads its first log. If it does not, check the upload token in the CAN Bridge app, and look for a CAN Bridge key waiting to be assigned to a car. A watch account only sees the cars an admin gave it.', 'settings-cars', 'Settings: Cars', 'vehicle list token key'],
  ['Why is the data a minute or more late?', 'The CAN Bridge sends finished logs, about a minute after the car records them. With no network in the car, logs wait on the device and arrive later; the pages fill in by themselves when they do.', 'health', 'Data health', 'delay slow upload offline'],
  ['What is the difference between asleep and unknown?', 'Asleep means the car went quiet and the CAN Bridge saw it do so. Unknown means no log covers that time: the CAN Bridge was off, recording was paused, or a log failed its check. Unknown is never counted as asleep, and charts never carry a value across it.', 'timeline', 'Timeline', 'gap hatched grey sleep'],
  ['How does pairing a CAN Bridge work?', 'Make an upload token for the car in Settings: Cars and enter it in the CAN Bridge app. The CAN Bridge signs every log with its own key; the first key is tied to the car, and other keys wait until an admin assigns them.', 'settings-cars', 'Settings: Cars', 'pair token key sign'],
  ['Does Sentry Mate wake the car?', 'No. It only listens to the messages the car sends anyway, so it never wakes the car, keeps it awake or sends it a command.', 'car', 'Car overview', 'sleep vampire drain command'],
  ['Where do I change units or rated against ideal range?', 'Each person picks distance, temperature and pressure units and the range type once; every page then uses them. The server keeps metric values, so changing units never changes your data.', 'settings-display', 'Settings: Display and units', 'miles km fahrenheit psi bar range'],
  ['How do I pick the dates a page shows?', 'Use the one period picker in the top bar. It works in the car’s own time zone and the period carries over when you change page or car.', 'statistics', 'Statistics', 'period range date month year time zone'],
  ['Can I take my data out?', 'Yes. Every table and chart has a Download CSV button that uses your units, and drives can also be saved as GPX tracks.', 'drives', 'Drives', 'export csv gpx download'],
  ['Can I bring my TeslaMate history?', 'Yes. An admin can import a TeslaMate backup. Imported rows are marked as imported, and where they overlap with CAN data you choose which one is shown.', 'import', 'Import', 'teslamate backup import overlap'],
  ['Why does an address show as coordinates?', 'Addresses are looked up offline from the place-name data on the server. Where that data has nothing close, the spot is shown as coordinates; you can name it as a place.', 'places', 'Places', 'address geocode location place'],
];

const isOutside = (u) => /^https?:\/\//i.test(u || '');
const outLink = (href, text) => h('a', { class: 'link', href, target: '_blank', rel: 'noopener noreferrer external' }, text, icon('ext', 'ico--sm'));

/** The notices as one plain text file: our own licence status, the bundled parts, the signal lists and the data credits. */
function noticeText(about) {
  const lines = [`Sentry Mate ${about.version || ''}`.trim(), `UI pack: ${about.ui_pack?.name || '-'} ${about.ui_pack?.version || ''}`.trim(), ''];
  lines.push(about.licence?.name ? `Sentry Mate is licensed under ${about.licence.name}.` : 'The licence of Sentry Mate itself is not published with this version yet.', '');
  lines.push('Parts shipped with this UI pack:');
  for (const b of BUNDLED) lines.push(`- ${b.name} (${b.what}): ${b.licence}. Full text: ${b.href}`);
  if ((about.licences || []).length) {
    lines.push('', 'Parts used by the server:');
    for (const l of about.licences) lines.push(`- ${l.name}: ${l.licence}`);
  }
  if ((about.data_credits || []).length) {
    lines.push('', 'Data credits:');
    for (const c of about.data_credits) lines.push(`- ${c.name}: ${c.text}${c.licence ? ` (${c.licence})` : ''}`);
  }
  return lines.join('\r\n') + '\r\n';
}

function downloadText(filename, text) {
  const href = URL.createObjectURL(new Blob([text], { type: 'text/plain;charset=utf-8' }));
  const a = h('a', { href, download: filename, rel: 'noopener', hidden: true });
  document.body.append(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(href), 30000);
}

export async function mount(ctx) {
  let about, notices, settings = null;
  try {
    [about, notices, settings] = await Promise.all([
      ctx.api.get('about', null, { maxAgeMs: 600000 }),
      ctx.api.get('notices').then((r) => r.items).catch(() => getNotices()),
      ctx.admin ? ctx.api.get('settings').catch(() => null) : Promise.resolve(null),
    ]);
  } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;

  // ---- About
  const pack = about.ui_pack || {};
  const apiMismatch = pack.api != null && about.api_version != null && String(pack.api) !== String(about.api_version);
  const versionCell = h('div', { class: 'help-version' },
    h('span', { class: 'help-version-main mono' }, about.version || '—'),
    about.dev_server ? pill('Dev server, made-up data', { tone: 'warm', ic: 'warn' }) : null);

  const releaseCell = h('div', { class: 'help-release' });
  const paintRelease = (list) => {
    const n = (list || []).find((x) => x.code === 'release_available');
    const checkOn = settings?.release_check?.enabled;
    const turnLink = ctx.admin ? appLink('settings-server', 'Release check in Settings: Server and access') : null;
    if (n) {
      replace(releaseCell,
        pill(n.message || 'A newer release is out', { tone: 'live', ic: 'upload' }),
        h('span', { class: 'explain' }, n.since_ms ? `Reported ${date(n.since_ms, ctx.tz)}. ` : '', 'Your data stays as it is when you update the container.'),
        turnLink ? h('span', { class: 'explain' }, turnLink) : null);
    } else if (checkOn === false) {
      replace(releaseCell, h('span', null, 'Not checked'),
        h('span', { class: 'explain' }, 'The release check is off, so Sentry Mate never asks the internet about new releases. ', turnLink));
    } else if (checkOn === true) {
      replace(releaseCell, h('span', { class: 'ok' }, 'This is the newest release'),
        h('span', { class: 'explain' }, 'The release check is on and has found nothing newer. ', turnLink));
    } else {
      replace(releaseCell, h('span', null, 'No newer release reported'),
        h('span', { class: 'explain' }, 'Shown only when an admin turns the release check on; until then Sentry Mate does not look.'));
    }
  };
  paintRelease(notices);
  ctx.onCleanup(onNotices((list) => { if (ctx.alive()) paintRelease(list); }));

  const source = about.links?.source;
  const docs = about.links?.docs;
  const docsCell = isOutside(docs)
    ? h('div', { class: 'help-cell' }, outLink(docs, 'Documentation site'), h('span', { class: 'explain' }, 'Install, upgrade and the API for UI packs. Opens in a new tab.'))
    : h('div', { class: 'help-cell' }, h('a', { class: 'link', href: '#qa' }, 'Questions and answers below'),
      h('span', { class: 'explain' }, source ? 'Guides for install, upgrade and UI packs are in the docs folder of the source code.' : 'The written guides come with the source code once it is published.'));

  const aboutPanel = panel({ title: 'About', icon: 'info', id: 'about', cls: 'help-about' },
    kv([
      ['Sentry Mate', h('div', { class: 'help-cell' }, versionCell,
        h('span', { class: 'explain' }, 'The server version, read from the build that is running.')), { cover: 'web.settings.about.version', icon: 'server' }],
      ['API', h('div', { class: 'help-cell' }, h('span', { class: 'mono' }, `/api/v${about.api_version ?? '?'}`),
        h('span', { class: 'explain' }, 'What UI packs talk to.')), { icon: 'chip' }],
      ['UI pack', h('div', { class: 'help-cell' }, h('span', null, `${pack.name || '—'} ${pack.version || ''}`.trim()),
        h('span', { class: ['explain', apiMismatch && 'warn'] }, apiMismatch
          ? `Made for API ${pack.api}, but the server speaks API ${about.api_version}: some pages may not work. Choose another pack or update this one.`
          : `The look you are using. Made for API ${pack.api ?? about.api_version}, which this server speaks.`)), { icon: 'monitor' }],
      ['Newer release', releaseCell, { cover: 'web.settings.about.update-available', icon: 'upload' }],
      ['Source code', h('div', { class: 'help-cell' },
        source ? outLink(source, source.replace(/^https?:\/\//i, '')) : h('span', null, 'Not published yet'),
        h('span', { class: 'explain' }, source ? 'Opens in a new tab.' : 'The public address goes here once the code is published.')), { cover: 'web.settings.about.source-link', icon: 'book' }],
      ['Documentation', docsCell, { cover: 'web.settings.about.docs-link', icon: 'question' }],
    ]));

  // ---- Licences and notices
  const serverParts = about.licences || [];
  const allRows = [
    ...BUNDLED.map((b) => ({ part: b.name, use: b.what, licence: b.licence, shipped: 'UI pack', text: b.href })),
    ...serverParts.map((l) => ({ part: l.name, use: 'Car signal list', licence: l.licence, shipped: 'Server', text: '' })),
  ];
  const own = about.licence;
  const licPanel = panel({
    title: 'Licences and notices', icon: 'book', id: 'licences', cls: 'help-licences',
    side: csvButton({ onClick: () => downloadCsv(csvFilename('sentry-mate', 'licences'), allRows, [
      { label: 'Part', value: (r) => r.part }, { label: 'Used for', value: (r) => r.use }, { label: 'Licence', value: (r) => r.licence },
      { label: 'Shipped with', value: (r) => r.shipped }, { label: 'Licence text', value: (r) => r.text }]) }),
  },
    h('div', { class: 'help-own', cover: 'web.legal.license' },
      h('b', null, 'Sentry Mate itself'),
      own?.name
        ? h('span', null, own.name, ' ', own.href ? h('a', { class: 'link', href: own.href, target: '_blank', rel: 'noopener' }, 'Full text') : null)
        : h('span', { class: 'faint' }, 'Its own licence is not published with this version yet.')),
    h('p', { class: 'note' }, 'Each part keeps its own licence. The full texts open as plain files from this server, never from another site.'),
    h('ul', { class: 'licence-list', cover: 'web.legal.license' },
      BUNDLED.map((b) => h('li', null, h('b', null, b.name), h('span', null, `${b.licence} · ${b.what}`),
        h('a', { class: 'link', href: b.href, target: '_blank', rel: 'noopener' }, 'Licence text'))),
      serverParts.map((l) => h('li', null, h('b', null, l.name), h('span', null, `${l.licence} · decodes the car’s messages on the server`)))),
    h('div', { class: 'help-notice-row' },
      h('button', {
        class: 'btn btn--sm', type: 'button', cover: 'web.legal.notice',
        onclick: () => { downloadText('sentry-mate-notices.txt', noticeText(about)); flash('Notices saved as a text file.', { kind: 'ok' }); },
      }, icon('download', 'ico--sm'), 'Download notices (text)'),
      h('span', { class: 'explain' }, 'Every licence and credit on this page in one plain text file, for this exact version.')));

  // ---- Data credits
  const credits = about.data_credits || [];
  const creditPanel = panel({ title: 'Data credits', icon: 'globe', id: 'credits', cls: 'help-credits' },
    credits.length
      ? h('ul', { class: 'licence-list' }, credits.map((c) => h('li', null, h('b', null, c.name), h('span', null, `${c.text}${c.licence ? ` · ${c.licence}` : ''}`))))
      : h('p', { class: 'note' }, 'This server uses no outside data that needs a credit.'),
    h('p', { class: 'note' }, 'Car signals are decoded with the signal lists named under Licences. Maps and addresses come from data stored on this server; nothing is looked up on another site unless an admin turns that on.'));

  // ---- Questions and answers, with a filter
  const items = QA.map(([q, a, page, label, words]) => {
    const el = h('div', { class: 'qa-item' }, h('dt', null, q), h('dd', null, a, ' ', h('a', { class: 'link nowrap', href: state.linkTo(page) }, `Open ${label}`, icon('chev-r', 'ico--sm'))));
    return { el, hay: `${q} ${a} ${label} ${words}`.toLowerCase() };
  });
  const none = h('p', { class: 'note', hidden: true }, 'No answer matches those words. Try fewer words, or open the page finder with / to go straight to a page.');
  const count = h('span', { class: 'faint qa-count' }, `${items.length} answers`);
  const filterIn = h('input', { class: 'input', type: 'search', placeholder: 'Find an answer', 'aria-label': 'Find an answer' });
  on(filterIn, 'input', () => {
    const words = filterIn.value.toLowerCase().split(/\s+/).filter(Boolean);
    let shown = 0;
    for (const it of items) { const ok = words.every((w) => it.hay.includes(w)); it.el.hidden = !ok; if (ok) shown += 1; }
    none.hidden = shown > 0;
    count.textContent = words.length ? `${shown} of ${items.length} answers` : `${items.length} answers`;
  });
  const qaPanel = panel({ title: 'Questions and answers', icon: 'question', cls: 'qa-panel', id: 'qa',
    side: h('div', { class: 'qa-tools' }, count, h('label', { class: 'field field--search' }, icon('search'), filterIn)) },
  h('dl', { class: 'qa' }, items.map((i) => i.el)), none);

  replace(ctx.view,
    pageHead({ title: ctx.info.title, purpose: ctx.info.purpose }),
    h('div', { class: 'help-grid' }, aboutPanel, licPanel, creditPanel),
    qaPanel);
  ctx.ready();
  if (location.hash) document.getElementById(location.hash.slice(1))?.scrollIntoView();
}
