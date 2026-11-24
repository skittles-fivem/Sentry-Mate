// A place's dated prices (docs/ui-api.md, "Places": `prices`), for every pack: the rules (which price is in force on a
// day, a typed price wins over one from invoices, what an entry needs) and the place price editor, a dialog that
// lists the prices (from date, price, who set it) with add, edit and remove, and saves the typed ones with
// PATCH /places/{place} `{ version, prices }`. The places page and the "Set price" shortcuts (Charges: Charging
// places, a charge's place) open the same editor.
//
// A dated entry holds any price the API takes: the editor changes only the fields it shows and keeps every other
// field of the price as it came (so a newer kind of price survives an edit here).

import { h, replace } from './dom.js';
import { format, formatMoney } from './units.js';

export const CURRENCIES = ['USD', 'CAD', 'EUR', 'GBP', 'NOK', 'SEK', 'DKK', 'CHF', 'AUD', 'NZD', 'JPY', 'CNY'];
const DAY_RX = /^\d{4}-\d{2}-\d{2}$/;
const HM_RX = /^(([01]\d|2[0-3]):[0-5]\d|24:00)$/;
const DAY_CHOICES = [['all', 'Every day'], ['mon-fri', 'Weekdays'], ['sat,sun', 'Weekends'],
  ['mon', 'Mon'], ['tue', 'Tue'], ['wed', 'Wed'], ['thu', 'Thu'], ['fri', 'Fri'], ['sat', 'Sat'], ['sun', 'Sun']];

/* ------------------------------------------------------------------ rules */

/** `YYYY-MM-DD` of a moment in a time zone (the browser's when none). */
export function dayIn(ms, timeZone) {
  const opt = { year: 'numeric', month: '2-digit', day: '2-digit' };
  let parts;
  try { parts = new Intl.DateTimeFormat('en-CA', { ...opt, timeZone: timeZone || undefined }).formatToParts(new Date(ms)); } catch { parts = new Intl.DateTimeFormat('en-CA', opt).formatToParts(new Date(ms)); }
  const g = (t) => parts.find((p) => p.type === t)?.value;
  return `${g('year')}-${g('month')}-${g('day')}`;
}

/** A real calendar day as YYYY-MM-DD. */
export function isDay(s) {
  if (typeof s !== 'string' || !DAY_RX.test(s)) return false;
  const d = new Date(`${s}T00:00:00Z`);
  return !Number.isNaN(d.getTime()) && d.toISOString().slice(0, 10) === s;
}

/** A place's dated prices, oldest first (one from the start first; typed before invoice on one day). A place that has
 *  only `price` (an older server) has it from the start. */
export function pricesOf(place) {
  if (!place) return [];
  const list = Array.isArray(place.prices) ? place.prices : place.price ? [{ from: null, source: 'typed', price: place.price }] : [];
  return sortPrices(list);
}

export function sortPrices(list) {
  return [...list].sort((a, b) => (a.from || '').localeCompare(b.from || '') || (srcOf(a) === 'typed' ? 0 : 1) - (srcOf(b) === 'typed' ? 0 : 1));
}

const srcOf = (x) => x.source || 'typed';

/** The price in force on `day`: the newest typed one that started by then, else the newest invoice one. */
export function priceOnDay(list, day) {
  for (const src of ['typed', 'invoice']) {
    let hit = null;
    for (const x of sortPrices(list)) if (srcOf(x) === src && (x.from == null || x.from <= day)) hit = x;
    if (hit) return hit;
  }
  return null;
}

/** Whether an invoice entry is used on any day: no typed price from the start covers it (an approximation the list
 *  uses to say "a typed price covers its days"). */
export function invoiceShadowed(list, entry) {
  const typed = sortPrices(list).filter((x) => srcOf(x) === 'typed');
  return typed.some((x) => x.from == null || (entry.from && x.from <= entry.from));
}

/** One line for a price: '$0.31/kWh · 3 tiered steps · 2 rates · $1.00 a session' (tiered steps are edited in the
 *  place's own form; this editor keeps them as they are). */
