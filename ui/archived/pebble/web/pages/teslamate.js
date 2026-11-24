// Sentry Mate, "Pebble" UI pack: Local TeslaMate, on Settings: Integrations (#teslamate).
//
// The owner's own TeslaMate on the local network as a live input: the server subscribes to TeslaMate's MQTT topics
// (it only reads, never publishes) and, when an admin turns it on, catches up from TeslaMate's PostgreSQL database
// now and then. Off until an admin adds it; data from it is labelled "Data from local TeslaMate".
//
// Admins only, reading included. Tiles: its status (MQTT, the database catch-up, errors, hosts, Pause and Remove),
// the MQTT form and the optional database catch-up form (each with Test and Save), and TeslaMate's cars, each
// matched to a car here, a new car or left out. Adding, saving, pausing and removing ask for the admin's own password
// again (and the two-step code): askPassword(), as the TeslaFi tiles do. A test and saving the matches take the
// admin's session alone: they store and show no password. The shared rules are in shared/teslamate.js.
//
// Data: GET/PUT/PATCH/DELETE integrations/teslamate, POST integrations/teslamate/test, PUT integrations/teslamate/cars.

import { h } from '../shared/dom.js';
import * as state from '../shared/state.js';
import { formatDateTime } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { TM_BASE, TM_DEFAULTS, TM_HOST_HINT, tmPinNote, tmTlsPort, TM_SOURCE, MQTT_STATE, DB_STATE, UNENCRYPTED_LABEL, UNENCRYPTED_WARNING, tmErrorText, tmWords, tmStale, tmDraft, tmCheck, tmPutBody, tmMatchChanges, tmPreselect, tmHeadline } from '../shared/teslamate.js';
import { tile, btn, chip, note, ago } from '../core/ui.js';
import { field, form, setFieldError } from '../core/form.js';
import { errorWords } from '../core/states.js';
import { facts } from '../core/bits.js';
import { srcPill } from '../core/source.js';
import { group, infoRow, pickRow, said, askPassword, tick } from '../core/settings.js';

const HINT = 'Local TeslaMate changes ask for it again.';
const TONE = { live: 'teal', warm: 'sun', plain: 'muted' };
const withPw = (cred, body = {}) => ({ ...body, confirm_password: cred.password, ...(cred.two_step ? { code: cred.two_step } : {}) });
const mono = (t) => h('code', { class: 'set-mono' }, t);
const inAttrs = { spellcheck: 'false', autocomplete: 'off', autocapitalize: 'off' };

/** A tick as a form field (read by name; errors under it). */
function tickField(text, name, checked, hint = null) {
  const t = tick(text, { name });
  t.input.checked = !!checked;
  const el = h('div', { class: 'field tm-tick', dataset: { field: name } }, t, hint ? h('p', { class: 'hint' }, hint) : null, h('p', { class: 'field-error', hidden: true }));
  el.control = t.input;
  return el;
}

/** run() for askPassword: our words on an error; a stale page closes the sheet with { stale } instead. */
const guarded = (fn) => async (...a) => {
  try { return await fn(...a); } catch (e) { tmWords(e); if (tmStale(e)) return { stale: e.message }; throw e; }
};

/**
 * The Local TeslaMate tiles. cars: Sentry Mate's cars ({ id, name }) for the match picker.
 * Returns { el, load() }: el is a display: contents box for the grid; load reads and paints.
 */
