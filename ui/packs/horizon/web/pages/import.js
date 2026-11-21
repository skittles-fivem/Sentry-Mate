// Import (/settings/import): bring TeslaMate history in from TeslaMate's own backup file. Horizon shows it as a
// tape deck: a row of step lamps (choose, upload, check, match, import, done), the file going up in chunks
// over HTTPS (each with its SHA-256, so a stop carries on from the last chunk the server has), three passes
// with their bars, the imported cars matched to cars here, then the report as trip counters and lists of rows
// per table, per car and why rows were skipped (counts only, never row values or positions). Spans where
// imported and CAN history overlap are listed with a switch for the side that is shown.
//
// There is no live event for imports: while a job is checked or imported (or uploaded from another tab) the
// page reads that one job again every 2 s, and stops once it is ready, done or failed.

import * as state from '../shared/state.js';
import { h, replace } from '../shared/dom.js';
import { toIsoZoned } from '../shared/time.js';
import { format } from '../shared/units.js';
import { icon, hud, plate, btn, tag, kv, bar, callout } from '../core/ui.js';
import { counters, band } from '../core/kit.js';
import { tracks } from '../core/tracks.js';
import { emptyState, errorWords } from '../core/states.js';
import { confirmDialog, toast } from '../core/layer.js';
import { dateTime, date, dur, num, agoEl, DASH } from '../core/fmt.js';
import { lampSteps, sig, code, errText, recLamp } from '../core/setting.js';

export const id = 'import';
export const css = true;
export const title = () => 'Import';

const enc = encodeURIComponent;
const bytes = (n) => (n == null ? DASH : format(n, 'bytes'));
const share = (a, b) => (b > 0 ? Math.max(0, Math.min(1, a / b)) : 0);
const pctText = (a, b) => `${num(share(a, b) * 100)} %`;
const UNFINISHED = ['uploading', 'checking', 'ready', 'importing'];
const STATUS = {
  uploading: ['Uploading', 'live', 'The file goes up to the server in chunks.'],
  checking: ['Checking', 'live', 'Pass 1: the server reads the whole file and checks every row. Nothing is stored yet.'],
  ready: ['Checked, waiting', 'warn', 'The whole file was checked. Match the cars below, then start the import.'],
  importing: ['Importing', 'live', 'Pass 2: the rows are being stored.'],
  done: ['Done', 'ok', 'The history is in. It shows on every page, marked as imported.'],
  failed: ['Failed', 'bad', 'The import stopped. Nothing from this file was stored.'],
};
const TABLES = {
  cars: ['Cars', 'One row per car in the backup.'],
  car_settings: ['Car settings', 'Per car: an LFP pack and free Supercharging. TeslaMate\'s polling settings are not kept.'],
  charges: ['Charge points', 'Readings taken while a car charged.'],
  drives: ['Drives', 'Trips from start to stop.'],
  charging_processes: ['Charges', 'Charging sessions with their energy and cost.'],
  positions: ['Track points', 'Positions along the drives.'],
  updates: ['Software updates', 'Versions the car installed.'],
  geofences: ['Places', 'Named areas; they become places here when no place covers them yet.'],
  addresses: ['Addresses', 'Street addresses of the places the car stopped.'],
  states: ['Car states', 'Online, asleep and offline spells.'],
};
const tableName = (t) => TABLES[t]?.[0] || t.replace(/_/g, ' ');
const REASONS = {
  'already-imported': ['Already imported', 'An earlier import stored this row (or left it out for good); it is not stored twice.'],
  open: ['Still going', 'The drive, charge or spell had no end when the backup was made; a later backup brings it.'],
  'unknown-car': ['Car not in the backup', 'The row names a car the backup\'s cars table does not have.'],
  'car-left-out': ['For a car left out', 'The row belongs to an imported car you chose to leave out.'],
  'unknown-charge': ['Charge not in the backup', 'A charge point whose charge is not in the backup, or was not kept.'],
  'bad-value': ['A value cannot be read', 'A value the row needs is missing or unreadable; Values cleared names the column.'],
  'wrong-field-count': ['Wrong number of fields', 'The row has not as many fields as the table has columns.'],
  'too-long': ['Line too long', 'The row is longer than the server reads in one line.'],
  duplicate: ['Already in the file', 'The same row (same TeslaMate id) is in the file twice; the first copy is kept.'],
  no_energy: ['No energy figures', 'A charge that recorded no energy; it would show as 0 kWh, so it is left out.'],
  no_position: ['No coordinates', 'A track point without a position; the drive keeps its other points.'],
  bad_time: ['Time cannot be read', 'The row\'s time is missing or not a date.'],
  other_car: ['For a car left out', 'The row belongs to an imported car you chose to leave out.'],
  bad_values: ['Values out of range', 'Every value of the row failed its check (for example a negative distance).'],
};
const reasonWords = (c) => REASONS[c] || [c.replace(/[_-]/g, ' '), 'Not one of the usual reasons; the code is the server\'s own.'];
const FAILURES = {
  not_a_backup: 'The file is not a TeslaMate backup. Make one with TeslaMate\'s database backup (a dump of its database), as it is or compressed with gzip or xz, and upload that file.',
  mixed_cars: 'The file holds rows that point at cars it does not describe. Make a fresh, whole backup and try again.',
  no_car: 'The file holds no car. A backup made before TeslaMate saw a car has nothing to import.',
  truncated: 'The file ends part way through: it was cut short when it was copied. Copy it again.',
};
function startWords(e) {
  switch (e?.code) {
    case 'not_ready': return 'The file is not checked yet. Start works once the check is done.';
    case 'cars_unmatched': return 'Some imported cars have no match yet. Pick a car, "Add as a new car" or "Leave out" for each.';
    case 'import_running': return 'Another import is running (perhaps from another tab). Wait for it to end or discard it.';
    case 'no_such_import': return 'This import was discarded, perhaps from another tab. Load the page again.';
    default: return errorWords(e).text;
  }
}
const hex = (buf) => [...new Uint8Array(buf)].map((b) => b.toString(16).padStart(2, '0')).join('');
const sum = (o) => Object.values(o || {}).reduce((a, b) => a + b, 0);
const STEPS = [['choose', 'Choose'], ['uploading', 'Upload'], ['checking', 'Check'], ['ready', 'Match cars'], ['importing', 'Import'], ['done', 'Done']];
const ALL_ITEMS = ['web.import.start', 'web.import.discard', 'web.import.start-error', 'web.import.run-error', 'web.import.files.col-status', 'web.import.restart-note',
  'web.import.rejected-count', 'web.import.rejected.col-reason', 'web.import.docs-overlap', 'web.import.docs-backfill', 'faq.no-backfill'];

