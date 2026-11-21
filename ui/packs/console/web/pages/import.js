// settings/import: TeslaMate history from TeslaMate's own backup file, or the CAN Bridge logs Sentry Six Manager
// already has, from its CAN export (a .zip), as numbered steps:
//   1. choose the file     it goes up in chunks over https, each with its SHA-256 (it can stop and go on)
//   2. check               the server reads the whole file and stores nothing yet (a zip: every log's signature)
//   3. match the cars      each imported car goes to a car here, a new car, or is left out (a zip has none:
//                          each log goes to the car its car key belongs to)
//   4. import              the rows are stored, marked as imported (a zip's logs as the car's own SMO data)
// A progress line under the steps, the report (rows per table and car, why rows were skipped: counts only; for a
// zip the logs per car key and why logs were refused), the spans where imported and live history overlap (with a
// switch), and every import so far.
// There is no live event for imports: while one is checked or imported the page reads it every 2 s.

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { format } from '../shared/units.js';
import { head, paint, section, note, link, btn, kv, textBar } from '../core/ui.js';
import { table } from '../core/table.js';
import { empty, errorWords } from '../core/states.js';
import { tag, dateTime, date, dur, ago, DASH } from '../core/fmt.js';
import { toast } from '../core/toast.js';
import { confirmDialog } from '../core/dialog.js';
import { src } from '../core/source.js';

export const id = 'import';
export const title = () => 'Import';

const enc = encodeURIComponent;
const bytes = (n) => (n == null ? DASH : format(n, 'bytes'));
const nf = (n) => (n == null ? DASH : format(n, 'count'));
const pct = (a, b) => (b > 0 ? Math.max(0, Math.min(100, (a / b) * 100)) : 0);
const hex = (buf) => [...new Uint8Array(buf)].map((b) => b.toString(16).padStart(2, '0')).join('');
const sum = (o) => Object.values(o || {}).reduce((a, b) => a + b, 0);
const UNFINISHED = ['uploading', 'checking', 'ready', 'importing'];
const TABLES = { cars: 'cars', car_settings: 'car settings', charges: 'charge points', drives: 'drives', charging_processes: 'charges', positions: 'track points',
  updates: 'software updates', geofences: 'places', addresses: 'addresses', states: 'car states' };
const REASONS = {
  'already-imported': 'already imported: stored by an earlier import, not stored twice', open: 'still going when the backup was made; a later backup brings it',
  'unknown-car': 'names a car the backup does not have', 'car-left-out': 'for a car you left out', 'unknown-charge': 'a charge point whose charge is not in the backup',
  'bad-value': 'a value it needs cannot be read', 'wrong-field-count': 'wrong number of fields', 'too-long': 'line too long to read',
};
const CAN = 'sentry_manager_can';
const isCan = (j) => j?.kind === CAN || j?.report?.kind === CAN;
// why a log of a CAN export was refused
const CAN_REASONS = {
  unsigned: ['not signed', 'no valid CAN Bridge signature, so nothing shows it came from the car'],
  failed_check: ['changed since signed', 'its bytes, name or car key are not the ones the CAN Bridge signed'],
  signed_by_another_key: ['another signing key', 'not the key this car key is paired with: trust the new key on settings/cars first if it is the car\'s'],
  no_sidecar: ['no sidecar', 'the zip has the log without its .log.xz.json, which holds the signature'],
  bad_sidecar: ['sidecar unreadable', 'its sidecar is not JSON, or over 1 MB'],
  different_file_here: ['another log of that name', 'a different log with the same name is here already; that one is kept'],
  being_uploaded: ['being uploaded', 'the CAN Bridge is uploading it now; that upload brings it'],
  too_large: ['too large', 'a log can be at most 1 GB'],
  car_full: ['car full', 'the car already holds the most logs a car keeps'],
};
const canReason = (c) => CAN_REASONS[c] || [String(c).replace(/[_-]/g, ' '), 'the server\'s own reason'];
const KEY_STATUS = { car: ['its car', 'ok'], new_car: ['a new car', 'warn'], only_car: ['the one car here', 'ok'], waiting: ['waiting for an admin', 'warn'], working: ['being decoded', 'running'] };
const FAILURES = {
  not_a_backup: 'The file is not a TeslaMate backup. Make one with TeslaMate\'s database backup (a pg_dump), plain or gzip or xz, and upload that.',
  mixed_cars: 'The file holds rows for cars it does not describe. Make a fresh, whole backup and try again.',
  no_car: 'The file holds no car: a backup made before TeslaMate saw a car has nothing to import.',
  truncated: 'The file ends part way: it was cut short when copied. Copy it again.',
  not_a_can_export: 'This zip is not a Sentry Six Manager CAN export. In Sentry Six Manager download the CAN export bundle (nerd panel, logs + ids.csv, the car\'s key, from before its first log to now) and upload that .zip as it is. A TeslaMate backup goes up as the .sql file itself (or .sql.gz, .sql.xz), not zipped.',
  unreadable: 'The file cannot be read as a zip: damaged, cut short, encrypted, or packed with a method other than deflate. Download it again and upload it as it is.',
  read_failed: 'The server could not read the uploaded file back. Upload it again.',
  store_failed: 'The server could not store a log (the disk may be full). Logs stored before stay; free some space and run it again.',
  stopped: 'The import was stopped. Logs stored before stay; run it again to import the rest.',
  interrupted: 'The server restarted part way. Logs stored before stay; run it again to import the rest.',
  internal_error: 'A fault in the import itself. Logs stored before stay; run it again, and if it fails the same way, the server log says more.',
};
const START_WORDS = {
  not_ready: 'The file is not checked yet. Import is possible once the check is done.',
  cars_unmatched: 'Some imported cars have no match yet: choose a car, new or leave out for each.',
  import_running: 'Another import runs already (perhaps in another tab). Wait for it or discard it.',
  no_such_import: 'This import was discarded, perhaps in another tab. Reload the page.',
};

