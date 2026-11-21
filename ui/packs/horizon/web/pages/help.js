// Help and about (/help): the version on a radio face, the opt-in release notice, the source and the guides,
// the licences of Sentry Mate and of every part this pack ships (each licence text served from this server),
// the data credits, a Download notices button, and short answers with a filter.
// GET /about for versions, licences and credits; GET /notices for the release notice (only there when the
// owner turned the release check on); GET /settings (admins) to say whether the release check is on.

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { icon, hud, plate, btn, tag, kv, note } from '../core/ui.js';
import { band, search, csvBtn } from '../core/kit.js';
import { getNotices, onNotices } from '../core/notices.js';
import { toast } from '../core/layer.js';
import { date } from '../core/fmt.js';
import { presets, sig, code, downloadText } from '../core/setting.js';

export const id = 'help';
export const css = true;

// what this pack ships, with the licence file served next to it
const SHIPPED = [
  { name: 'MapLibre GL JS', use: 'Draws the maps', licence: 'BSD-3-Clause', href: '/vendor/maplibre-gl/LICENSE.txt' },
  { name: 'uPlot', use: 'Draws the charts', licence: 'MIT', href: '/vendor/uplot/LICENSE.txt' },
  { name: 'Orbitron', use: 'Display font (headings, the radio)', licence: 'SIL Open Font License 1.1', href: '/fonts/LICENSE-Orbitron.txt' },
  { name: 'Chakra Petch', use: 'Text font', licence: 'SIL Open Font License 1.1', href: '/fonts/LICENSE-ChakraPetch.txt' },
  { name: 'JetBrains Mono', use: 'Font for figures and codes', licence: 'Apache-2.0', href: '/fonts/LICENSE-JetBrainsMono.txt' },
];

// short answers: [question, answer, page, link words, more words that find it]
const QA = [
  ['Why is a car missing?', 'A car appears when its CAN Bridge uploads its first log. If it does not, check the upload token in the CAN Bridge app, and look for a key waiting to be tied to a car. A watch-only account sees only the cars an admin gave it.', 'settings-cars', 'Car settings', 'vehicle list token key'],
  ['Why is the data a minute or more late?', 'The CAN Bridge sends finished logs, about a minute after the car records them. With no network in the car, logs wait on the device and arrive later; the pages fill in by themselves when they do.', 'health', 'Data health', 'delay slow upload offline'],
  ['What is the difference between asleep and no data?', 'Asleep means the car went quiet and the CAN Bridge saw it do so. No data means no log covers that time: the CAN Bridge was off, recording was paused, or a log failed its check. No data is never counted as asleep, and charts never carry a value across it.', 'timeline', 'Timeline', 'gap unknown sleep hatched'],
  ['How is a CAN Bridge tied to a car?', 'Make an upload token for the car in Car settings and type it into the CAN Bridge app. The CAN Bridge signs every log with its own key; the first key is tied to the car, and other keys wait until an admin ties them.', 'settings-cars', 'Car settings', 'pair token key sign'],
  ['Does Sentry Mate wake the car?', 'No. It only listens to the messages the car sends anyway, so it never wakes the car, keeps it awake or sends it a command.', 'car', 'Cluster', 'sleep drain command'],
  ['Where do I change units, or rated against ideal range?', 'Each person picks distance, temperature and pressure units and the range type once; every page then uses them. The server keeps metric values, so units never change your data.', 'settings-display', 'Display and units', 'miles km fahrenheit psi bar range'],
  ['How do I pick the dates a page shows?', 'Turn the period dial under the dashboard strip. It works in the car\'s own time zone, and the period stays when you tune to another page or car.', 'statistics', 'Statistics', 'period range date month year time zone'],
  ['Can I take my data out?', 'Yes. Every list and chart has a CSV button that uses your units, and drives also save as GPX tracks.', 'drives', 'Drives', 'export csv gpx download'],
  ['Can I bring my TeslaMate history?', 'Yes. An admin imports a TeslaMate backup. Imported rows are marked as imported, and where they overlap with CAN data you choose which one is shown.', 'import', 'Import', 'teslamate backup overlap'],
  ['Why does an address show as a position?', 'Addresses are looked up offline from the place names on the server. Where that data has nothing close, the spot shows its position; you can name it as a place.', 'places', 'Places', 'address geocode location place'],
  ['The background moves too much. Can I calm it?', 'Yes: Background in Display and units (or the account knob) has Auto, Full, Calm and Off, kept in each browser. A device that asks for less motion always gets a still picture.', 'settings-display', 'Display and units', 'sky grid sun animation motion background'],
  ['How do I move Sentry Mate to another server?', 'An admin makes a move code (both servers on) or an export file in Server and access. The new server takes either on its first page; no shell is needed.', 'settings-server', 'Server and access', 'move export restore backup recovery key'],
];

