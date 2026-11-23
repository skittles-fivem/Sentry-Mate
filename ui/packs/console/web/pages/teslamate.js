// settings/integrations, the local teslamate block (#teslamate): the owner's own TeslaMate on the local network as a
// live input. The server subscribes to TeslaMate's MQTT topics (it only reads, never publishes) and, when an admin
// turns it on, catches up from TeslaMate's PostgreSQL database now and then. Off until an admin adds it; data from it
// says "Data from local TeslaMate" ([tml]).
//
// Admins only, reading included. Every change, and every test, asks for the admin's own password again (and the
// two-step code): askPassword() from core/secret.js, sent as confirm_password and code, as the teslafi block does.
// The rules and words every pack shares are in shared/teslamate.js.
//
// API: GET/PUT/PATCH/DELETE integrations/teslamate, POST integrations/teslamate/test, PUT integrations/teslamate/cars.

import { h, replace } from '../shared/dom.js';
import { formatNumber } from '../shared/i18n.js';
import { TM_BASE, TM_DEFAULTS, TM_HOST_HINT, tmPinNote, tmTlsPort, MQTT_STATE, DB_STATE, UNENCRYPTED_LABEL, UNENCRYPTED_WARNING, tmErrorText, tmWords, tmStale, tmDraft, tmCheck, tmPutBody, tmMatchChanges, tmPreselect, tmMatchWords, tmHeadline } from '../shared/teslamate.js';
import { section, kv, btn, note } from '../core/ui.js';
import { table } from '../core/table.js';
import { field, form } from '../core/form.js';
import { errorWords } from '../core/states.js';
import { tag, dateTime, ago, DASH } from '../core/fmt.js';
import { toast } from '../core/toast.js';
import { askPassword } from '../core/secret.js';

const withPw = (cred, body = {}) => ({ ...body, confirm_password: cred.password, ...(cred.two_step ? { code: cred.two_step } : {}) });
const code = (t) => h('span', { class: 'prompt' }, t);
const TONE = { live: 'ok', warm: 'warn', plain: 'muted' };
const lower = (s) => String(s || '').toLowerCase();
const inAttrs = { autocomplete: 'off', spellcheck: 'false', autocapitalize: 'off' };