export function priceLine(price) {
  if (!price) return 'No price';
  const c = price.currency;
  const out = [];
  if (price.per_kwh != null) out.push(format(price.per_kwh, 'price_energy', { currency: c }));
  if (price.per_minute != null) out.push(format(price.per_minute, 'price_time', { currency: c }));
  const steps = (price.tiers?.steps || []).length;
  if (steps) out.push(steps === 1 ? '1 tiered step' : `${steps} tiered steps`);
  const n = (price.rates || []).length;
  if (n) out.push(n === 1 ? '1 time-of-use rate' : `${n} time-of-use rates`);
  if (price.session_fee != null) out.push(`${formatMoney(price.session_fee, c)} a session`);
  return out.join(' · ') || c || 'No price';
}

/** Who set a price in force, in words: 'from invoice' for one worked out from Supercharger invoices (`source` or
 *  `price_source` "invoice"), '' for a typed one or none. */
export const sourceWord = (source) => (source === 'invoice' ? 'from invoice' : '');

/** The price in force of a place (GET /places/{place}) or a charging place (GET /charging-stats/places), with who set
 *  it: { price, source, line, invoice } (invoice: the entry's { sessions } when it is from invoices), or null with no
 *  price. Uses `price_source` when the server gives it, else the dated entry in force today. */
export function priceInForce(place, now = Date.now()) {
  if (!place) return null;
  const price = place.place_price !== undefined ? place.place_price : place.price;
  if (!price) return null;
  let source = place.price_source || null;
  let entry = null;
  if (Array.isArray(place.prices)) {
    entry = priceOnDay(pricesOf(place), dayIn(now, place.time_zone));
    if (!source) source = entry ? srcOf(entry) : 'typed';
  }
  source = source || 'typed';
  return { price, source, line: priceLine(price), invoice: source === 'invoice' ? entry?.invoice || null : null };
}

/** A sentence for a place price from invoices, for an editor: "Today's price, $0.42/kWh, is from Supercharger
 *  invoices: the median of the 8 most recent sessions here. A price you type wins over it." ('' when typed). */
export function invoiceNote(place, now = Date.now()) {
  const f = priceInForce(place, now);
  if (!f || f.source !== 'invoice') return '';
  const n = f.invoice?.sessions;
  const of = n ? `: the median of the ${n === 1 ? 'one most recent session' : `${n} most recent sessions`} here` : '';
  return `Today's price, ${f.line}, is from Supercharger invoices${of}. A price you type wins over it.`;
}

/** The words for an entry's first day. */
export const fromWord = (from) => (from ? `From ${from}` : 'From the start');

/** Field errors of the typed entries before they are sent (keys as the server names them: prices.N.…). */
export function checkPrices(list) {
  const e = {};
  const seen = new Set();
  list.forEach((x, i) => {
    const k = `prices.${i}`;
    if (x.from != null && !isDay(x.from)) e[`${k}.from`] = 'A date as YYYY-MM-DD.';
    if (seen.has(x.from ?? '')) e[`${k}.from`] = x.from ? `Another price starts on ${x.from}.` : 'Only one price can be from the start.';
    seen.add(x.from ?? '');
    const p = x.price || {};
    if (!/^[A-Z]{3}$/.test(p.currency || '')) e[`${k}.price.currency`] = 'A currency code such as USD.';
    for (const f of ['per_kwh', 'per_minute']) if (p[f] != null && (!Number.isFinite(p[f]) || p[f] < -10 || p[f] > 100)) e[`${k}.price.${f}`] = 'From −10 to 100.';
    if (p.session_fee != null && (!Number.isFinite(p.session_fee) || p.session_fee < 0 || p.session_fee > 1000)) e[`${k}.price.session_fee`] = 'From 0 to 1,000.';
    (p.rates || []).forEach((r, j) => {
      if (!(r.name || '').trim()) e[`${k}.price.rates.${j}.name`] = `Rate ${j + 1} needs a name.`;
      for (const t of ['from', 'to']) if (!HM_RX.test(r[t] || '')) e[`${k}.price.rates.${j}.${t}`] = `Rate ${j + 1}: a time as HH:MM.`;
      if (r.per_kwh == null && r.per_minute == null) e[`${k}.price.rates.${j}.per_kwh`] = `Rate ${j + 1} needs a price.`;
    });
    if (p.per_kwh == null && p.per_minute == null && p.session_fee == null && !(p.rates || []).length && !hasOtherKind(p)) e[`${k}.price`] = 'Give a price per kWh, per minute, a rate or a session fee.';
  });
  return e;
}

