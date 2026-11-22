// Help and about (/help): versions, the opt-in release check, links, licences and notices, data credits,
// and short answers to common questions.
// GET /api/v1/about for the versions (and this build), licences and credits; GET /api/v1/release for the
// release check's last answer; GET /api/v1/settings (admins only) for whether release-notes links are allowed.

import * as state from '../shared/state.js';
import { h, replace, on } from '../shared/dom.js';
import { downloadCsv, csvFilename } from '../shared/csv.js';
import { icon } from '../core/icons.js';
import { pageHead, panel, kv, pill, csvButton, appLink } from '../core/page.js';
import { getNotices, onNotices } from '../core/notices.js';
import { buildLabel, releaseStatus, changeLines, checkedAgo } from '../shared/release.js';
import { flash } from '../core/flash.js';

export const id = 'help';
export const css = true;

// The third-party parts this pack ships, with their licence files (served from the pack itself).
const BUNDLED = [
  { name: 'MapLibre GL JS', what: 'Draws the maps', licence: 'BSD-3-Clause', href: '/vendor/maplibre-gl/LICENSE.txt' },
  { name: 'uPlot', what: 'Draws the charts', licence: 'MIT', href: '/vendor/uplot/LICENSE.txt' },
  { name: 'Inter', what: 'Text font', licence: 'SIL Open Font License 1.1', href: '/fonts/LICENSE-Inter.txt' },
  { name: 'JetBrains Mono', what: 'Font for figures and codes', licence: 'Apache-2.0', href: '/fonts/LICENSE-JetBrainsMono.txt' },
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

const hostOf = (u) => { try { return new URL(String(u).replace(/[{}]/g, '')).hostname || null; } catch { return null; } };

/**
 * Where maps and addresses come from, read from the settings (admins only). Viewers who cannot read the
 * settings get a sentence that claims neither.
 */
function lookupSentence(settings) {
  if (!settings) return ['Where maps and addresses come from is set by an admin in the server settings.'];
  const tilesOn = settings.tiles?.mode === 'online';
  const geoOn = settings.geocode?.mode === 'online';
  if (!tilesOn && !geoOn) return ['Maps and addresses come from data stored on this server; nothing is looked up on another site.'];
  const parts = [];
  if (tilesOn) parts.push(`Map tiles come from ${hostOf(settings.tiles.online_url) || 'an outside tile service'}`);
  else parts.push('Map tiles come from data stored on this server');
  if (geoOn) parts.push(`addresses are looked up at ${hostOf(settings.geocode.url) || 'an outside address service'}`);
  else parts.push('addresses come from data stored on this server');
  return [`${parts.join('; ')}. Change this in `, appLink('settings-integrations', 'Settings: Integrations'), '.'];
}

/**
 * A link to the page an answer is about. Car pages need a car: with none chosen yet, use the only car if
 * there is exactly one, else say that a car must be chosen first (the link then goes to All cars).
 */
function carLink(page, label, cars) {
  let href = state.linkTo(page);
  let text = `Open ${label}`;
  if (href === state.buildUrl('fleet') && page !== 'fleet') {
    if (cars && cars.length === 1) href = state.linkTo(page, { car: cars[0].id });
    else text = `Choose a car, then open ${label}`;
  }
  return h('a', { class: 'link nowrap', href }, text, icon('chev-r', 'ico--sm'));
}

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
  let about, notices, settings = null, cars = null, release = null;
  const needCar = !(ctx.state.car ?? state.lastCar());
  try {
    [about, notices, settings, cars, release] = await Promise.all([
      ctx.api.get('about', null, { maxAgeMs: 600000 }),
      ctx.api.get('notices').then((r) => r.items).catch(() => getNotices()),
      ctx.admin ? ctx.api.get('settings').catch(() => null) : Promise.resolve(null),
      // Only when no car is chosen yet: with exactly one car, links to car pages can name it.
      needCar ? ctx.api.get('cars', { limit: 2 }).then((r) => r.items || []).catch(() => null) : Promise.resolve(null),
      ctx.api.get('release').catch(() => null),
    ]);
  } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;

  // ---- About
  const pack = about.ui_pack || {};
  const apiMismatch = pack.api != null && about.api_version != null && String(pack.api) !== String(about.api_version);
  const versionCell = h('div', { class: 'help-version' },
    h('span', { class: 'help-version-main mono' }, buildLabel(about, ctx.api.now()) || '—'),
    about.dev_server ? pill('Dev server, made-up data', { tone: 'warm', ic: 'warn' }) : null);

  const releaseCell = h('div', { class: 'help-release' });
  const paintRelease = (list) => {
    const now = ctx.api.now();
    const n = (list || []).find((x) => x.code === 'release_available');
    const turnLink = ctx.admin ? h('a', { class: 'link', href: `${state.buildUrl('settings-display')}#release-check` }, 'Release check in Settings: Display and units') : null;
    const st = release ? releaseStatus(release, now) : null;
    const when = release ? checkedAgo(release, now) : '';
    if (!release) {
      replace(releaseCell, h('span', null, 'Could not read the release check'), h('span', { class: 'explain' }, 'The server did not answer; try again later.'));
    } else if (release.status === 'newer') {
      // the notes link only while an admin allows release-notes links (settings.outside_links.release_notes)
      const notes = settings?.outside_links?.release_notes ? (n?.detail?.notes_url || release.newest?.compare_url) : null;
      const lines = changeLines(release.newest);
      replace(releaseCell,
        h('span', { class: 'ok help-release-head' }, icon('upload', 'ico--sm'), ' ', st.text),
        lines.length ? h('ul', { class: 'help-release-titles' }, lines.map((x) => h('li', null, x))) : null,
        release.pull ? h('span', { class: 'explain' }, 'To update, run on the server: ', h('code', { class: 'mono help-release-pull' }, release.pull)) : null,
        h('span', { class: 'explain' }, when ? `${when[0].toUpperCase()}${when.slice(1)}. ` : '', 'Your data stays as it is when you update the container.'),
        isOutside(notes) ? h('span', { class: 'explain' }, outLink(notes, 'What changed')) : null,
        turnLink ? h('span', { class: 'explain' }, turnLink) : null);
    } else if (release.status === 'up_to_date') {
      replace(releaseCell, h('span', { class: 'ok' }, 'Up to date'),
        h('span', { class: 'explain' }, when ? `The release check is on and has found nothing newer; ${when}. ` : 'The release check is on and has found nothing newer. ', turnLink));
    } else if (release.status === 'error') {
      replace(releaseCell, h('span', { class: 'warn' }, st.text),
        h('span', { class: 'explain' }, when ? `${when[0].toUpperCase()}${when.slice(1)}. ` : '', 'It tries again at the next daily check. ', turnLink));
    } else if (release.status === 'never' || release.status === 'checking') {
      replace(releaseCell, h('span', null, st.text),
        h('span', { class: 'explain' }, 'The release check is on; its first answer shows here. ', turnLink));
    } else {
      replace(releaseCell, h('span', null, ctx.admin ? 'Not checked' : 'Off'),
        h('span', { class: 'explain' }, 'The release check is off, so Sentry Mate never asks the internet about new releases. ', turnLink));
    }
  };
  paintRelease(notices);
  ctx.onCleanup(onNotices(async (list) => {
    release = await ctx.api.get('release').catch(() => release);
    if (ctx.alive()) paintRelease(list);
  }));

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
    h('p', { class: 'note' }, 'Car signals are decoded with the signal lists named under Licences. ', ...lookupSentence(settings)));

  // ---- Questions and answers, with a filter
  const items = QA.map(([q, a, page, label, words]) => {
    const el = h('div', { class: 'qa-item' }, h('dt', null, q), h('dd', null, a, ' ', carLink(page, label, cars)));
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
