// Settings: Integrations, Local TeslaMate (#teslamate): the owner's own TeslaMate on the local network as a live
// input, part of pages/settings-integrations.js. The server subscribes to TeslaMate's MQTT topics (it only reads and
// never publishes) and, when an admin turns it on, catches up from TeslaMate's PostgreSQL database now and then. Off
// until an admin adds it; data from it is labelled "Data from local TeslaMate".
//
// Built like the TeslaFi section (pages/settings-teslafi.js, its tz-* look): status on a dark face, the MQTT form
// and the optional database catch-up form (each with Test and Save), the match of TeslaMate's cars, Pause, Remove.
// Admins only, reading included. Every change, and every test, asks for the admin's own password again (and the
// two-step code). The rules and words every pack shares are in shared/teslamate.js.
//
// API: GET/PUT/PATCH/DELETE integrations/teslamate, POST integrations/teslamate/test, PUT integrations/teslamate/cars.

import { h, replace } from '../shared/dom.js';
import { formatNumber } from '../shared/i18n.js';
import { TM_BASE, TM_DEFAULTS, MQTT_STATE, DB_STATE, UNENCRYPTED_LABEL, UNENCRYPTED_WARNING, tmErrorText, tmWords, tmStale, tmDraft, tmCheck, tmPutBody, tmMatchChanges, tmPreselect, tmMatchWords, tmHeadline } from '../shared/teslamate.js';
import { icon, hud, btn, tag, note } from '../core/ui.js';
import { select } from '../core/kit.js';
import { field, form } from '../core/form.js';
import { emptyState, errorWords } from '../core/states.js';
import { toast } from '../core/layer.js';
import { dateTime, ago, DASH } from '../core/fmt.js';
import { code, sig, hint } from '../core/setting.js';
import { askPassword } from './server-move.js';

const withPw = (cred, body = {}) => ({ ...body, confirm_password: cred.password, ...(cred.two_step ? { code: cred.two_step } : {}) });
const SIG = { live: 'ok', warm: 'warn', plain: 'off' };
const inputAttrs = { spellcheck: 'false', autocomplete: 'off', autocapitalize: 'off' };