const SHOWN = new Set(['currency', 'per_kwh', 'per_minute', 'session_fee', 'rates']);
/** A price that carries a kind this editor does not show (kept as it is). */
const hasOtherKind = (p) => Object.keys(p).some((k) => !SHOWN.has(k) && p[k] != null && !(Array.isArray(p[k]) && !p[k].length));

/** The typed entries as the PATCH body sends them. */
export const typedBody = (list) => sortPrices(list).filter((x) => srcOf(x) === 'typed').map((x) => ({ from: x.from ?? null, price: x.price }));

/** The most used currency of a list of places (USD when none). */
export function usualCurrency(places) {
  const n = {};
  for (const p of places || []) for (const x of pricesOf(p)) if (x.price?.currency) n[x.price.currency] = (n[x.price.currency] || 0) + 1;
  return Object.entries(n).sort((a, b) => b[1] - a[1])[0]?.[0] || 'USD';
}

/** A new place where none is saved yet (POST /places), for "Save place and set price". */
export function newPlaceBody({ name, lat, lon, kind = 'charger', radius_m = 100 }) {
  return { name: String(name || 'Charging place').trim().slice(0, 64) || 'Charging place', kind, lat, lon, radius_m };
}

/* ------------------------------------------------------------------ the editor */

/**
 * Open the place price editor. client: the API client; place: the place as GET /places/{place} gives it. cls: the
 * pack's class names { dialog, btn, primary, ghost, input, select } (each optional). Resolves with the saved place,
 * or null when nothing was saved. cls.plain: the class of a button that is neither primary nor ghost. A non-admin sees the prices and cannot change them.
 */
