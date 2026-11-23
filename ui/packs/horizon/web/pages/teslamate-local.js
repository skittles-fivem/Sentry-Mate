// Settings: Integrations, the Local TeslaMate section (#teslamate): the owner's own TeslaMate on the local network
// as a live input. The server subscribes to TeslaMate's MQTT topics (read only: it never publishes) and, when an
// admin turns it on, catches up from TeslaMate's PostgreSQL database now and then. Off until an admin adds it.
//
// Admins only, reading included. Adding, saving, pausing and removing ask for the admin's own password again (and the
// two-step code), like the TeslaFi section: askPassword() from settings-server-move.js, sent as confirm_password and
// code. A test and saving the car matches take the admin's session alone: they store and show no password. The rules and words shared by every pack are in shared/teslamate.js.
//
// API: GET/PUT/PATCH/DELETE integrations/teslamate, POST integrations/teslamate/test, PUT integrations/teslamate/cars.

import { h, replace } from '../shared/dom.js';
import { TM_BASE, TM_DEFAULTS, TM_HOST_HINT, tmPinNote, tmTlsPort, MQTT_STATE, DB_STATE, UNENCRYPTED_LABEL, UNENCRYPTED_WARNING, tmErrorText, tmWords, tmStale, tmDraft, tmCheck, tmPutBody, tmMatchChanges, tmPreselect, tmHeadline } from '../shared/teslamate.js';
import { icon } from '../core/icons.js';
import { kv, tag, pill, note, metric, figures } from '../core/page.js';
import { field, form } from '../core/form.js';
import { emptyState, errorWords } from '../core/states.js';
import { flash } from '../core/flash.js';
import { dateTime, ago, num, tzName } from '../core/fmt.js';
import { askPassword } from './settings-server-move.js';

const LOCAL = Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
const at = (ms) => `${dateTime(ms, LOCAL)} ${tzName(LOCAL)}`;
const withPw = (cred, body = {}) => ({ ...body, confirm_password: cred.password, ...(cred.two_step ? { code: cred.two_step } : {}) });
const code = (t) => h('code', { class: 'si-code' }, t);
const when = (ms, none = 'none yet') => (ms ? h('span', { title: at(ms) }, ago(ms)) : h('span', { class: 'faint' }, none));
const PART_NAME = { mqtt: 'MQTT', database: 'database' };