export function teslamateTiles(ctx, { cars = [] } = {}) {
  const el = h('div', { class: 'contents' });
  let t = null;
  const carName = (id) => cars.find((c) => c.id === id)?.name || id;

  const head = () => h('div', { class: 'section-head w-full', id: 'teslamate' },
    h('h2', { class: 'section-title' }, 'Local TeslaMate'),
    h('div', { class: 'tf-head-text' }, h('p', { class: 'note' }, 'Your own TeslaMate on the local network as a live input: this server subscribes to TeslaMate’s MQTT topics and, if you turn it on, catches up from its database for what MQTT missed. It only reads: it never publishes, never wakes the car and never sends a command. Where SMO, the Tesla API or TeslaFi cover the same time, theirs win. Its data is marked:'),
      h('p', { class: 'src-line' }, srcPill({ sources: [TM_SOURCE] }))));

  async function load() {
    if (!ctx.admin) {
      el.replaceChildren(head(), tile({ w: 2, title: 'Local TeslaMate', icon: 'key', side: chip('Admins only', { tone: 'muted' }) },
        said('info', 'Only an admin sees and changes the local TeslaMate: its connections, status and cars. Every page still says where its data came from, “Data from local TeslaMate” included.')));
      return;
    }
    try {
      t = await ctx.api.get(TM_BASE);
    } catch (e) {
      if (e?.name === 'AbortError') return;
      el.replaceChildren(head(), tile({ w: 2, title: 'Local TeslaMate', icon: 'key', side: chip('Not read', { tone: 'bad' }) }, said('bad', 'The local TeslaMate settings could not be read: ', errorWords(e).text)));
      return;
    }
    if (!ctx.alive()) return;
    el.replaceChildren(head(), statusTile(), connTile('mqtt'), connTile('database'), t.enabled ? carsTile() : addTile());
  }
  // a test and the car matches: the admin's session alone, no password asked again; an error not for a field is said
  // in a toast
  const direct = async (b, fn) => {
    b.disabled = true;
    try { return await fn(); } catch (e) {
      if (e?.name !== 'AbortError') ctx.toast(errorWords(e).text, { kind: 'error' });
      return null;
    } finally { b.disabled = false; }
  };
  const after = async (r, msg) => {
    if (!r) return false;
    if (r.stale) { ctx.toast(r.stale, { kind: 'info' }); await load(); return true; }
    if (r.fieldErr) return false;
    if (msg) ctx.toast(msg);
    await load();
    return true;
  };

  /* ---------- status */
  function statusTile() {
    const sm = t.status?.mqtt || {};
    const sd = t.status?.database || {};
    const ms = MQTT_STATE[sm.state] || [sm.state || 'Off', 'plain', ''];
    const ds = DB_STATE[sd.state] || [sd.state || 'Off', 'plain', ''];
    const [word, tone] = tmHeadline(t);
    const lvl = (x) => (x === 'live' ? 'ok' : x === 'warm' ? 'warn' : 'off');
    const check = (level, name, sub, w) => h('li', { class: ['check', `is-${level}`] }, h('span', { class: 'check-dot', 'aria-hidden': 'true' }),
      h('span', { class: 'check-text' }, h('span', { class: 'check-name' }, name), sub ? h('span', { class: 'check-sub' }, sub) : null), h('span', { class: 'check-word' }, w));
    const now = ctx.api.now();
    const errText = (e) => (e ? [tmErrorText(e), e.at_ms ? `, ${formatDateTime(e.at_ms, ctx.tz, { now })}` : ''] : 'None.');
    const pause = btn(t.paused ? 'Resume' : 'Pause', { kind: t.paused ? 'primary' : 'soft', icon: t.paused ? 'refresh' : 'minus', onclick: async () => {
      const r = await askPassword(ctx, {
        title: t.paused ? 'Resume the local TeslaMate?' : 'Pause the local TeslaMate?', yes: t.paused ? 'Resume' : 'Pause', hint: HINT,
        text: t.paused ? 'MQTT is read again from now on, and the database catch-up (when on) starts again.' : 'Both connections are closed and nothing is read until you resume it. The history already stored stays.',
        run: guarded((cred) => ctx.api.patch(TM_BASE, withPw(cred, { version: t.version, paused: !t.paused }))),
      });
      await after(r, t.paused ? 'Local TeslaMate resumed.' : 'Local TeslaMate paused.');
    } });
    const remove = btn('Remove', { kind: 'danger', icon: 'trash', onclick: async () => {
      const r = await askPassword(ctx, {
        title: 'Remove the local TeslaMate?', danger: true, yes: 'Remove it', hint: HINT,
        text: 'Reading stops at once and its sealed settings, passwords included, are deleted. The history already stored stays, marked “Data from local TeslaMate”.',
        run: guarded(async (cred) => (await ctx.api.send('DELETE', TM_BASE, withPw(cred))) ?? true),
      });
      await after(r, 'Local TeslaMate removed. Its history stays.');
    } });
    return tile({ w: 2, title: 'Local TeslaMate input', icon: 'key', side: chip(word, { tone: TONE[tone] }) },
      !t.enabled ? said('info', 'Off until it is added. Until then the server makes no connection to TeslaMate at all.') : null,
      h('ul', { class: 'checks' },
        check(lvl(ms[1]), 'MQTT', sm.connected_since_ms ? `Connected since ${formatDateTime(sm.connected_since_ms, ctx.tz, { now })}.` : ms[2], ms[0]),
        check(sm.last_message_ms ? 'ok' : 'off', 'Last message', sm.last_message_ms ? `${formatNumber(sm.messages_today ?? 0)} messages today.` : 'None yet.', sm.last_message_ms ? ago(sm.last_message_ms) : '—'),
        check(lvl(ds[1]), 'Database catch-up', sd.last_sync_ms ? [`Last ${ago(sd.last_sync_ms)}: ${formatNumber(sd.drives)} drives, ${formatNumber(sd.charges)} charges.`, sd.next_sync_ms ? ` Next ${formatDateTime(sd.next_sync_ms, ctx.tz, { now })}.` : ''] : ds[2], ds[0]),
        check(sm.last_error ? 'warn' : 'ok', 'MQTT error', errText(sm.last_error), sm.last_error ? 'Yes' : 'None'),
        check(sd.last_error ? 'warn' : 'ok', 'Database error', errText(sd.last_error), sd.last_error ? 'Yes' : 'None')),
      group('Hosts it talks to', (t.hosts || []).length ? t.hosts.map((x) => infoRow({ icon: 'globe', name: mono(x), sub: 'On your local network; read only' }))
        : [infoRow({ icon: 'globe', name: 'None', sub: 'No host while it is not added.' })]),
      t.enabled ? h('div', { class: 'form-actions tesla-acts' }, pause, remove) : null);
  }

  function addTile() {
    return tile({ w: 2, title: 'Add your local TeslaMate', icon: 'plus' },
      h('ol', { class: 'set-list' },
        h('li', null, 'Type the broker TeslaMate publishes to (often the same machine, port 1883) and its topic prefix (teslamate unless you changed it).'),
        h('li', null, 'Press Test to see the cars TeslaMate publishes, then Save with your own Sentry Mate password.'),
        h('li', null, 'Match each TeslaMate car to a car here. Nothing is read until you do.')),
      note('Passwords are sealed on the server and never shown again; no page and no answer holds them.'));
  }

  /* ---------- the MQTT and database forms */
  function connTile(part) {
    const saved = t[part] || TM_DEFAULTS[part];
    const isDb = part === 'database';
    const n = (x) => `${part}.${x}`;
    const fEnabled = isDb ? tickField('Catch up from TeslaMate’s database', n('enabled'), saved.enabled, 'Off by default. Reads TeslaMate’s drives, charges and positions now and then, for what MQTT missed. A read-only database user is enough.') : null;
    const fHost = field({ label: 'Host', name: n('host'), value: saved.host || '', hint: isDb ? 'Where TeslaMate’s PostgreSQL listens.' : TM_HOST_HINT, attrs: { ...inAttrs, placeholder: '192.0.2.10', class: 'input-mono' } });
    const fPort = field({ label: 'Port', name: n('port'), type: 'number', value: saved.port, hint: isDb ? '5432 is PostgreSQL’s usual port.' : '1883 without TLS, 8883 with.', attrs: { min: 1, max: 65535, step: 1 } });
    const fDb = isDb ? field({ label: 'Database', name: n('database'), value: saved.database || '', hint: 'TeslaMate’s is teslamate.', attrs: inAttrs }) : null;
    const fUser = field({ label: 'User name', name: n('username'), value: saved.username || '', hint: isDb ? 'A user that may read TeslaMate’s tables.' : 'Empty for a broker without accounts.', attrs: inAttrs });
    const fPass = field({ label: 'Password', name: n('password'), type: 'password', hint: saved.password_set ? 'A password is saved. Leave this empty to keep it; it is never shown again.' : 'No password saved.', attrs: { autocomplete: 'new-password', placeholder: saved.password_set ? 'saved' : '' } });
    const fRemove = saved.password_set ? tickField('Remove the saved password', n('password_remove'), false) : null;
    const fPrefix = !isDb ? field({ label: 'Topic prefix', name: n('prefix'), value: saved.prefix || 'teslamate', hint: 'TeslaMate’s MQTT_NAMESPACE, teslamate unless you changed it. The server subscribes to <prefix>/cars/+/#.', attrs: { ...inAttrs, class: 'input-mono' } }) : null;
    const fIv = isDb ? field({ label: 'Catch up every (seconds)', name: n('interval_s'), type: 'number', value: saved.interval_s, hint: 'From 60 to 86400. 300 is every five minutes.', attrs: { min: 60, max: 86400, step: 1 } }) : null;
    const fTls = tickField('Encrypt the connection (TLS)', n('tls'), saved.tls, isDb ? 'When TeslaMate’s PostgreSQL offers TLS.' : 'When the broker offers TLS (usually on port 8883).');
    const fPin = field({ label: 'Pinned certificate (SHA-256)', name: n('pin_sha256'), value: saved.pin_sha256 || '', hint: tmPinNote(part), attrs: { ...inAttrs, class: 'input-mono' } });
    const fPlain = tickField(UNENCRYPTED_LABEL, n('allow_unencrypted'), saved.allow_unencrypted, 'Off by default. Needed only to send a password without TLS.');
    fPlain.append(said('warn', UNENCRYPTED_WARNING));
    const fields = [fEnabled, fHost, fPort, fDb, fUser, fPass, fRemove, fPrefix, fIv, fTls, fPin, fPlain].filter(Boolean);
    const ofPart = (raw) => Object.fromEntries(Object.entries(raw).filter(([k]) => k.startsWith(`${part}.`)).map(([k, v]) => [k.slice(part.length + 1), v]));
    const read = (formEl) => {
      const o = {};
      for (const c of formEl.elements) if (c.name) o[c.name] = c.type === 'checkbox' ? c.checked : c.type === 'number' ? (c.value === '' ? null : Number(c.value)) : c.value;
      return ofPart(o);
    };
    const out = h('div', { class: 'tm-test', 'aria-live': 'polite' });
    const putErrors = (fl) => { for (const [k, v] of Object.entries(fl)) { const x = fields.find((y) => y.dataset.field === k); if (x) setFieldError(x, v); } };
    // a sheet's run(): field errors of this tile's form close the sheet and go under their fields
    const toForm = (fn) => async (...a) => {
      try { return await fn(...a); } catch (e) {
        tmWords(e);
        if (tmStale(e)) return { stale: e.message };
        if (e.fields && Object.keys(e.fields).some((k) => k.startsWith(`${part}.`))) { putErrors(e.fields); return { fieldErr: true }; }
        throw e;
      }
    };
    const testBtn = btn('Test', { kind: 'soft', icon: 'refresh', onclick: async () => {
      f.clearErrors();
      const d = tmDraft(part, read(f));
      if (isDb) d.enabled = true;
      const bad = tmCheck(part, d, t.enabled ? saved : null);
      if (Object.keys(bad).length) { putErrors(bad); return; }
      out.replaceChildren(note(isDb ? 'Testing: connecting to the database and looking for TeslaMate’s tables. Nothing is saved.' : 'Testing: connecting to the broker and listening a few seconds for TeslaMate’s cars. It subscribes only and publishes nothing; nothing is saved.'));
      const r = await direct(testBtn, toForm(() => ctx.api.post(`${TM_BASE}/test`, { target: part, [part]: d })));
      if (!r || r.fieldErr || r.stale) out.replaceChildren();
      if (!r || r.fieldErr) return;
      if (r.stale) { ctx.toast(r.stale, { kind: 'info' }); await load(); return; }
      out.replaceChildren(said(r.ok ? 'ok' : 'warn',
        r.ok ? r.message : `${tmErrorText({ code: r.error, message: r.message }) || 'It did not work'}.`,
        r.ok && r.latency_ms != null ? ` ${formatNumber(r.latency_ms)} ms.` : '',
        r.ok && r.cars_seen?.length ? [' TeslaMate cars: ', r.cars_seen.map((x, i) => [i ? ', ' : '', mono(x)]), '.'] : ''));
    } });
    const f = form({
      fields, submitLabel: t.enabled ? 'Save' : 'Save and add', busyLabel: 'Saving…', cls: 'tm-form', secondary: testBtn,
      onsubmit: async (raw) => {
        const d = tmDraft(part, ofPart(raw));
        const bad = tmCheck(part, d, t.enabled ? saved : null);
        if (Object.keys(bad).length) throw Object.assign(new Error('Not saved yet.'), { fields: bad });
        f.setBusy(false);
        const r = await askPassword(ctx, {
          title: t.enabled ? `Save the ${isDb ? 'database' : 'MQTT'} settings` : 'Add the local TeslaMate', yes: t.enabled ? 'Save' : 'Add', hint: HINT,
          text: t.enabled ? 'The server reconnects with these settings at once.' : 'The server subscribes to TeslaMate’s topics. Nothing is read from a car until you match it.',
          run: toForm((cred) => ctx.api.put(TM_BASE, withPw(cred, tmPutBody(t, part, d)))),
        });
        await after(r, t.enabled ? `${isDb ? 'Database' : 'MQTT'} settings saved.` : 'Local TeslaMate added. Match its cars to start reading them.');
      },
    });
    const sync = () => {
      const tls = fTls.control.checked;
      const on = !isDb || fEnabled.control.checked;
      for (const x of fields) if (x !== fEnabled) x.hidden = !on;
      fPin.hidden = !on || !tls;
      fPlain.hidden = !on || tls;
      testBtn.hidden = !on;
    };
    // TLS on moves the port from 1883 to 8883, off moves it back; a port typed by hand stays
    fTls.control.addEventListener('change', () => { const to = tmTlsPort(part, fPort.control.value, fTls.control.checked); if (to != null) fPort.control.value = String(to); });
    fTls.control.addEventListener('change', sync);
    fEnabled?.control.addEventListener('change', sync);
    sync();
    const needsMqtt = isDb && !t.enabled;
    if (needsMqtt) f.submitButton.disabled = true;
    return tile({ w: 2, title: isDb ? 'Database catch-up' : 'MQTT', icon: isDb ? 'clock' : 'globe', id: isDb ? 'teslamate-database' : 'teslamate-mqtt',
      side: saved.password_set ? chip('Password saved', { tone: 'muted' }) : isDb ? chip('Optional', { tone: 'muted' }) : null },
    f, needsMqtt ? note('Add the MQTT connection first; the catch-up is saved with it afterwards.') : null, out);
  }

  /* ---------- TeslaMate's cars */
  function carsTile() {
    const items = t.cars || [];
    const picked = new Map(items.map((c) => [c.teslamate_car, tmPreselect(c)]));
    const nameOf = (c) => c.display_name || `TeslaMate car ${c.teslamate_car}`;
    const opts = [['', 'Not decided', 'Not read until you choose'], ...cars.map((x) => [x.id, x.name || x.id, 'Read into this car']),
      ['new', 'A new car', 'A new car is made for it here'], ['leave_out', 'Leave out', 'Never read']];
    const save = btn('Save the matches', { kind: 'primary', icon: 'check', onclick: () => saveMatches() });
    const pending = h('p', { class: 'note' });
    const sync = () => {
      const k = tmMatchChanges(items, picked).length;
      save.hidden = !k;
      pending.textContent = k ? `${k} ${k === 1 ? 'change' : 'changes'} not saved yet.` : '';
    };
    const rows = items.map((c) => {
      const known = [...opts];
      if (!known.some(([v]) => v === picked.get(c.teslamate_car))) known.push([picked.get(c.teslamate_car), carName(picked.get(c.teslamate_car))]);
      return h('div', { class: 'tesla-car' },
        pickRow({
          name: nameOf(c),
          sub: [`TeslaMate car ${c.teslamate_car}`, c.vin_prefix ? ` · ${c.vin_prefix}` : '', c.last_message_ms ? [' · last message ', ago(c.last_message_ms)] : '', c.match == null && c.suggested ? ` · Suggested: ${carName(c.suggested)}, same VIN prefix` : ''],
          value: picked.get(c.teslamate_car), options: known, sheetTitle: `Car for ${nameOf(c)}`,
          sheetNote: 'A TeslaMate car is read only once it is matched to a car here or to a new car. Saved with Save the matches.',
          onpick: (v) => { picked.set(c.teslamate_car, v); sync(); },
        }));
    });
    async function saveMatches() {
      const ch = tmMatchChanges(items, picked);
      if (!ch.length) return;
      const r = await direct(save, guarded(() => ctx.api.put(`${TM_BASE}/cars`, { version: t.version, items: ch })));
      await after(r, 'Matches saved.');
    }
    sync();
    return tile({ w: 2, title: 'TeslaMate’s cars', icon: 'car', side: chip(`${formatNumber(items.length)} in TeslaMate`, { tone: 'muted' }) },
      items.length ? h('div', { class: 'rows' }, h('div', { class: 'rows-list' }, rows))
        : said('info', 'No car has been seen on TeslaMate’s topics yet. It shows here once TeslaMate publishes one.'),
      note('Pick the car each TeslaMate car’s data goes to. A suggestion has the same VIN prefix as a car here; nothing is saved until you press Save the matches.'),
      facts([['Not matched', `${formatNumber(items.filter((c) => c.match == null).length)} of ${formatNumber(items.length)}`]]),
      h('div', { class: 'form-actions' }, save, pending));
  }

  return { el, load };
}

