// Server and access (/settings/server): how the server is reached, who can sign in and what they see, and
// moving it. Everyone gets the check of this address against the server's names and port, how access works,
// and their own sessions (end one, every other one, or this one). Admins also get the host names for the
// certificate and the ports, the certificate, recovery key and backups, moving to another server (a move code
// or an export file: pages/server-move.js), the accounts (role, cars, two-step sign-in) and the audit log
// (each entry links to what it changed). Changes go through the API with the object's version; the REC lamp
// and a toast confirm each one.

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { format } from '../shared/units.js';
import { timeColumn } from '../shared/csv.js';
import { icon, hud, plate, btn, tag, kv, note } from '../core/ui.js';
import { counters, band } from '../core/kit.js';
import { tracks } from '../core/tracks.js';
import { field, form } from '../core/form.js';
import { emptyState } from '../core/states.js';
import { openDialog, confirmDialog, toast } from '../core/layer.js';
import { signOut } from '../core/session.js';
import { dateTime, date, agoEl, zone, DASH } from '../core/fmt.js';
import { chan, recLamp, presets, errText, code, sig, checkLine } from '../core/setting.js';
import { movePanel, showKey, newKey, keySaved } from './server-move.js';

export const id = 'settings-server';
export const css = true;

const enc = encodeURIComponent;
const ROLE = { admin: 'Admin', watch: 'Watch only' };
const ROLE_TEXT = {
  admin: 'sees every car and every setting, and can change everything: accounts, cars, places, integrations, imports.',
  watch: 'sees only the cars on its list and changes only its own preferences and sessions.',
};
const PURPOSE = { web: 'Web pages and API', upload: 'CAN Bridge uploads' };
const HOST_RE = /^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)*$/;