/** The local teslamate block. cars: Sentry Mate's cars ({ id, name }). Returns { el, side, load() }. */
export function teslamateSection(ctx, { cars = [] } = {}) {
  const admin = ctx.admin;
  const side = h('span');
  const body = h('div', { 'aria-live': 'polite' });
  const el = section({ title: 'local teslamate', id: 'teslamate', side },
    note('Your own TeslaMate on the local network as a live input: this server subscribes to TeslaMate\'s MQTT topics and, if you turn it on, catches up from its database for what MQTT missed. It only reads: never a publish, a wake or a command. Off until an admin adds it. Where SMO, the Tesla API or TeslaFi saw the same time, theirs win; the rest says data from local TeslaMate.'),
    body);

  let t = null;
  const carName = (id) => cars.find((c) => c.id === id)?.name || id;
  const tables = [];

  async function load() {
    if (!admin) {
      replace(side, tag('admins only', 'muted'));
      replace(body, note(tag('admin', 'muted'), ' Only an admin sees and changes the local TeslaMate: its connections, status and cars. Every page still says where its data came from, ', h('span', { class: 'src' }, h('span', { class: 'src-b' }, '[', h('span', { class: 'src-k src-k--teslamate_local' }, 'tml'), ']')), ' for data from local TeslaMate.'));
      return;
    }
    try {
      t = await ctx.api.get(TM_BASE);
    } catch (e) {
      if (e?.name === 'AbortError') return;
      replace(side, tag('not read', 'bad'));
      replace(body, h('p', { class: 'note tone--bad' }, 'The local TeslaMate settings could not be read: ', errorWords(e).text));
      return;
    }
    if (!ctx.alive()) return;
    paint();
    await Promise.all(tables.map((x) => x.ready)).catch(() => {});
  }
  const reload = async (msg) => { if (msg) toast(msg); await load().catch(() => {}); };

  // the password again; stale answers reload; field errors of a page form go back to that form
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
    if (!r && stale && ctx.alive()) await reload();
    return r;
  }
  const showFieldErr = (f, e) => {
    const left = [];
    for (const [p, m] of Object.entries(e.fields || {})) { const x = f.fields.find((y) => y.path === p); if (x) x.setError(m); else left.push(m); }
    f.setMessage(left.join(' ') || null);
  };

  function paint() {
    tables.length = 0;
    const [word, tone] = tmHeadline(t);
    replace(side, tag(lower(word), TONE[tone]));
    replace(body, statusBlock(), connBlock('mqtt'), connBlock('database'), t.enabled ? carsBlock() : null);
  }

  function statusBlock() {
    const sm = t.status?.mqtt || {};
    const sd = t.status?.database || {};
    const ms = MQTT_STATE[sm.state] || [sm.state || 'Off', 'plain', ''];
    const ds = DB_STATE[sd.state] || [sd.state || 'Off', 'plain', ''];
    const pause = btn(t.paused ? 'resume' : 'pause', async () => {
      const r = await ask({
        title: t.paused ? 'resume the local teslamate?' : 'pause the local teslamate?', submit: t.paused ? 'resume' : 'pause',
        intro: note(t.paused ? 'MQTT is read again from now on, and the database catch-up (when on) starts again.' : 'Both connections are closed and nothing is read until you resume it. The history already stored stays.'),
        run: async (cred) => ctx.api.patch(TM_BASE, withPw(cred, { version: t.version, paused: !t.paused })),
      });
      if (r) await reload(t.paused ? 'Local TeslaMate resumed.' : 'Local TeslaMate paused.');
    }, { kind: t.paused ? 'primary' : '' });
    const remove = btn('remove', async () => {
      const r = await ask({
        title: 'remove the local teslamate?', danger: true, submit: 'remove it',
        intro: note('Reading stops at once and its sealed settings, passwords included, are deleted. The history already stored stays, marked as data from local TeslaMate.'),
        run: async (cred) => ctx.api.send('DELETE', TM_BASE, withPw(cred)),
      });
      if (r) await reload('Local TeslaMate removed. Its history stays.');
    }, { kind: 'danger' });
    const errLine = (e) => (e ? [tmErrorText(e), e.at_ms ? h('span', { class: 'muted' }, ` ${dateTime(e.at_ms, ctx.tz)} (${ago(e.at_ms)})`) : null] : h('span', { class: 'muted' }, 'none'));
    return h('div', null,
      h('p', { class: 'sub-title' }, 'status'),
      kv([
        ['mqtt', [tag(lower(ms[0]), TONE[ms[1]]), h('span', { class: 'muted' }, sm.connected_since_ms ? ` since ${dateTime(sm.connected_since_ms, ctx.tz)}` : ` ${ms[2]}`)]],
        ['last message', sm.last_message_ms ? [h('span', { title: dateTime(sm.last_message_ms, ctx.tz) }, ago(sm.last_message_ms)), h('span', { class: 'muted' }, ` ${formatNumber(sm.messages_today ?? 0)} today`)] : h('span', { class: 'muted' }, 'none yet')],
        ['mqtt error', errLine(sm.last_error), { tone: sm.last_error ? 'warn' : null }],
        ['database', [tag(lower(ds[0]), TONE[ds[1]]), h('span', { class: 'muted' }, ` ${ds[2]}`)]],
        ['last sync', sd.last_sync_ms ? [ago(sd.last_sync_ms), h('span', { class: 'muted' }, ` ${formatNumber(sd.drives)} drives, ${formatNumber(sd.charges)} charges, ${formatNumber(sd.positions)} positions${sd.next_sync_ms ? `; next ${dateTime(sd.next_sync_ms, ctx.tz)}` : ''}`)] : h('span', { class: 'muted' }, 'none yet')],
        ['database error', errLine(sd.last_error), { tone: sd.last_error ? 'warn' : null }],
        ['hosts', t.hosts?.length ? h('span', { class: 'tesla-hosts' }, t.hosts.map((x) => code(x))) : h('span', { class: 'muted' }, 'none while it is not added')],
      ], { cols: 1 }),
      t.enabled ? h('div', { class: 'acts tesla-acts' }, pause, remove) : note('Not added yet: type the broker TeslaMate publishes to below, test it, and save. Nothing is read from a car until you match it.'));
  }

  function connBlock(part) {
    const saved = t[part] || TM_DEFAULTS[part];
    const isDb = part === 'database';
    const p = (n) => `${part}.${n}`;
    const fEnabled = isDb ? field({ label: 'catch up from teslamate\'s database', name: 'enabled', path: p('enabled'), type: 'checkbox', value: saved.enabled, hint: 'Off by default. Reads TeslaMate\'s drives, charges and positions now and then, for what MQTT missed. A read-only database user is enough.' }) : null;
    const fHost = field({ label: 'host', name: 'host', path: p('host'), value: saved.host || '', hint: isDb ? 'Where TeslaMate\'s PostgreSQL listens.' : TM_HOST_HINT, attrs: { ...inAttrs, placeholder: '192.0.2.10' } });
    const fPort = field({ label: 'port', name: 'port', path: p('port'), type: 'number', value: saved.port, hint: isDb ? '5432 is PostgreSQL\'s usual port.' : '1883 without TLS, 8883 with.', attrs: { min: '1', max: '65535', step: '1' } });
    const fDb = isDb ? field({ label: 'database', name: 'database', path: p('database'), value: saved.database || '', hint: 'TeslaMate\'s is teslamate.', attrs: inAttrs }) : null;
    const fUser = field({ label: 'user', name: 'username', path: p('username'), value: saved.username || '', hint: isDb ? 'A user that may read TeslaMate\'s tables.' : 'Empty for a broker without accounts.', attrs: inAttrs });
    const fPass = field({ label: 'password', name: 'password', path: p('password'), type: 'password', hint: saved.password_set ? 'A password is saved. Leave this empty to keep it; it is never shown again.' : 'No password saved.', attrs: { autocomplete: 'new-password', placeholder: saved.password_set ? 'saved' : '' } });
    const fRemove = saved.password_set ? field({ label: 'remove the saved password', name: 'password_remove', path: p('password_remove'), type: 'checkbox', value: false }) : null;
    const fPrefix = !isDb ? field({ label: 'prefix', name: 'prefix', path: p('prefix'), value: saved.prefix || 'teslamate', hint: 'TeslaMate\'s MQTT_NAMESPACE, teslamate unless you changed it. The server subscribes to <prefix>/cars/+/#.', attrs: inAttrs }) : null;
    const fIv = isDb ? field({ label: 'every (s)', name: 'interval_s', path: p('interval_s'), type: 'number', value: saved.interval_s, hint: 'Seconds between catch-ups, from 60 to 86400. 300 is every five minutes.', attrs: { min: '60', max: '86400', step: '1' } }) : null;
    const fTls = field({ label: 'encrypt the connection (tls)', name: 'tls', path: p('tls'), type: 'checkbox', value: saved.tls, hint: isDb ? 'When TeslaMate\'s PostgreSQL offers TLS.' : 'When the broker offers TLS (usually on port 8883).' });
    const fPin = field({ label: 'pinned certificate (sha-256)', name: 'pin_sha256', path: p('pin_sha256'), value: saved.pin_sha256 || '', hint: tmPinNote(part), attrs: inAttrs });
    const fPlain = field({ label: lower(UNENCRYPTED_LABEL), name: 'allow_unencrypted', path: p('allow_unencrypted'), type: 'checkbox', value: saved.allow_unencrypted, hint: 'Off by default. Needed only to send a password without TLS.' });
    fPlain.el.append(h('p', { class: 'note tone--warn tm-warn' }, tag('warn', 'warn'), ' ', UNENCRYPTED_WARNING));
    const fields = [fEnabled, fHost, fPort, fDb, fUser, fPass, fRemove, fPrefix, fIv, fTls, fPin, fPlain].filter(Boolean);
    const read = () => Object.fromEntries(fields.map((x) => [x.name, x.type === 'checkbox' ? x.input.checked : x.input.value]));
    const out = h('div', { class: 'tm-test', 'aria-live': 'polite' });
    const testBtn = btn('test', () => test());
    const f = form({
      fields, submit: t.enabled ? 'save' : 'save and add', secondary: testBtn, cls: 'tm-form',
      async onSubmit(v) {
        const d = tmDraft(part, v);
        const bad = tmCheck(part, d, t.enabled ? saved : null);
        if (Object.keys(bad).length) { showFieldErr(f, { fields: bad }); return; }
        const r = await ask({
          title: t.enabled ? `save the ${isDb ? 'database' : 'mqtt'} settings` : 'add the local teslamate', submit: t.enabled ? 'save' : 'add',
          intro: note(t.enabled ? 'The server reconnects with these settings at once.' : 'The server subscribes to TeslaMate\'s topics. Nothing is read from a car until you match it below.'),
          run: async (cred) => ctx.api.put(TM_BASE, withPw(cred, tmPutBody(t, part, d))),
        }, f);
        if (r) await reload(t.enabled ? `${isDb ? 'Database' : 'MQTT'} settings saved.` : 'Local TeslaMate added. Match its cars to start reading them.');
      },
    });
    async function test() {
      f.setMessage(null);
      for (const x of fields) x.setError(null);
      const d = tmDraft(part, read());
      if (isDb) d.enabled = true;
      const bad = tmCheck(part, d, t.enabled ? saved : null);
      if (Object.keys(bad).length) { showFieldErr(f, { fields: bad }); return; }
      const r = await ask({
        title: `test the ${isDb ? 'database' : 'mqtt'} connection`, submit: 'test',
        intro: note(isDb ? 'Connects to the database with these settings and looks for TeslaMate\'s tables. Nothing is saved.' : 'Connects to the broker with these settings and listens a few seconds for TeslaMate\'s cars. It subscribes only and publishes nothing; nothing is saved.'),
        run: async (cred) => ctx.api.post(`${TM_BASE}/test`, withPw(cred, { target: part, [part]: d })),
      }, f);
      if (!r) return;
      replace(out, h('p', { class: ['note', !r.ok && 'tone--warn'] }, tag(r.ok ? 'ok' : 'failed', r.ok ? 'ok' : 'warn'), ' ',
        r.ok ? r.message : `${tmErrorText({ code: r.error, message: r.message }) || 'It did not work'}.`,
        r.ok && r.latency_ms != null ? ` ${formatNumber(r.latency_ms)} ms.` : '',
        r.ok && r.cars_seen?.length ? [' teslamate cars: ', r.cars_seen.map((x, i) => [i ? ', ' : '', code(x)]), '.'] : '',
        h('span', { class: 'muted' }, ` ${dateTime(r.at_ms, ctx.tz)}`)));
    }
    const sync = () => {
      const tls = fTls.input.checked;
      const on = !isDb || fEnabled.input.checked;
      for (const x of fields) if (x !== fEnabled) x.el.hidden = !on;
      fPin.el.hidden = !on || !tls;
      fPlain.el.hidden = !on || tls;
      testBtn.hidden = !on;
    };
    // TLS on moves the port from 1883 to 8883, off moves it back; a port typed by hand stays
    fTls.input.addEventListener('change', () => { const to = tmTlsPort(part, fPort.input.value, fTls.input.checked); if (to != null) fPort.input.value = String(to); });
    fTls.input.addEventListener('change', sync);
    fEnabled?.input.addEventListener('change', sync);
    sync();
    const needsMqtt = isDb && !t.enabled;
    if (needsMqtt) f.submitButton.disabled = true;
    return h('div', { id: isDb ? 'teslamate-database' : 'teslamate-mqtt' },
      h('div', { class: 'tesla-cars-head' }, h('p', { class: 'sub-title' }, isDb ? 'database catch-up (optional)' : 'mqtt'), saved.password_set ? tag('password saved', 'muted') : null),
      f.el, needsMqtt ? note('Add the MQTT connection first; the catch-up is saved with it afterwards.') : null, out);
  }

  function carsBlock() {
    const items = t.cars || [];
    const opts = [['', 'not decided (not read)'], ...cars.map((c) => [c.id, c.name || c.id]), ['new', 'a new car'], ['leave_out', 'leave out (not read)']];
    const picked = new Map(items.map((c) => [c.teslamate_car, tmPreselect(c)]));
    const save = btn('save matches', () => saveMatches(), { kind: 'primary' });
    const sync = () => { save.hidden = !tmMatchChanges(items, picked).length; };
    const select = (c) => {
      const pre = picked.get(c.teslamate_car);
      const known = opts.some(([v]) => v === pre) ? opts : [...opts, [pre, carName(pre)]];
      const sel = h('select', { class: 'in in--cell tesla-sel tf-sel', 'aria-label': `Sentry Mate car for the TeslaMate car ${c.display_name || c.teslamate_car}` }, known.map(([v, l]) => h('option', { value: v, selected: v === pre }, l)));
      sel.addEventListener('change', () => { picked.set(c.teslamate_car, sel.value); sync(); });
      return sel;
    };
    async function saveMatches() {
      const ch = tmMatchChanges(items, picked);
      if (!ch.length) return;
      const r = await ask({
        title: 'match teslamate\'s cars', submit: 'save matches',
        intro: h('ul', { class: 'ln-list' }, ch.map((c) => { const off = c.match === 'leave_out' || !c.match; return h('li', null, tag(off ? 'off' : 'read', off ? 'muted' : 'ok'), h('span', { class: 'ln-list-t' }, h('b', null, items.find((x) => x.teslamate_car === c.teslamate_car)?.display_name || `TeslaMate car ${c.teslamate_car}`), ': ', tmMatchWords(c.match, carName))); })),
        run: async (cred) => ctx.api.put(`${TM_BASE}/cars`, withPw(cred, { version: t.version, items: ch })),
      });
      if (r) await reload('Matches saved.');
    }
    const tb = table(ctx, {
      bare: true, rows: items, csv: false, label: 'TeslaMate\'s cars',
      columns: [
        { key: 'car', label: 'car in teslamate', ph: 'main', cls: 'tf-car-col', render: (c) => [h('b', null, c.display_name || 'no name in TeslaMate'), h('span', { class: 'muted' }, ` car ${c.teslamate_car} ${c.vin_prefix || ''}`)] },
        { key: 'match', label: 'sentry mate car', ph: 'sub', grow: true, render: (c) => [select(c), c.match == null && c.suggested ? [' ', tag('suggested: same vin prefix', 'accent')] : null] },
        { key: 'last', label: 'last message', num: true, ph: 'num', render: (c) => (c.last_message_ms ? h('span', { title: dateTime(c.last_message_ms, ctx.tz) }, ago(c.last_message_ms)) : h('span', { class: 'muted' }, DASH)) },
      ],
      empty: () => h('div', { class: 'empty' }, h('p', { class: 'empty-line' }, '-- no car yet --'), h('p', { class: 'empty-hint' }, 'No car has been seen on TeslaMate\'s topics yet.')),
    });
    tables.push(tb);
    sync();
    return h('div', null, h('div', { class: 'tesla-cars-head' }, h('p', { class: 'sub-title' }, `its cars (${items.length})`), save), tb.el,
      note('A TeslaMate car is read only once it is matched to a car here or to a new car. A suggestion has the same VIN prefix as a car here; it is not saved until you press save matches.'));
  }

  return { el, side, load };
}