/** The Local TeslaMate section. cars: this server's cars ({ id, name }) for the match picker. Returns { el, load() }. */
export function teslamateSection(ctx, { cars = [] } = {}) {
  const admin = ctx.admin;
  const side = h('span', { class: 'tz-side' });
  const body = h('div', { class: 'stack' });
  const el = h('section', { class: 'stack tz', id: 'teslamate', 'aria-label': 'Local TeslaMate' },
    hud({ title: 'Local TeslaMate', icon: 'antenna', tone: 'cyan', side },
      h('div', { class: 'stack' },
        h('p', { class: 'tz-intro' }, 'Your own TeslaMate on the local network as a live input: the server subscribes to TeslaMate\'s MQTT topics and, if you turn it on, catches up from its database for what MQTT missed. It only reads: it never publishes, never wakes the car and never sends a command. Off until an admin adds it. Where SMO, the Tesla API or TeslaFi cover the same time, theirs win; pages label the rest "Data from local TeslaMate".'),
        body)));
  const carSlot = h('div', { class: 'stack' });
  el.append(carSlot);

  let t = null;
  const carName = (id) => cars.find((c) => c.id === id)?.name || id;

  // the password again; a stale version or a removed input reloads; field errors of a page form go to that form
  async function ask(opts, f = null) {
    let stale = false, fieldErr = null;
    const r = await askPassword(ctx, { ...opts, run: async (cred, v) => {
      try { return await opts.run(cred, v); } catch (e) {
        tmWords(e);
        if (tmStale(e)) { stale = true; throw e; }
        if (f && e.fields && Object.keys(e.fields).some((k) => k.startsWith('mqtt.') || k.startsWith('database.'))) { fieldErr = e; return null; }
        throw e;
      }
    } });
    if (fieldErr) { showFieldErr(f, fieldErr); return null; }
    if (!r && stale && ctx.alive()) await load();
    return r;
  }
  const showFieldErr = (f, e) => {
    const left = [];
    for (const [p, m] of Object.entries(e.fields || {})) { const x = f.fields.find((y) => y.path === p); if (x) x.setError(m); else left.push(m); }
    f.setMessage(left.join(' ') || null);
  };

  async function load() {
    if (!admin) {
      replace(side, tag('Admins only', 'org'));
      replace(body, note('Only an admin sees and changes the local TeslaMate: its connections, status and cars. Every page still says where its data came from, "Data from local TeslaMate" included.'));
      replace(carSlot);
      return;
    }
    try {
      t = await ctx.api.get(TM_BASE);
    } catch (e) {
      if (e?.name === 'AbortError') throw e;
      replace(side, sig('warn', 'Not read'));
      replace(body, h('p', { class: 'warn' }, 'The local TeslaMate settings could not be read: ', errorWords(e).text));
      return;
    }
    if (!ctx.alive()) return;
    paint();
  }
  const reload = async (msg) => { if (msg) toast(msg); await load(); };

  function paint() {
    const [word, tone] = tmHeadline(t);
    replace(side, sig(SIG[tone], word));
    replace(body, h('div', { class: 'tz-grid' }, h('div', { class: 'stack' }, statusPanel(), t.enabled ? actions() : addPanel(), connPanel('database')), connPanel('mqtt')));
    replace(carSlot, t.enabled ? carsPanel() : null);
  }

  function addPanel() {
    return h('div', { class: 'tz-add' },
      h('p', { class: 'tz-add-head' }, icon('plus', 'i--sm'), h('span', null, 'Add your local TeslaMate')),
      h('p', { class: 'tz-dim' }, 'Type the broker TeslaMate publishes to and its topic prefix (teslamate unless you changed it), press Test to see TeslaMate\'s cars, then Save. Nothing is read from a car until you match it here.'));
  }

  /* ---------- status, pause, remove */
  function statusPanel() {
    const sm = t.status?.mqtt || {};
    const sd = t.status?.database || {};
    const ms = MQTT_STATE[sm.state] || [sm.state || 'Off', 'plain', ''];
    const ds = DB_STATE[sd.state] || [sd.state || 'Off', 'plain', ''];
    const errs = [['MQTT', sm.last_error], ['Database', sd.last_error]].filter(([, e]) => e);
    return h('div', { class: 'tz-face' },
      h('div', { class: 'tz-read' },
        h('div', { class: 'tz-fig' }, h('span', { class: 'tz-k' }, 'MQTT'), h('span', { class: 'tz-v' }, ms[0]), h('span', { class: 'tz-how' }, sm.connected_since_ms ? `since ${dateTime(sm.connected_since_ms, ctx.tz)}` : ms[2])),
        h('div', { class: 'tz-fig' }, h('span', { class: 'tz-k' }, 'Last message'), h('span', { class: 'tz-v' }, sm.last_message_ms ? ago(sm.last_message_ms) : DASH), h('span', { class: 'tz-how' }, `${formatNumber(sm.messages_today ?? 0)} today`)),
        h('div', { class: 'tz-fig' }, h('span', { class: 'tz-k' }, 'Database'), h('span', { class: 'tz-v' }, ds[0]), h('span', { class: 'tz-how' }, sd.last_sync_ms ? `synced ${ago(sd.last_sync_ms)}` : ds[2])),
        sd.last_sync_ms ? h('div', { class: 'tz-fig' }, h('span', { class: 'tz-k' }, 'Caught up'), h('span', { class: 'tz-v' }, `${formatNumber(sd.drives)} drives`), h('span', { class: 'tz-how' }, `${formatNumber(sd.charges)} charges${sd.next_sync_ms ? `, next ${dateTime(sd.next_sync_ms, ctx.tz)}` : ''}`)) : null),
      errs.length ? errs.map(([n, e]) => h('p', { class: 'tz-err' }, icon('warn', 'i--sm'), h('span', null, `${n}: `, h('b', null, tmErrorText(e)), e.at_ms ? `, ${dateTime(e.at_ms, ctx.tz)} (${ago(e.at_ms)})` : '', '.')))
        : h('p', { class: 'tz-ok' }, icon('check', 'i--sm'), h('span', null, t.enabled ? 'No error.' : 'Nothing is read from TeslaMate yet.')),
      h('div', { class: 'tz-block' }, h('p', { class: 'tz-k' }, 'Hosts it talks to'),
        t.hosts?.length ? h('ul', { class: 'tz-hosts' }, t.hosts.map((x) => h('li', null, code(x)))) : h('p', { class: 'tz-dim' }, 'None while it is not added.')),
    );
  }
  function actions() {
    const pauseBtn = btn(t.paused ? 'Resume' : 'Pause', async () => {
      const r = await ask({
        title: t.paused ? 'Resume the local TeslaMate?' : 'Pause the local TeslaMate?', submit: t.paused ? 'Resume' : 'Pause',
        intro: h('p', null, t.paused ? 'MQTT is read again from now on, and the database catch-up (when on) starts again.' : 'Both connections are closed and nothing is read until you resume it. The history already stored stays.'),
        run: async (cred) => ctx.api.patch(TM_BASE, withPw(cred, { version: t.version, paused: !t.paused })),
      });
      if (r) await reload(t.paused ? 'Local TeslaMate resumed.' : 'Local TeslaMate paused.');
    }, { sm: true, icon: t.paused ? 'live' : 'moon' });
    const removeBtn = btn('Remove', async () => {
      const r = await ask({
        title: 'Remove the local TeslaMate?', danger: true, submit: 'Remove it',
        intro: h('p', null, 'Reading stops at once and its sealed settings, passwords included, are deleted. The history already stored stays, marked as data from local TeslaMate.'),
        run: async (cred) => ctx.api.send('DELETE', TM_BASE, withPw(cred)),
      });
      if (r) await reload('Local TeslaMate removed. Its history stays.');
    }, { sm: true, icon: 'x', kind: 'ghost' });
    return h('div', { class: 'stack' }, h('div', { class: 'row' }, pauseBtn, removeBtn),
      h('p', { class: 'tz-dim' }, 'Every change asks for your password again. Pause closes both connections; Remove deletes the sealed settings, and the history already stored stays.'));
  }

  /* ---------- the MQTT and database forms */
  function connPanel(part) {
    const saved = t[part] || TM_DEFAULTS[part];
    const isDb = part === 'database';
    const p = (n) => `${part}.${n}`;
    const fEnabled = isDb ? field({ label: 'Catch up from TeslaMate\'s database', name: 'enabled', path: p('enabled'), type: 'checkbox', value: saved.enabled, hint: 'Off by default. Reads TeslaMate\'s drives, charges and positions now and then, for what MQTT missed. A read-only database user is enough.' }) : null;
    const fHost = field({ label: 'Host', name: 'host', path: p('host'), value: saved.host || '', hint: isDb ? 'Where TeslaMate\'s PostgreSQL listens.' : 'The broker TeslaMate publishes to: a host name or IP address, without mqtt://.', attrs: { ...inputAttrs, class: 'input mono', placeholder: '192.0.2.10' } });
    const fPort = field({ label: 'Port', name: 'port', path: p('port'), type: 'number', value: saved.port, hint: isDb ? '5432 is PostgreSQL\'s usual port.' : '1883 without TLS, 8883 with.', attrs: { min: '1', max: '65535', step: '1' } });
    const fDb = isDb ? field({ label: 'Database', name: 'database', path: p('database'), value: saved.database || '', hint: 'TeslaMate\'s is teslamate.', attrs: inputAttrs }) : null;
    const fUser = field({ label: 'User name', name: 'username', path: p('username'), value: saved.username || '', hint: isDb ? 'A user that may read TeslaMate\'s tables.' : 'Empty for a broker without accounts.', attrs: inputAttrs });
    const fPass = field({ label: 'Password', name: 'password', path: p('password'), type: 'password', hint: saved.password_set ? 'A password is saved. Leave this empty to keep it; it is never shown again.' : 'No password saved.', attrs: { autocomplete: 'new-password', placeholder: saved.password_set ? 'saved' : '' } });
    const fRemove = saved.password_set ? field({ label: 'Remove the saved password', name: 'password_remove', path: p('password_remove'), type: 'checkbox', value: false }) : null;
    const fPrefix = !isDb ? field({ label: 'Topic prefix', name: 'prefix', path: p('prefix'), value: saved.prefix || 'teslamate', hint: 'TeslaMate\'s MQTT_NAMESPACE, teslamate unless you changed it. The server subscribes to <prefix>/cars/+/#.', attrs: { ...inputAttrs, class: 'input mono' } }) : null;
    const fIv = isDb ? field({ label: 'Catch up every (seconds)', name: 'interval_s', path: p('interval_s'), type: 'number', value: saved.interval_s, hint: 'From 60 to 86400. 300 is every five minutes.', attrs: { min: '60', max: '86400', step: '1' } }) : null;
    const fTls = field({ label: 'Encrypt the connection (TLS)', name: 'tls', path: p('tls'), type: 'checkbox', value: saved.tls, hint: isDb ? 'When TeslaMate\'s PostgreSQL offers TLS.' : 'When the broker offers TLS (usually on port 8883).' });
    const fPin = field({ label: 'Pinned certificate (SHA-256)', name: 'pin_sha256', path: p('pin_sha256'), value: saved.pin_sha256 || '', hint: 'For a host with its own (self-signed) certificate: 64 hex digits, colons allowed. Empty: the certificate must be valid for the host.', attrs: { ...inputAttrs, class: 'input mono' } });
    const fPlain = field({ label: UNENCRYPTED_LABEL, name: 'allow_unencrypted', path: p('allow_unencrypted'), type: 'checkbox', value: saved.allow_unencrypted, hint: 'Off by default. Needed only to send a password without TLS.' });
    fPlain.el.append(h('p', { class: 'tz-err tm-warn' }, icon('warn', 'i--sm'), h('span', null, UNENCRYPTED_WARNING)));
    const fields = [fEnabled, fHost, fPort, fDb, fUser, fPass, fRemove, fPrefix, fIv, fTls, fPin, fPlain].filter(Boolean);
    const values = () => Object.fromEntries(fields.map((x) => [x.name, x.read()]));
    const out = h('div', { class: 'tm-test', 'aria-live': 'polite' });
    const testBtn = btn('Test', () => test(), { icon: 'pulse' });
    const f = form({
      fields, submit: t.enabled ? 'Save' : 'Save and add', secondary: testBtn,
      async onSubmit(v) {
        const d = tmDraft(part, v);
        const bad = tmCheck(part, d, t.enabled ? saved : null);
        if (Object.keys(bad).length) { showFieldErr(f, { fields: bad }); return; }
        const r = await ask({
          title: t.enabled ? `Save the ${isDb ? 'database' : 'MQTT'} settings` : 'Add the local TeslaMate', submit: t.enabled ? 'Save' : 'Add',
          intro: h('p', null, t.enabled ? 'The server reconnects with these settings at once.' : 'The server subscribes to TeslaMate\'s topics. Nothing is read from a car until you match it below.'),
          run: async (cred) => ctx.api.put(TM_BASE, withPw(cred, tmPutBody(t, part, d))),
        }, f);
        if (r) await reload(t.enabled ? `${isDb ? 'Database' : 'MQTT'} settings saved.` : 'Local TeslaMate added. Match its cars to start reading them.');
      },
    });
    async function test() {
      f.clear();
      const d = tmDraft(part, values());
      if (isDb) d.enabled = true;
      const bad = tmCheck(part, d, t.enabled ? saved : null);
      if (Object.keys(bad).length) { showFieldErr(f, { fields: bad }); return; }
      const r = await ask({
        title: `Test the ${isDb ? 'database' : 'MQTT'} connection`, submit: 'Test',
        intro: h('p', null, isDb ? 'Connects to the database with these settings and looks for TeslaMate\'s tables. Nothing is saved.' : 'Connects to the broker with these settings and listens a few seconds for TeslaMate\'s cars. It subscribes only and publishes nothing; nothing is saved.'),
        run: async (cred) => ctx.api.post(`${TM_BASE}/test`, withPw(cred, { target: part, [part]: d })),
      }, f);
      if (!r) return;
      replace(out, h('p', { class: ['tm-test-line', !r.ok && 'warn'] }, icon(r.ok ? 'check' : 'warn', 'i--sm'),
        h('span', null, r.ok ? r.message : `${tmErrorText({ code: r.error, message: r.message }) || 'It did not work'}.`,
          r.ok && r.latency_ms != null ? ` ${formatNumber(r.latency_ms)} ms.` : '',
          r.ok && r.cars_seen?.length ? [' TeslaMate cars: ', r.cars_seen.map((x, i) => [i ? ', ' : '', code(x)]), '.'] : '',
          h('span', { class: 'tz-dim' }, ` ${dateTime(r.at_ms, ctx.tz)}`))));
    }
    const sync = () => {
      const tls = fTls.input.checked;
      const on = !isDb || fEnabled.input.checked;
      for (const x of fields) if (x !== fEnabled) x.el.hidden = !on;
      fPin.el.hidden = !on || !tls;
      fPlain.el.hidden = !on || tls;
      testBtn.hidden = !on;
    };
    fTls.input.addEventListener('change', sync);
    fEnabled?.input.addEventListener('change', sync);
    sync();
    const needsMqtt = isDb && !t.enabled;
    if (needsMqtt) f.submitButton.disabled = true;
    return h('div', { class: 'tz-add tm-conn', id: isDb ? 'teslamate-database' : 'teslamate-mqtt' },
      h('p', { class: 'tz-add-head' }, icon(isDb ? 'clock' : 'antenna', 'i--sm'), h('span', null, isDb ? 'Database catch-up (optional)' : 'MQTT'), saved.password_set ? tag('Password saved', 'cyan') : null),
      f.el, needsMqtt ? hint('Add the MQTT connection first; the catch-up is saved with it afterwards.') : null, out);
  }

  /* ---------- TeslaMate's cars, matched to cars here */
  function carsPanel() {
    const items = t.cars || [];
    const wrap = (...c) => hud({ title: 'TeslaMate\'s cars', icon: 'car', id: 'teslamate-cars', cls: 'tz-acc', label: 'TeslaMate\'s cars' }, h('div', { class: 'stack' }, c));
    if (!items.length) return wrap(emptyState({ icon: 'car', title: 'No TeslaMate car yet', text: 'No car has been seen on TeslaMate\'s topics yet. It shows here once TeslaMate publishes one.' }));
    const opts = [['', 'Not decided (not read)'], ...cars.map((c) => [c.id, c.name]), ['new', 'A new car'], ['leave_out', 'Leave out (not read)']];
    const sels = new Map();
    const chosen = () => new Map([...sels].map(([k, s]) => [k, s.value]));
    const save = btn('Save matches', () => saveMatches(), { kind: 'go', sm: true, icon: 'check' });
    const sync = () => { save.hidden = !tmMatchChanges(items, chosen()).length; };
    async function saveMatches() {
      const ch = tmMatchChanges(items, chosen());
      if (!ch.length) return;
      const r = await ask({
        title: 'Match TeslaMate\'s cars', submit: 'Save matches',
        intro: h('ul', { class: 'tz-list' }, ch.map((c) => h('li', null, h('b', null, items.find((x) => x.teslamate_car === c.teslamate_car)?.display_name || `TeslaMate car ${c.teslamate_car}`), ': ', tmMatchWords(c.match, carName)))),
        run: async (cred) => ctx.api.put(`${TM_BASE}/cars`, withPw(cred, { version: t.version, items: ch })),
      });
      if (r) await reload('Matches saved.');
    }
    const rows = items.map((c) => {
      const pre = tmPreselect(c);
      const known = opts.some(([v]) => v === pre) ? opts : [...opts, [pre, carName(pre)]];
      const sel = select(known, pre, sync, { label: `Car here for ${c.display_name || c.teslamate_car}`, cls: 'tz-sel' });
      sels.set(c.teslamate_car, sel);
      return h('li', { class: 'tz-car' },
        h('div', { class: 'tz-car-id' }, h('p', { class: 'tz-car-name' }, icon('car', 'i--sm'), h('b', null, c.display_name || 'No name in TeslaMate')),
          h('p', { class: 'tz-dim' }, `TeslaMate car ${c.teslamate_car} · `, h('span', { class: 'mono' }, c.vin_prefix || DASH))),
        h('div', { class: 'tz-car-match' }, h('span', { class: 'tz-k' }, 'Car here'), sel,
          c.match == null && c.suggested ? h('span', { class: 'tz-sugg' }, tag('Suggested', 'grn'), ' same VIN start as ', carName(c.suggested), '; saved only with Save matches') : null),
        h('div', { class: 'tz-car-mode' }, h('span', { class: 'tz-k' }, 'Last message'), h('span', null, c.last_message_ms ? ago(c.last_message_ms) : 'none yet')));
    });
    sync();
    return wrap(h('div', { class: 'tz-cars' },
      h('div', { class: 'tz-cars-head' }, h('p', { class: 'tz-k' }, `${items.length} ${items.length === 1 ? 'car' : 'cars'} seen on TeslaMate's topics`), save),
      h('ul', { class: 'tz-car-list' }, rows),
      hint('A TeslaMate car is read only once it is matched to a car here or to a new car. A suggestion has the same VIN start as a car here; nothing changes until you press Save matches.')));
  }

  return { el, load };
}