export async function mount(ctx) {
  const admin = ctx.admin;
  const me = ctx.me || {};
  const query = ctx.query || {};
  const S = { settings: null, network: null, notices: [], accounts: [], cars: [], sessions: [], trips: [] };
  const loadAccounts = async () => { S.accounts = admin ? await ctx.api.list('accounts', { limit: 500 }).all({ max: 5000 }) : []; };
  const loadSessions = async () => { S.sessions = await ctx.api.list('me/sessions', { limit: 500 }).all({ max: 5000 }); };
  try {
    const [settings, network, notices, cars] = await Promise.all([
      ctx.api.get('settings'),
      admin ? ctx.api.get('network').catch(() => null) : null,
      ctx.api.get('notices').catch(() => null),
      ctx.api.list('cars', { sort: 'custom', limit: 500 }).all({ max: 5000 }).catch(() => []),
      loadAccounts(), loadSessions(),
      admin ? ctx.api.list('trips', { limit: 500 }).all({ max: 5000 }).then((t) => { S.trips = t; }).catch(() => {}) : null,
    ]);
    Object.assign(S, { settings, network, notices: notices?.items || [], cars });
  } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;
  const rec = recLamp();

  const carName = new Map(S.cars.map((c) => [c.id, c.name]));
  // times on this page: the clock's zone (the last car's, else the first car's), else this browser's
  const clockCar = S.cars.find((c) => c.id === state.lastCar()) || S.cars[0] || null;
  const tz = clockCar?.time_zone || ctx.tz;
  const zn = zone(tz);
  const zoneText = clockCar?.time_zone ? `${zn}, ${clockCar.name}'s time zone like the clock at the top` : `${zn}, this browser's time zone`;
  const isoNote = `ISO 8601 with its UTC offset, ${tz}`;

  /* ---------------------------------------------------------------- reaching this server */
  const here = { https: location.protocol === 'https:', host: location.hostname.toLowerCase(), port: Number(location.port || (location.protocol === 'https:' ? 443 : 80)) };
  const webIn = S.network?.inbound?.find((x) => x.purpose === 'web') || null;
  const upIn = S.network?.inbound?.find((x) => x.purpose === 'upload') || null;
  const local = ['localhost', '127.0.0.1', '::1', '[::1]'].includes(here.host);
  const certNames = Array.isArray(S.network?.certificate?.names) ? S.network.certificate.names.map((n) => n.toLowerCase()) : null;
  const reachSlot = h('ul', { class: 'chks' });
  function paintReach() {
    const saved = (S.settings.host_names || []).map((n) => n.toLowerCase());
    const names = certNames || saved;
    const named = names.includes(here.host);
    const savedOnly = !named && saved.includes(here.host);
    replace(reachSlot,
      here.https ? checkLine('ok', 'HTTPS', 'This page came over an encrypted connection, so the password you sign in with never travels in plain text.')
        : checkLine('warn', 'Plain HTTP', `This page came over plain HTTP. Sentry Mate itself answers only HTTPS (TLS 1.3), so something else serves it${local ? ' (on this machine: a development server)' : ', such as a proxy'}. Never type a password on a plain HTTP page that goes over a network.`),
      named ? checkLine('ok', `Host name ${here.host}`, certNames ? 'This name is in the certificate the server uses now, so the browser takes the address without a warning (once it trusts the certificate).' : 'This name is on the server\'s list for its certificate. An admin sees whether the certificate in use already has it.')
        : savedOnly ? checkLine('info', `${here.host} is saved, not yet in the certificate`, `The certificate in use names ${names.join(', ') || 'no host'}. Browsers keep warning about ${here.host} until the server makes a new certificate.`)
          : checkLine('warn', `${here.host} is not on the certificate`, `You opened Sentry Mate as ${here.host}, but the certificate names ${names.join(', ') || 'no host'}. Browsers warn about a name that is not on it. The CAN Bridge does not care: it pins the certificate's SHA-256.`,
            admin ? btn(`Add ${here.host} to the names`, () => addName(here.host), { icon: 'plus', sm: true }) : null),
      webIn ? (here.port === webIn.port ? checkLine('ok', `Port ${here.port}`, `The same port the server listens on for the web pages (${webIn.port}).`)
        : checkLine('info', `Port ${here.port}, server port ${webIn.port}`, `Your address uses port ${here.port}; the server listens on ${webIn.port} inside its container. A port mapping such as "${here.port}:${webIn.port}" in the compose file (or a proxy) joins the two. That only matters when you type the address into another device.`))
        : checkLine('info', `Port ${here.port}`, 'The port in your address. An admin sees the ports the server listens on.'),
      checkLine('ok', 'At the root of its address', `Sentry Mate always lives at / on its own name or port, so links never need a path prefix. Behind a shared proxy, give it its own name (such as ${names[0] || 'sentry-mate.local'}) rather than a path.`),
      checkLine('ok', 'Works from any address', 'There is no list of allowed origins to keep in step: the session cookie is never sent by other sites and every change carries a header other sites cannot set.'));
  }

  /* ---------------------------------------------------------------- host names */
  const namesSlot = h('div', { class: 'stack' });
  let draft = [...(S.settings.host_names || [])];
  const nameIn = h('input', { class: 'input', type: 'text', placeholder: 'sentry-mate.local', 'aria-label': 'Host name to add', autocomplete: 'off', spellcheck: 'false', disabled: !admin,
    onkeydown: (e) => { if (e.key === 'Enter') { e.preventDefault(); addDraft(); } } });
  const nameErr = h('p', { class: 'field-error', hidden: true });
  const sayName = (t) => { nameErr.hidden = !t; if (t) replace(nameErr, icon('warn', 'i--sm'), h('span', null, t)); };
  function paintNames() {
    const changed = JSON.stringify(draft) !== JSON.stringify(S.settings.host_names || []);
    replace(namesSlot,
      h('ul', { class: 'ss-names', 'aria-label': 'Host names for the certificate' }, draft.map((n, i) => h('li', { class: 'ss-name' },
        h('span', { class: 'ss-name-t' }, n), n.toLowerCase() === here.host ? tag('This address', 'cyan') : null,
        admin ? btn(null, () => { draft = draft.filter((_, j) => j !== i); paintNames(); }, { kind: 'ghost', icon: 'x', sm: true, label: `Remove ${n}`, disabled: draft.length <= 1 }) : null))),
      admin ? h('div', { class: 'ss-add' }, nameIn, btn('Add', addDraft, { icon: 'plus' })) : null,
      nameErr,
      admin ? h('div', { class: 'row' }, btn('Save host names', () => putNames(draft), { kind: 'go', sm: true, disabled: !changed || !draft.length }),
        changed ? btn('Undo changes', () => { draft = [...S.settings.host_names]; paintNames(); }, { kind: 'ghost', sm: true }) : null,
        changed ? h('span', { class: 'chan-hint' }, 'Not saved yet.') : null) : h('p', { class: 'chan-hint' }, 'Only an admin changes them.'));
  }
  function addDraft() {
    const v = nameIn.value.trim().toLowerCase();
    sayName(null);
    if (!v) return;
    if (!HOST_RE.test(v) || v.length > 253) { sayName('A host name has letters, digits, hyphens and dots, such as sentry-mate.local, or an address such as 192.168.1.20.'); return; }
    if (draft.length >= 10 && !draft.includes(v)) { sayName('The certificate takes up to 10 names.'); return; }
    if (!draft.includes(v)) draft.push(v);
    nameIn.value = '';
    paintNames();
  }
  const mergeDraft = (base, onto) => {
    const added = draft.filter((n) => !base.includes(n)), removed = base.filter((n) => !draft.includes(n));
    const out = onto.filter((n) => !removed.includes(n));
    for (const n of added) if (!out.includes(n)) out.push(n);
    return out.length ? out.slice(0, 10) : [...onto];
  };
  async function putNames(list, { fromDraft = true } = {}) {
    sayName(null);
    const before = [...(S.settings.host_names || [])];
    try {
      S.settings = await ctx.api.patch('settings', { version: S.settings.version, host_names: list });
      draft = fromDraft ? [...S.settings.host_names] : mergeDraft(before, S.settings.host_names);
      rec.saved('Host names', 'Host names saved. They go into the next certificate the server makes; the one in use keeps its names until then.');
      paintNames(); paintReach();
    } catch (e) {
      if (e.status === 409) {
        S.settings = await ctx.api.get('settings');
        draft = mergeDraft(before, S.settings.host_names);
        paintNames(); paintReach();
        toast('Someone else changed the server settings meanwhile. Your changes are kept on top of their list: check it and save again.', { kind: 'info' });
        return;
      }
      sayName(errText(e));
    }
  }
  function addName(n) {
    const saved = S.settings.host_names || [];
    if (saved.includes(n)) return;
    if (saved.length >= 10) { toast('The certificate takes up to 10 names. Remove one under Host names first.', { kind: 'error' }); return; }
    putNames([...saved, n], { fromDraft: false });
  }

  /* ---------------------------------------------------------------- ports */
  const portRows = S.network?.inbound?.length
    ? S.network.inbound.map((p) => [PURPOSE[p.purpose] || p.purpose, h('span', { class: 'ss-port' }, h('b', { class: 'mono' }, String(p.port)), p.tls ? sig('ok', 'HTTPS, TLS 1.3') : sig('warn', 'no TLS'))])
    : [['Your address', h('span', { class: 'mono' }, String(here.port))]];
  const webPort = webIn?.port ?? null;
  const hostPort = webPort != null && here.port !== webPort ? here.port : (webPort === 443 ? 8443 : 10443);

  /* ---------------------------------------------------------------- certificate, recovery key, backups */
  const cert = S.notices.find((n) => n.code === 'certificate_expiring');
  let keyUnsaved = !!S.notices.find((n) => n.code === 'recovery_key_unsaved');
  const keySlot = h('ul', { class: 'chks' });
  const keyBtn = (label, run, kind) => btn(label, async () => {
    const r = await run(ctx, { tz });
    if (!r || !ctx.alive()) return;
    keyUnsaved = r === 'unsaved';
    rec.light('Recovery key');
    paintKey();
  }, { kind, sm: true });
  function paintKey() {
    replace(keySlot, keyUnsaved
      ? checkLine('warn', 'The recovery key is not marked as saved yet',
        ['Until you say it is saved, the server keeps a copy in its keys folder. ', admin ? 'Show it again (your password first), keep it in a password manager or on paper away from the server, then tick "I have saved it": the server deletes its copy.' : 'An admin can show it again here and mark it as saved.'],
        admin ? [keyBtn('Show the key again', showKey, 'go'), keyBtn('I have saved it already', keySaved), keyBtn('Make a new recovery key', newKey, 'ghost')] : null)
      : checkLine('ok', 'Recovery key', ['Saved away from the server; the server keeps no copy. ', admin ? 'Lost it, or someone saw it? Make a new one: the old one stops working at once, and export files and backups made before still need the old one.' : 'If it is lost, an admin can make a new one here.'],
        admin ? keyBtn('Make a new recovery key', newKey) : null));
  }
  paintKey();

  /* ---------------------------------------------------------------- accounts */
  const nAdmins = S.accounts.filter((a) => a.role === 'admin').length;
  const carsText = (cars) => {
    if (cars === 'all') return 'All cars';
    if (!Array.isArray(cars) || !cars.length) return 'No cars';
    const names = cars.map((c) => carName.get(c) || c);
    return names.length > 3 ? `${names.slice(0, 3).join(', ')} and ${names.length - 3} more` : names.join(', ');
  };
  const focusAccount = query.account || null;
  const accountList = admin ? tracks(ctx, {
    title: 'Accounts', icon: 'user', rows: S.accounts, label: 'Accounts that can sign in', csvName: 'accounts',
    note: 'Everyone who can sign in, with what they see. Edit an account to change its role, its cars, its password or two-step sign-in.',
    side: btn('Add account', () => editAccount(null), { kind: 'go', icon: 'plus', sm: true }),
    sort: { key: 'name', dir: 'asc' },
    columns: [
      { key: 'name', label: 'Name', lead: true, sort: true, render: (a) => h('span', { class: 'flagline' }, h('b', null, a.name), a.id === me.id ? tag('You', 'cyan') : null, a.id === focusAccount ? tag('From the audit log', 'org') : null) },
      { key: 'role', label: 'Role', sort: true, render: (a) => tag(ROLE[a.role] || a.role, a.role === 'admin' ? 'mag' : null), csv: (a) => ROLE[a.role] || a.role },
      { key: 'cars', label: 'Cars it sees', sort: true, value: (a) => (a.cars === 'all' ? -1 : a.cars.length), render: (a) => carsText(a.cars), csv: (a) => (a.cars === 'all' ? 'all' : a.cars.map((c) => carName.get(c) || c).join('; ')) },
      { key: 'two_step_required', label: 'Two-step sign-in', sort: true, render: (a) => (a.two_step_required ? sig('ok', 'Required') : sig('off', 'Password only')), csv: (a) => (a.two_step_required ? 'required' : 'no') },
      { key: 'created_ms', label: `Made (${zn})`, sort: true, render: (a) => date(a.created_ms, tz), csv: timeColumn(`Made (${isoNote})`, 'created_ms', tz) },
      { key: 'act', label: '', cls: 'is-act', csv: false, render: (a) => h('div', { class: 'row' }, btn('Edit', () => editAccount(a), { sm: true }),
        btn('Remove', () => removeAccount(a), { kind: 'ghost', sm: true, disabled: a.id === me.id, title: a.id === me.id ? 'You cannot remove the account you are signed in with' : null })) },
    ],
    empty: () => emptyState({ icon: 'user', title: 'No accounts', text: 'There is always at least one admin; this list is empty only while it loads.' }),
  }) : null;
  async function refreshAccounts() { await loadAccounts(); if (ctx.alive()) accountList.setRows(S.accounts); }

  function carPicker(current) {
    const all = field({ label: 'All cars, also cars that come later', name: 'all_cars', type: 'checkbox', value: current === 'all' });
    const chosen = new Set(current === 'all' ? [] : current || []);
    const find = h('input', { class: 'input', type: 'search', placeholder: 'Find a car', 'aria-label': 'Find a car' });
    const list = h('div', { class: 'ss-carlist', role: 'group', 'aria-label': 'Cars this account sees' });
    const countLine = h('p', { class: 'field-hint' });
    const err = h('p', { class: 'field-error', hidden: true });
    const paint = () => {
      const q = find.value.trim().toLowerCase();
      const shown = S.cars.filter((c) => !q || c.name.toLowerCase().includes(q) || (c.model || '').toLowerCase().includes(q));
      replace(list, shown.length ? shown.map((c) => h('label', { class: 'check' },
        h('input', { type: 'checkbox', checked: chosen.has(c.id), onchange: (e) => { if (e.target.checked) chosen.add(c.id); else chosen.delete(c.id); paintCount(); } }),
        h('span', null, c.name, c.model ? h('span', { class: 'faint' }, ` · ${c.model}`) : null))) : h('p', { class: 'faint small' }, S.cars.length ? 'No car matches.' : 'No cars yet.'));
    };
    const paintCount = () => { countLine.textContent = all.read() ? 'Sees every car, now and later.' : `${format(chosen.size, 'count')} of ${format(S.cars.length, 'count')} cars chosen.`; };
    const box = h('div', { class: 'stack ss-carpick' }, find, list);
    const toggleBox = () => { box.hidden = all.read(); paintCount(); };
    all.input.addEventListener('change', toggleBox);
    find.addEventListener('input', paint);
    paint(); toggleBox();
    return {
      el: h('div', { class: 'field' }, h('span', { class: 'field-label' }, 'Cars it sees'), all.el, box, countLine, err),
      name: 'cars', path: 'cars',
      read: () => (all.read() ? 'all' : [...chosen]),
      setError: (t) => { err.hidden = !t; if (t) replace(err, icon('warn', 'i--sm'), h('span', null, t)); else err.replaceChildren(); },
    };
  }

  function editAccount(a) {
    const isNew = !a;
    const name = field({ label: 'Name', name: 'name', value: a?.name || '', required: true, hint: 'What they type to sign in. 1 to 64 characters.', attrs: { maxlength: '64', autocomplete: 'off' } });
    const password = field({ label: isNew ? 'Password' : 'New password', name: 'password', type: 'password', required: isNew,
      hint: isNew ? 'At least 10 characters. Tell it to them in person or over a channel you trust.' : 'Empty keeps the current password. At least 10 characters.', attrs: { minlength: '10', autocomplete: 'new-password' } });
    const role = field({ label: 'Role', name: 'role', type: 'select', value: a?.role || 'watch', options: [['watch', 'Watch only'], ['admin', 'Admin']],
      hint: `Watch only ${ROLE_TEXT.watch} Admin ${ROLE_TEXT.admin}` });
    const two = field({ label: 'Require two-step sign-in', name: 'two_step_required', type: 'checkbox', value: !!a?.two_step_required, hint: 'After the password, a six-digit code from an authenticator app. Wise for every admin.' });
    const cars = carPicker(a ? a.cars : []);
    let d = null;
    const f = form({
      fields: [name, password, role, cars, two], submit: isNew ? 'Add account' : 'Save changes', secondary: btn('Cancel', () => d?.close(), { kind: 'ghost' }),
      async onSubmit(v) {
        const body = { name: v.name.trim(), role: v.role, cars: v.cars, two_step_required: v.two_step_required };
        if (v.password) body.password = v.password;
        try {
          if (isNew) await ctx.api.post('accounts', body); else await ctx.api.patch(`accounts/${enc(a.id)}`, { version: a.version, ...body });
          rec.saved(isNew ? 'Account' : body.name, isNew ? `${body.name} can sign in now, as ${ROLE[body.role].toLowerCase()}.` : `${body.name} saved.`);
        } catch (e) {
          if (e.status === 409 && e.code === 'stale_version') { await refreshAccounts(); f.setMessage('Someone else changed this account meanwhile. Close this, look at their change, and try again.'); return; }
          if (e.code === 'name_taken') { name.setError('That name is taken.'); return; }
          if (e.code === 'last_admin') { role.setError('This is the last admin. Make another account an admin first.'); return; }
          throw e;
        }
        d?.close();
        await refreshAccounts();
      },
    });
    d = openDialog({ title: isNew ? 'Add an account' : `Edit ${a.name}`, body: f.el, cls: 'dlg--wide' });
    name.input.focus();
  }
  async function removeAccount(a) {
    if (!(await confirmDialog(`${a.name} can no longer sign in, and every session of theirs ends now. Their changes stay in the audit log.`, { title: `Remove ${a.name}?`, ok: 'Remove account', danger: true }))) return;
    try { await ctx.api.del(`accounts/${enc(a.id)}`); rec.light('Account removed'); toast(`${a.name} was removed.`); await refreshAccounts(); } catch (e) {
      toast(e.code === 'last_admin' ? 'This is the last admin. Make another account an admin first.' : `Could not remove ${a.name}: ${errText(e)}`, { kind: 'error' });
    }
  }

  /* ---------------------------------------------------------------- your sessions */
  const focusSession = query.session || null;
  const sessionList = tracks(ctx, {
    title: 'Your sessions', icon: 'monitor', rows: S.sessions, label: 'Where you are signed in', csvName: 'sessions',
    note: `Every browser where your account is signed in; times in ${zoneText}. End one you do not know, then ${admin ? 'give your account a new password (Edit under Accounts)' : 'ask an admin for a new password'}.`,
    side: btn('Sign out everywhere else', endOthers, { icon: 'signout', sm: true }),
    sort: { key: 'last_seen_ms', dir: 'desc' },
    columns: [
      { key: 'device', label: 'Browser', lead: true, sort: true, render: (s) => h('span', { class: 'flagline' }, s.device || 'Not reported', s.this_one ? tag('This browser', 'cyan') : null, s.id === focusSession ? tag('From the audit log', 'org') : null) },
      { key: 'created_ms', label: `Signed in (${zn})`, sort: true, render: (s) => dateTime(s.created_ms, tz), csv: timeColumn(`Signed in (${isoNote})`, 'created_ms', tz) },
      { key: 'last_seen_ms', label: 'Last used', sort: true, render: (s) => (s.this_one ? 'Now' : agoEl(s.last_seen_ms)), csv: timeColumn(`Last used (${isoNote})`, 'last_seen_ms', tz) },
      { key: 'act', label: '', cls: 'is-act', csv: false, render: (s) => btn(s.this_one ? 'Sign out here' : 'Sign out', () => endSession(s), { icon: 'signout', sm: true }) },
    ],
    empty: () => emptyState({ icon: 'check', title: 'No sessions', text: 'Only this browser is signed in.' }),
  });
  async function refreshSessions() { await loadSessions(); if (ctx.alive()) sessionList.setRows(S.sessions); }
  async function endSession(s) {
    if (s.this_one) {
      try { await signOut(); toast('Signed out.', { next: true }); state.navigate('/sign-in', { replace: true }); } catch (e) { toast(`Could not sign out: ${errText(e)}`, { kind: 'error' }); }
      return;
    }
    try { await ctx.api.del(`me/sessions/${enc(s.id)}`); toast(`Signed out ${s.device || 'that browser'}.`); await refreshSessions(); } catch (e) { toast(`Could not end the session: ${errText(e)}`, { kind: 'error' }); }
  }
  async function endOthers() {
    const others = S.sessions.filter((s) => !s.this_one).length;
    if (!others) { toast('No other browser is signed in.', { kind: 'info' }); return; }
    if (!(await confirmDialog(`${format(others, 'count')} other ${others === 1 ? 'browser is' : 'browsers are'} signed out now. This one stays signed in.`, { title: 'Sign out everywhere else?', ok: 'Sign out the others' }))) return;
    try { const r = await ctx.api.del('me/sessions'); const n = r?.ended ?? others; toast(`Signed out ${format(n, 'count')} other ${n === 1 ? 'session' : 'sessions'}.`); await refreshSessions(); } catch (e) { toast(`Could not sign out the others: ${errText(e)}`, { kind: 'error' }); }
  }

  /* ---------------------------------------------------------------- audit log */
  const accountNameOf = (aid) => S.accounts.find((a) => a.id === aid)?.name;
  function target(t, car = null) {
    const i = String(t || '').indexOf(':');
    const kind = i < 0 ? t : t.slice(0, i);
    const tid = i < 0 ? '' : t.slice(i + 1);
    const a = (href, text) => h('a', { class: 'plc', href }, text);
    const known = car && carName.has(car);
    const of = known ? ` of ${carName.get(car)}` : '';
    switch (kind) {
      case 'drive': return known ? a(state.buildUrl('drive', { car, drive: tid }), `A drive${of}`) : 'A drive';
      case 'charge': return known ? a(state.buildUrl('charge', { car, charge: tid }), `A charge${of}`) : 'A charge';
      case 'update': return known ? a(state.buildUrl('software', { car }), `A software update${of}`) : 'A software update';
      case 'trip': {
        const tr = S.trips.find((x) => x.id === tid);
        const c = car || tr?.car;
        if (!c || !carName.has(c)) return 'A saved trip';
        return a(state.buildUrl('trip', { car: c, period: tr ? { from: tr.from_ms, to: tr.to_ms } : null }), tr?.name ? `Trip ${tr.name}` : `A saved trip of ${carName.get(c)}`);
      }
      case 'server': return 'The server';
      case 'account': return a(state.buildUrl('settings-server', { query: { account: tid } }), `Account ${accountNameOf(tid) || tid}`);
      case 'session': return a(state.buildUrl('settings-server', { query: { session: tid } }), 'A session');
      case 'car': return carName.has(tid) ? a(state.buildUrl('settings-cars', { car: tid }), carName.get(tid)) : h('span', { class: 'faint' }, 'A car no longer here');
      case 'settings': return tid === 'mqtt' ? a(state.buildUrl('settings-integrations'), 'MQTT settings') : a(state.buildUrl('settings-display'), 'Server defaults');
      case 'place': return a(state.buildUrl('places', { query: { place: tid } }), 'A place');
      case 'import': return a(state.buildUrl('import'), 'An import');
      case 'key': return a(state.buildUrl('settings-cars'), `CAN Bridge key ${tid}`);
      default: return h('span', { class: 'faint' }, t ? 'Another item' : DASH);
    }
  }
  const auditList = admin ? tracks(ctx, {
    title: 'Audit log', icon: 'book', path: 'audit', label: 'Who changed what', csvName: 'audit-log',
    note: `Every change and every sign-in, newest first. The item links to what was changed, where it still exists. Times in ${zoneText}.`,
    columns: [
      { key: 'at', label: `When (${zn})`, render: (e) => h('span', { class: 'nowrap' }, dateTime(e.at_ms, tz)), csv: timeColumn(`When (${isoNote})`, 'at_ms', tz) },
      { key: 'who', label: 'Who', lead: true, render: (e) => (e.account ? h('b', null, e.account) : h('span', { class: 'faint' }, 'The server')), csv: (e) => e.account || 'server' },
      { key: 'action', label: 'Action', render: (e) => code(e.action), csv: (e) => e.action },
      { key: 'target', label: 'Item', render: (e) => target(e.target, e.car), csv: (e) => e.target },
      { key: 'summary', label: 'What happened', render: (e) => e.summary || DASH, csv: (e) => e.summary || '' },
    ],
    empty: () => emptyState({ icon: 'book', title: 'Nothing recorded yet', text: 'Each sign-in and each change shows here as it happens.' }),
  }) : null;

  const move = admin ? movePanel(ctx, { tz, zoneText }) : null;

  /* ---------------------------------------------------------------- the page */
  paintReach(); paintNames();
  replace(ctx.view, h('div', { class: 'page ss' },
    plate({ kicker: 'Set-up · server', title: 'Server and access', aim: ctx.info.aim, tools: rec.el }),
    h('section', { class: 'hud hud--plain jump', 'aria-label': 'Sections' },
      presets([['ss-reach', 'Address'], ['ss-access', 'Access'], admin ? ['ss-safety', 'Safety'] : null, admin ? ['ss-move', 'Move'] : null, admin ? ['ss-accounts', 'Accounts'] : null, ['ss-sessions', 'Sessions'], admin ? ['ss-audit', 'Audit log'] : null].filter(Boolean)),
      h('p', { class: 'note jump-note' }, icon('lock', 'i--sm'), h('span', null, 'Changes save through the API and are recorded in the audit log. Moving, export files and the recovery key ask for your password again.'))),

    band(1, 'Reaching this server'),
    h('div', { class: 'cols' },
      hud({ title: 'How this server is reached', icon: 'globe', id: 'ss-reach', cover: 'web.shell.hosting' },
        h('div', { class: 'stack' }, h('div', { class: 'ss-addr' }, h('span', { class: 'ss-addr-l' }, 'This page'), h('span', { class: 'ss-addr-v' }, `${location.protocol}//${location.host}/`)), reachSlot)),
      h('div', { class: 'stack' },
        hud({ title: 'Host names for the certificate', icon: 'server', cover: 'docs.env.web.virtual-host' },
          h('div', { class: 'chans' }, chan({ no: 1, title: 'Names on the certificate', text: 'The names (or addresses) people and devices use to reach this server. They go into the certificate the server makes, so browsers accept those names. A change is used the next time a certificate is made.', wide: true }, namesSlot))),
        hud({ title: 'Ports', icon: 'plug', cover: 'docs.env.web.port' },
          h('div', { class: 'stack' }, kv(portRows),
            h('p', { class: 'chan-hint' }, S.network ? `${upIn ? 'Both ports speak' : 'The port speaks'} only HTTPS with TLS 1.3; there is no plain HTTP port.${upIn ? ` The upload port (${upIn.port}) takes only CAN Bridge uploads with a car's upload token, turns away wrong tokens after a few tries, and is closed while uploads are off.` : ''}` : 'An admin sees which ports the server listens on.'),
            h('p', { class: 'chan-hint' }, webPort != null
              ? ['To reach the web pages on another port of the host, change only the left number of the web port\'s mapping in the compose or TrueNAS file, for example ', code(`${hostPort}:${webPort}`), `, and use that number in the address.${upIn ? ` The upload port maps the same way; a CAN Bridge then uses the left number.` : ''} Inside the container the server listens on 0.0.0.0; the mapping decides what is reachable.`]
              : 'To use another port on the host, change only the left number of the port mapping in the compose or TrueNAS file (host port:container port) and use that number in the address.'))))),

    band(2, 'Who gets in'),
    hud({ title: 'How access works', icon: 'user', id: 'ss-access', cover: 'web.shell.access-model' },
      h('div', { class: 'ss-access' },
        h('div', { class: 'stack' },
          h('p', { class: 'ss-you' }, 'You are signed in as ', h('b', null, me.name || DASH), ' ', tag(ROLE[me.role] || me.role || DASH, me.role === 'admin' ? 'mag' : null), ' ',
            me.cars === 'all' ? 'and see every car.' : `and see ${format(Array.isArray(me.cars) ? me.cars.length : 0, 'count')} ${Array.isArray(me.cars) && me.cars.length === 1 ? 'car' : 'cars'}.`),
          admin ? counters([
            { label: 'Accounts', value: S.accounts.length, kind: 'count', how: 'everyone who can sign in' },
            { label: 'Admins', value: nAdmins, kind: 'count', how: 'at least one is kept' },
            { label: 'Watch only', value: S.accounts.length - nAdmins, kind: 'count', how: 'see their cars' },
            { label: 'Two-step', value: S.accounts.filter((a) => a.two_step_required).length, kind: 'count', how: 'a code after the password' },
          ]) : null),
        h('ul', { class: 'ss-rules' },
          h('li', null, h('b', null, 'Nothing without signing in. '), 'Every page and every API call needs a session; there is no open view and no open API.'),
          h('li', null, h('b', null, 'Two roles. '), `An admin ${ROLE_TEXT.admin} A watch-only account ${ROLE_TEXT.watch}`),
          h('li', null, h('b', null, 'A car list per account. '), 'A car not on an account\'s list looks exactly like a car that does not exist: it is in no list, total, map or export.'),
          h('li', null, h('b', null, 'Two-step sign-in. '), 'An account can be made to give a six-digit code from an authenticator app after its password.'),
          h('li', null, h('b', null, 'Guessing is slowed down. '), 'Five wrong passwords for one name within a minute, or 30 from one address, and sign-in waits a minute.'),
          h('li', null, h('b', null, 'Sessions you can see. '), 'A session lasts 30 days in its browser. Below you can end one, or every other one.'),
          h('li', null, h('b', null, 'Cars never sign in. '), 'A CAN Bridge uploads with its car\'s own upload token on the upload port, made in ', h('a', { class: 'plc', href: state.buildUrl('settings-cars') }, 'Car settings'), '.'),
          h('li', null, h('b', null, 'Every change is recorded. '), admin ? 'The audit log below says who changed what, and when.' : 'Admins can read who changed what, and when.')))),
    admin && focusAccount && !S.accounts.some((a) => a.id === focusAccount) ? note('The account from the audit log entry has been removed since.') : null,
    accountList ? h('div', { id: 'ss-accounts' }, accountList.el) : null,
    focusSession && !S.sessions.some((s) => s.id === focusSession) ? note('The session from the audit log entry has ended since.') : null,
    h('div', { id: 'ss-sessions' }, sessionList.el),
    auditList ? h('div', { id: 'ss-audit' }, auditList.el) : null,

    admin ? band(3, 'Safety and moving') : null,
    admin ? hud({ title: 'Certificate, recovery key and backups', icon: 'lock', id: 'ss-safety' },
      h('div', { class: 'stack' }, h('ul', { class: 'chks' },
        cert ? checkLine('warn', `The certificate ends ${cert.detail?.expires_ms ? date(cert.detail.expires_ms, tz, { year: true }) : 'soon'}${cert.detail?.days != null ? ` (in ${format(cert.detail.days, 'count')} days)` : ''}`,
          ['Make a new one before then: stop the container, delete ', code('data/tls/cert.pem'), ' (or put your own ', code('cert.pem'), ' and ', code('key.pem'), ' there) and start it again. A new certificate has a new SHA-256: type it again in each CAN Bridge.'])
          : checkLine('ok', 'Certificate', 'The server reports no problem with its certificate. It keeps the same SHA-256 across restarts, updates and moves.')),
        keySlot,
        h('ul', { class: 'chks' }, checkLine('ok', 'Everything is encrypted at rest', ['Logs, the database, tokens and the certificate\'s key are sealed. A backup is a copy of the data folder: it opens only with the key file (kept apart in ', code('/keys'), ') or the recovery key, so keep those out of the same backup.'])))) : null,
    move ? move.el : null));

  await Promise.all([auditList?.ready, move?.ready].filter(Boolean));
  if (ctx.alive()) ctx.ready();
}
