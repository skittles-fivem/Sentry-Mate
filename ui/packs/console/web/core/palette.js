// The command list (Ctrl/Cmd+K or '/'): a fuzzy list of every page, every car, the current car's recent drives
// and charges, and the settings keys. Arrow keys move, Enter opens, Esc closes. It fades in over 120 ms
// (off with prefers-reduced-motion).

import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { PAGES } from './pages.js';
import { openDialog } from './dialog.js';
import { dateTime, numText, slug } from './fmt.js';
import { defaultZone } from './router.js';
import { navCar } from './frame.js';

/** Settings keys: [key, page, anchor, words]. Pages give the line the same id, so the link lands on it. */
export const SETTINGS_KEYS = Object.freeze([
  ['display.units.distance', 'settings-display', 'units', 'km miles'],
  ['display.units.temperature', 'settings-display', 'units', 'celsius fahrenheit'],
  ['display.units.pressure', 'settings-display', 'units', 'bar psi tyre'],
  ['display.range', 'settings-display', 'range', 'rated ideal'],
  ['display.theme', 'settings-display', 'theme', 'dark light auto'],
  ['display.ui_pack', 'settings-display', 'pack', 'interface look'],
  ['display.interface_size', 'settings-display', 'size', 'zoom bigger smaller'],
  ['display.language', 'settings-display', 'language', 'locale dates'],
  ['cars.name', 'settings-cars', 'name', 'rename'],
  ['cars.time_zone', 'settings-cars', 'time-zone', 'zone clock'],
  ['cars.upload_token', 'settings-cars', 'tokens', 'can bridge'],
  ['integrations.teslafi', 'settings-integrations', 'teslafi', 'teslafi token pace'],
  ['integrations.teslamate', 'settings-integrations', 'teslamate', 'local teslamate mqtt database'],
  ['integrations.mqtt', 'settings-integrations', 'mqtt', 'broker'],
  ['integrations.home_assistant', 'settings-integrations', 'home-assistant', 'discovery'],
  ['integrations.api', 'settings-integrations', 'api', 'token access'],
  ['server.accounts', 'settings-server', 'accounts', 'users password'],
  ['server.move', 'settings-server', 'move', 'move code new server'],
  ['server.export', 'settings-server', 'export', 'backup file'],
  ['server.certificate', 'settings-server', 'certificate', 'https tls'],
  ['import.file', 'import', 'file', 'teslamate history'],
]);

/** Fuzzy score of query q against text: higher is better, -1 is no match. Letters in order; runs and starts count more. */
export function fuzzy(q, text) {
  q = q.toLowerCase(); text = text.toLowerCase();
  if (!q) return 0;
  let score = 0, ti = 0, run = 0;
  for (const ch of q) {
    if (ch === ' ') continue;
    const at = text.indexOf(ch, ti);
    if (at < 0) return -1;
    run = at === ti ? run + 1 : 0;
    score += 1 + run * 2 + (at === 0 || /[\s/._-]/.test(text[at - 1]) ? 3 : 0) - Math.min(3, (at - ti) * 0.1);
    ti = at + 1;
  }
  return score - text.length * 0.01;
}

function pageItems() {
  const s = state.current();
  const car = s.car ?? navCar();   // the page's car, else the person's preferred car
  const out = [];
  for (const p of PAGES) {
    if (p.bare || p.id === 'drive' || p.id === 'charge') continue;
    if (p.car === 'required' && !car) continue;
    if (p.id === 'health') {
      if (car) out.push({ kind: 'page', label: 'health', href: state.linkTo('health', { car }), path: `cars/${car}/health`, words: p.words });
      out.push({ kind: 'page', label: 'system', href: '/system', path: 'system', words: 'server storage disk' });
      continue;
    }
    const href = state.linkTo(p.id, p.car && car ? { car } : {});
    out.push({ kind: 'page', label: p.id.replace('settings-', 'settings '), href, path: href.split('?')[0].slice(1) || 'fleet', words: `${p.name} ${p.words} ${p.desc}` });
  }
  for (const [key, page, anchor, words] of SETTINGS_KEYS) {
    out.push({ kind: 'key', label: key, href: `${state.linkTo(page)}#${anchor}`, path: page.replace('settings-', 'settings/'), words });
  }
  return out;
}