/** The Local TeslaMate section. Returns { el, load() } (load reads and paints; the page awaits it before ready()). */
export function teslamateSection(ctx, { cars = [] } = {}) {
  const admin = ctx.admin;
  const body = h('div', { class: 'ta-body' });
  const headPill = h('span', { class: 'ta-head-pill' });
  const el = h('section', { class: 'si-section ta tm', id: 'teslamate', 'aria-label': 'Local TeslaMate' },
    h('div', { class: 'si-section-head' }, h('h2', { class: 'si-h2' }, icon('share'), 'Local TeslaMate'), headPill),
    h('p', { class: 'note' }, 'For owners who already run TeslaMate on their own network: a live input beside SMO, the Tesla API and TeslaFi. The server subscribes to TeslaMate\'s MQTT topics and only reads: it never publishes, never wakes the car and never sends a command. Optionally it also catches up from TeslaMate\'s database for what MQTT missed. Off until an admin adds it. Where SMO, the Tesla API or TeslaFi cover the same time, theirs win, value by value; rows from it say "Data from local TeslaMate".'),
    body);

  let t = null;
  const carName = (id) => cars.find((c) => c.id === id)?.name || id;

  async function load() {
    if (!admin) {
      replace(headPill, pill('Admins only'));
      replace(body, h('div', { class: 'panel panel-pad' }, note(icon('lock', 'ico--sm'), ' Only an admin sees and changes the local TeslaMate: its connections, status and cars. Every page still says where its data came from, "Data from local TeslaMate" included.')));
      return;
    }
    try {
      t = await ctx.api.get(TM_BASE);
    } catch (e) {
      if (e?.name === 'AbortError') return;
      replace(headPill, pill('Not read', { tone: 'warm' }));
      replace(body, h('div', { class: 'panel panel-pad' }, h('p', { class: 'warn' }, icon('warn', 'ico--sm'), ' The local TeslaMate settings could not be read: ', errorWords(e).text)));
      return;
    }
    if (!ctx.alive()) return;
    paint();
  }
  const after = async (r, msg) => {
    if (!r) return false;
    if (r.stale) { flash(r.stale, { kind: 'info' }); await load(); return false; }
    if (r.fieldErr) return false;
    flash(typeof msg === 'function' ? msg(r) : msg);
    await load();
    return true;
  };
  // a change: errors that name a field of the page's form go back to that form, stale ones reload
  const guarded = (fn, f) => async (...a) => {
    try { return await fn(...a); } catch (e) {
      tmWords(e);
      if (tmStale(e)) return { stale: e.message };
      if (f && e.fields && Object.keys(e.fields).some((k) => k.startsWith('mqtt.') || k.startsWith('database.'))) {
        f.setErrors(e.fields); f.setMessage(e.message); return { fieldErr: true };
      }
      throw e;
    }
  };
  // a test and the car matches: the admin's session alone, no password asked again; other errors in a flash
  const direct = async (b, fn, f = null) => {
    b.disabled = true;
    try { return await guarded(fn, f)(); } catch (e) {
      if (e?.name !== 'AbortError') flash(errorWords(e).text, { kind: 'error' });
      return null;
    } finally { b.disabled = false; }
  };

  function paint() {
    const [word, tone] = tmHeadline(t);
    replace(headPill, pill(word, { tone, lg: true }));
    replace(body,
      h('div', { class: 'ta-grid' }, h('div', { class: 'tm-col' }, statusPanel(), connPanel('database')), h('div', { class: 'tm-col' }, connPanel('mqtt'))),
      t.enabled ? carsPanel() : addSteps());
  }

  function addSteps() {
    return h('div', { class: 'panel panel-pad ta-add' },
      h('div', { class: 'panel-head' }, h('h3', { class: 'panel-title' }, icon('plus'), 'Add your local TeslaMate')),
      h('ol', { class: 'ta-step-list' },
        h('li', null, 'Type the address of the MQTT broker TeslaMate publishes to (often the same machine, port 1883) and its topic prefix (', code('teslamate'), ' unless you changed it).'),
        h('li', null, 'Press ', h('b', null, 'Test'), ' to see the cars TeslaMate publishes, then ', h('b', null, 'Save'), ' with your own Sentry Mate password.'),
        h('li', null, 'Match each TeslaMate car to a car here. Nothing is read before that.')),
      h('p', { class: 'setting-note' }, 'Passwords are sealed on the server and never shown again, not even to an admin.'));
  }

  function statusPanel() {
    const sm = t.status?.mqtt || {};
    const sd = t.status?.database || {};
    const ms = MQTT_STATE[sm.state] || [sm.state || 'Off', 'plain', ''];
    const ds = DB_STATE[sd.state] || [sd.state || 'Off', 'plain', ''];
    const errLine = (label, e) => (e ? h('p', { class: 'si-last-error warn' }, icon('warn', 'ico--sm'), ` ${label}: `, h('b', null, tmErrorText(e)), e.at_ms ? `, ${at(e.at_ms)} (${ago(e.at_ms)})` : '', '.') : null);
    const pauseBtn = t.enabled ? h('button', { class: 'btn btn--sm', type: 'button', onclick: async () => {
      const r = await askPassword(ctx, {
        title: t.paused ? 'Resume the local TeslaMate?' : 'Pause the local TeslaMate?', submit: t.paused ? 'Resume' : 'Pause',
        intro: h('p', { class: 'note' }, t.paused ? 'MQTT is read again from now on, and the database catch-up (when on) starts again.' : 'Both connections are closed and nothing is read until you resume it. The history already stored stays.'),
        run: guarded((cred) => ctx.api.patch(TM_BASE, withPw(cred, { version: t.version, paused: !t.paused }))),
      });
      await after(r, t.paused ? 'Local TeslaMate resumed.' : 'Local TeslaMate paused.');
    } }, icon(t.paused ? 'live' : 'moon', 'ico--sm'), t.paused ? 'Resume' : 'Pause') : null;
    const removeBtn = t.enabled ? h('button', { class: 'btn btn--sm btn--ghost ta-remove', type: 'button', onclick: async () => {
      const r = await askPassword(ctx, {
        title: 'Remove the local TeslaMate?', danger: true, submit: 'Remove',
        intro: h('p', { class: 'note' }, 'Reading stops at once and the sealed settings, passwords included, are deleted. The history already stored from it stays, marked as data from local TeslaMate.'),
        run: guarded((cred) => ctx.api.send('DELETE', TM_BASE, withPw(cred))),
      });
      await after(r, 'Local TeslaMate removed. Its history stays.');
    } }, icon('x', 'ico--sm'), 'Remove') : null;
    return h('div', { class: 'panel panel-pad ta-status' },
      h('div', { class: 'ta-acc-head' },
        h('h3', { class: 'panel-title' }, icon('pulse'), 'Status'),
        t.enabled ? h('div', { class: 'row ta-acc-acts' }, pauseBtn, removeBtn) : null),
      figures([
        metric('MQTT', h('span', { title: ms[2] }, ms[0]), null, { size: 'sm', explain: sm.connected_since_ms ? `Connected since ${at(sm.connected_since_ms)}.` : ms[2] }),
        metric('Last message', sm.last_message_ms ? ago(sm.last_message_ms) : '—', null, { size: 'sm', explain: sm.last_message_ms ? at(sm.last_message_ms) : 'None yet.' }),
        metric('Messages today', sm.messages_today ?? 0, 'number', { size: 'sm', digits: 0 }),
        metric('Database catch-up', h('span', { title: ds[2] }, ds[0]), null, { size: 'sm', explain: ds[2] }),
        metric('Last sync', sd.last_sync_ms ? ago(sd.last_sync_ms) : '—', null, { size: 'sm', explain: sd.next_sync_ms ? `Next: ${at(sd.next_sync_ms)}.` : 'None yet.' }),
        metric('Caught up', sd.last_sync_ms ? `${num(sd.drives)} drives, ${num(sd.charges)} charges` : '—', null, { size: 'sm', explain: sd.last_sync_ms ? `${num(sd.positions)} positions.` : 'Nothing yet.' }),
      ], 'si-figs'),
      errLine('MQTT error', sm.last_error), errLine('Database error', sd.last_error),
      !sm.last_error && !sd.last_error ? h('p', { class: 'si-last-error faint' }, icon('check', 'ico--sm'), ' No error.') : null,
      h('div', { class: 'ta-hosts' }, h('span', { class: 'metric-label' }, 'Hosts it talks to'),
        t.hosts?.length ? h('ul', { class: 'ta-host-list' }, t.hosts.map((x) => h('li', null, code(x))))
          : h('p', { class: 'faint' }, 'None while it is not added.')));
  }

  /* ---------- the two connection forms */

  function connPanel(part) {
    const saved = t[part] || TM_DEFAULTS[part];
    const isDb = part === 'database';
    const path = (n) => `${part}.${n}`;
    const enabledF = isDb ? field({ label: 'Catch up from TeslaMate\'s database', name: 'enabled', path: path('enabled'), type: 'checkbox', value: saved.enabled, hint: 'Off by default. Reads TeslaMate\'s drives, charges and positions now and then, for what MQTT missed. A read-only database user is enough.' }) : null;
    const hostF = field({ label: 'Host', name: 'host', path: path('host'), value: saved.host || '', hint: isDb ? 'Where TeslaMate\'s PostgreSQL listens.' : TM_HOST_HINT, attrs: { autocomplete: 'off', spellcheck: 'false', placeholder: '192.0.2.10' } });
    const portF = field({ label: 'Port', name: 'port', path: path('port'), type: 'number', value: saved.port, hint: isDb ? '5432 is PostgreSQL\'s usual port.' : '1883 without TLS, 8883 with.', attrs: { min: '1', max: '65535', step: '1' } });
    const dbF = isDb ? field({ label: 'Database', name: 'database', path: path('database'), value: saved.database || '', hint: 'TeslaMate\'s is teslamate.', attrs: { autocomplete: 'off', spellcheck: 'false' } }) : null;
    const userF = field({ label: 'User name', name: 'username', path: path('username'), value: saved.username || '', hint: isDb ? 'A user that may read TeslaMate\'s tables.' : 'Empty for a broker without accounts.', attrs: { autocomplete: 'off', spellcheck: 'false' } });
    const passF = field({ label: 'Password', name: 'password', path: path('password'), type: 'password', hint: saved.password_set ? 'A password is saved. Leave this empty to keep it; it is never shown again.' : 'No password saved.', attrs: { autocomplete: 'new-password', placeholder: saved.password_set ? 'saved' : '' } });
    const removeF = saved.password_set ? field({ label: 'Remove the saved password', name: 'password_remove', path: path('password_remove'), type: 'checkbox', value: false }) : null;
    const tlsF = field({ label: 'Encrypt the connection (TLS)', name: 'tls', path: path('tls'), type: 'checkbox', value: saved.tls, hint: isDb ? 'When TeslaMate\'s PostgreSQL offers TLS.' : 'When the broker offers TLS (usually on port 8883).' });
    const pinF = field({ label: 'Pinned certificate (SHA-256)', name: 'pin_sha256', path: path('pin_sha256'), value: saved.pin_sha256 || '', hint: tmPinNote(part), attrs: { autocomplete: 'off', spellcheck: 'false' } });
    const plainF = field({ label: UNENCRYPTED_LABEL, name: 'allow_unencrypted', path: path('allow_unencrypted'), type: 'checkbox', value: saved.allow_unencrypted, hint: 'Off by default. Needed only to send a password without TLS.' });
    const warn = h('p', { class: 'setting-note warn tm-warn' }, icon('warn', 'ico--sm'), ' ', UNENCRYPTED_WARNING);
    const prefixF = !isDb ? field({ label: 'Topic prefix', name: 'prefix', path: path('prefix'), value: saved.prefix || 'teslamate', hint: 'TeslaMate\'s MQTT_NAMESPACE, teslamate unless you changed it. The server subscribes to <prefix>/cars/+/#.', attrs: { autocomplete: 'off', spellcheck: 'false' } }) : null;
    const ivF = isDb ? field({ label: 'Catch up every (seconds)', name: 'interval_s', path: path('interval_s'), type: 'number', value: saved.interval_s, hint: 'From 60 to 86400. 300 is every five minutes.', attrs: { min: '60', max: '86400', step: '1' } }) : null;
    const fields = [enabledF, hostF, portF, dbF, userF, passF, removeF, prefixF, ivF, tlsF, pinF, plainF].filter(Boolean);
    const testOut = h('div', { class: 'si-test', 'aria-live': 'polite' });
    const testBtn = h('button', { class: 'btn', type: 'button' }, icon('pulse', 'ico--sm'), 'Test');
    const f = form({
      fields, submit: t.enabled ? 'Save' : isDb ? 'Save' : 'Save and add', cls: 'si-form tm-form', secondary: testBtn,
      async onSubmit(v) {
        const d = tmDraft(part, v);
        const bad = tmCheck(part, d, t.enabled ? saved : null);
        if (Object.keys(bad).length) { f.setErrors(bad); return; }
        const r = await askPassword(ctx, {
          title: t.enabled ? `Save the ${PART_NAME[part]} settings` : 'Add the local TeslaMate', submit: t.enabled ? 'Save' : 'Add',
          intro: h('p', { class: 'note' }, t.enabled ? 'The server reconnects with these settings at once.' : 'The server subscribes to TeslaMate\'s topics. Nothing is read from a car until you match it below.'),
          run: guarded((cred) => ctx.api.put(TM_BASE, withPw(cred, tmPutBody(t, part, d))), f),
        });
        await after(r, t.enabled ? `${PART_NAME[part] === 'MQTT' ? 'MQTT' : 'Database'} settings saved.` : 'Local TeslaMate added. Match its cars to start reading them.');
      },
    });
    testBtn.addEventListener('click', async () => {
      f.setErrors(null); f.setMessage(null);
      const d = tmDraft(part, f.values());
      if (isDb) d.enabled = true;
      const bad = tmCheck(part, d, t.enabled ? saved : null);
      if (Object.keys(bad).length) { f.setErrors(bad); return; }
      replace(testOut, h('p', { class: 'setting-note' }, isDb ? 'Testing: connecting to the database and looking for TeslaMate\'s tables. Nothing is saved.' : 'Testing: connecting to the broker and listening a few seconds for TeslaMate\'s cars. It subscribes only and publishes nothing; nothing is saved.'));
      const r = await direct(testBtn, () => ctx.api.post(`${TM_BASE}/test`, { target: part, [part]: d }), f);
      if (!r || r.fieldErr || r.stale) replace(testOut);
      if (!r || r.fieldErr) return;
      if (r.stale) { flash(r.stale, { kind: 'info' }); await load(); return; }
      replace(testOut, h('p', { class: ['si-test-line', 'tm-test-line', r.ok ? 'ok' : 'warn'] }, icon(r.ok ? 'check' : 'warn', 'ico--sm'), h('span', null,
        r.ok ? r.message : (tmErrorText({ code: r.error, message: r.message }) || 'It did not work') + '.',
        r.ok && r.latency_ms != null ? ` ${num(r.latency_ms)} ms.` : '',
        r.ok && r.cars_seen?.length ? [' TeslaMate cars: ', r.cars_seen.map((x, i) => [i ? ', ' : '', code(x)]), '.'] : '',
        h('span', { class: 'faint' }, ` ${at(r.at_ms)}`))));
    });
    const sync = () => {
      const tls = tlsF.input.checked;
      const on = !isDb || enabledF.input.checked;
      for (const x of fields) if (x !== enabledF) x.el.hidden = !on;
      pinF.el.hidden = !on || !tls;
      plainF.el.hidden = !on || tls;
      warn.hidden = !on || tls;
      testBtn.hidden = !on;
    };
    // TLS on moves the port from 1883 to 8883, off moves it back; a port typed by hand stays
    tlsF.input.addEventListener('change', () => { const to = tmTlsPort(part, portF.input.value, tlsF.input.checked); if (to != null) portF.input.value = String(to); });
    tlsF.input.addEventListener('change', sync);
    enabledF?.input.addEventListener('change', sync);
    plainF.el.after(warn);
    sync();
    const needsMqtt = isDb && !t.enabled ? h('p', { class: 'setting-note' }, 'Add the MQTT connection first; the catch-up is saved with it afterwards.') : null;
    if (needsMqtt) f.el.querySelector('button[type="submit"]').disabled = true;
    return h('div', { class: 'panel panel-pad ta-status tm-conn', id: isDb ? 'teslamate-database' : 'teslamate-mqtt' },
      h('div', { class: 'panel-head' }, h('h3', { class: 'panel-title' }, icon(isDb ? 'calendar' : 'globe'), isDb ? 'Database catch-up (optional)' : 'MQTT'),
        saved.password_set ? tag('Password saved', 'plain') : null),
      f.el, needsMqtt, testOut);
  }

  /* ---------- the cars */

  function carsPanel() {
    const items = t.cars || [];
    if (!items.length) return h('div', { class: 'panel panel-pad ta-account' }, emptyState({ icon: 'car', title: 'No TeslaMate car yet', text: 'No car has been seen on TeslaMate\'s topics yet. It shows here once TeslaMate publishes one.' }));
    const opts = [['', 'Not decided (not read)'], ...cars.map((c) => [c.id, c.name]), ['new', 'A new car'], ['leave_out', 'Leave out (not read)']];
    const sels = new Map();
    const chosen = () => new Map([...sels].map(([k, s]) => [k, s.value]));
    const saveBtn = h('button', { class: 'btn btn--primary btn--sm', type: 'button' }, icon('check', 'ico--sm'), 'Save matches');
    const sync = () => { saveBtn.hidden = !tmMatchChanges(items, chosen()).length; };
    saveBtn.addEventListener('click', async () => {
      const ch = tmMatchChanges(items, chosen());
      if (!ch.length) return;
      const r = await direct(saveBtn, () => ctx.api.put(`${TM_BASE}/cars`, { version: t.version, items: ch }));
      await after(r, 'Matches saved.');
    });
    const rows = items.map((c) => {
      const pre = tmPreselect(c);
      const known = opts.some(([v]) => v === pre) ? opts : [...opts, [pre, carName(pre)]];
      const sel = h('select', { class: 'select', 'aria-label': `Sentry Mate car for ${c.display_name || c.teslamate_car}` },
        known.map(([v, l]) => h('option', { value: v, selected: v === pre }, l)));
      sel.addEventListener('change', sync);
      sels.set(c.teslamate_car, sel);
      return h('tr', null,
        h('td', null, h('span', { class: 'cell-text' }, h('b', null, c.display_name || 'No name in TeslaMate'), h('span', { class: 'sub' }, `TeslaMate car ${c.teslamate_car}`, ' · ', h('span', { class: 'mono' }, c.vin_prefix || '—')))),
        h('td', null, h('div', { class: 'ta-match' }, sel, c.match == null && c.suggested ? tag('Suggested: same VIN prefix', 'live') : null)),
        h('td', { class: 'num' }, h('span', { class: 'ta-cell-label' }, 'Last message '), when(c.last_message_ms)));
    });
    sync();
    return h('div', { class: 'panel panel-pad ta-account' },
      h('div', { class: 'ta-cars-head' }, h('h3', { class: 'panel-title' }, icon('car'), items.length === 1 ? 'Its car' : `Its cars (${items.length})`), saveBtn),
      h('div', { class: 'tbl-scroll' }, h('table', { class: 'tbl ta-tbl' },
        h('thead', null, h('tr', null, ['Car in TeslaMate', 'Sentry Mate car', 'Last message'].map((x, i) => h('th', { class: i === 2 ? 'num' : null, scope: 'col' }, x)))),
        h('tbody', null, rows))),
      kv([['Not matched', `${num(items.filter((c) => c.match == null).length)} of ${num(items.length)}`, { icon: 'info' }]]),
      h('p', { class: 'setting-note' }, 'A TeslaMate car is read only once it is matched to a car here or to a new car. A suggestion has the same VIN prefix as a car here; it is not saved until you press Save matches.'));
  }

  return { el, load };
}