/** The link to the Local TeslaMate section of Settings: Integrations. */
export const teslamateHref = () => `${state.buildUrl('settings-integrations')}#teslamate`;

/**
 * The facts of the local TeslaMate input for one car, for the car's Health page: [[name, value]], or null when no
 * TeslaMate car is matched to it (or the input is not added). t: GET integrations/teslamate.
 */
export function teslamateCarFacts(ctx, t, carId) {
  if (!t?.enabled) return null;
  const c = (t.cars || []).find((x) => x.match === carId);
  if (!c) return null;
  const sm = t.status?.mqtt || {};
  const sd = t.status?.database || {};
  const now = ctx.api.now();
  const when = (ms) => (ms ? formatDateTime(ms, ctx.tz, { now }) : 'None yet');
  const err = sm.last_error || sd.last_error;
  return [
    ['TeslaMate car', `${c.display_name || 'Not named'} (its id ${c.teslamate_car})`],
    ['MQTT', t.paused ? 'Paused' : (MQTT_STATE[sm.state] || [sm.state || 'Off'])[0]],
    ['Its newest MQTT value', when(c.last_message_ms)],
    ['Database catch-up', sd.state === 'off' || !sd.state ? 'Off: MQTT only' : `${(DB_STATE[sd.state] || [sd.state])[0]}${sd.last_sync_ms ? `, last ${formatDateTime(sd.last_sync_ms, ctx.tz, { now })}` : ''}`],
    err ? [sm.last_error ? 'MQTT error' : 'Database error', tmErrorText(err)] : null,
    ['Settings', h('a', { class: 'place-link', href: teslamateHref() }, 'Open local TeslaMate')],
  ].filter(Boolean);
}