export function openPalette() {
  if (document.querySelector('dialog.dlg--palette')) return null;
  const q = h('input', { class: 'in in--wide', type: 'text', placeholder: 'page, car, drive, charge or settings key', 'aria-label': 'Command', autocomplete: 'off', spellcheck: 'false', role: 'combobox', 'aria-expanded': 'true', 'aria-controls': 'pal-list' });
  const list = h('ul', { class: 'pick pal-list', id: 'pal-list', role: 'listbox', 'aria-label': 'Matches' });
  const base = pageItems();
  let cars = [], recent = [], sel = 0, shownItems = [];
  const s = state.current();
  const car = s.car ?? navCar();   // the page's car, else the person's preferred car

  const row = (it, i) => h('li', { class: 'pick-row', role: 'option', 'aria-selected': String(i === sel), id: `pal-${i}` },
    h('a', { class: 'pick-link', href: it.href, onclick: () => { if (it.pick) state.rememberCar(it.pick.id, { sold: state.isSold(it.pick) }); d.close(); } },
      h('span', { class: ['pal-kind', `pal-kind--${it.kind}`] }, `[${it.kind}]`),
      h('span', { class: 'pick-main' }, it.label),
      h('span', { class: 'pal-path' }, it.path)));

  function paint() {
    const text = q.value.trim();
    const all = [...base, ...cars, ...recent];
    const scored = text ? all.map((it) => [fuzzy(text, `${it.label} ${it.path}`), Math.max(fuzzy(text, it.label), fuzzy(text, it.words || '') - 2), it])
      .map(([a, b, it]) => [Math.max(a, b), it]).filter(([sc]) => sc >= 0).sort((a, b) => b[0] - a[0]).map(([, it]) => it) : all;
    shownItems = scored.slice(0, 60);
    sel = Math.min(sel, Math.max(0, shownItems.length - 1));
    replace(list, shownItems.length ? shownItems.map(row) : h('li', { class: 'pick-note' }, '-- nothing matches --'));
    q.setAttribute('aria-activedescendant', shownItems.length ? `pal-${sel}` : '');
  }
  const mark = (i) => {
    if (!shownItems.length) return;
    sel = (i + shownItems.length) % shownItems.length;
    for (const [k, x] of [...list.children].entries()) x.setAttribute('aria-selected', String(k === sel));
    list.children[sel]?.scrollIntoView({ block: 'nearest' });
    q.setAttribute('aria-activedescendant', `pal-${sel}`);
  };

  let ct = 0, cseq = 0;
  async function loadCars() {
    const my = ++cseq;
    try {
      const p = await api.page('cars', { sort: 'name', search: q.value.trim() || undefined }, { limit: 20 });
      if (my !== cseq) return;
      // sold cars only when searched for (or the one open), marked sold
      const text = q.value.trim();
      cars = p.items.filter((c) => text || !state.isSold(c) || c.id === s.car)
        .map((c) => ({ kind: 'car', pick: c, label: state.isSold(c) ? `${c.name || c.id} (sold)` : c.name || c.id, href: state.linkForCar(c.id), path: `cars/${slug(c.name)}`, words: `${c.model || ''} ${c.id}${state.isSold(c) ? ' sold' : ''}` }));
      paint();
    } catch { /* the list works without cars */ }
  }
  async function loadRecent() {
    if (!car) return;
    const tz = defaultZone();
    try {
      const [dr, ch] = await Promise.all([
        api.page(api.apiPath`cars/${car}/drives`, {}, { limit: 5 }).catch(() => ({ items: [] })),
        api.page(api.apiPath`cars/${car}/charges`, {}, { limit: 5 }).catch(() => ({ items: [] })),
      ]);
      recent = [
        ...dr.items.map((x) => ({ kind: 'drive', label: `${dateTime(x.start_ms, tz)}  ${numText(x.distance_km, 'distance')}`, href: state.buildUrl('drive', { car, drive: x.id }), path: `drives/${x.id}`, words: 'drive recent' })),
        ...ch.items.map((x) => ({ kind: 'charge', label: `${dateTime(x.start_ms, tz)}  ${numText(x.energy_added_kwh, 'energy')}`, href: state.buildUrl('charge', { car, charge: x.id }), path: `charges/${x.id}`, words: 'charge recent' })),
      ];
      paint();
    } catch { /* fine */ }
  }

  q.addEventListener('input', () => { sel = 0; paint(); clearTimeout(ct); ct = setTimeout(loadCars, 150); });
  q.addEventListener('keydown', (e) => {
    if (e.key === 'ArrowDown' || (e.ctrlKey && e.key === 'n')) { e.preventDefault(); mark(sel + 1); }
    else if (e.key === 'ArrowUp' || (e.ctrlKey && e.key === 'p')) { e.preventDefault(); mark(sel - 1); }
    else if (e.key === 'Enter') { e.preventDefault(); list.children[sel]?.querySelector('a')?.click(); }
  });
  const d = openDialog({
    title: 'command', cls: 'dlg--palette', label: 'Command list',
    body: [h('div', { class: 'pick-q' }, h('span', { class: 'prompt' }, '>'), q), list,
      h('p', { class: 'pal-help' }, h('span', { class: 'kbd' }, 'up'), ' ', h('span', { class: 'kbd' }, 'down'), ' move  ', h('span', { class: 'kbd' }, 'enter'), ' open  ', h('span', { class: 'kbd' }, 'esc'), ' close')],
  });
  paint();
  q.focus();
  loadCars();
  loadRecent();
  return d;
}
