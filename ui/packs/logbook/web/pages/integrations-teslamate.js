// Sentry Mate, "Logbook" UI pack: Settings: Integrations, the Local TeslaMate part (#teslamate, admins only).
//
// The owner's own TeslaMate on the local network as a live input, labelled "Data from local TeslaMate". The server
// subscribes to TeslaMate's MQTT topics (it only reads, never publishes) and, when an admin turns it on, catches up
// from TeslaMate's PostgreSQL database now and then. Off until an admin adds it. This part shows its status, the MQTT
// connection and the optional database catch-up (each with Test and Save), which car here each TeslaMate car is,
// pause and resume, and removing it. Every change, and every test, asks for the admin's own password again (and the
// two-step code), as the TeslaFi part does. The rules and words every pack shares are in shared/teslamate.js.
//
// API: GET / PUT / PATCH / DELETE integrations/teslamate, POST integrations/teslamate/test, PUT
// integrations/teslamate/cars, GET cars (the match picker).

import { h, replace } from '../shared/dom.js';
import * as api from '../shared/api.js';
import { formatDateTime } from '../shared/time.js';
import { formatNumber } from '../shared/i18n.js';
import { TM_BASE, TM_DEFAULTS, MQTT_STATE, DB_STATE, UNENCRYPTED_LABEL, UNENCRYPTED_WARNING, tmErrorText, tmWords, tmDraft, tmCheck, tmPutBody, tmMatchChanges, tmPreselect, tmHeadline } from '../shared/teslamate.js';
import { section, figure, chip, button, icon, ago, errorView, empty } from '../core/ui.js';
import { field, form, setFieldError } from '../core/form.js';
import { dataTable } from '../core/table.js';
import { askAgain } from './integrations-tesla.js';

const ERROR_WORDS = {
  password_needed: 'Type your own Sentry Mate password again.',
  wrong_password: 'That is not your Sentry Mate password.',
  wrong_code: 'Not the right two-step code. Type the one your authenticator app shows now.',
  wrong_two_step: 'Not the right two-step code. Type the one your authenticator app shows now.',
  invalid: 'A value was not taken; see the field.',
  bad_input: 'A value was not taken; see the field.',
  not_allowed: 'Only an admin may change the local TeslaMate input.',
};
const errorText = (e) => { tmWords(e); return (e && ERROR_WORDS[e.code]) || (e && e.message) || 'Something went wrong.'; };
const code = (t) => h('code', { class: 'si-code' }, t);
const quiet = (t) => h('span', { class: 'si-quiet' }, t);
const KIND = { live: 'on', warm: 'warn', plain: null };

/** A tick with its words, as a form field (its value is read by name, true or false). */
function tick(label, name, checked, hint = null) {
  const input = h('input', { type: 'checkbox', name, checked: !!checked });
  const el = h('div', { class: 'field tm-tick', dataset: { field: name } },
    h('label', { class: 'check' }, input, h('span', null, label)),
    hint ? h('p', { class: 'field-hint' }, hint) : null,
    h('p', { class: 'field-error', hidden: true }));
  el.control = input;
  return el;
}