const outside = (u) => /^https?:\/\//i.test(u || '');
const outLink = (href, text) => h('a', { class: 'plc', href, target: '_blank', rel: 'noopener noreferrer external' }, text, ' ', icon('ext', 'i--sm'));
const hostOf = (u) => { try { return new URL(String(u).replace(/[{}]/g, '')).hostname || null; } catch { return null; } };

function lookups(settings) {
  if (!settings) return 'Where maps and addresses come from is set by an admin in the server settings.';
  const tilesOn = settings.tiles?.mode === 'online', geoOn = settings.geocode?.mode === 'online';
  if (!tilesOn && !geoOn) return 'Maps and addresses come from data stored on this server; nothing is looked up on another site.';
  return `${tilesOn ? `Map pictures come from ${hostOf(settings.tiles.online_url) || 'an outside tile service'}` : 'Map pictures come from data on this server'}; ${geoOn ? `addresses are looked up at ${hostOf(settings.geocode.url) || 'an outside address service'}` : 'addresses come from data on this server'}.`;
}

function notices(about) {
  const lines = [`Sentry Mate ${about.version || ''}`.trim(), `UI pack: ${about.ui_pack?.name || '-'} ${about.ui_pack?.version || ''}`.trim(), ''];
  lines.push(about.licence?.name ? `Sentry Mate is licensed under ${about.licence.name}.` : 'The licence of Sentry Mate itself is not published with this version yet.', '');
  lines.push('Parts shipped with this UI pack:');
  for (const p of SHIPPED) lines.push(`- ${p.name} (${p.use}): ${p.licence}. Full text: ${p.href}`);
  if ((about.licences || []).length) { lines.push('', 'Parts used by the server:'); for (const l of about.licences) lines.push(`- ${l.name}: ${l.licence}`); }
  if ((about.data_credits || []).length) { lines.push('', 'Data credits:'); for (const c of about.data_credits) lines.push(`- ${c.name}: ${c.text}${c.licence ? ` (${c.licence})` : ''}`); }
  return `${lines.join('\r\n')}\r\n`;
}