export async function mount(ctx) {
  const tz = ctx.tz;
  const trail = h('nav', { class: 'im-trail', 'aria-label': 'Where this page is', cover: 'web.import.breadcrumb' },
    h('a', { href: '/' }, 'All cars'), icon('chev-r', 'i--sm'), h('a', { href: state.buildUrl('settings-server') }, 'Set-up'), icon('chev-r', 'i--sm'), h('span', { 'aria-current': 'page' }, 'Import'));
  const rec = recLamp({ idle: 'Deck ready' });
  const head = plate({ kicker: trail, title: 'Import TeslaMate history', aim: 'Brings in the history TeslaMate recorded before the CAN Bridge, from TeslaMate\'s own backup file. The whole file is checked before anything is stored, and running it again stores nothing twice.', tools: rec.el });

  if (!ctx.admin) {
    replace(ctx.view, h('div', { class: 'page im' }, head, hud({ title: 'Import', icon: 'lock', tone: 'orange' },
      emptyState({ icon: 'lock', title: 'Only an admin can import', text: 'Importing adds history to cars, so it needs an admin account. Ask an admin to upload the TeslaMate backup; the history then shows on every page, marked as imported.' }),
      h('p', { class: 'note', cover: ALL_ITEMS }, icon('info', 'i--sm'), h('span', null, 'An admin sees the upload, its progress, the report and the overlap with CAN history here.')))));
    ctx.ready();
    return;
  }

  const S = { jobs: [], cars: [], job: null, match: [], overlaps: [], overlapErr: null, report: null };
  const up = { file: null, active: false, paused: false, error: null, startedAt: 0, sent0: 0 };
  const loadJobs = async () => { S.jobs = await ctx.api.list('imports', { limit: 100 }).all({ max: 1000 }); };
  try {
    const [, cars] = await Promise.all([loadJobs(), ctx.api.list('cars', { limit: 500 }).all({ max: 10000 })]);
    S.cars = cars;
  } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;
  const carName = (cid) => S.cars.find((c) => c.id === cid)?.name || cid;
  const pick = () => {
    const want = ctx.query?.import;
    S.job = (want && S.jobs.find((j) => j.id === want)) || S.jobs.find((j) => UNFINISHED.includes(j.status)) || S.jobs[0] || null;
  };
  pick();

  /* ---------- the deck: steps, the file slot, Start and Discard */
  const stepsSlot = h('div');
  function paintSteps() {
    const j = S.job;
    const at = !j ? 0 : j.status === 'failed' ? (j.checked_ms ? 4 : 2) : STEPS.findIndex(([k]) => k === j.status);
    replace(stepsSlot, lampSteps(STEPS, at, { failed: j?.status === 'failed', done: j?.status === 'done', label: 'Steps of an import' }));
  }
  const fileInput = h('input', { class: 'im-file', type: 'file', accept: '.sql,.gz,.xz,.dump,.bak,.txt,application/sql,application/gzip,application/x-xz', 'aria-label': 'TeslaMate backup file',
    onchange: () => { const f = fileInput.files?.[0]; fileInput.value = ''; if (f) chose(f); } });
  const slot = h('label', { class: 'im-slot' }, fileInput,
    h('span', { class: 'im-slot-ic' }, icon('upload', 'i--lg')),
    h('span', { class: 'im-slot-text' }, h('span', { class: 'im-slot-main' }, 'Choose the backup file, or drop it here'),
      h('span', { class: 'im-slot-sub' }, 'The .sql file of a TeslaMate database backup, as it is or compressed (.gz, .xz). Up to 64 GB.')));
  ctx.on(slot, 'dragover', (e) => { e.preventDefault(); slot.classList.add('is-over'); });
  ctx.on(slot, 'dragleave', () => slot.classList.remove('is-over'));
  ctx.on(slot, 'drop', (e) => { e.preventDefault(); slot.classList.remove('is-over'); const f = e.dataTransfer?.files?.[0]; if (f) chose(f); });
  const upErr = h('p', { class: 'field-error', hidden: true, role: 'alert' });
  const sayUp = (t) => { upErr.hidden = !t; if (t) replace(upErr, icon('warn', 'i--sm'), h('span', null, t)); };

  async function chose(file) {
    sayUp(null);
    const j = S.job;
    if (j && j.status === 'uploading' && !up.active) {
      if (file.size !== j.bytes_total || file.name !== j.name) { sayUp(`That is not the file this upload began with (${j.name}, ${bytes(j.bytes_total)}). Choose that file to carry on, or discard this upload to start another.`); return; }
      up.file = file; upload(j); return;
    }
    if (j && UNFINISHED.includes(j.status)) { sayUp('An import is not finished yet. Wait for it, or discard it, before you upload another file.'); return; }
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
      sayUp(e?.code === 'import_running' ? 'Another import is not finished yet (perhaps from another tab). Wait for it, or discard it first.' : errText(e));
    }
  }

  async function upload(job) {
    if (up.active || !up.file) return;
    Object.assign(up, { active: true, paused: false, error: null, startedAt: Date.now(), sent0: job.uploaded_bytes || 0 });
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
        paintUploadBar();
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
    if (up.error) sayUp(`The upload stopped at ${bytes(off)}: ${errorWords(up.error).text} Choose the file again to carry on from there.`);
    if (off >= file.size) up.file = null;
    await refreshJob();
  }

  const upBarSlot = h('div', { class: 'im-up' });
  function paintUploadBar() {
    const j = S.job;
    if (!j) return;
    const done = j.uploaded_bytes ?? 0;
    const secs = (Date.now() - up.startedAt) / 1000;
    const rate = up.active && secs > 1 ? (done - up.sent0) / secs : null;
    replace(upBarSlot, bar(share(done, j.bytes_total), { label: 'Uploaded' }),
      h('p', { class: 'im-bar-text' }, h('b', null, bytes(done)), ` of ${bytes(j.bytes_total)} uploaded (${pctText(done, j.bytes_total)})`, rate ? ` · ${bytes(rate)} a second · about ${dur(((j.bytes_total - done) / rate) * 1000)} to go` : null));
  }

  const startErr = h('p', { class: 'field-error', hidden: true, role: 'alert', cover: 'web.import.start-error' });
  const startBtn = btn('Start import', () => start(), { kind: 'go', icon: 'arrow-r', cover: 'web.import.start' });
  const discardBtn = btn('Discard', () => discard(), { kind: 'ghost', icon: 'x', cover: 'web.import.discard' });
  const why = h('span', { class: 'chan-hint im-why' });
  async function start() {
    const j = S.job;
    startErr.hidden = true;
    if (!j) return;
    startBtn.disabled = true;
    try {
      const r = await ctx.api.post(`imports/${enc(j.id)}/start`, {});
      if (!ctx.alive()) return;
      setJob(r);
      rec.light('Importing');
      toast('Import started. You can leave this page; it goes on without it.');
    } catch (e) {
      if (!ctx.alive()) return;
      replace(startErr, icon('warn', 'i--sm'), h('span', null, startWords(e)));
      startErr.hidden = false;
      await refreshJob();
      return;
    }
    paintAll();
  }
  async function discard() {
    const j = S.job;
    if (!j) return;
    const open = UNFINISHED.includes(j.status);
    if (!(await confirmDialog(j.status === 'done' ? `Remove ${j.name} from the list of imports? The history it brought stays; only this record and its report go.`
      : j.status === 'importing' ? `Stop and discard ${j.name}? Rows stored so far stay (running the same file again skips them); the rest is not imported.`
        : `Discard ${j.name}? Nothing from it has been stored, so your history does not change. You can upload it again later.`,
    { title: open ? 'Discard this import?' : 'Remove this import?', ok: open ? 'Discard' : 'Remove', danger: true })) || !ctx.alive()) return;
    up.paused = true; up.file = null;
    try { await ctx.api.del(`imports/${enc(j.id)}`); if (!ctx.alive()) return; toast(open ? 'Import discarded.' : 'Import removed from the list.'); } catch (e) {
      if (!ctx.alive()) return;
      if (e?.code !== 'no_such_import') { toast(`Could not discard: ${errorWords(e).text}`, { kind: 'error' }); return; }
      toast('This import was discarded already.', { kind: 'info' });
    }
    S.jobs = S.jobs.filter((x) => x.id !== j.id);
    S.job = null; S.match = [];
    ctx.setQuery({ import: null });
    pick();
    await loadMatch();
    paintAll();
  }

  const deckSlot = h('div', { class: 'stack' });
  const failSlot = h('div', { role: 'alert', cover: 'web.import.run-error' });
  function paintDeck() {
    const j = S.job;
    const open = j && UNFINISHED.includes(j.status);
    const parts = [];
    if (!j || !open) parts.push(slot);
    if (j?.status === 'uploading') {
      paintUploadBar();
      parts.push(h('div', { class: 'im-tape' },
        h('p', { class: 'im-tape-name' }, icon('book', 'i--sm'), code(j.name, { long: true }), h('span', { class: 'faint small' }, bytes(j.bytes_total))),
        upBarSlot,
        up.active
          ? h('div', { class: 'row' }, btn('Pause', () => { up.paused = true; }, { sm: true, icon: 'x' }), h('span', { class: 'chan-hint' }, 'Sent over HTTPS in chunks of about 1 MB, each with its SHA-256: a chunk that arrives damaged is sent again.'))
          : h('div', { class: 'stack' }, h('p', { class: 'small' }, up.paused ? 'Paused. ' : 'This upload stopped (the page was closed or the connection broke). ', `The server has the first ${bytes(j.uploaded_bytes)}. Choose the same file again to carry on from there.`),
            up.file && up.paused ? btn('Carry on', () => upload(S.job), { kind: 'go', sm: true, icon: 'arrow-r' }) : slot)));
    }
    parts.push(upErr);
    if (j?.status === 'failed') {
      const c = j.error?.code;
      replace(failSlot, callout('warn', j.checked_ms ? 'The import failed' : 'The check found a problem', FAILURES[c] || j.error?.message || 'The server gave no reason.',
        FAILURES[c] && j.error?.message ? ` The server says: ${j.error.message}` : '', ' Nothing from this file was stored. Discard it, then upload a good file.'));
      failSlot.hidden = false;
    } else { replace(failSlot); failSlot.hidden = true; }
    parts.push(failSlot);
    replace(deckSlot, parts);
    const unmatched = S.match.filter((c) => c.match == null).length;
    startBtn.disabled = !(j?.status === 'ready' && !unmatched);
    replace(why, !j ? 'Choose a file first.'
      : j.status === 'ready' ? (unmatched ? `Match ${unmatched === 1 ? 'the last imported car' : `${unmatched} imported cars`} below first.` : 'Checked and matched: ready to start.')
        : j.status === 'uploading' || j.status === 'checking' ? 'Possible once the whole file is uploaded and checked.'
          : j.status === 'importing' ? 'Running now.' : j.status === 'done' ? 'This import is done. Upload a file to import again.' : 'This import failed: discard it and upload a good file.');
    discardBtn.disabled = !j;
    replace(discardBtn, icon('x'), h('span', null, j && !open ? 'Remove from list' : 'Discard'));
  }

  /* ---------- the three passes */
  const passSlot = h('div', { class: 'stack', cover: 'web.import.files.col-status' });
  const restartNote = () => h('p', { class: 'note', cover: 'web.import.restart-note' }, icon('info', 'i--sm'), h('span', null,
    'Safe to leave: close this page or the browser while it runs. If the server restarts, an upload carries on from the last chunk it has once you choose the file again; a check starts over (nothing was stored); an import runs again from the file, and rows already in are known by their TeslaMate id, so nothing is stored twice.'));
  function paintPasses() {
    const j = S.job;
    if (!j) { replace(passSlot, emptyState({ icon: 'upload', title: 'No import yet', text: 'Choose a TeslaMate backup file. Its upload, check and import show here as they run.' }), restartNote()); return; }
    const s = j.status, failedAt = s === 'failed' ? (j.checked_ms ? 'import' : 'check') : null;
    const p1 = s === 'uploading' ? 'now' : 'done';
    const p2 = s === 'uploading' ? 'wait' : s === 'checking' ? 'now' : failedAt === 'check' ? 'bad' : 'done';
    const p3 = ['uploading', 'checking', 'ready'].includes(s) ? 'wait' : s === 'importing' ? 'now' : s === 'done' ? 'done' : failedAt === 'import' ? 'bad' : 'skip';
    const SIG = { wait: ['off', 'Waiting'], now: ['live', 'Running'], done: ['ok', 'Done'], bad: ['bad', 'Failed'], skip: ['off', 'Not run'] };
    const read = (st) => (st === 'now' ? `${bytes(j.bytes_read)} of ${bytes(j.bytes_total)} read (${pctText(j.bytes_read, j.bytes_total)})` : null);
    const pass = (n, name, what, st, detail, frac) => h('li', { class: ['im-pass', `im-pass--${st}`] },
      h('span', { class: 'im-pass-n' }, String(n)),
      h('div', { class: 'im-pass-body' },
        h('div', { class: 'im-pass-head' }, h('b', null, name), sig(...SIG[st])),
        h('p', { class: 'im-pass-what' }, what),
        frac != null ? bar(frac, { label: name }) : null,
        detail ? h('p', { class: 'im-pass-detail' }, detail) : null));
    const st = STATUS[s] || [s, 'off', ''];
    replace(passSlot,
      h('div', { class: 'im-status' }, sig(st[1], st[0]), h('p', { class: 'small' }, st[2])),
      kv([['File', code(j.name, { long: true })], ['Size', `${bytes(j.bytes_total)}${j.compression && j.compression !== 'plain' ? ` (${j.compression})` : ''}`],
        ['Started', dateTime(j.started_ms, tz)], ['Checked', j.checked_ms ? dateTime(j.checked_ms, tz) : DASH],
        ['Finished', j.finished_ms ? `${dateTime(j.finished_ms, tz)} · took ${dur(j.finished_ms - j.started_ms)}` : DASH]]),
      h('ol', { class: 'im-passes', 'aria-label': 'State of each pass' },
        pass(1, 'Upload', 'The file goes to the server in chunks.', p1, p1 === 'done' ? `${bytes(j.bytes_total)} uploaded` : null, p1 === 'now' ? share(j.uploaded_bytes, j.bytes_total) : null),
        pass(2, 'Check', 'Every row is read and checked; nothing is stored.', p2, read(p2) || (p2 === 'done' && j.checked_ms ? `Checked ${dateTime(j.checked_ms, tz)}` : null), p2 === 'now' ? share(j.bytes_read, j.bytes_total) : null),
        pass(3, 'Import', 'The rows are stored and the timeline is built again for the matched cars.', p3, read(p3) || (p3 === 'done' ? `Done ${dateTime(j.finished_ms, tz)}` : p3 === 'skip' ? 'Not run: the check failed.' : null), p3 === 'now' ? share(j.bytes_read, j.bytes_total) : null)),
      restartNote());
  }

  /* ---------- matching the cars */
  async function loadMatch() {
    S.match = [];
    const j = S.job;
    if (!j || !['checking', 'ready'].includes(j.status)) return;
    try { S.match = (await ctx.api.get(`imports/${enc(j.id)}/cars`)).items || []; } catch { /* not checked yet */ }
  }
  async function setMatch(c, value) {
    try {
      const r = await ctx.api.put(`imports/${enc(S.job.id)}/cars`, { items: [{ tm_car: c.tm_car, match: value }] });
      if (!ctx.alive()) return;
      S.match = r.items || S.match;
      rec.light('Car matched');
      toast(value === 'new' ? `${c.name} will be added as a new car.` : value === 'leave_out' ? `${c.name} will be left out.` : `${c.name} goes to ${carName(value)}.`);
    } catch (e) { if (ctx.alive()) toast(errText(e), { kind: 'error' }); }
    paintMatch(); paintDeck();
  }
  const matchList = tracks(ctx, {
    title: 'Match the imported cars', icon: 'car', tone: 'orange', rows: [], label: 'Cars in the backup and where their history goes', csvName: 'import-cars',
    note: 'Each car in the backup goes to a car here, becomes a new car, or is left out. A car here with the same VIN start (the same for every car of a model and factory year) is chosen at first. One car here takes at most one imported car.',
    columns: [
      { key: 'name', label: 'In the backup', lead: true, sort: true, render: (c) => h('span', { class: 'flagline' }, h('b', null, c.name), h('span', { class: 'mono faint' }, `#${c.tm_car}`)), csv: (c) => c.name },
      { key: 'vin_prefix', label: 'VIN start', sort: true, render: (c) => code(c.vin_prefix) },
      { key: 'drives', label: 'Drives', num: true, sort: true, render: (c) => num(c.drives) },
      { key: 'charges', label: 'Charges', num: true, sort: true, render: (c) => num(c.charges) },
      { key: 'first_ms', label: 'History', sort: true, render: (c) => `${date(c.first_ms, tz, { year: true })} to ${date(c.last_ms, tz, { year: true })}`, csv: (c) => `${toIsoZoned(c.first_ms, tz)} to ${toIsoZoned(c.last_ms, tz)}` },
      { key: 'match', label: 'Goes to', csv: (c) => (c.match === 'new' ? 'a new car' : c.match === 'leave_out' ? 'left out' : c.match ? carName(c.match) : 'not chosen'), render: (c) => {
        const taken = new Set(S.match.filter((x) => x.tm_car !== c.tm_car && x.match && x.match !== 'new' && x.match !== 'leave_out').map((x) => x.match));
        const sel = h('select', { class: 'input im-select', 'aria-label': `Where ${c.name}'s history goes`, disabled: !['ready', 'checking'].includes(S.job?.status), onchange: () => setMatch(c, sel.value) },
          c.match == null ? h('option', { value: '', selected: true, disabled: true }, 'Choose…') : null,
          S.cars.map((x) => h('option', { value: x.id, selected: c.match === x.id, disabled: taken.has(x.id) }, `${x.name}${x.id === c.suggested ? ' (same VIN start)' : ''}${taken.has(x.id) ? ' (taken)' : ''}`)),
          h('option', { value: 'new', selected: c.match === 'new' }, 'Add as a new car'),
          h('option', { value: 'leave_out', selected: c.match === 'leave_out' }, 'Leave out'));
        return h('div', { class: 'flagline im-goes' }, sel, c.suggested && c.match === c.suggested ? tag('VIN match', 'grn') : null, c.match == null ? tag('Not chosen', 'org') : null);
      } },
    ],
    empty: () => emptyState({ icon: 'car', title: 'No cars to match yet', text: 'The cars in the file are known once the whole file is uploaded.' }),
  });
  const matchWrap = h('div', null, matchList.el);
  function paintMatch() { const show = S.job && ['checking', 'ready'].includes(S.job.status); matchWrap.hidden = !show; if (show) matchList.setRows(S.match); }

  /* ---------- the report */
  const reportSlot = h('div', { class: 'stack' });
  const tablesList = tracks(ctx, {
    title: 'Rows per table', icon: 'book', rows: [], csvName: 'import-report-tables', label: 'Rows read, imported and skipped per table',
    note: 'Counts only: the report never keeps row values or positions.', sort: { key: 'order', dir: 'asc' },
    columns: [
      { key: 'order', label: 'Table', lead: true, sort: true, value: (r) => r.order, render: (r) => h('span', { class: 'cell' }, h('b', null, tableName(r.name)), h('span', { class: 'cell-sub' }, TABLES[r.name]?.[1] || '')), csv: (r) => r.name },
      { key: 'read', label: 'Read', num: true, sort: true, render: (r) => num(r.read) },
      { key: 'imported', label: 'Imported', num: true, sort: true, render: (r) => (S.report?.imported ? num(r.imported) : h('span', { class: 'faint', title: 'From the check: what the import would store' }, `${num(r.would)} to import`)), csv: (r) => (S.report?.imported ? r.imported : r.would) },
      { key: 'skipped', label: 'Skipped', num: true, sort: true, render: (r) => (r.skipped ? h('span', { class: 'warn' }, num(r.skipped)) : h('span', { class: 'faint' }, '0')) },
      { key: 'bad', label: 'Values cleared', render: (r) => (r.bad.length ? h('span', { class: 'flagline' }, r.bad.map(([col, n]) => h('span', { class: 'im-chip' }, code(col), ` ${num(n)}`))) : h('span', { class: 'faint' }, 'none')), csv: (r) => r.bad.map(([c, n]) => `${c}:${n}`).join('; ') },
    ],
    empty: () => emptyState({ icon: 'book', title: 'No report yet', text: 'The report shows once the whole file has been checked.' }),
  });
  const reasonsList = tracks(ctx, {
    title: 'Why rows were skipped', icon: 'warn', tone: 'orange', rows: [], csvName: 'import-report-skipped', label: 'Skipped rows by reason', cover: 'web.import.rejected.col-reason', sort: { key: 'n', dir: 'desc' },
    columns: [
      { key: 'table', label: 'Table', sort: true, render: (r) => tableName(r.table), csv: (r) => r.table },
      { key: 'reason', label: 'Reason', lead: true, sort: true, render: (r) => h('span', { class: 'cell' }, h('b', null, reasonWords(r.reason)[0]), h('span', { class: 'cell-sub' }, reasonWords(r.reason)[1])), csv: (r) => reasonWords(r.reason)[0] },
      { key: 'code', label: 'Code', render: (r) => code(r.reason), csv: (r) => r.reason },
      { key: 'n', label: 'Rows', num: true, sort: true, render: (r) => num(r.n) },
    ],
    empty: () => emptyState({ icon: 'check', title: 'No row was skipped', text: 'Every row of the file passed its checks.' }),
  });
  const goesTo = (m) => (m === 'new' ? 'a new car' : m === 'leave_out' ? 'left out' : m ? carName(m) : 'not matched yet');
  const carsList = tracks(ctx, {
    title: 'Rows per car', icon: 'car', rows: [], csvName: 'import-report-cars', label: 'Rows per imported car', cover: 'web.import.rejected-count',
    note: 'Each car\'s rows over every table. Places belong to no car, so they count only in Rows per table.', sort: { key: 'tm_car', dir: 'asc' },
    columns: [
      { key: 'tm_car', label: 'In the backup', lead: true, sort: true, render: (c) => h('span', { class: 'flagline' }, h('b', null, c.name || `Car ${c.tm_car}`), h('span', { class: 'mono faint' }, `#${c.tm_car}`)), csv: (c) => c.name || c.tm_car },
      { key: 'match', label: 'Went to', sort: true, value: (c) => goesTo(c.match), render: (c) => (c.match && c.match !== 'new' && c.match !== 'leave_out' ? h('a', { class: 'plc', href: state.buildUrl('car', { car: c.match }) }, carName(c.match)) : h('span', { class: c.match ? 'faint' : 'warn' }, goesTo(c.match))), csv: (c) => goesTo(c.match) },
      { key: 'read', label: 'Read', num: true, sort: true, render: (c) => (c.read == null ? h('span', { class: 'faint' }, 'not counted') : num(c.read)) },
      { key: 'imported', label: 'Imported', num: true, sort: true, value: (c) => (c.read == null ? null : S.report?.imported ? c.imported : c.read - sum(c.skipped)),
        render: (c) => (c.read == null ? h('span', { class: 'faint' }, DASH) : S.report?.imported ? num(c.imported) : h('span', { class: 'faint' }, `${num(c.read - sum(c.skipped))} to import`)) },
      { key: 'skipped', label: 'Skipped', num: true, sort: true, value: (c) => (c.skipped ? sum(c.skipped) : null), render: (c) => (c.skipped == null ? h('span', { class: 'faint' }, DASH) : sum(c.skipped) ? h('span', { class: 'warn' }, num(sum(c.skipped))) : h('span', { class: 'faint' }, '0')) },
      { key: 'why', label: 'Why skipped', render: (c) => { const e = Object.entries(c.skipped || {}).filter(([, n]) => n); return e.length ? h('span', { class: 'flagline' }, e.map(([k, n]) => h('span', { class: 'im-chip', title: reasonWords(k)[1] }, `${reasonWords(k)[0]} ${num(n)}`))) : h('span', { class: 'faint' }, c.skipped ? 'none' : DASH); }, csv: (c) => Object.entries(c.skipped || {}).map(([k, n]) => `${reasonWords(k)[0]}: ${n}`).join('; ') },
      { key: 'drives', label: 'Drives', num: true, sort: true, render: (c) => num(c.drives) },
      { key: 'charges', label: 'Charges', num: true, sort: true, render: (c) => num(c.charges) },
    ],
    empty: () => emptyState({ icon: 'car', title: 'No cars in the report', text: 'The report lists each car of the backup once the whole file has been checked.' }),
  });
  const reportLists = h('div', { class: 'stack' }, h('div', { class: 'cols' }, tablesList.el, reasonsList.el), carsList.el);
  function paintReport() {
    const j = S.job, rep = j?.report || null;
    S.report = rep;
    if (!rep) {
      replace(reportSlot, emptyState({ icon: 'book', title: j ? 'No report yet' : 'No import yet', text: j ? 'The report shows once the whole file has been checked (pass 1).' : 'Each import gets a report: rows read, imported and skipped per table, and why.' }));
      reportLists.hidden = true;
      tablesList.setRows([]); reasonsList.setRows([]); carsList.setRows([]);
      return;
    }
    reportLists.hidden = false;
    const names = Object.keys(rep.tables || {});
    const rows = names.map((name, i) => { const t = rep.tables[name]; const sk = sum(t.skipped); return { name, order: i, read: t.read, imported: t.imported, would: t.read - sk, skipped: sk, bad: Object.entries(t.bad_values || {}) }; });
    const reasons = names.flatMap((name) => Object.entries(rep.tables[name].skipped || {}).map(([reason, n]) => ({ table: name, reason, n })));
    const tot = rows.reduce((a, r) => ({ read: a.read + r.read, imp: a.imp + (rep.imported ? r.imported : r.would), sk: a.sk + r.skipped, bad: a.bad + r.bad.reduce((x, [, n]) => x + n, 0) }), { read: 0, imp: 0, sk: 0, bad: 0 });
    replace(reportSlot,
      h('p', { class: 'small' }, rep.imported ? 'What the import stored.' : 'From the check: what the import will store. Nothing is stored yet.'),
      counters([
        { label: 'Rows read', value: tot.read, kind: 'count', how: 'every row of every table used' },
        { label: rep.imported ? 'Imported' : 'To import', value: tot.imp, kind: 'count', how: rep.imported ? 'stored, marked as imported' : 'read minus skipped' },
        { label: 'Skipped', value: tot.sk, kind: 'count', how: tot.sk ? 'each with its reason below' : 'every row passed' },
        { label: 'Values cleared', value: tot.bad, kind: 'count', how: 'out of range, left blank; row kept' },
        { label: 'History', text: rep.span ? dur(rep.span.to_ms - rep.span.from_ms) : DASH, how: rep.span ? `${date(rep.span.from_ms, tz, { year: true })} to ${date(rep.span.to_ms, tz, { year: true })}` : 'no dated rows' },
      ]),
      tot.sk ? callout('warn', `${num(tot.sk)} rows ${rep.imported ? 'were' : 'will be'} skipped.`, 'Skipping a row did not stop the import; every other row is in. A drive or charge built around a skipped row can come out a little short. The report keeps only how many rows were skipped and why, never what they held.') : null);
    tablesList.setRows(rows); reasonsList.setRows(reasons); carsList.setRows(rep.cars || []);
  }

  /* ---------- overlap with CAN history */
  async function loadOverlaps() {
    S.overlapErr = null;
    const lists = await Promise.all(S.cars.map(async (c) => {
      try { return (await ctx.api.list(`cars/${enc(c.id)}/overlaps`, { limit: 200 }).all({ max: 5000 })).map((o) => ({ ...o, car: c.id })); } catch (e) { if (e?.name !== 'AbortError') S.overlapErr = e; return []; }
    }));
    S.overlaps = lists.flat().sort((a, b) => b.start_ms - a.start_ms);
  }
  async function flip(o, shown) {
    if (o.shown === shown) return;
    try {
      const r = await ctx.api.patch(`cars/${enc(o.car)}/overlaps`, { id: o.id, shown });
      if (!ctx.alive()) return;
      const ch = r.changed || { ...o, shown };
      S.overlaps = S.overlaps.map((x) => (x.car === o.car && x.id === o.id ? { ...x, ...ch, car: o.car } : x));
      rec.saved('Shown history', `${carName(o.car)}: ${shown === 'can' ? 'CAN' : 'TeslaMate'} history is shown for that span on every page.`);
    } catch (e) { if (ctx.alive()) toast(`Could not switch: ${errorWords(e).text}`, { kind: 'error' }); }
    overlapList.setRows(S.overlaps);
  }
  const zoneOf = (o) => S.cars.find((c) => c.id === o.car)?.time_zone || tz;
  const side = (x) => (x ? `${num(x.drives)} drives, ${num(x.charges)} charges` : DASH);
  const overlapList = tracks(ctx, {
    title: 'Spans with both histories', icon: 'timeline', rows: [], csvName: 'import-overlaps', label: 'Spans where imported and CAN history overlap',
    note: 'Where TeslaMate and the CAN Bridge both recorded the same time, one side is shown on Drives, Charges, Timeline, Statistics and Travel: CAN at first, since it is measured. Neither side is deleted; switch a span to show the other.',
    sort: { key: 'start_ms', dir: 'desc' },
    columns: [
      { key: 'car', label: 'Car', lead: true, sort: true, value: (o) => carName(o.car), render: (o) => h('b', null, carName(o.car)), csv: (o) => carName(o.car) },
      { key: 'start_ms', label: 'From', sort: true, render: (o) => dateTime(o.start_ms, zoneOf(o)), csv: (o) => toIsoZoned(o.start_ms, zoneOf(o)) },
      { key: 'end_ms', label: 'To', sort: true, render: (o) => dateTime(o.end_ms, zoneOf(o)), csv: (o) => toIsoZoned(o.end_ms, zoneOf(o)) },
      { key: 'len', label: 'Length', num: true, sort: true, value: (o) => o.end_ms - o.start_ms, render: (o) => dur(o.end_ms - o.start_ms), csv: (o) => Math.round((o.end_ms - o.start_ms) / 1000) },
      { key: 'can', label: 'CAN has', render: (o) => side(o.can), csv: (o) => side(o.can) },
      { key: 'teslamate', label: 'TeslaMate has', render: (o) => side(o.teslamate), csv: (o) => side(o.teslamate) },
      { key: 'shown', label: 'Shown', sort: true, csv: (o) => o.shown, render: (o) => h('div', { class: 'seg', role: 'group', 'aria-label': `Which history is shown for ${carName(o.car)} from ${dateTime(o.start_ms, zoneOf(o))}` },
        [['can', 'CAN'], ['teslamate', 'TeslaMate']].map(([v, l]) => h('button', { class: 'seg-btn', type: 'button', 'aria-pressed': String(o.shown === v), onclick: () => flip(o, v) }, l))) },
      { key: 'go', label: '', cls: 'is-act', csv: false, render: (o) => h('a', { class: 'plc nowrap', href: state.buildUrl('timeline', { car: o.car, period: { from: o.start_ms, to: o.end_ms } }) }, 'Timeline') },
    ],
    empty: () => (S.overlapErr ? emptyState({ icon: 'warn', title: 'Overlaps could not be read', text: `${errorWords(S.overlapErr).text} Load the page again to try once more.` })
      : emptyState({ icon: 'check', title: 'No overlap', text: S.cars.length ? 'No span has both imported and CAN history, so every span shows the one history it has. Imported history that ends before the CAN Bridge began never overlaps.' : 'There is no car yet, so nothing can overlap.' })),
  });
  const tlLink = h('span');
  function paintTimelineLink() {
    const done = S.jobs.filter((j) => j.status === 'done').flatMap((j) => (j.report?.cars || []).map((c) => c.match));
    const car = S.cars.find((c) => c.id === S.overlaps[0]?.car) || S.cars.find((c) => done.includes(c.id)) || S.cars[0];
    replace(tlLink, car ? h('a', { class: 'plc', href: state.buildUrl('timeline', { car: car.id }) }, S.cars.length > 1 ? `Timeline of ${car.name}` : 'Timeline') : 'Timeline of each car');
  }

  /* ---------- every import */
  const jobsList = tracks(ctx, {
    title: 'Every import', icon: 'book', rows: [], csvName: 'imports', label: 'Every import, newest first',
    note: 'Pick one to see its progress and report. The history an import brought stays when its record is removed.',
    sort: { key: 'started_ms', dir: 'desc' },
    rowHref: (j) => state.buildUrl('import', { query: { import: j.id } }),
    columns: [
      { key: 'name', label: 'File', lead: true, sort: true, render: (j) => h('span', { class: 'flagline' }, code(j.name, { long: true }), j.id === S.job?.id ? tag('In the deck', 'cyan') : null), csv: (j) => j.name },
      { key: 'status', label: 'State', sort: true, render: (j) => sig(STATUS[j.status]?.[1] || 'off', STATUS[j.status]?.[0] || j.status), csv: (j) => STATUS[j.status]?.[0] || j.status },
      { key: 'bytes_total', label: 'Size', num: true, sort: true, render: (j) => bytes(j.bytes_total) },
      { key: 'started_ms', label: 'Started', sort: true, render: (j) => dateTime(j.started_ms, tz), csv: (j) => toIsoZoned(j.started_ms, tz) },
      { key: 'finished_ms', label: 'Finished', sort: true, render: (j) => (j.finished_ms ? agoEl(j.finished_ms) : h('span', { class: 'faint' }, UNFINISHED.includes(j.status) ? 'not yet' : DASH)), csv: (j) => toIsoZoned(j.finished_ms, tz) },
    ],
    empty: () => emptyState({ icon: 'book', title: 'No imports yet', text: 'Each file you upload is listed here with its state.' }),
  });

  /* ---------- keeping up with the server */
  function setJob(j) { S.jobs = S.jobs.map((x) => (x.id === j.id ? { ...x, ...j } : x)); if (S.job?.id === j.id) S.job = { ...S.job, ...j }; }
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
      if (fresh.status === 'done' && was !== 'done') { await loadJobs(); S.cars = await ctx.api.list('cars', { limit: 500 }).all({ max: 10000 }); pick(); await loadOverlaps(); toast('Import done. The history shows on every page, marked as imported.'); }
      if (fresh.status === 'failed' && was !== 'failed') toast('The import failed; the deck says why.', { kind: 'error' });
    } catch (e) {
      if (ctx.alive() && e?.code === 'no_such_import') { S.jobs = S.jobs.filter((x) => x.id !== j.id); S.job = null; pick(); }
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
    paintSteps(); paintDeck(); paintPasses(); paintMatch(); paintReport();
    jobsList.setRows(S.jobs); overlapList.setRows(S.overlaps); paintTimelineLink();
  }
  try { await Promise.all([loadMatch(), loadOverlaps()]); } catch (e) { ctx.fail(e); return; }
  if (!ctx.alive()) return;

  replace(ctx.view, h('div', { class: 'page im' },
    head,
    band(1, 'The deck'),
    h('div', { class: 'cols im-top' },
      hud({ title: 'Upload', icon: 'upload', tone: 'cyan' },
        h('div', { class: 'stack' }, stepsSlot, deckSlot,
          h('div', { class: 'im-acts' }, startBtn, discardBtn, why), startErr)),
      hud({ title: 'Progress', icon: 'pulse' }, passSlot)),
    matchWrap,
    band(2, 'Report'),
    hud({ title: 'Report', icon: 'stats' }, reportSlot),
    reportLists,
    band(3, 'Overlap with CAN history'),
    hud({ title: 'How the two histories meet', icon: 'timeline' },
      h('ul', { class: 'im-facts' },
        h('li', { cover: 'web.import.docs-overlap' }, h('span', { class: 'im-fact-ic' }, icon('timeline', 'i--sm')), h('div', null, h('b', null, 'Every span is imported, and overlaps are kept. '), 'History is imported for all of its time, also where the CAN Bridge was recording too. Those spans are listed below with the side that is shown, and a switch.')),
        h('li', { cover: 'web.import.docs-backfill' }, h('span', { class: 'im-fact-ic' }, icon('pin', 'i--sm')), h('div', null, h('b', null, 'Addresses and heights come with the backup. '), 'Imported drives have them at once. A stop without an address gets one from the offline address lookup in the minutes after the import, and shows its position until then.')),
        h('li', { cover: 'faq.no-backfill' }, h('span', { class: 'im-fact-ic' }, icon('info', 'i--sm')), h('div', null, h('b', null, 'Time nobody recorded cannot be fetched later. '), 'Tesla keeps no history to download. The CAN Bridge keeps its logs and uploads them when it can, so its own gaps fill themselves; time before it, or a spell when nothing recorded, shows as unknown on the ', tlLink, '. Only an import from another logger, such as TeslaMate, fills it.')))),
    overlapList.el,
    jobsList.el));
  paintAll();
  ctx.ready();
}