/** The Local TeslaMate part: { el, load() }. A watch account is told the part is for admins. */
export function teslamatePart(ctx) {
  const admin = ctx.me && ctx.me.role === 'admin';
  const tz = ctx.tz;
  const slot = h('div', { class: 'tesla teslamate' });
  const el = section({ title: 'Local TeslaMate', note: 'your own TeslaMate as a live input, admins only', id: 'teslamate', cls: 'si-sec tesla-sec' }, slot);
  let t = null, cars = [], loadErr = null;
  const carName = (cid) => (cars.find((c) => c.id === cid) || {}).name || cid;
  const ask = (opts) => askAgain(ctx, { input: 'the local TeslaMate input', errorText, ...opts });

  async function load() {
    if (!admin) { draw(); return; }
    try {
      const [o, c] = await Promise.all([
        ctx.api.get(TM_BASE),
        ctx.api.get('cars', { sort: 'name', limit: 500 }, { maxAgeMs: 60000 }).catch((e) => { if (api.isAbort(e)) throw e; return { items: [] }; }),
      ]);
      t = o; cars = c.items || []; loadErr = null;
    } catch (e) {
      if (api.isAbort(e)) throw e;
      loadErr = e;
    }
    if (ctx.alive()) draw();
  }
  const again = async () => { await load(); };
  // a stale version or a removed input: say so and show the settings as they are now
  const settle = async (out, msg) => { if (out) ctx.flash(msg); await again(); };

  function draw() {
    if (!admin) {
      replace(slot, h('p', { class: 'si-lead' }, 'Sentry Mate can also read your cars from your own TeslaMate on the local network (its MQTT topics, and optionally its database), as a live input beside SMO, the Tesla API and TeslaFi (“Data from local TeslaMate”). Only admins see and change it.'));
      return;
    }
    if (loadErr) { replace(slot, errorView(loadErr, { title: 'The local TeslaMate settings could not be loaded.', retry: () => again(), home: false, eyebrow: 'Local TeslaMate' })); return; }
    const [word, tone] = tmHeadline(t);
    replace(slot,
      h('p', { class: 'si-lead' },
        'If you already run TeslaMate at home, Sentry Mate can listen to the MQTT messages TeslaMate publishes and, if you like, catch up from its database for what MQTT missed. It only reads: it never publishes, never wakes the car and never sends a command. Where SMO, the Tesla API or TeslaFi cover the same time, their values win. ',
        t.enabled ? null : h('strong', null, 'Off until it is added: until then the server makes no connection to TeslaMate at all.')),
      overview(word, tone),
      h('div', { class: 'si-two tm-two' }, connPart('mqtt'), connPart('database')),
      t.enabled ? carsPart() : null);
  }

  function overview(word, tone) {
    const sm = t.status?.mqtt || {};
    const sd = t.status?.database || {};
    const ms = MQTT_STATE[sm.state] || [sm.state || 'Off', 'plain', ''];
    const ds = DB_STATE[sd.state] || [sd.state || 'Off', 'plain', ''];
    const pauseBtn = button(t.paused ? 'Resume' : 'Pause', {
      onclick: async () => {
        const out = await ask({
          title: t.paused ? 'Resume the local TeslaMate?' : 'Pause the local TeslaMate?', yes: t.paused ? 'Resume' : 'Pause',
          text: t.paused ? 'MQTT is read again from now on, and the database catch-up (when on) starts again.' : 'Both connections are closed and nothing is read until you resume it. The history already stored stays.',
          run: (creds) => ctx.api.patch(TM_BASE, { version: t.version, paused: !t.paused, ...creds }),
        });
        await settle(out, t.paused ? 'The local TeslaMate is read again.' : 'The local TeslaMate is paused.');
      },
    });
    const removeBtn = button('Remove', {
      kind: 'danger', onclick: async () => {
        const out = await ask({
          title: 'Remove the local TeslaMate?', yes: 'Remove it', danger: true,
          text: 'Reading stops at once and its sealed settings, passwords included, are deleted. The history already stored stays, marked as data from local TeslaMate.',
          run: (creds) => ctx.api.del(TM_BASE, { headers: { 'X-Sentry-Mate-Password': creds.confirm_password, ...(creds.code ? { 'X-Sentry-Mate-Code': creds.code } : {}) } }),
        });
        await settle(out, 'The local TeslaMate was removed. Its history stays.');
      },
    });
    const errs = [['MQTT', sm.last_error], ['Database', sd.last_error]].filter(([, e]) => e);
    return h('div', { class: 'tesla-over' },
      h('div', { class: 'si-sub-head' }, h('div', null, chip(word, { kind: KIND[tone] })), t.enabled ? h('div', { class: 'tesla-actions' }, pauseBtn, removeBtn) : null),
      h('div', { class: 'figs tesla-figs' },
        figure('MQTT', ms[0], sm.connected_since_ms ? `connected since ${formatDateTime(sm.connected_since_ms, tz, { now: api.now() })}` : ms[2]),
        figure('Last message', sm.last_message_ms ? ago(sm.last_message_ms) : '—', `${formatNumber(sm.messages_today || 0)} messages today`),
        figure('Database catch-up', ds[0], sd.last_sync_ms ? `last ${ago(sd.last_sync_ms)}${sd.next_sync_ms ? `, next ${formatDateTime(sd.next_sync_ms, tz, { now: api.now() })}` : ''}` : ds[2]),
        figure('Caught up', sd.last_sync_ms ? `${formatNumber(sd.drives)} drives` : '—', sd.last_sync_ms ? `${formatNumber(sd.charges)} charges, ${formatNumber(sd.positions)} positions` : 'nothing yet')),
      errs.map(([n, e]) => h('p', { class: 'tesla-alert', role: 'note' }, icon('warning'), `${n}: ${tmErrorText(e)}${e.at_ms ? `, ${formatDateTime(e.at_ms, tz, { now: api.now() })}` : ''}.`)),
      h('div', { class: 'tm-hosts' }, h('span', { class: 'si-quiet' }, 'Hosts it talks to: '),
        t.hosts && t.hosts.length ? h('span', { class: 'tesla-hostlist' }, t.hosts.map((x) => code(x))) : quiet('none while it is not added')));
  }

  /* ---------------- the MQTT connection and the database catch-up */
  function connPart(part) {
    const saved = t[part] || TM_DEFAULTS[part];
    const isDb = part === 'database';
    const n = (x) => `${part}.${x}`;
    const fEnabled = isDb ? tick('Catch up from TeslaMate’s database', n('enabled'), saved.enabled, 'Off by default. Reads TeslaMate’s drives, charges and positions now and then, for what MQTT missed. A read-only database user is enough.') : null;
    const fHost = field({ label: 'Host', name: n('host'), value: saved.host || '', hint: isDb ? 'Where TeslaMate’s PostgreSQL listens.' : 'The broker TeslaMate publishes to: a host name or IP address, without mqtt://.', attrs: { spellcheck: 'false', autocomplete: 'off', placeholder: '192.0.2.10' } });
    const fPort = field({ label: 'Port', name: n('port'), type: 'number', value: saved.port, hint: isDb ? '5432 is PostgreSQL’s usual port.' : '1883 without TLS, 8883 with.', attrs: { min: 1, max: 65535, step: 1 } });
    const fDb = isDb ? field({ label: 'Database', name: n('database'), value: saved.database || '', hint: 'TeslaMate’s is teslamate.', attrs: { spellcheck: 'false', autocomplete: 'off' } }) : null;
    const fUser = field({ label: 'User name', name: n('username'), value: saved.username || '', hint: isDb ? 'A user that may read TeslaMate’s tables.' : 'Empty for a broker without accounts.', attrs: { spellcheck: 'false', autocomplete: 'off' } });
    const fPass = field({ label: 'Password', name: n('password'), type: 'password', hint: saved.password_set ? 'A password is saved. Leave this empty to keep it; it is never shown again.' : 'No password saved.', attrs: { autocomplete: 'new-password', placeholder: saved.password_set ? 'saved' : '' } });
    const fRemove = saved.password_set ? tick('Remove the saved password', n('password_remove'), false) : null;
    const fPrefix = !isDb ? field({ label: 'Topic prefix', name: n('prefix'), value: saved.prefix || 'teslamate', hint: 'TeslaMate’s MQTT_NAMESPACE, teslamate unless you changed it. The server subscribes to <prefix>/cars/+/#.', attrs: { spellcheck: 'false', autocomplete: 'off' } }) : null;
    const fIv = isDb ? field({ label: 'Catch up every (seconds)', name: n('interval_s'), type: 'number', value: saved.interval_s, hint: 'From 60 to 86400. 300 is every five minutes.', attrs: { min: 60, max: 86400, step: 1 } }) : null;
    const fTls = tick('Encrypt the connection (TLS)', n('tls'), saved.tls, isDb ? 'When TeslaMate’s PostgreSQL offers TLS.' : 'When the broker offers TLS (usually on port 8883).');
    const fPin = field({ label: 'Pinned certificate (SHA-256)', name: n('pin_sha256'), value: saved.pin_sha256 || '', hint: 'For a host with its own (self-signed) certificate: 64 hex digits, colons allowed. Empty: the certificate must be valid for the host.', attrs: { spellcheck: 'false', autocomplete: 'off' } });
    const fPlain = tick(UNENCRYPTED_LABEL, n('allow_unencrypted'), saved.allow_unencrypted, 'Off by default. Needed only to send a password without TLS.');
    const warn = h('p', { class: 'tesla-alert tm-warn', role: 'note' }, icon('warning'), UNENCRYPTED_WARNING);
    fPlain.append(warn);
    const fields = [fEnabled, fHost, fPort, fDb, fUser, fPass, fRemove, fPrefix, fIv, fTls, fPin, fPlain].filter(Boolean);
    const values = (raw) => Object.fromEntries(Object.entries(raw).filter(([k]) => k.startsWith(`${part}.`)).map(([k, v]) => [k.slice(part.length + 1), v]));
    const read = (formEl) => {
      const out = {};
      for (const c of formEl.elements) if (c.name) out[c.name] = c.type === 'checkbox' ? c.checked : c.type === 'number' ? (c.value === '' ? null : Number(c.value)) : c.value;
      return values(out);
    };
    const testOut = h('div', { class: 'tm-test', 'aria-live': 'polite' });
    const f = form({
      fields, submitLabel: t.enabled ? 'Save' : 'Save and add', busyLabel: 'Saving…', cls: 'tm-form',
      onsubmit: async (raw) => {
        const d = tmDraft(part, values(raw));
        const bad = tmCheck(part, d, t.enabled ? saved : null);
        if (Object.keys(bad).length) throw Object.assign(new Error('A value was not taken; see the field.'), { fields: bad });
        const out = await ask({
          title: t.enabled ? `Save the ${isDb ? 'database' : 'MQTT'} settings?` : 'Add the local TeslaMate?', yes: t.enabled ? 'Save' : 'Add it',
          text: t.enabled ? 'The server reconnects with these settings at once.' : 'The server subscribes to TeslaMate’s topics. Nothing is read from a car until you choose which car it is below.',
          run: (creds) => ctx.api.put(TM_BASE, { ...tmPutBody(t, part, d), ...creds }),
        });
        await settle(out, t.enabled ? `The ${isDb ? 'database' : 'MQTT'} settings were saved.` : 'The local TeslaMate was added. Choose which car each TeslaMate car is.');
      },
    });
    const testBtn = button('Test', {
      onclick: async () => {
        f.clearErrors();
        const d = tmDraft(part, read(f));
        if (isDb) d.enabled = true;
        const bad = tmCheck(part, d, t.enabled ? saved : null);
        if (Object.keys(bad).length) { for (const [k, v] of Object.entries(bad)) { const x = fields.find((y) => y.dataset.field === k); if (x) setFieldError(x, v); } return; }
        const r = await ask({
          title: `Test the ${isDb ? 'database' : 'MQTT'} connection`, yes: 'Test',
          text: isDb ? 'Connects to the database with these settings and looks for TeslaMate’s tables. Nothing is saved.' : 'Connects to the broker with these settings and listens a few seconds for TeslaMate’s cars. It subscribes only and publishes nothing; nothing is saved.',
          run: (creds) => ctx.api.post(`${TM_BASE}/test`, { target: part, [part]: d, ...creds }),
        });
        if (!r) return;
        replace(testOut, h('p', { class: ['tm-test-line', r.ok ? 'is-ok' : 'is-warn'] }, icon(r.ok ? 'check' : 'warning'),
          r.ok ? r.message : `${tmErrorText({ code: r.error, message: r.message }) || 'It did not work'}.`,
          r.ok && r.latency_ms != null ? ` ${formatNumber(r.latency_ms)} ms.` : '',
          r.ok && r.cars_seen && r.cars_seen.length ? [' TeslaMate cars: ', r.cars_seen.map((x, i) => [i ? ', ' : '', code(x)]), '.'] : '',
          quiet(` ${formatDateTime(r.at_ms, tz, { now: api.now() })}`)));
      },
    });
    f.querySelector('.form-actions').append(testBtn);
    const sync = () => {
      const tls = fTls.control.checked;
      const on = !isDb || fEnabled.control.checked;
      for (const x of fields) if (x !== fEnabled) x.hidden = !on;
      fPin.hidden = !on || !tls;
      fPlain.hidden = !on || tls;
      testBtn.hidden = !on;
    };
    fTls.control.addEventListener('change', sync);
    if (fEnabled) fEnabled.control.addEventListener('change', sync);
    sync();
    const needsMqtt = isDb && !t.enabled;
    if (needsMqtt) f.querySelector('button[type=submit]').disabled = true;
    return h('div', { class: 'tm-conn', id: isDb ? 'teslamate-database' : 'teslamate-mqtt' },
      h('h3', { class: 'si-sub' }, isDb ? 'Database catch-up' : 'MQTT', h('span', { class: 'label-note' }, isDb ? 'optional, off by default' : 'TeslaMate’s broker'), saved.password_set ? chip('Password saved') : null),
      f, needsMqtt ? h('p', { class: 'si-quiet' }, 'Add the MQTT connection first; the catch-up is saved with it afterwards.') : null, testOut);
  }

  /* ---------------- which car here each TeslaMate car is */
  function carsPart() {
    const list = t.cars || [];
    const chosen = new Map(list.map((c) => [c.teslamate_car, tmPreselect(c)]));
    const nameOf = (c) => c.display_name || `TeslaMate car ${c.teslamate_car}`;
    const save = button('Save matches', {
      kind: 'primary', onclick: async () => {
        const changes = tmMatchChanges(list, chosen);
        if (!changes.length) { ctx.flash('Nothing changed in the matches.', { kind: 'info' }); return; }
        const out = await ask({
          title: 'Save the matches?', yes: 'Save',
          text: 'A TeslaMate car matched to a car here, or to a new car, is read from now on; one left out or not decided is not read.',
          run: (creds) => ctx.api.put(`${TM_BASE}/cars`, { version: t.version, items: changes, ...creds }),
        });
        await settle(out, 'The matches were saved.');
      },
    });
    const sync = () => { save.hidden = !tmMatchChanges(list, chosen).length; };
    const labelled = (label, v) => h('span', { class: 'tesla-cell' }, h('span', { class: 'tesla-cell-label', 'aria-hidden': 'true' }, label), v);
    const picker = (c) => {
      const sel = h('select', { class: 'field-select tesla-match', 'aria-label': `Which car here is ${nameOf(c)}` },
        h('option', { value: '' }, 'Not decided: not read'),
        h('optgroup', { label: 'A car here' }, cars.map((x) => h('option', { value: x.id }, `${x.name}${c.suggested === x.id ? ' (suggested: same VIN prefix)' : ''}`))),
        h('option', { value: 'new' }, 'A new car'),
        h('option', { value: 'leave_out' }, 'Leave it out'));
      sel.value = chosen.get(c.teslamate_car) || '';
      sel.addEventListener('change', () => { chosen.set(c.teslamate_car, sel.value); sync(); });
      return labelled('Is the car', sel);
    };
    const tb = dataTable(ctx, {
      rows: list, itemName: ['car', 'cars'], label: 'TeslaMate’s cars', csv: false, cls: 'tesla-cars-table',
      caption: 'The cars on TeslaMate’s topics; choose which car here each one is',
      columns: [
        { key: 'name', label: 'TeslaMate’s name', th: true, cls: 'tesla-c-name', cell: (c) => h('span', null, h('span', { class: 'cell-main' }, nameOf(c)), h('span', { class: 'cell-sub' }, `car ${c.teslamate_car} · `, h('span', { class: 'mono' }, c.vin_prefix || '—'))) },
        { key: 'match', label: 'Is the car', cls: 'tesla-c-match', cell: picker },
        {
          key: 'read', label: 'Read here', cls: 'si-wrap tesla-c-mode',
          cell: (c) => labelled('Read here', h('span', null,
            h('span', { class: 'cell-main' }, c.match && c.match !== 'leave_out' ? (c.match === 'new' ? 'Into a new car' : `Into ${carName(c.match)}`) : 'Not read'),
            h('span', { class: 'cell-sub' }, c.match == null && c.suggested ? `Suggested: ${carName(c.suggested)}, the same VIN prefix. Save matches to read it.`
              : c.match == null ? 'Not read until you choose which car it is.' : c.match === 'leave_out' ? 'Left out on purpose.' : 'Its values are labelled “Data from local TeslaMate”.'))),
        },
        { key: 'last', label: 'Last message', cls: 'tesla-c-state', cell: (c) => labelled('Last message', c.last_message_ms ? ago(c.last_message_ms) : quiet('none yet')) },
      ],
      empty: () => empty({ title: 'TeslaMate published no car yet.', text: 'A car shows here once it is seen on TeslaMate’s topics.' }),
    });
    tb.load?.();
    sync();
    return h('div', { class: 'tesla-cars' }, h('div', { class: 'si-sub-head' }, h('h4', { class: 'tesla-cars-title' }, 'Its cars'), list.length ? save : null), tb.el);
  }

  return { el, load };
}