export function openPriceEditor({ client, place, admin = true, cls = {}, currency = null, now = Date.now() } = {}) {
  return new Promise((resolve) => {
    const C = { dialog: '', btn: 'btn', primary: 'btn--primary', ghost: 'btn--ghost', plain: '', input: 'input', select: 'select', ...cls };
    const back = document.activeElement;
    const today = dayIn(now, place.time_zone);
    let list = pricesOf(place).map((x) => ({ ...x, price: x.price ? JSON.parse(JSON.stringify(x.price)) : null }));
    let editing = null; // { index | null for new, draft }
    let dirty = false;
    let saved = null;
    const btn = (label, kind, onclick, attrs = {}) => h('button', { class: [C.btn, kind ? C[kind] : C.plain], type: 'button', onclick, ...attrs }, label);
    const msg = h('p', { class: 'pp-msg', role: 'alert', hidden: true });
    const body = h('div', { class: 'pp-body' });
    const titleId = `pp-${Math.random().toString(36).slice(2, 8)}`;
    const saveBtn = btn('Save prices', 'primary', save, { disabled: true });
    const foot = h('div', { class: 'pp-foot' }, btn(admin ? 'Cancel' : 'Close', 'ghost', () => dlg.close()), admin ? saveBtn : null);
    const dlg = h('dialog', { class: ['pp-dialog', C.dialog], 'aria-labelledby': titleId },
      h('div', { class: 'pp-head' }, h('h2', { id: titleId }, `Prices at ${place.name}`),
        h('button', { class: [C.btn, C.ghost, 'pp-x'], type: 'button', 'aria-label': 'Close', onclick: () => dlg.close() }, '×')),
      body, msg, foot);

    const say = (t) => { msg.hidden = !t; msg.textContent = t || ''; };
    const touch = () => { dirty = true; saveBtn.disabled = false; };

    function paintList() {
      editing = null;
      const now0 = priceOnDay(list, today);
      const rows = list.map((x, i) => {
        const inv = srcOf(x) === 'invoice';
        const who = inv
          ? `From invoices${x.invoice?.sessions ? ` (median of ${x.invoice.sessions} sessions)` : ''}${invoiceShadowed(list, x) ? ', not used: a typed price covers its days' : ''}`
          : 'Typed';
        return h('li', { class: ['pp-row', x === now0 && 'pp-row--now', inv && 'pp-row--invoice'] },
          h('span', { class: 'pp-from' }, fromWord(x.from)),
          h('span', { class: 'pp-price' }, priceLine(x.price), x === now0 ? h('span', { class: 'pp-tag' }, 'in force today') : null),
          h('span', { class: 'pp-who' }, who),
          admin && !inv ? h('span', { class: 'pp-acts' },
            btn('Edit', 'ghost', () => paintEntry(i), { 'aria-label': `Edit the price ${fromWord(x.from).toLowerCase()}` }),
            btn('Remove', 'ghost', () => { list.splice(i, 1); touch(); paintList(); }, { 'aria-label': `Remove the price ${fromWord(x.from).toLowerCase()}` })) : h('span', { class: 'pp-acts' }));
      });
      replace(body,
        h('p', { class: 'pp-hint' }, 'Each price holds from its first day until the next one starts, so a charge is costed at the price of its own day. A price you type always wins over one worked out from Supercharger invoices.'),
        list.length ? h('ul', { class: 'pp-list' }, rows) : h('p', { class: 'pp-none' }, 'No price: charges here get no cost.'),
        admin ? h('div', { class: 'pp-add' }, btn(list.length ? 'Add a price from a date' : 'Add a price', null, () => paintEntry(null))) : null,
        dirty ? h('p', { class: 'pp-hint' }, 'Charges that already ended keep their cost; reprice them on the place’s page to use these prices.') : null);
    }

    function paintEntry(i) {
      say('');
      const src = i == null ? priceOnDay(list, today) : list[i];
      const base = src?.price ? JSON.parse(JSON.stringify(src.price)) : { currency: currency || 'USD', per_kwh: null, per_minute: null, rates: [], session_fee: null };
      const d = { from: i == null ? (list.length ? today : null) : list[i].from ?? null, price: { ...base, rates: (base.rates || []).map((r) => ({ ...r })) } };
      editing = { i, d };
      const num = (label, key, attrs = {}) => {
        const el = h('input', { class: C.input, type: 'number', step: 'any', value: d.price[key] ?? '', 'aria-label': label, ...attrs });
        el.addEventListener('input', () => { d.price[key] = el.value === '' ? null : Number(el.value); });
        return h('label', { class: 'pp-field' }, h('span', null, label), el);
      };
      const startBox = h('input', { type: 'checkbox', checked: d.from == null });
      const dateIn = h('input', { class: C.input, type: 'date', value: d.from || today, disabled: d.from == null, 'aria-label': 'First day' });
      startBox.addEventListener('change', () => { dateIn.disabled = startBox.checked; d.from = startBox.checked ? null : dateIn.value || today; });
      dateIn.addEventListener('input', () => { d.from = dateIn.value || null; });
      const cur = h('select', { class: C.select, 'aria-label': 'Currency' },
        [...new Set([d.price.currency, ...CURRENCIES].filter(Boolean))].map((c) => h('option', { value: c, selected: c === d.price.currency }, c)));
      cur.addEventListener('change', () => { d.price.currency = cur.value; });
      const ratesBox = h('div', { class: 'pp-rates' });
      const paintRates = () => replace(ratesBox, d.price.rates.length ? d.price.rates.map((r, j) => {
        const inp = (key, attrs, label) => {
          const el = h('input', { class: C.input, value: r[key] ?? '', 'aria-label': `${label}, rate ${j + 1}`, ...attrs });
          el.addEventListener('input', () => { r[key] = attrs.type === 'number' ? (el.value === '' ? null : Number(el.value)) : el.value; });
          return el;
        };
        const days = h('select', { class: C.select, 'aria-label': `Days, rate ${j + 1}` },
          [...DAY_CHOICES, ...(DAY_CHOICES.some(([v]) => v === r.days) ? [] : [[r.days, r.days]])].map(([v, l]) => h('option', { value: v, selected: v === (r.days || 'all') }, l)));
        days.addEventListener('change', () => { r.days = days.value; });
        return h('div', { class: 'pp-rate' }, inp('name', { type: 'text', maxlength: '24' }, 'Name'), days,
          inp('from', { type: 'text', placeholder: 'HH:MM', inputmode: 'numeric' }, 'From'), inp('to', { type: 'text', placeholder: 'HH:MM', inputmode: 'numeric' }, 'To'),
          inp('per_kwh', { type: 'number', step: 'any', placeholder: '/kWh' }, 'Price per kWh'),
          btn('Remove', 'ghost', () => { d.price.rates.splice(j, 1); paintRates(); }, { 'aria-label': `Remove rate ${j + 1}` }));
      }) : h('p', { class: 'pp-none' }, 'No time-of-use rates: one price all day.'));
      paintRates();
      replace(body,
        h('div', { class: 'pp-entry' },
          h('div', { class: 'pp-when' }, h('label', { class: 'pp-check' }, startBox, h('span', null, 'From the start')), h('label', { class: 'pp-field' }, h('span', null, 'First day'), dateIn)),
          h('div', { class: 'pp-grid' }, h('label', { class: 'pp-field' }, h('span', null, 'Currency'), cur),
            num('Price per kWh', 'per_kwh', { placeholder: 'e.g. 0.31' }), num('Price per minute', 'per_minute'), num('Session fee', 'session_fee', { min: '0' })),
          h('div', { class: 'pp-sub' }, h('span', null, 'Time-of-use rates'), btn('Add a rate', 'ghost', () => {
            d.price.rates.push({ name: d.price.rates.length ? `Rate ${d.price.rates.length + 1}` : 'Off-peak', days: 'all', from: '00:00', to: '07:00', per_kwh: null, per_minute: null });
            paintRates();
          })),
          ratesBox,
          h('div', { class: 'pp-entry-acts' }, btn('Back to the list', 'ghost', () => { say(''); paintList(); }), btn(i == null ? 'Add this price' : 'Keep this change', null, keepEntry))));
    }

    function keepEntry() {
      const { i, d } = editing;
      const next = list.slice();
      if (i == null) next.push({ from: d.from, source: 'typed', price: d.price });
      else next[i] = { ...next[i], from: d.from, price: d.price };
      const typed = sortPrices(next).filter((x) => srcOf(x) === 'typed');
      const errs = checkPrices(typed.map((x) => ({ from: x.from, price: x.price })));
      const at = typed.findIndex((x) => x.price === d.price);
      const mine = Object.entries(errs).filter(([k]) => k.startsWith(`prices.${at}.`)).map(([, v]) => v);
      if (mine.length) { say(mine.join(' ')); return; }
      list = sortPrices(next);
      touch();
      say('');
      paintList();
    }

    async function save() {
      if (editing) { keepEntry(); if (editing) return; }
      const send = typedBody(list);
      const errs = checkPrices(send);
      if (Object.keys(errs).length) { say(Object.values(errs).join(' ')); return; }
      saveBtn.disabled = true;
      try {
        saved = await client.patch(`places/${encodeURIComponent(place.id)}`, { version: place.version, prices: send });
        dlg.close();
      } catch (e) {
        saveBtn.disabled = false;
        say(e?.fields ? Object.values(e.fields).join(' ') : e?.message || 'The prices were not saved.');
      }
    }

    dlg.addEventListener('cancel', (e) => { if (editing) { e.preventDefault(); paintList(); } });
    dlg.addEventListener('close', () => {
      dlg.remove();
      if (back && typeof back.focus === 'function' && back.isConnected) back.focus();
      resolve(saved);
    });
    paintList();
    document.body.append(dlg);
    dlg.showModal();
  });
}

/**
 * The "Set price" shortcut: loads the place (GET /places/{place}) and opens the editor. With no place but a spot
 * (`spot: { name, lat, lon }`), saves a new place there first ("Save place and set price"). Resolves with the saved
 * place or null.
 */
export async function setPrice({ client, placeId = null, spot = null, cls, currency = null }) {
  let place;
  if (placeId) place = await client.get(`places/${encodeURIComponent(placeId)}`);
  else if (spot) place = await client.post('places', newPlaceBody(spot));
  else return null;
  const out = await openPriceEditor({ client, place, cls, currency, admin: true });
  return out || (placeId ? null : place);
}