export async function mount(ctx) {
  const tz = ctx.tz;
  const trail = h('p', { class: 'trail', cover: 'web.import.breadcrumb' }, link('/', 'fleet'), ' / ', link(state.buildUrl('settings-server'), 'settings'), ' / ', h('span', null, 'import'));
  if (!ctx.admin) {
    paint(ctx, head(ctx), trail, section({ title: 'import' }, empty('only an admin can import', 'Importing adds history to cars, so it needs an admin. The imported history then shows on every page, marked as imported.')));
    ctx.ready();
    return;
  }
  const S = { jobs: [], cars: [], job: null, match: [], overlaps: [], overlapErr: null };
  const up = { file: null, active: false, paused: false, error: null, t0: 0, b0: 0 };
  const loadJobs = async () => { S.jobs = await ctx.api.list('imports', { limit: 100 }).all({ max: 1000 }); };
  const loadCars = async () => { S.cars = await ctx.api.list('cars', { limit: 500 }).all({ max: 10000 }); };
  await Promise.all([loadJobs(), loadCars()]);
  if (!ctx.alive()) return;
  const carName = (cid) => S.cars.find((c) => c.id === cid)?.name || cid;
  const pick = () => {
    const want = ctx.query?.import;
    S.job = (want && S.jobs.find((j) => j.id === want)) || S.jobs.find((j) => UNFINISHED.includes(j.status)) || S.jobs[0] || null;
  };
  pick();

  /* ---------------------------------------------------------------- the file and the upload */
  const fileInput = h('input', { type: 'file', id: 'im-file', accept: '.sql,.gz,.xz,.zip,.dump,.bak,.txt,application/sql,application/gzip,application/x-xz,application/zip', 'aria-label': 'TeslaMate backup or Sentry Six Manager CAN export',
    onchange: () => { const f = fileInput.files?.[0]; fileInput.value = ''; if (f) chose(f); } });
  const choose = h('label', { class: 'file-pick', id: 'file' }, fileInput, h('span', { class: 'btn btn--primary', 'aria-hidden': 'true' }, 'choose file'),
    h('span', { class: 'muted' }, 'or drop it here: the .sql of a TeslaMate database backup (plain, .gz or .xz), or the .zip of Sentry Six Manager\'s CAN export; up to 64 GB'));
  ctx.on(choose, 'dragover', (e) => { e.preventDefault(); choose.classList.add('is-over'); });
  ctx.on(choose, 'dragleave', () => choose.classList.remove('is-over'));
  ctx.on(choose, 'drop', (e) => { e.preventDefault(); choose.classList.remove('is-over'); const f = e.dataTransfer?.files?.[0]; if (f) chose(f); });
  const upErr = h('p', { class: 'ke-msg', role: 'alert', hidden: true });
  const sayUp = (t) => { upErr.hidden = !t; replace(upErr, t || ''); };

  async function chose(file) {
    sayUp(null);
    const j = S.job;
    if (j && j.status === 'uploading' && !up.active) {
      if (file.size !== j.bytes_total || file.name !== j.name) { sayUp(`That is not the file this upload began with (${j.name}, ${bytes(j.bytes_total)}). Choose that file to go on, or discard this upload.`); return; }
      up.file = file; upload(j); return;
    }
    if (j && UNFINISHED.includes(j.status)) { sayUp('An import is not finished yet. Wait for it, or discard it, before you choose another file.'); return; }
    try {
      const made = await ctx.api.post('imports', { name: file.name, bytes_total: file.size });
      if (!ctx.alive()) return;
      up.file = file;
      S.jobs = [made, ...S.jobs.filter((x) => x.id !== made.id)];
      S.job = made;
      ctx.setQuery({ import: made.id });
      paintAll();
      upload(made);
    } catch (e) {
      if (!ctx.alive()) return;
      sayUp(e?.code === 'import_running' ? START_WORDS.import_running : e?.fields ? Object.values(e.fields).join(' ') : errorWords(e).text);
    }
  }
  async function upload(job) {
    if (up.active || !up.file) return;
    Object.assign(up, { active: true, paused: false, error: null, t0: Date.now(), b0: job.uploaded_bytes || 0 });
    paintAll();
    const file = up.file;
    const max = job.chunk_max_bytes || S.chunkMax || (1024 * 1024 - 1024);
    S.chunkMax = max;
    let off = job.uploaded_bytes || 0, tries = 0;
    while (off < file.size && ctx.alive() && !up.paused) {
      try {
        const buf = await file.slice(off, Math.min(file.size, off + max)).arrayBuffer();
        const sha = hex(await crypto.subtle.digest('SHA-256', buf));
        const r = await ctx.api.patch(`imports/${enc(job.id)}`, buf, { headers: { 'Content-Type': 'application/offset+octet-stream', 'Upload-Offset': String(off), 'Upload-Length': String(file.size), 'Upload-SHA256': sha } });
        if (!ctx.alive()) return;
        tries = 0;
        off = r.offset ?? r.uploaded_bytes;
        setJob(r);
        paintProgress();
      } catch (e) {
        if (!ctx.alive()) return;
        if (['wrong_offset', 'checksum_mismatch', 'network', 'timeout'].includes(e?.code) && tries < 3) {
          tries++;
          try { const fresh = await ctx.api.get(`imports/${enc(job.id)}`); if (!ctx.alive()) return; setJob(fresh); off = fresh.uploaded_bytes; if (fresh.status !== 'uploading') break; } catch { /* the chunk again */ }
          continue;
        }
        up.error = e?.code === 'not_uploading' ? null : e;
        break;
      }
    }
    up.active = false;
    if (up.error) sayUp(`The upload stopped at ${bytes(off)}: ${errorWords(up.error).text} Choose the file again to go on from there.`);
    if (off >= file.size) up.file = null;
    await refreshJob();
  }

  /* ---------------------------------------------------------------- start, discard */
  const startErr = h('p', { class: 'ke-msg', role: 'alert', hidden: true, cover: 'web.import.start-error' });
  async function start() {
    const j = S.job;
    startErr.hidden = true;
    if (!j) return;
    try {
      const r = await ctx.api.post(`imports/${enc(j.id)}/start`, {});
      if (!ctx.alive()) return;
      setJob(r);
      toast('Import started. You can leave this page; it goes on without it.');
    } catch (e) {
      if (!ctx.alive()) return;
      replace(startErr, START_WORDS[e?.code] || errorWords(e).text); startErr.hidden = false;
      await refreshJob();
      return;
    }
    paintAll();
  }
  async function discard() {
    const j = S.job;
    if (!j) return;
    const open = UNFINISHED.includes(j.status);
    const ok = await confirmDialog(j.status === 'done' ? `Remove ${j.name} from the list? The history it brought stays; only this record and its report go.`
      : j.status === 'importing' ? `Stop and discard ${j.name}? Rows stored so far stay (the same file again later skips them).`
        : `Discard ${j.name}? Nothing from it was stored, so your history does not change.`, { title: open ? 'discard this import?' : 'remove this import?', ok: open ? 'discard' : 'remove', danger: true });
    if (!ok || !ctx.alive()) return;
    up.paused = true; up.file = null;
    try { await ctx.api.del(`imports/${enc(j.id)}`); if (ctx.alive()) toast(open ? 'Import discarded.' : 'Import removed from the list.'); }
    catch (e) { if (!ctx.alive()) return; if (e?.code !== 'no_such_import') { toast(`Could not discard: ${errorWords(e).text}`, { kind: 'error' }); return; } }
    S.jobs = S.jobs.filter((x) => x.id !== j.id);
    S.job = null; S.match = [];
    ctx.setQuery({ import: null });
    pick();
    await loadMatch();
    paintAll();
  }

  /* ---------------------------------------------------------------- the steps */
  const stepsSlot = h('div');
  const progSlot = h('div', { cover: 'web.import.files.col-status' });
  function paintSteps() {
    const j = S.job, s = j?.status;
    const can = isCan(j);
    const unmatched = can ? 0 : S.match.filter((c) => c.match == null).length;
    const failedAt = s === 'failed' ? (j.checked_ms ? 'import' : 'check') : null;
    const T = (w, tone) => tag(w, tone);
    const st = {
      file: !j || s === 'done' || s === 'failed' ? 'todo' : s === 'uploading' ? 'running' : 'ok',
      check: !j || s === 'uploading' ? 'later' : s === 'checking' ? 'running' : failedAt === 'check' ? 'bad' : 'ok',
      match: !j || ['uploading', 'checking'].includes(s) || failedAt ? 'later' : s === 'ready' ? (unmatched ? 'todo' : 'ok') : 'ok',
      run: !j || ['uploading', 'checking'].includes(s) ? 'later' : s === 'ready' ? (unmatched ? 'later' : 'todo') : s === 'importing' ? 'running' : s === 'done' ? 'ok' : failedAt === 'import' ? 'bad' : 'later',
    };
    const tags = { ok: T('ok', 'ok'), todo: T('todo', 'warn'), running: T('running', 'running'), later: T('later', 'muted'), bad: T('bad', 'bad') };
    const step = (n, k, headText, text, acts, extra = null) => h('li', { class: ['run', st[k] === 'todo' || st[k] === 'running' ? 'run--now' : st[k] === 'later' ? 'run--later' : null], dataset: { row: '' } },
      h('span', { class: 'run-n' }, `${n}.`), h('span', { class: 'run-tag' }, tags[st[k]]),
      h('div', { class: 'run-body' }, h('p', { class: 'run-head' }, headText), h('p', { class: 'run-text' }, text), extra), h('div', { class: 'run-acts' }, acts));
    const canStart = s === 'ready' && !unmatched;
    replace(stepsSlot, h('ol', { class: 'runs' },
      step(1, 'file', 'choose the file', 'A TeslaMate backup or a Sentry Six Manager CAN export (.zip). It goes up over https in chunks of about 1 MB, each with its SHA-256, so a damaged chunk is sent again and a stopped upload goes on from the last chunk.',
        s === 'uploading' && up.active ? btn('pause', () => { up.paused = true; }) : s === 'uploading' && up.paused && up.file ? btn('go on', () => upload(S.job), { kind: 'primary' }) : null,
        [(!j || !UNFINISHED.includes(s) || (s === 'uploading' && !up.active)) ? choose : null, upErr]),
      step(2, 'check', 'check', can ? 'Pass 1: every log of the zip is read and its signature checked, as an upload\'s is. Nothing is stored yet.' : 'Pass 1: every row is read and checked. Nothing is stored yet; a failed check stores nothing.', null,
        failedAt ? h('div', { class: 'errbox-line', cover: 'web.import.run-error', role: 'alert' }, tag('bad', 'bad'), ` ${failedAt === 'check' ? 'the check found a problem' : 'the import failed'}: ${FAILURES[j.error?.code] || j.error?.message || 'the server gave no reason'}`,
          FAILURES[j.error?.code] && j.error?.message ? h('span', { class: 'muted' }, ` (the server says: ${j.error.message})`) : null, h('span', { class: 'muted' }, can ? ' Logs stored before stay (they are signed and checked); running it again skips them.' : ' Nothing from this file was stored. Discard it, then choose a good file.'))
          : h('span', { cover: 'web.import.run-error', class: 'muted' }, 'If the check or the import fails, why is said here in plain words.')),
      step(3, 'match', 'match the cars', can ? 'Nothing to match: each log goes to the car its car key belongs to (a new key as its first upload would, by settings/cars).' : unmatched ? `${unmatched} imported car${unmatched === 1 ? '' : 's'} still need a choice below.` : 'Each car of the backup goes to a car here, a new car, or is left out.', null),
      step(4, 'run', 'import', s === 'done' ? `Done ${dateTime(j.finished_ms, tz)}; it took ${dur(j.finished_ms - j.started_ms)}. ${can ? ' The logs are in as the car\'s own SMO data; drives and charges are built from them.' : ' The history shows on every page, marked as imported.'}` : can ? 'Pass 2: each log is stored and checked as if the CAN Bridge had uploaded it, then decoded.' : 'Pass 2: the rows are stored and the timeline is built again for the matched cars.',
        [h('button', { class: 'btn btn--primary', type: 'button', cover: 'web.import.start', disabled: !canStart, onclick: start }, 'run'),
          h('button', { class: 'btn', type: 'button', cover: 'web.import.discard', disabled: !j, onclick: discard }, j && !UNFINISHED.includes(s) ? 'remove' : 'discard')],
        [startErr, h('p', { class: 'fld-hint' }, !j ? 'choose a file first' : s === 'ready' ? (unmatched ? 'match the cars first' : 'checked and matched: ready') : s === 'uploading' || s === 'checking' ? 'possible once the file is uploaded and checked' : s === 'importing' ? 'running now' : s === 'done' ? 'choose a file to import again' : 'discard it and choose a good file')])));
  }
  function paintProgress() {
    const j = S.job;
    if (!j) { replace(progSlot, h('pre', { class: 'mono-box' }, 'no import yet: choose a TeslaMate backup or a CAN export above')); return; }
    const s = j.status;
    let doneB = 0, of = j.bytes_total, what = s;
    if (s === 'uploading') { doneB = j.uploaded_bytes ?? 0; what = up.active ? 'uploading' : up.paused ? 'paused' : 'stopped'; }
    else if (s === 'checking' || s === 'importing') doneB = j.bytes_read ?? 0;
    else if (s !== 'failed') doneB = of;
    const p = pct(doneB, of);
    const secs = (Date.now() - up.t0) / 1000;
    const rate = s === 'uploading' && up.active && secs > 1 ? (doneB - up.b0) / secs : null;
    replace(progSlot, h('pre', { class: 'mono-box', 'aria-live': 'polite' },
      `${what.padEnd(9)} `, textBar(p, { tone: s === 'failed' ? 'bad' : 'accent' }), ` ${format(p, 'number', { digits: 0 }).padStart(3)} %  ${bytes(doneB)} of ${bytes(of)}`,
      rate ? `  ${bytes(rate)}/s, about ${dur(((of - doneB) / rate) * 1000)} left` : '',
      `\nfile      ${j.name}${j.compression && j.compression !== 'plain' ? ` (${j.compression})` : ''}`,
      `\nkind      ${isCan(j) ? 'Sentry Six Manager CAN export' : j.kind === 'teslamate' ? 'TeslaMate backup' : 'known once checked'}`,
      `\nstarted   ${dateTime(j.started_ms, tz)}`, j.checked_ms ? `\nchecked   ${dateTime(j.checked_ms, tz)}` : '', j.finished_ms ? `\nfinished  ${dateTime(j.finished_ms, tz)}` : '',
      s === 'uploading' && !up.active ? `\nThe server has the first ${bytes(j.uploaded_bytes)}. Choose the same file again to go on from there.` : ''));
  }

  /* ---------------------------------------------------------------- matching */
  async function loadMatch() {
    S.match = [];
    const j = S.job;
    if (!j || isCan(j) || !['checking', 'ready'].includes(j.status)) return;
    try { S.match = (await ctx.api.get(`imports/${enc(j.id)}/cars`)).items || []; } catch { S.match = []; }
  }
  async function setMatch(c, value) {
    try {
      const r = await ctx.api.put(`imports/${enc(S.job.id)}/cars`, { items: [{ tm_car: c.tm_car, match: value }] });
      if (!ctx.alive()) return;
      S.match = r.items || S.match;
      toast(value === 'new' ? `${c.name} becomes a new car.` : value === 'leave_out' ? `${c.name} is left out.` : `${c.name} goes to ${carName(value)}.`);
    } catch (e) { if (ctx.alive()) toast(e?.fields ? Object.values(e.fields).join(' ') : errorWords(e).text, { kind: 'error' }); }
    paintAll();
  }
  const matchT = table(ctx, {
    title: 'match the cars', rows: [], csv: false, label: 'Cars in the backup and where their history goes',
    note: 'A car here with the same VIN prefix (the same for every car of a model and factory year) is chosen at first. One car here takes at most one imported car.',
    columns: [
      { key: 'name', label: 'in the backup', ph: 'main', render: (c) => [c.name, h('span', { class: 'muted' }, ` #${c.tm_car}`)] },
      { key: 'vin_prefix', label: 'vin prefix', ph: 'sub' },
      { key: 'drives', label: 'drives', num: true, ph: 'sub', render: (c) => nf(c.drives) },
      { key: 'charges', label: 'charges', num: true, render: (c) => nf(c.charges) },
      { key: 'span', label: 'history', render: (c) => `${date(c.first_ms, tz)} .. ${date(c.last_ms, tz)}` },
      { key: 'match', label: 'goes to', grow: true, ph: 'num', render: (c) => {
        const taken = new Set(S.match.filter((x) => x.tm_car !== c.tm_car && x.match && !['new', 'leave_out'].includes(x.match)).map((x) => x.match));
        const sel = h('select', { class: 'in in--cell', 'aria-label': `Where ${c.name}'s history goes`, disabled: !['ready', 'checking'].includes(S.job?.status), onchange: () => setMatch(c, sel.value) },
          c.match == null ? h('option', { value: '', selected: true, disabled: true }, 'choose') : null,
          S.cars.map((x) => h('option', { value: x.id, selected: c.match === x.id, disabled: taken.has(x.id) }, `${x.name}${x.id === c.suggested ? ' (same vin prefix)' : ''}${taken.has(x.id) ? ' (taken)' : ''}`)),
          h('option', { value: 'new', selected: c.match === 'new' }, 'a new car'), h('option', { value: 'leave_out', selected: c.match === 'leave_out' }, 'leave out'));
        return [sel, c.match == null ? [' ', tag('todo', 'warn')] : c.suggested && c.match === c.suggested ? [' ', tag('vin match', 'ok')] : null];
      } },
    ],
    empty: () => empty('no cars to match yet', 'The cars of the file are known once the whole file is uploaded.'),
  });

  /* ---------------------------------------------------------------- report */
  const repSlot = h('div', { cover: 'web.import.rejected-count' });
  const tablesT = table(ctx, {
    title: 'rows per table', rows: [], csv: false, label: 'Rows read, imported and skipped per table', note: 'Counts only: the report never keeps row values or positions.',
    columns: [
      { key: 'name', label: 'table', ph: 'main', grow: true, render: (r) => TABLES[r.name] || r.name.replace(/_/g, ' ') },
      { key: 'read', label: 'read', num: true, ph: 'sub', render: (r) => nf(r.read) },
      { key: 'imp', label: 'imported', num: true, ph: 'num', render: (r) => (S.rep?.imported ? nf(r.imported) : h('span', { class: 'muted' }, `${nf(r.would)} to import`)) },
      { key: 'skipped', label: 'skipped', num: true, ph: 'sub', render: (r) => (r.skipped ? h('span', { class: 'tone--warn' }, nf(r.skipped)) : h('span', { class: 'muted' }, '0')) },
      { key: 'bad', label: 'values cleared', render: (r) => (r.bad.length ? r.bad.map(([c, n]) => `${c} ${nf(n)}`).join(', ') : h('span', { class: 'muted' }, 'none')) },
    ],
  });
  const reasonsT = table(ctx, {
    title: 'why rows were skipped', rows: [], csv: false, cover: 'web.import.rejected.col-reason', label: 'Skipped rows by reason', sort: { key: 'n', dir: 'desc' },
    columns: [
      { key: 'table', label: 'table', ph: 'sub', render: (r) => TABLES[r.table] || r.table },
      { key: 'reason', label: 'reason', ph: 'main', grow: true, render: (r) => [r.reason, h('span', { class: 'muted' }, ` ${REASONS[r.reason] || ''}`)] },
      { key: 'n', label: 'rows', num: true, sort: true, ph: 'num', render: (r) => nf(r.n) },
    ],
    empty: () => empty('no row was skipped'),
  });
  const carsT = table(ctx, {
    title: 'rows per car', rows: [], csv: false, label: 'Rows per imported car', note: 'Places belong to no car, so they count only per table.',
    columns: [
      { key: 'name', label: 'in the backup', ph: 'main', render: (c) => [c.name || `car ${c.tm_car}`, h('span', { class: 'muted' }, ` #${c.tm_car}`)] },
      { key: 'match', label: 'went to', ph: 'sub', render: (c) => (c.match && !['new', 'leave_out'].includes(c.match) ? link(state.buildUrl('car', { car: c.match }), carName(c.match)) : c.match === 'new' ? 'a new car' : c.match === 'leave_out' ? h('span', { class: 'muted' }, 'left out') : h('span', { class: 'tone--warn' }, 'not matched')) },
      { key: 'read', label: 'read', num: true, render: (c) => (c.read == null ? h('span', { class: 'muted' }, 'not counted') : nf(c.read)) },
      { key: 'imp', label: 'imported', num: true, ph: 'num', render: (c) => (c.read == null ? null : S.rep?.imported ? nf(c.imported) : h('span', { class: 'muted' }, `${nf(c.read - sum(c.skipped))} to import`)) },
      { key: 'skipped', label: 'skipped', num: true, grow: true, render: (c) => (c.skipped == null ? null : sum(c.skipped) ? h('span', { class: 'tone--warn' }, `${nf(sum(c.skipped))} (${Object.entries(c.skipped).filter(([, n]) => n).map(([k, n]) => `${k} ${nf(n)}`).join(', ')})`) : '0') },
    ],
  });
  /* ---------------------------------------------------------------- a CAN export's report */
  const keysT = table(ctx, {
    title: 'logs per car key', rows: [], csv: false, label: 'Logs read, imported, already here and refused per car key',
    note: 'A car key is the CAN Bridge\'s install on one car. A key a car has goes to that car; a new key gets a car as a new key\'s first upload does (settings/cars).',
    columns: [
      { key: 'key', label: 'car key', ph: 'main', render: (k) => k.key },
      { key: 'car', label: 'goes to', ph: 'sub', render: (k) => (k.car ? link(state.buildUrl('car', { car: k.car }), k.car_name || carName(k.car)) : tag(...(KEY_STATUS[k.status] || [k.status, 'muted']))) },
      { key: 'logs', label: 'logs', num: true, render: (k) => nf(k.logs) },
      { key: 'imp', label: 'imported', num: true, ph: 'num', render: (k) => (S.rep?.imported ? nf(k.imported) : h('span', { class: 'muted' }, `${nf((k.logs || 0) - (k.already_here || 0) - (k.refused || 0))} to import`)) },
      { key: 'already_here', label: 'here already', num: true, render: (k) => nf(k.already_here) },
      { key: 'refused', label: 'refused', num: true, ph: 'sub', render: (k) => (k.refused ? h('span', { class: 'tone--warn' }, nf(k.refused)) : h('span', { class: 'muted' }, '0')) },
      { key: 'chain', label: 'chain', grow: true, render: (k) => (k.chain_gaps || k.chain_forks
        ? h('span', { class: 'tone--warn', title: 'As for live uploads: a gap is a log the chain names that is not here; a fork two logs at one place' }, [k.chain_gaps ? `${nf(k.chain_gaps)} gap${k.chain_gaps === 1 ? '' : 's'}` : null, k.chain_forks ? `${nf(k.chain_forks)} fork${k.chain_forks === 1 ? '' : 's'}` : null].filter(Boolean).join(', '))
        : h('span', { class: 'muted' }, 'whole')) },
      { key: 'span', label: 'logs from', render: (k) => (k.first_ms ? `${date(k.first_ms, tz)} .. ${date(k.last_ms, tz)}` : DASH) },
    ],
    empty: () => empty('no car keys', 'The zip holds no CAN Bridge log.'),
  });
  const refusedT = table(ctx, {
    title: 'refused logs', rows: [], csv: false, label: 'Logs that were refused, with why',
    note: 'A refused log is never stored: only logs signed by the car\'s CAN Bridge and unchanged since are taken. The first 100 are named; all are counted.',
    columns: [
      { key: 'name', label: 'log', ph: 'main', cls: 'wrap-cell', render: (r) => h('span', { class: 'cert-sha' }, r.name) },
      { key: 'reason', label: 'why', ph: 'sub', grow: true, cls: 'wrap-cell', render: (r) => [canReason(r.reason)[0], h('span', { class: 'muted' }, `: ${canReason(r.reason)[1]}`)] },
      { key: 'code', label: 'code', ph: 'num', render: (r) => h('span', { class: 'muted' }, r.reason) },
    ],
    empty: () => empty('no log was refused', 'Every log of the zip is signed by its car\'s CAN Bridge and unchanged since.'),
  });
  function paintCanReport(rep) {
    const L = rep.logs || {};
    tablesT.el.hidden = reasonsT.el.hidden = carsT.el.hidden = true;
    keysT.el.hidden = refusedT.el.hidden = false;
    const by = Object.entries(rep.refused || {}).filter(([, n]) => n);
    replace(repSlot, kv([
      ['logs read', [nf(L.read), h('span', { class: 'muted' }, ` ${bytes(L.bytes)} of CAN Bridge logs`)]],
      [rep.imported ? 'logs imported' : 'logs to import', [nf(rep.imported ? L.imported : L.to_import), h('span', { class: 'muted' }, ' signed by the car\'s CAN Bridge, unchanged since')]],
      ['here already', [nf(L.already_here), h('span', { class: 'muted' }, ' the same name and SHA-256: not stored twice')]],
      ['refused', L.refused ? [h('span', { class: 'tone--warn' }, nf(L.refused)), ` ${by.map(([k, n]) => `${canReason(k)[0]} ${nf(n)}`).join(', ')}`] : 'none: every log checked out', { tone: L.refused ? 'warn' : null }],
      L.sidecars_added ? ['sidecars added', nf(L.sidecars_added)] : null,
      ['logs from', rep.span?.from_ms ? `${date(rep.span.from_ms, tz)} .. ${date(rep.span.to_ms, tz)}` : 'the zip holds no log'],
      rep.manifest ? ['manifest', `car key ${rep.manifest.vehicle_key || DASH}, ${nf(rep.manifest.logs)} logs, made ${rep.manifest.made_at || DASH}`] : null,
      rep.files ? ['files', `${nf(rep.files.entries)} entries${rep.files.candump_text ? `, ${nf(rep.files.candump_text)} candump text copies (not signed: skipped)` : ''}${rep.files.other ? `, ${nf(rep.files.other)} other` : ''}`] : null,
    ].filter(Boolean)),
    note(rep.imported ? 'What the import stored: the car\'s own signed CAN logs, as if the CAN Bridge had uploaded them. Their drives and charges say data from SMO. ' : 'From the check: what the import will store. Nothing is stored yet. ',
      L.refused ? 'A refused log is never stored, so it cannot make up history.' : ''),
    (rep.warnings || []).length ? h('ul', { class: 'ln-list' }, rep.warnings.map((w) => h('li', null, tag('warn', 'warn'), h('span', { class: 'ln-list-t' }, typeof w === 'string' ? w : w.message || w.code)))) : null);
    keysT.setRows(rep.cars || []);
    refusedT.setRows(rep.refused_logs || []);
  }
  function paintReport() {
    const j = S.job, rep = j?.report || null;
    S.rep = rep;
    keysT.el.hidden = refusedT.el.hidden = true;
    if (rep && rep.kind === CAN) { paintCanReport(rep); return; }
    if (!rep) {
      replace(repSlot, empty(j ? 'no report yet' : 'no import yet', j ? 'The report appears once the whole file has been checked.' : 'Each import gets a report: rows read, imported and skipped per table, and why.'));
      tablesT.el.hidden = reasonsT.el.hidden = carsT.el.hidden = true;
      return;
    }
    tablesT.el.hidden = reasonsT.el.hidden = false; carsT.el.hidden = !(rep.cars || []).length;
    const names = Object.keys(rep.tables || {});
    const rows = names.map((name) => { const t = rep.tables[name]; const sk = sum(t.skipped); return { name, read: t.read, imported: t.imported, would: t.read - sk, skipped: sk, bad: Object.entries(t.bad_values || {}) }; });
    const tot = rows.reduce((a, r) => ({ read: a.read + r.read, imp: a.imp + (rep.imported ? r.imported : r.would), sk: a.sk + r.skipped }), { read: 0, imp: 0, sk: 0 });
    replace(repSlot, kv([
      ['rows read', nf(tot.read)],
      [rep.imported ? 'rows imported' : 'rows to import', nf(tot.imp)],
      ['rows skipped', tot.sk ? [h('span', { class: 'tone--warn' }, nf(tot.sk)), ' each with its reason below'] : 'none: every row passed', { tone: tot.sk ? 'warn' : null }],
      ['history', rep.span ? `${date(rep.span.from_ms, tz)} .. ${date(rep.span.to_ms, tz)} (${dur(rep.span.to_ms - rep.span.from_ms)})` : 'no dated rows'],
    ]), note(rep.imported ? 'What the import stored. ' : 'From the check: what the import will store. Nothing is stored yet. ',
      tot.sk ? `Skipping a row did not stop the import. A drive or charge built around a skipped row can come out a little short.` : ''));
    tablesT.setRows(rows);
    reasonsT.setRows(names.flatMap((name) => Object.entries(rep.tables[name].skipped || {}).map(([reason, n]) => ({ table: name, reason, n }))));
    carsT.setRows(rep.cars || []);
  }

  /* ---------------------------------------------------------------- overlaps */
  async function loadOverlaps() {
    S.overlapErr = null;
    const all = await Promise.all(S.cars.map(async (c) => {
      try { return (await ctx.api.list(`cars/${enc(c.id)}/overlaps`, { limit: 200 }).all({ max: 5000 })).map((o) => ({ ...o, car: c.id })); }
      catch (e) { if (e?.name !== 'AbortError') S.overlapErr = e; return []; }
    }));
    S.overlaps = all.flat().sort((a, b) => b.start_ms - a.start_ms);
  }
  const zoneOf = (o) => S.cars.find((c) => c.id === o.car)?.time_zone || tz;
  // the side shown: live (SMO or the Tesla API) or import:teslamate; the old words can and teslamate still read
  const shownOf = (o) => o.shown_source || (o.shown === 'teslamate' ? 'import:teslamate' : 'live');
  const liveOf = (o) => o.live || o.can;
  const importedOf = (o) => o.imported || o.teslamate;
  const liveSrc = (o) => ({ sources: (o.sources || []).filter((x) => x.kind !== 'import') });
  const impSrc = (o) => ({ sources: (o.sources || []).filter((x) => x.kind === 'import') });
  async function flip(o, shown) {
    if (shownOf(o) === shown) return;
    try {
      const r = await ctx.api.patch(`cars/${enc(o.car)}/overlaps`, { id: o.id, shown });
      if (!ctx.alive()) return;
      const ch = r.changed || { ...o, shown_source: shown };
      S.overlaps = S.overlaps.map((x) => (x.car === o.car && x.id === o.id ? { ...x, ...ch, car: o.car } : x));
      toast(`${carName(o.car)}: ${shown === 'live' ? 'live' : 'imported'} history shown for that span on every page.`);
    } catch (e) { if (ctx.alive()) toast(`Could not switch: ${errorWords(e).text}`, { kind: 'error' }); }
    overlapT.setRows(S.overlaps);
  }
  const side = (x) => (x ? `${nf(x.drives)} drives, ${nf(x.charges)} charges` : DASH);
  const overlapT = table(ctx, {
    title: 'spans with both histories', rows: [], csv: false, label: 'Spans where imported and live history overlap',
    sort: { key: 'start_ms', dir: 'desc' },
    columns: [
      { key: 'car', label: 'car', ph: 'main', render: (o) => carName(o.car) },
      { key: 'start_ms', label: 'from', sort: true, ph: 'sub', render: (o) => dateTime(o.start_ms, zoneOf(o)) },
      { key: 'end_ms', label: 'to', render: (o) => dateTime(o.end_ms, zoneOf(o)) },
      { key: 'len', label: 'length', num: true, render: (o) => dur(o.end_ms - o.start_ms) },
      { key: 'live', label: 'live has', render: (o) => [side(liveOf(o)), ' ', src(liveSrc(o))] },
      { key: 'imported', label: 'imported has', render: (o) => [side(importedOf(o)), ' ', src(impSrc(o))] },
      { key: 'shown', label: 'shown', grow: true, ph: 'num', render: (o) => h('div', { class: 'keys', role: 'group', 'aria-label': `Which history is shown for ${carName(o.car)}` },
        [['live', 'live'], ['import:teslamate', 'imported']].map(([v, l]) => h('button', { class: 'key', type: 'button', 'aria-pressed': String(shownOf(o) === v), onclick: () => flip(o, v) }, l)),
        link(state.buildUrl('timeline', { car: o.car, period: { from: o.start_ms, to: o.end_ms } }), 'timeline')) },
    ],
    empty: () => (S.overlapErr ? empty('overlaps could not be read', `${errorWords(S.overlapErr).text} Reload the page to try again.`)
      : empty('no overlap', 'No span has both imported and live (SMO or Tesla API) history, so every span shows the one history it has.')),
  });

  /* ---------------------------------------------------------------- every import */
  const STATE_TAG = { uploading: ['uploading', 'running'], checking: ['checking', 'running'], ready: ['checked', 'warn'], importing: ['importing', 'running'], done: ['done', 'ok'], failed: ['failed', 'bad'] };
  const historyT = table(ctx, {
    title: 'imports', rows: [], csv: false, label: 'Every import, newest first',
    rowHref: (j) => state.buildUrl('import', { query: { import: j.id } }),
    sort: { key: 'started_ms', dir: 'desc' },
    columns: [
      { key: 'name', label: 'file', ph: 'main', grow: true, render: (j) => [link(state.buildUrl('import', { query: { import: j.id } }), j.name), j.id === S.job?.id ? [' ', tag('shown', 'accent')] : null] },
      { key: 'status', label: 'state', ph: 'num', render: (j) => tag(...(STATE_TAG[j.status] || [j.status, 'muted'])) },
      { key: 'bytes_total', label: 'size', num: true, ph: 'sub', render: (j) => bytes(j.bytes_total) },
      { key: 'started_ms', label: 'started', sort: true, ph: 'sub', render: (j) => dateTime(j.started_ms, tz) },
      { key: 'finished_ms', label: 'finished', render: (j) => (j.finished_ms ? ago(j.finished_ms) : h('span', { class: 'muted' }, UNFINISHED.includes(j.status) ? 'not yet' : DASH)) },
    ],
    empty: () => empty('no imports yet'),
  });

  /* ---------------------------------------------------------------- refresh */
  function setJob(j) {
    S.jobs = S.jobs.map((x) => (x.id === j.id ? { ...x, ...j } : x));
    if (S.job?.id === j.id) S.job = { ...S.job, ...j };
  }
  let busy = false, tick = 0;
  async function refreshJob() {
    const j = S.job;
    if (!j || busy) return;
    busy = true;
    const was = j.status;
    try {
      const fresh = await ctx.api.get(`imports/${enc(j.id)}`);
      if (!ctx.alive()) return;
      setJob(fresh);
      if (fresh.status !== was || ['checking', 'ready'].includes(fresh.status)) await loadMatch();
      if (fresh.status === 'done' && was !== 'done') { await loadJobs(); await loadCars(); pick(); await loadOverlaps(); toast('Import done. The history shows on every page, marked as imported.'); }
      if (fresh.status === 'failed' && was !== 'failed') toast('The import failed; step 2 says why.', { kind: 'error' });
    } catch (e) {
      if (!ctx.alive()) return;
      if (e?.code === 'no_such_import') { S.jobs = S.jobs.filter((x) => x.id !== j.id); S.job = null; pick(); }
    } finally { busy = false; }
    if (ctx.alive()) paintAll();
  }
  ctx.every(2000, () => {
    const j = S.job;
    tick++;
    if (!j || up.active) return;
    if (j.status === 'checking' || j.status === 'importing') refreshJob();
    else if (j.status === 'uploading' && tick % 5 === 0) refreshJob();
  });
  function paintAll() {
    paintSteps(); paintProgress(); paintReport();
    const show = S.job && !isCan(S.job) && ['checking', 'ready'].includes(S.job.status);
    matchT.el.hidden = !show;
    if (show) matchT.setRows(S.match);
    historyT.setRows(S.jobs);
    overlapT.setRows(S.overlaps);
  }

  await Promise.all([loadMatch(), loadOverlaps()]);
  if (!ctx.alive()) return;
  const firstCar = S.cars.find((c) => c.id === S.overlaps[0]?.car) || S.cars[0];
  paint(ctx, head(ctx, { title: 'Import' }), trail,
    h('p', { class: 'note' }, 'Brings in the history TeslaMate recorded before the CAN Bridge, from TeslaMate\'s own backup file, or the CAN Bridge logs Sentry Six Manager already has, from its CAN export (.zip). The whole file is checked before anything is stored, and running it again stores nothing twice.'),
    section({ title: 'steps' }, stepsSlot, h('p', { class: 'sub-title' }, 'progress'), progSlot,
      h('p', { class: 'note', cover: 'web.import.restart-note' }, 'Safe to leave: close the page or the browser while it runs. After a server restart an upload goes on from the last chunk once you choose the file again; a check starts over (nothing was stored); an import runs again and rows already in are known by their TeslaMate id (a CAN export\'s logs by name and SHA-256), so nothing is stored twice.')),
    matchT.el,
    section({ title: 'report' }, repSlot),
    tablesT.el, reasonsT.el, carsT.el, keysT.el, refusedT.el,
    section({ title: 'overlap with live history', id: 'overlaps', cover: ['web.import.docs-overlap', 'web.import.docs-backfill', 'faq.no-backfill'] },
      h('ul', { class: 'ln-list' },
        h('li', { cover: 'web.import.docs-overlap' }, tag('kept', 'muted'), h('span', { class: 'ln-list-t' }, 'Every span is imported, also where SMO or the Tesla API recorded too. Those spans are listed below with the side shown on drives, charges, timeline, statistics and travel: live at first, as SMO measures. Neither side is deleted.')),
        h('li', { cover: 'web.import.docs-backfill' }, tag('kept', 'muted'), h('span', { class: 'ln-list-t' }, 'Addresses and heights come with the backup, so imported drives have them at once. A stop without an address gets one from the offline lookup in the minutes after; it shows its coordinates until then.')),
        h('li', { cover: 'faq.no-backfill' }, tag('gap', 'warn'), h('span', { class: 'ln-list-t' }, 'Time nobody recorded cannot be fetched later: Tesla keeps no history to download. The CAN Bridge uploads its own logs when it can, so its gaps fill themselves; time before it shows as unknown on the ',
          firstCar ? link(state.buildUrl('timeline', { car: firstCar.id }), 'timeline') : 'timeline', '. Only an import from another logger, such as TeslaMate, fills it.')))),
    overlapT.el,
    historyT.el);
  paintAll();
  ctx.ready();
}