export async function mount(ctx) {
  let about, list, settings = null, cars = null;
  const needCar = !(ctx.state.car ?? state.lastCar());
  try {
    [about, list, settings, cars] = await Promise.all([
      ctx.api.get('about', null, { maxAgeMs: 600000 }),
      ctx.api.get('notices').then((r) => r.items).catch(() => getNotices()),
      ctx.admin ? ctx.api.get('settings').catch(() => null) : null,
      needCar ? ctx.api.get('cars', { limit: 2 }).then((r) => r.items || []).catch(() => null) : null,
    ]);
  } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;

  const pack = about.ui_pack || {};
  const mismatch = pack.api != null && about.api_version != null && String(pack.api) !== String(about.api_version);

  /* ---------- release notice */
  const releaseSlot = h('div', { class: 'hp-dd' });
  const paintRelease = (items) => {
    const n = (items || []).find((x) => x.code === 'release_available');
    const on = settings?.release_check?.enabled;
    const turn = ctx.admin ? h('a', { class: 'plc', href: state.buildUrl('settings-server') }, 'Release check in Server and access') : null;
    if (n) replace(releaseSlot, sig('ok', n.message || 'A newer release is out'), h('p', { class: 'chan-hint' }, n.since_ms ? `Reported ${date(n.since_ms, ctx.tz, { year: true })}. ` : '', 'Your data stays as it is when you update the container. ', turn));
    else if (on === false) replace(releaseSlot, sig('off', 'Not checked'), h('p', { class: 'chan-hint' }, 'The release check is off, so Sentry Mate never asks the internet about new releases. ', turn));
    else if (on === true) replace(releaseSlot, sig('ok', 'This is the newest release'), h('p', { class: 'chan-hint' }, 'The release check is on and found nothing newer. ', turn));
    else replace(releaseSlot, sig('off', 'No newer release reported'), h('p', { class: 'chan-hint' }, 'Shown only when an admin turns the release check on; until then Sentry Mate does not look.'));
  };
  paintRelease(list);
  ctx.onCleanup(onNotices((items) => { if (ctx.alive()) paintRelease(items); }));

  const source = about.links?.source, docs = about.links?.docs;
  const aboutPanel = hud({ title: 'About', icon: 'info', id: 'about', tone: 'cyan' },
    h('div', { class: 'hp-about' },
      h('div', { class: 'hp-face', role: 'img', 'aria-label': `Sentry Mate ${about.version || ''}`, cover: 'web.settings.about.version' },
        h('span', { class: 'hp-face-l' }, 'Sentry Mate'),
        h('span', { class: 'hp-face-v' }, about.version || '—'),
        h('span', { class: 'hp-face-s' }, `API v${about.api_version ?? '?'} · ${pack.name || 'Horizon'} ${pack.version || ''}`.trim())),
      kv([
        ['Version', h('span', { class: 'tagline' }, code(about.version || '—'), about.dev_server ? tag('Dev server, made-up data', 'org') : null), { cover: 'web.settings.about.version' }],
        ['API', h('span', null, code(`/api/v${about.api_version ?? '?'}`), ' · what UI packs talk to')],
        ['UI pack', h('span', { class: mismatch ? 'warn' : null }, `${pack.name || '—'} ${pack.version || ''}`.trim(), mismatch ? ` · made for API ${pack.api}, but the server speaks API ${about.api_version}: some pages may not work.` : ` · made for API ${pack.api ?? about.api_version}, which this server speaks.`)],
        ['Newer release', releaseSlot, { cover: 'web.settings.about.update-available' }],
        ['Source code', h('div', { class: 'hp-dd' }, source ? outLink(source, source.replace(/^https?:\/\//i, '')) : h('span', null, 'Not published yet'), h('p', { class: 'chan-hint' }, source ? 'Opens in a new tab.' : 'The public address goes here once the code is published.')), { cover: 'web.settings.about.source-link' }],
        ['Guides', h('div', { class: 'hp-dd' }, outside(docs) ? outLink(docs, 'Documentation site') : h('a', { class: 'plc', href: '#qa' }, 'Questions and answers below'),
          h('p', { class: 'chan-hint' }, outside(docs) ? 'Install, upgrade and the API for UI packs. Opens in a new tab.' : source ? 'Guides for install, upgrade and UI packs are in the docs folder of the source code.' : 'The written guides come with the source code once it is published.')), { cover: 'web.settings.about.docs-link' }],
      ])));

  /* ---------- licences */
  const rows = [
    ...SHIPPED.map((p) => ({ part: p.name, use: p.use, licence: p.licence, by: 'UI pack', text: p.href })),
    ...(about.licences || []).map((l) => ({ part: l.name, use: 'Car signal list', licence: l.licence, by: 'Server', text: '' })),
  ];
  const own = about.licence;
  const licPanel = hud({ title: 'Licences and notices', icon: 'book', id: 'licences',
    side: csvBtn({ name: ['sentry-mate', 'licences'], rows, columns: [{ label: 'Part', value: (r) => r.part }, { label: 'Used for', value: (r) => r.use }, { label: 'Licence', value: (r) => r.licence }, { label: 'Shipped with', value: (r) => r.by }, { label: 'Licence text', value: (r) => r.text }] }) },
  h('div', { class: 'stack' },
    h('div', { class: 'hp-own', cover: 'web.legal.license' }, h('b', null, 'Sentry Mate itself'),
      own?.name ? h('span', null, own.name, ' ', own.url || own.href ? h('a', { class: 'plc', href: own.url || own.href, target: '_blank', rel: 'noopener' }, 'Full text') : null) : h('span', { class: 'faint' }, 'Its own licence is not published with this version yet.')),
    h('ul', { class: 'hp-parts', cover: 'web.legal.license' },
      SHIPPED.map((p) => h('li', { class: 'hp-part' }, h('span', { class: 'hp-part-n' }, p.name), h('span', { class: 'hp-part-u' }, p.use), tag(p.licence, 'cyan'), h('a', { class: 'plc', href: p.href, target: '_blank', rel: 'noopener' }, 'Licence text'))),
      (about.licences || []).map((l) => h('li', { class: 'hp-part' }, h('span', { class: 'hp-part-n' }, l.name), h('span', { class: 'hp-part-u' }, 'Decodes the car\'s messages on the server'), tag(l.licence, 'cyan'), h('span', { class: 'faint small' }, 'Server')))),
    note('Each part keeps its own licence. The full texts open as plain files from this server, never from another site.'),
    h('div', { class: 'row' },
      btn('Download notices', () => { downloadText('sentry-mate-notices.txt', notices(about)); toast('Notices saved as a text file.'); }, { icon: 'download', sm: true, cover: 'web.legal.notice' }),
      h('span', { class: 'chan-hint' }, 'Every licence and credit on this page in one plain text file, for this exact version.'))));

  /* ---------- data credits */
  const credits = about.data_credits || [];
  const creditPanel = hud({ title: 'Data credits', icon: 'globe', id: 'credits' },
    h('div', { class: 'stack' },
      credits.length ? h('ul', { class: 'hp-parts' }, credits.map((c) => h('li', { class: 'hp-part' }, h('span', { class: 'hp-part-n' }, c.name), h('span', { class: 'hp-part-u' }, c.text), c.licence ? tag(c.licence, 'cyan') : null)))
        : note('This server uses no outside data that needs a credit.'),
      note('Car signals are decoded with the signal lists named under Licences. ', lookups(settings))));

  /* ---------- questions and answers */
  const link = (page, label) => {
    let href = state.linkTo(page), text = `Open ${label}`;
    if (href === state.buildUrl('fleet') && page !== 'fleet') { if (cars && cars.length === 1) href = state.linkTo(page, { car: cars[0].id }); else text = `Pick a car, then open ${label}`; }
    return h('a', { class: 'plc nowrap', href }, text, ' ', icon('chev-r', 'i--sm'));
  };
  const items = QA.map(([q, a, page, label, words], i) => ({
    el: h('li', { class: 'hp-qa' }, h('span', { class: 'hp-qa-n' }, String(i + 1).padStart(2, '0')), h('div', { class: 'hp-qa-body' }, h('h3', { class: 'hp-qa-q' }, q), h('p', { class: 'hp-qa-a' }, a), link(page, label))),
    hay: `${q} ${a} ${label} ${words}`.toLowerCase(),
  }));
  const none = h('p', { class: 'note', hidden: true }, icon('info', 'i--sm'), h('span', null, 'No answer has those words. Try fewer words, or press / to tune straight to a page.'));
  const countEl = h('span', { class: 'faint small nowrap' }, `${items.length} answers`);
  const filter = (v) => {
    const words = v.toLowerCase().split(/\s+/).filter(Boolean);
    let n = 0;
    for (const it of items) { const ok = words.every((w) => it.hay.includes(w)); it.el.hidden = !ok; if (ok) n++; }
    none.hidden = n > 0;
    countEl.textContent = words.length ? `${n} of ${items.length} answers` : `${items.length} answers`;
  };
  const qaPanel = hud({ title: 'Questions and answers', icon: 'question', id: 'qa', side: [countEl, search('', filter, { placeholder: 'Find an answer', ctx })] },
    h('div', { class: 'stack' }, h('ol', { class: 'hp-qas' }, items.map((i) => i.el)), none));

  replace(ctx.view, h('div', { class: 'page hp' },
    plate({ kicker: 'Set-up · help', title: 'Help and about', aim: ctx.info.aim }),
    h('section', { class: 'hud hud--plain jump', 'aria-label': 'Sections' }, presets([['about', 'About'], ['licences', 'Licences'], ['credits', 'Data credits'], ['qa', 'Questions']]),
      h('p', { class: 'note jump-note' }, icon('question', 'i--sm'), h('span', null, 'Press / anywhere to find a page by what it holds, and [ or ] to tune to the page before or after.'))),
    band(1, 'This server'),
    h('div', { class: 'cols' }, aboutPanel, h('div', { class: 'stack' }, licPanel, creditPanel)),
    band(2, 'Short answers'),
    qaPanel));
  ctx.ready();
}
