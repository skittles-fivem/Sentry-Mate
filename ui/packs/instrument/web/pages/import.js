// Import (/settings/import): bring history in from TeslaMate's own backup file, or the CAN Bridge
// logs Sentry Six Manager already has (its CAN export bundle, a .zip). An admin
// chooses the file; it goes up in chunks over HTTPS (each with its SHA-256, so it can stop and go on
// from the last chunk the server has), the server checks the whole file before it stores anything,
// the imported cars are matched to cars here (by VIN prefix, a new car, or left out), and then the
// import runs. The report counts rows per table and says why each skipped row was skipped (counts
// only, never row values or positions). Where imported and CAN history cover the same time, the
// overlap is listed with the side that is shown and a switch. A CAN export has no cars to match (its
// logs name their car key); its report counts logs: imported, already here, and refused with why.
//
// Progress: there is no live event for imports, so while a job is being checked or imported (or
// uploaded from another tab) the page reads that one job again every few seconds, and stops as soon
// as the job is ready, done or failed.

import { h, replace, setVars } from '../shared/dom.js';
import { buildUrl } from '../shared/state.js';
import { toIsoZoned } from '../shared/time.js';
import { format } from '../shared/units.js';
import { icon } from '../core/icons.js';
import { pageHead, panel, metric, figures, kv, tag, pill, note } from '../core/page.js';
import { dataTable } from '../core/table.js';
import { emptyState, errorWords } from '../core/states.js';
import { flash } from '../core/flash.js';
import { confirmDialog } from '../core/dialog.js';
import { dateTime, date, dur, num, agoEl, DASH } from '../core/fmt.js';

export const id = 'import';
export const css = true;
export const title = () => 'Import';

const enc = encodeURIComponent;
const bytes = (n) => (n == null ? DASH : format(n, 'bytes'));
const UNFINISHED = ['uploading', 'checking', 'ready', 'importing'];
const CAN = 'sentry_manager_can';
const isCan = (j) => j?.kind === CAN || j?.report?.kind === CAN;

const STATUS = {
  uploading: { label: 'Uploading', tone: 'live', text: 'The file is going up to the server in chunks.' },
  checking: { label: 'Checking', tone: 'live', text: 'Pass 1: the server reads the whole file and checks every row. Nothing is stored yet.' },
  ready: { label: 'Checked, waiting', tone: 'warm', text: 'The whole file was checked. Match the cars below, then start the import.' },
  importing: { label: 'Importing', tone: 'live', text: 'Pass 2: the rows are being stored.' },
  done: { label: 'Done', tone: 'ok', text: 'The history is in. It shows on every page, marked as imported.' },
  failed: { label: 'Failed', tone: 'danger', text: 'The import stopped. Nothing from this file was stored.' },
};
// the same states of a CAN export, where they differ
const CAN_STATUS = {
  checking: 'Pass 1: the server reads every log of the zip and checks its signature, as it checks an upload. Nothing is stored yet.',
  ready: 'The whole zip was checked. Its logs go to the car their car key belongs to; start the import when the report below looks right.',
  importing: 'Pass 2: each log is stored and checked as if the CAN Bridge had uploaded it, then decoded into drives and charges.',
  done: 'The logs are in, as the car\'s own CAN data. Drives, charges and the timeline are built from them as from any upload.',
  failed: 'The import stopped. Logs stored before it stopped stay (they are checked and signed); running the zip again skips them.',
};
const statusText = (j) => (isCan(j) && CAN_STATUS[j.status]) || STATUS[j.status]?.text || '';

// why a log of a CAN export was refused, in words
const CAN_REASONS = {
  unsigned: ['Not signed', 'The log has no valid CAN Bridge signature, so nothing shows it came from the car. It is not stored.'],
  failed_check: ['Changed since it was signed', 'Its bytes, name or length are not the ones the CAN Bridge signed: it was changed or damaged.'],
  signed_by_another_key: ['Another signing key', 'Signed by a key that is not the one this car key is paired with. Trust that key on Settings → Cars first if it is the car\'s new one.'],
  no_sidecar: ['No sidecar', 'The zip has the log without its .log.xz.json beside it, which holds the signature.'],
  bad_sidecar: ['Sidecar unreadable', 'Its sidecar is not JSON, or over 1 MB.'],
  different_file_here: ['Another log of that name', 'A different log with the same name is already here; the one here is kept.'],
  being_uploaded: ['Being uploaded', 'The CAN Bridge is uploading this log now; that upload brings it.'],
  too_large: ['Too large', 'A log can be at most 1 GB.'],
  car_full: ['Car full', 'The car already holds the most logs a car can keep.'],
};
const canReason = (code) => CAN_REASONS[code] || [String(code).replace(/[_-]/g, ' '), 'The server\'s own reason.'];
const KEY_STATUS = {
  car: ['Goes to its car', 'live'],
  new_car: ['A new car', 'warm'],
  only_car: ['The one car here', 'live'],
  new_key: ['New car key', 'warm'],
  waiting: ['Waiting for an admin', 'warm'],
  working: ['Being decoded', 'live'],
};

// TeslaMate's table names, in words
const TABLES = {
  cars: ['Cars', 'One row per car in the backup.'],
  car_settings: ['Car settings', 'Per car: an LFP pack and free Supercharging. TeslaMate\'s polling settings are not kept.'],
  charges: ['Charge points', 'Readings taken while a car charged (power, energy, range).'],
  drives: ['Drives', 'Trips from start to stop.'],
  charging_processes: ['Charges', 'Charging sessions with their energy and cost.'],
  positions: ['Track points', 'Positions recorded along the drives.'],
  updates: ['Software updates', 'Versions the car installed.'],
  geofences: ['Places', 'Named areas; they become places here when no place covers them yet.'],
  addresses: ['Addresses', 'Street addresses of the places the car stopped.'],
  states: ['Car states', 'Online, asleep and offline spells.'],
};
const tableName = (t) => TABLES[t]?.[0] || t.replace(/_/g, ' ');

// why a row was skipped, in words, with what it means for the history
const REASONS = {
  'already-imported': ['Already imported', 'An earlier import stored this row (or left it out for good); it is not stored twice.'],
  open: ['Still going', 'The drive, charge or spell had no end when the backup was made; a later backup brings it.'],
  'unknown-car': ['Car not in the backup', 'The row names a car the backup\'s cars table does not have.'],
  'car-left-out': ['For a car left out', 'The row belongs to an imported car you chose to leave out.'],
  'unknown-charge': ['Charge not in the backup', 'A charge point whose charge is not in the backup, or was not kept.'],
  'bad-value': ['A value cannot be read', 'A value the row needs is missing or unreadable; Values cleared names the column.'],
  'wrong-field-count': ['Wrong number of fields', 'The row has not as many fields as the table has columns.'],
  'too-long': ['Line too long', 'The row is longer than the server reads in one line.'],
  duplicate: ['Already in the file', 'The same row (same TeslaMate id) appears twice; the first copy is kept.'],
  no_energy: ['No energy figures', 'A charge that recorded no energy added; it would show as 0 kWh, so it is left out.'],
  no_position: ['No coordinates', 'A track point without a latitude or longitude; the drive keeps its other points.'],
  bad_time: ['Time cannot be read', 'The row\'s time is missing or not a valid date.'],
  other_car: ['For a car left out', 'The row belongs to an imported car you chose to leave out.'],
  bad_values: ['Values out of range', 'Every value of the row failed its check (for example a negative distance).'],
};
const reasonWords = (code) => REASONS[code] || [code.replace(/[_-]/g, ' '), 'Not one of the usual reasons; the code is the server\'s own.'];

// why a check or an import failed, in words, and what to do
const FAILURES = {
  not_a_backup: 'The file is not a TeslaMate backup. Make one with TeslaMate\'s database backup (a pg_dump of its database), plain or compressed with gzip or xz, and upload that file.',
  mixed_cars: 'The file holds rows that point at cars the file does not describe. Make a fresh, complete backup and try again.',
  no_car: 'The file holds no car. A backup made before TeslaMate saw a car has nothing to import.',
  truncated: 'The file ends part-way through. It was cut short when it was copied; copy it again.',
  not_a_can_export: 'This zip is not a Sentry Six Manager CAN export. In Sentry Six Manager, download the CAN export bundle (Nerd panel, Logs + ids.csv, the car\'s key, From before its first log to now) and upload that .zip as it is. A TeslaMate backup is uploaded as the .sql file itself (or .sql.gz, .sql.xz), not zipped.',
  unreadable: 'The file cannot be read as a zip (it is damaged, cut short, encrypted or packed with a method other than deflate). Download it again and upload it as it is.',
  read_failed: 'The server could not read the uploaded file back. Upload it again.',
  store_failed: 'The server could not store a log (for example the disk is full). Logs stored before it stay; free some space and start again.',
  stopped: 'The import was stopped. Logs stored before it stopped stay; start it again to import the rest.',
};

// a refused Start, in words
function startWords(e) {
  switch (e?.code) {
    case 'not_ready': return 'The file is not checked yet. Start becomes possible once the check is done.';
    case 'cars_unmatched': return 'Some imported cars have no match yet. Choose a car, "Add as a new car" or "Leave out" for each.';
    case 'import_running': return 'Another import is already running (perhaps from another browser tab). Wait for it to end or discard it.';
    case 'no_such_import': return 'This import was discarded, perhaps from another browser tab. Reload the page.';
    default: return errorWords(e).text;
  }
}

const hex = (buf) => [...new Uint8Array(buf)].map((b) => b.toString(16).padStart(2, '0')).join('');
const rowsOf = (o) => Object.values(o || {}).reduce((a, b) => a + b, 0);
const pct = (a, b) => (b > 0 ? Math.max(0, Math.min(100, (a / b) * 100)) : 0);

/* ====================================================================== the page */

export async function mount(ctx) {
  const tz = ctx.tz;
  ctx.setCrumbs([{ label: 'All cars', href: '/' }, { label: 'Settings', href: buildUrl('settings-server') }, { label: 'Import' }]);

  const trail = h('nav', { class: 'im-trail', 'aria-label': 'Where this page is', cover: 'web.import.breadcrumb' },
    h('a', { href: '/' }, 'All cars'), icon('chev-r', 'ico--sm'),
    h('a', { href: buildUrl('settings-server') }, 'Settings'), icon('chev-r', 'ico--sm'),
    h('span', { 'aria-current': 'page' }, 'Import'));
  const head = pageHead({
    eyebrow: trail, title: 'Import history',
    sub: ['Admins only', 'TeslaMate backup (plain, gzip or xz) or Sentry Six Manager CAN export (.zip)', 'times in ' + tz],
    purpose: 'Brings in the history TeslaMate recorded before the CAN Bridge, from TeslaMate\'s own backup file, or the CAN Bridge logs Sentry Six Manager already has, from its CAN export bundle. The whole file is checked before anything is stored, and running it again stores nothing twice.',
  });

  if (!ctx.admin) {
    replace(ctx.view, head, panel({ title: 'Import', icon: 'lock' },
      emptyState({ icon: 'lock', title: 'Only an admin can import', text: 'Importing adds history to cars, so it needs an admin account. Ask an admin to upload the TeslaMate backup; the imported history then shows on every page, marked as imported.' })));
    ctx.ready();
    return;
  }

  const S = { jobs: [], cars: [], job: null, match: [], matchErr: null, overlaps: [] };
  const up = { file: null, active: false, paused: false, error: null, startedAt: 0, sentAt0: 0 };

  async function loadJobs() { S.jobs = await ctx.api.list('imports', { limit: 100 }).all({ max: 1000 }); }
  try {
    const [, cars] = await Promise.all([loadJobs(), ctx.api.list('cars', { limit: 500 }).all({ max: 10000 })]);
    S.cars = cars;
  } catch (e) { if (!ctx.alive()) return; ctx.fail(e); return; }
  if (!ctx.alive()) return;
  const carName = (cid) => S.cars.find((c) => c.id === cid)?.name || cid;

  function pick() {
    const want = ctx.query?.import;
    S.job = (want && S.jobs.find((j) => j.id === want)) || S.jobs.find((j) => UNFINISHED.includes(j.status)) || S.jobs[0] || null;
  }
  pick();

  /* ---------------------------------------------------------------- the steps */
  const STEPS = [['choose', 'Choose file'], ['uploading', 'Upload'], ['checking', 'Check'], ['ready', 'Match cars'], ['importing', 'Import'], ['done', 'Done']];
  function stepIndex(j) {
    if (!j) return 0;
    if (j.status === 'failed') return j.checked_ms ? 4 : 2;
    return STEPS.findIndex(([k]) => k === j.status);
  }
  const stepsEl = h('ol', { class: 'im-steps', 'aria-label': 'Steps of an import' });
  function paintSteps() {
    const j = S.job;
    const at = stepIndex(j);
    replace(stepsEl, STEPS.map(([k, l], i) => {
      const st = j?.status === 'done' || i < at ? 'done' : i === at ? (j?.status === 'failed' ? 'failed' : 'now') : 'later';
      return h('li', { class: ['im-step', `im-step--${st}`], 'aria-current': st === 'now' ? 'step' : null },
        h('span', { class: 'im-step-n' }, st === 'done' ? icon('check', 'ico--sm') : st === 'failed' ? icon('x', 'ico--sm') : String(i + 1)),
        h('span', { class: 'im-step-l' }, k === 'ready' && isCan(j) ? 'Review' : l));
    }));
  }

  /* ---------------------------------------------------------------- upload */
  const fileInput = h('input', { class: 'im-file-input', type: 'file', accept: '.sql,.gz,.xz,.zip,.dump,.bak,.txt,application/sql,application/gzip,application/x-xz,application/zip', 'aria-label': 'TeslaMate backup or Sentry Six Manager CAN export',
    onchange: () => { const f = fileInput.files?.[0]; fileInput.value = ''; if (f) chose(f); } });
  const drop = h('label', { class: 'im-drop' }, fileInput,
    h('span', { class: 'im-drop-ic' }, icon('upload')),
    h('span', { class: 'im-drop-main' }, 'Choose the backup file, or drop it here'),
    h('span', { class: 'im-drop-sub' }, 'The .sql file of a TeslaMate database backup, as it is or compressed (.gz, .xz), or the .zip of Sentry Six Manager\'s CAN export bundle. Up to 64 GB.'));
  ctx.on(drop, 'dragover', (e) => { e.preventDefault(); drop.classList.add('im-drop--over'); });
  ctx.on(drop, 'dragleave', () => drop.classList.remove('im-drop--over'));
  ctx.on(drop, 'drop', (e) => { e.preventDefault(); drop.classList.remove('im-drop--over'); const f = e.dataTransfer?.files?.[0]; if (f) chose(f); });

  const upErr = h('p', { class: 'form-error im-err', hidden: true, role: 'alert' });
  function showUpErr(text) { if (text) replace(upErr, icon('warn', 'ico--sm'), text); upErr.hidden = !text; }

  async function chose(file) {
    showUpErr(null);
    const j = S.job;
    if (j && j.status === 'uploading' && !up.active) {
      // go on with an upload that stopped: it must be the same file
      if (file.size !== j.bytes_total || file.name !== j.name) {
        showUpErr(`That is not the file this upload was started with (${j.name}, ${bytes(j.bytes_total)}). Choose that file to go on, or discard this upload to start with another.`);
        return;
      }
      up.file = file;
      upload(j);
      return;
    }
    if (j && UNFINISHED.includes(j.status)) { showUpErr('An import is not finished yet. Wait for it, or discard it, before you upload another file.'); return; }
    try {
      const created = await ctx.api.post('imports', { name: file.name, bytes_total: file.size });
      if (!ctx.alive()) return;
      up.file = file;
      S.jobs = [created, ...S.jobs.filter((x) => x.id !== created.id)];
      S.job = created;
      ctx.setQuery({ import: created.id });
      paintAll();
      upload(created);
    } catch (e) {
      if (!ctx.alive()) return;
      showUpErr(e?.code === 'import_running' ? 'Another import is not finished yet (perhaps from another browser tab). Wait for it, or discard it first.'
        : e?.fields ? Object.values(e.fields).join(' ') : errorWords(e).text);
    }
  }

  async function upload(job) {
    if (up.active || !up.file) return;
    up.active = true; up.paused = false; up.error = null;
    up.startedAt = Date.now(); up.sentAt0 = job.uploaded_bytes || 0;
    paintAll();
    const file = up.file;
    const max = job.chunk_max_bytes || S.chunkMax || (1024 * 1024 - 1024);
    S.chunkMax = max;
    let off = job.uploaded_bytes || 0, tries = 0;
    while (off < file.size && ctx.alive() && !up.paused) {
      try {
        const buf = await file.slice(off, Math.min(file.size, off + max)).arrayBuffer();
        const sum = hex(await crypto.subtle.digest('SHA-256', buf));
        const r = await ctx.api.patch(`imports/${enc(job.id)}`, buf, { headers: {
          'Content-Type': 'application/offset+octet-stream', 'Upload-Offset': String(off), 'Upload-Length': String(file.size), 'Upload-SHA256': sum } });
        if (!ctx.alive()) return;
        tries = 0;
        off = r.offset ?? r.uploaded_bytes;
        setJob(r);
        paintUploadProgress();
      } catch (e) {
        if (!ctx.alive()) return;
        if ((e?.code === 'wrong_offset' || e?.code === 'checksum_mismatch' || e?.code === 'network' || e?.code === 'timeout') && tries < 3) {
          tries++;
          try { const fresh = await ctx.api.get(`imports/${enc(job.id)}`); if (!ctx.alive()) return; setJob(fresh); off = fresh.uploaded_bytes; if (fresh.status !== 'uploading') break; } catch { /* try the chunk again */ }
          continue;
        }
        up.error = e?.code === 'not_uploading' ? null : e;
        break;
      }
    }
    up.active = false;
    if (up.error) showUpErr(`The upload stopped at ${bytes(off)}: ${errorWords(up.error).text} Choose the file again to go on from there.`);
    if (off >= file.size) up.file = null;
    await refreshJob();
  }

  const upBox = h('div', { class: 'im-up' });
  const upBar = h('div', { class: 'bar im-bar' }, h('i'));
  const upText = h('p', { class: 'im-bar-text' });
  function paintUploadProgress() {
    const j = S.job;
    if (!j) return;
    const done = j.uploaded_bytes ?? 0;
    setVars(upBar, { '--w': `${pct(done, j.bytes_total).toFixed(1)}%` });
    const secs = (Date.now() - up.startedAt) / 1000;
    const rate = up.active && secs > 1 ? (done - up.sentAt0) / secs : null;
    replace(upText, h('b', null, bytes(done)), ` of ${bytes(j.bytes_total)} uploaded (${num(pct(done, j.bytes_total), 0)} %)`,
      rate ? ` · ${bytes(rate)} per second · about ${dur(((j.bytes_total - done) / rate) * 1000)} to go` : null);
  }

  // Start and Discard are always in the head of the Upload panel; each says why when it cannot be used
  const startErr = h('p', { class: 'form-error im-err', hidden: true, role: 'alert', cover: 'web.import.start-error' });
  const startBtn = h('button', { class: 'btn btn--primary', type: 'button', cover: 'web.import.start', onclick: () => start() }, icon('arrow-r', 'ico--sm'), 'Start import');
  const startWhy = h('span', { class: 'setting-note im-why' });
  const discardBtn = h('button', { class: 'btn btn--ghost', type: 'button', cover: 'web.import.discard', onclick: () => discard() }, icon('x', 'ico--sm'), 'Discard');

  async function start() {
    const j = S.job;
    replace(startErr); startErr.hidden = true;
    if (!j) return;
    startBtn.disabled = true;
    try {
      const r = await ctx.api.post(`imports/${enc(j.id)}/start`, {});
      if (!ctx.alive()) return;
      setJob(r);
      flash('Import started. You can leave this page; it goes on without it.');
    } catch (e) {
      if (!ctx.alive()) return;
      replace(startErr, icon('warn', 'ico--sm'), startWords(e));
      startErr.hidden = false;
      await refreshJob();
      return;
    }
    paintAll();
  }

  async function discard() {
    const j = S.job;
    if (!j) return;
    const unfinished = UNFINISHED.includes(j.status);
    const ok = await confirmDialog(
      j.status === 'done' ? `Remove ${j.name} from the list of imports? The history it brought in stays; only this record and its report go.`
        : j.status === 'importing' ? `Stop and discard ${j.name}? Rows stored so far stay (running the same file again later skips them); the rest is not imported.`
          : `Discard ${j.name}? Nothing from it has been stored, so nothing changes in your history. You can upload it again later.`,
      { title: unfinished ? 'Discard this import?' : 'Remove this import?', ok: unfinished ? 'Discard' : 'Remove', danger: true });
    if (!ok || !ctx.alive()) return;
    up.paused = true; up.file = null;
    try {
      await ctx.api.del(`imports/${enc(j.id)}`);
      if (!ctx.alive()) return;
      flash(unfinished ? 'Import discarded.' : 'Import removed from the list.');
    } catch (e) {
      if (!ctx.alive()) return;
      if (e?.code !== 'no_such_import') { flash(`Could not discard: ${errorWords(e).text}`, { kind: 'error' }); return; }
      flash('This import was already discarded.', { kind: 'info' });
    }
    S.jobs = S.jobs.filter((x) => x.id !== j.id);
    S.job = null; S.match = [];
    ctx.setQuery({ import: null });
    pick();
    await loadMatch();
    paintAll();
  }

  const failBox = h('div', { class: 'im-fail', hidden: true, role: 'alert', cover: 'web.import.run-error' });
  function paintUpload() {
    const j = S.job;
    const unfinished = j && UNFINISHED.includes(j.status);
    const parts = [];
    if (!j || !unfinished) parts.push(drop);
    if (j && j.status === 'uploading') {
      paintUploadProgress();
      parts.push(h('div', { class: 'im-upload-now' },
        h('p', { class: 'im-file' }, icon('logs', 'ico--sm'), h('b', { class: 'mono' }, j.name), ` · ${bytes(j.bytes_total)}`),
        upBar, upText,
        up.active
          ? h('div', { class: 'row' }, h('button', { class: 'btn btn--sm', type: 'button', onclick: () => { up.paused = true; } }, 'Pause'),
            h('span', { class: 'setting-note' }, 'Sent over HTTPS in chunks of about 1 MB, each with its SHA-256, so a chunk that arrives damaged is sent again.'))
          : h('div', { class: 'im-resume' },
            h('p', null, up.paused ? 'Paused. ' : 'This upload stopped (the page was closed or the connection broke). ',
              `The server has the first ${bytes(j.uploaded_bytes)}. Choose the same file again to go on from there.`),
            up.file && up.paused ? h('button', { class: 'btn btn--sm btn--primary', type: 'button', onclick: () => upload(S.job) }, 'Go on') : drop)));
    }
    parts.push(upErr);
    // what failed, in words
    if (j && j.status === 'failed') {
      const code = j.error?.code;
      replace(failBox, h('p', { class: 'im-fail-head' }, icon('warn', 'ico--sm'), j.checked_ms ? 'The import failed' : 'The check found a problem'),
        h('p', null, FAILURES[code] || j.error?.message || 'The server gave no reason.'),
        FAILURES[code] && j.error?.message ? h('p', { class: 'setting-note' }, 'The server says: ', j.error.message) : null,
        h('p', { class: 'setting-note' }, isCan(j) && j.checked_ms ? 'Logs stored before it stopped stay, checked and signed. Start a new import of the same zip to bring the rest: what is here is skipped.' : 'Nothing from this file was stored. Discard it, then upload a good file.'));
      failBox.hidden = false;
    } else { failBox.hidden = true; replace(failBox); }
    parts.push(failBox);
    replace(upBox, parts);

    // Start: possible only when checked and every car has a match
    const unmatched = S.match.filter((c) => c.match == null).length;
    const canStart = j?.status === 'ready' && !unmatched;
    startBtn.disabled = !canStart;
    replace(startWhy, !j ? 'Choose a file first.'
      : j.status === 'ready' ? (isCan(j) ? 'Checked: ready to start. Each log goes to the car of its car key.' : unmatched ? `Match ${unmatched === 1 ? 'the last imported car' : `${unmatched} imported cars`} below first.` : 'Checked and matched: ready to start.')
        : j.status === 'uploading' || j.status === 'checking' ? 'Possible once the whole file is uploaded and checked.'
          : j.status === 'importing' ? 'Running now.' : j.status === 'done' ? 'This import is done. Upload a file to import again.'
            : 'This import failed; discard it and upload a good file.');
    discardBtn.disabled = !j;
    replace(discardBtn, icon('x', 'ico--sm'), j && !UNFINISHED.includes(j.status) ? 'Remove from list' : 'Discard');
    discardBtn.title = j ? null : 'There is no import to discard';
  }

  const uploadPanel = panel({ title: 'Upload', icon: 'upload', cls: 'im-upload' },
    stepsEl, upBox,
    h('div', { class: 'im-actions' }, startBtn, discardBtn, startWhy), startErr);

  /* ---------------------------------------------------------------- progress */
  const progBox = h('div', { cover: 'web.import.files.col-status' });
  function stageRow(name, what, st, detail, bar) {
    const P = { waiting: ['Waiting', 'plain'], now: ['In progress', 'live'], done: ['Done', 'ok'], failed: ['Failed', 'danger'], skipped: ['Not run', 'plain'] }[st];
    return h('li', { class: ['im-stage', `im-stage--${st}`] },
      h('div', { class: 'im-stage-head' }, h('b', null, name), h('span', { class: ['im-st', `im-st--${P[1]}`] }, P[0])),
      h('p', { class: 'im-stage-what' }, what),
      bar != null ? h('div', { class: ['bar', 'im-bar', st === 'done' && 'bar--muted'], vars: { '--w': `${bar.toFixed(1)}%` } }, h('i')) : null,
      detail ? h('p', { class: 'im-stage-detail' }, detail) : null);
  }
  // what a restart or a closed page does to an import: shown before one starts too
  const restartNote = () => h('p', { class: 'note im-restart', cover: 'web.import.restart-note' }, icon('info', 'ico--sm'),
    ' Safe to leave: you can close this page or the browser while it runs. If the server restarts, an upload goes on from the last chunk it has once you choose the file again; a check starts over (nothing was stored); an import is run again from the file, and rows that are already in are recognised by their TeslaMate id (a CAN export\'s logs by their name and SHA-256), so nothing is stored twice.');
  function paintProgress() {
    const j = S.job;
    if (!j) {
      replace(progBox, emptyState({ icon: 'upload', title: 'No import yet', text: 'Choose a TeslaMate backup file or a Sentry Six Manager CAN export under Upload. Its upload, check and import show here as they run.' }), restartNote());
      return;
    }
    const s = j.status, failedAt = s === 'failed' ? (j.checked_ms ? 'import' : 'check') : null;
    const up1 = s === 'uploading' ? 'now' : 'done';
    const ck = s === 'uploading' ? 'waiting' : s === 'checking' ? 'now' : failedAt === 'check' ? 'failed' : 'done';
    const im = ['uploading', 'checking', 'ready'].includes(s) ? 'waiting' : s === 'importing' ? 'now' : s === 'done' ? 'done' : failedAt === 'import' ? 'failed' : 'skipped';
    const read = (st) => (st === 'now' ? `${bytes(j.bytes_read)} of ${bytes(j.bytes_total)} read (${num(pct(j.bytes_read, j.bytes_total))} %)` : null);
    const S1 = STATUS[s] || { label: s, tone: 'plain', text: '' };
    replace(progBox,
      h('div', { class: 'im-status' }, pill(S1.label, { tone: S1.tone === 'ok' ? 'live' : S1.tone === 'danger' ? 'warm' : S1.tone, lg: true }), h('p', null, statusText(j))),
      kv([
        ['File', h('span', { class: 'mono im-name' }, j.name), { icon: 'logs' }],
        ['Kind', isCan(j) ? 'Sentry Six Manager CAN export' : j.kind === 'teslamate' ? 'TeslaMate backup' : 'known once checked'],
        ['Size', `${bytes(j.bytes_total)}${j.compression && j.compression !== 'plain' ? ` (${j.compression})` : ''}`],
        ['Started', dateTime(j.started_ms, tz), { icon: 'clock' }],
        ['Checked', j.checked_ms ? dateTime(j.checked_ms, tz) : DASH],
        ['Finished', j.finished_ms ? h('span', null, dateTime(j.finished_ms, tz), ' · took ', dur(j.finished_ms - j.started_ms)) : DASH],
      ]),
      h('ol', { class: 'im-stages', 'aria-label': 'State of each pass' },
        stageRow('Upload', 'The file goes to the server in chunks.', up1, up1 === 'done' ? `${bytes(j.bytes_total)} uploaded` : null, up1 === 'now' ? pct(j.uploaded_bytes, j.bytes_total) : null),
        stageRow('Check (pass 1)', isCan(j) ? 'Every log is read and its signature checked; nothing is stored.' : 'Every row is read and checked; nothing is stored.', ck, read(ck) || (ck === 'done' ? `Checked ${j.checked_ms ? dateTime(j.checked_ms, tz) : ''}` : null), ck === 'now' ? pct(j.bytes_read, j.bytes_total) : null),
        stageRow('Import (pass 2)', isCan(j) ? 'The logs are stored and checked as uploads are, then decoded into drives and charges.' : 'The rows are stored and the timeline is built again for the matched cars.', im, read(im) || (im === 'done' ? `Done ${dateTime(j.finished_ms, tz)}` : im === 'skipped' ? 'Not run: the check failed.' : null), im === 'now' ? pct(j.bytes_read, j.bytes_total) : null)),
      restartNote());
  }
  const progressPanel = panel({ title: 'Progress', icon: 'pulse', side: h('span', { class: 'im-age' }) }, progBox);

  /* ---------------------------------------------------------------- matching the cars */
  async function loadMatch() {
    S.match = []; S.matchErr = null;
    const j = S.job;
    if (!j || !['checking', 'ready'].includes(j.status)) return;
    try { S.match = (await ctx.api.get(`imports/${enc(j.id)}/cars`)).items || []; } catch (e) { if (e?.code !== 'not_checked') S.matchErr = e; }
  }
  async function setMatch(c, value) {
    try {
      const r = await ctx.api.put(`imports/${enc(S.job.id)}/cars`, { items: [{ tm_car: c.tm_car, match: value }] });
      if (!ctx.alive()) return;
      S.match = r.items || S.match;
      flash(value === 'new' ? `${c.name} will be added as a new car.` : value === 'leave_out' ? `${c.name} will be left out.` : `${c.name} goes to ${carName(value)}.`);
    } catch (e) {
      if (!ctx.alive()) return;
      flash(e?.fields ? Object.values(e.fields).join(' ') : errorWords(e).text, { kind: 'error' });
    }
    paintMatch(); paintUpload();
  }
  const matchTable = dataTable(ctx, {
    title: 'Match the imported cars', icon: 'car', rows: [], label: 'Cars in the backup and where their history goes', csvName: 'import-cars',
    note: 'Each car in the backup goes to a car here, becomes a new car, or is left out. A car here with the same VIN prefix (the first 11 characters, the same for every car of a model and factory year) is chosen at first. One car here takes at most one imported car.',
    columns: [
      { key: 'name', label: 'In the backup', sort: true, render: (c) => h('span', { class: 'row im-car' }, h('b', null, c.name), h('span', { class: 'mono faint' }, `#${c.tm_car}`)), csv: (c) => c.name },
      { key: 'vin_prefix', label: 'VIN prefix', sort: true, render: (c) => h('span', { class: 'mono' }, c.vin_prefix) },
      { key: 'drives', label: 'Drives', num: true, sort: true, render: (c) => num(c.drives) },
      { key: 'charges', label: 'Charges', num: true, sort: true, render: (c) => num(c.charges) },
      { key: 'first_ms', label: 'History', sort: true, render: (c) => `${date(c.first_ms, tz)} – ${date(c.last_ms, tz)}`, csv: false },
      { key: 'match', label: 'Goes to', csv: (c) => c.match ?? 'not chosen', render: (c) => {
        const taken = new Set(S.match.filter((x) => x.tm_car !== c.tm_car && x.match && x.match !== 'new' && x.match !== 'leave_out').map((x) => x.match));
        const sel = h('select', { class: 'input select im-select', 'aria-label': `Where ${c.name}'s history goes`, disabled: S.job?.status !== 'ready' && S.job?.status !== 'checking',
          onchange: () => setMatch(c, sel.value) },
        c.match == null ? h('option', { value: '', selected: true, disabled: true }, 'Choose…') : null,
        S.cars.map((x) => h('option', { value: x.id, selected: c.match === x.id, disabled: taken.has(x.id) }, `${x.name}${x.id === c.suggested ? ' (same VIN prefix)' : ''}${taken.has(x.id) ? ' (taken)' : ''}`)),
        h('option', { value: 'new', selected: c.match === 'new' }, 'Add as a new car'),
        h('option', { value: 'leave_out', selected: c.match === 'leave_out' }, 'Leave out'));
        return h('div', { class: 'row im-goes' }, sel,
          c.match && c.match !== 'new' && c.match !== 'leave_out' ? h('a', { class: 'link', href: buildUrl('settings-cars', { car: c.match }) }, 'Car settings') : null,
          c.suggested && c.match === c.suggested ? tag('VIN match', 'live') : null,
          c.match == null ? tag('not chosen', 'warm') : null);
      } },
    ],
    csv: { columns: [
      { label: 'TeslaMate car number', value: (c) => c.tm_car },
      { label: 'In the backup', value: (c) => c.name },
      { label: 'VIN prefix', value: (c) => c.vin_prefix },
      { label: 'Drives (count)', value: (c) => c.drives },
      { label: 'Charges (count)', value: (c) => c.charges },
      { label: `History from (ISO 8601, ${tz})`, value: (c) => toIsoZoned(c.first_ms, tz) },
      { label: `History to (ISO 8601, ${tz})`, value: (c) => toIsoZoned(c.last_ms, tz) },
      { label: 'Goes to', value: (c) => (c.match === 'new' ? 'a new car' : c.match === 'leave_out' ? 'left out' : c.match ? carName(c.match) : 'not chosen') },
      { label: 'Goes to (car id)', value: (c) => c.match ?? '' },
    ] },
    empty: () => emptyState({ icon: 'car', title: 'No cars to match yet', text: 'The cars in the file are known once the whole file is uploaded.' }),
  });
  const matchWrap = h('div', { class: 'im-match' }, matchTable.el);
  function paintMatch() {
    const show = S.job && ['checking', 'ready'].includes(S.job.status) && !isCan(S.job);
    matchWrap.hidden = !show;
    if (show) matchTable.setRows(S.match);
  }

  /* ---------------------------------------------------------------- report */
  const reportBox = h('div', { class: 'im-report' });
  const skipBox = h('div', { class: 'im-skip', role: 'note', cover: 'web.import.rejected-count' });
  const tablesTable = dataTable(ctx, {
    title: 'Rows per table', icon: 'book', rows: [], csvName: 'import-report-tables', label: 'Rows read, imported and skipped per table',
    note: 'Counts only: the report never keeps row values or positions.',
    sort: { key: 'order', dir: 'asc' },
    columns: [
      { key: 'order', label: 'Table', sort: true, value: (r) => r.order, render: (r) => h('span', null, h('b', null, tableName(r.name)), h('span', { class: 'im-tdesc' }, TABLES[r.name]?.[1] || '')), csv: (r) => r.name },
      { key: 'read', label: 'Read', num: true, sort: true, render: (r) => num(r.read) },
      { key: 'imported', label: 'Imported', num: true, sort: true, render: (r) => (S.report?.imported ? num(r.imported) : h('span', { class: 'faint', title: 'From the check: what the import would store' }, num(r.would), ' to import')), csv: (r) => (S.report?.imported ? r.imported : r.would) },
      { key: 'skipped', label: 'Skipped', num: true, sort: true, render: (r) => (r.skipped ? h('span', { class: 'warn' }, num(r.skipped)) : h('span', { class: 'faint' }, '0')) },
      { key: 'bad', label: 'Values cleared', render: (r) => (r.bad.length ? r.bad.map(([col, n]) => h('span', { class: 'im-badval' }, h('span', { class: 'mono' }, col), ` ${num(n)}`)) : h('span', { class: 'faint' }, 'none')),
        csv: (r) => r.bad.map(([c, n]) => `${c}:${n}`).join('; ') },
    ],
    csv: { get columns() {
      const done = !!S.report?.imported;
      return [
        { label: 'Table (TeslaMate name)', value: (r) => r.name },
        { label: 'Table', value: (r) => tableName(r.name) },
        { label: 'Read (rows)', value: (r) => r.read },
        done ? { label: 'Imported (rows)', value: (r) => r.imported } : { label: 'To import (rows, not stored yet)', value: (r) => r.would },
        { label: 'Skipped (rows)', value: (r) => r.skipped },
        { label: 'Values cleared (column: rows)', value: (r) => r.bad.map(([c, n]) => `${c}: ${n}`).join('; ') },
      ];
    } },
    empty: () => emptyState({ icon: 'book', title: 'No report yet', text: 'The report appears once the whole file has been checked.' }),
  });
  const reasonsTable = dataTable(ctx, {
    title: 'Why rows were skipped', icon: 'warn', rows: [], csvName: 'import-report-skipped', label: 'Skipped rows by reason',
    cover: 'web.import.rejected.col-reason', sort: { key: 'n', dir: 'desc' },
    columns: [
      { key: 'table', label: 'Table', sort: true, render: (r) => tableName(r.table), csv: (r) => r.table },
      { key: 'reason', label: 'Reason', sort: true, render: (r) => h('span', null, h('b', null, reasonWords(r.reason)[0]), h('span', { class: 'im-tdesc' }, reasonWords(r.reason)[1])), csv: (r) => reasonWords(r.reason)[0] },
      { key: 'code', label: 'Code', render: (r) => h('span', { class: 'mono faint' }, r.reason), csv: (r) => r.reason },
      { key: 'n', label: 'Rows', num: true, sort: true, render: (r) => num(r.n), csv: { label: 'Skipped (rows)', value: (r) => r.n } },
    ],
    empty: () => emptyState({ icon: 'check', title: 'No row was skipped', text: 'Every row of the file passed its checks.' }),
  });
  // per car: rows read, imported (or to import) and skipped over every table, so a skipped count leads to a car
  const goesTo = (m) => (m === 'new' ? 'a new car' : m === 'leave_out' ? 'left out' : m ? carName(m) : 'not matched yet');
  const carsTable = dataTable(ctx, {
    title: 'Rows per car', icon: 'car', rows: [], csvName: 'import-report-cars', label: 'Rows read, imported and skipped per imported car',
    cover: 'web.import.rejected-count',
    note: 'Each car\'s rows over every table. Places (geofences) belong to no car, so they count only in Rows per table.',
    sort: { key: 'tm_car', dir: 'asc' },
    columns: [
      { key: 'tm_car', label: 'In the backup', sort: true, render: (c) => h('span', { class: 'row im-car' }, h('b', null, c.name || `Car ${c.tm_car}`), h('span', { class: 'mono faint' }, `#${c.tm_car}`)) },
      { key: 'match', label: 'Went to', sort: true, value: (c) => goesTo(c.match), render: (c) => (c.match && c.match !== 'new' && c.match !== 'leave_out'
        ? h('a', { class: 'link', href: buildUrl('car', { car: c.match }) }, carName(c.match))
        : h('span', { class: c.match === 'leave_out' ? 'faint' : c.match ? null : 'warn' }, goesTo(c.match))) },
      { key: 'read', label: 'Read', num: true, sort: true, render: (c) => (c.read == null ? h('span', { class: 'faint', title: 'Left out: its rows were not counted' }, 'not counted') : num(c.read)) },
      { key: 'imported', label: 'Imported', num: true, sort: true, value: (c) => (c.read == null ? null : S.report?.imported ? c.imported : c.read - rowsOf(c.skipped)),
        render: (c) => (c.read == null ? h('span', { class: 'faint' }, DASH) : S.report?.imported ? num(c.imported) : h('span', { class: 'faint', title: 'From the check: what the import would store' }, num(c.read - rowsOf(c.skipped)), ' to import')) },
      { key: 'skipped', label: 'Skipped', num: true, sort: true, value: (c) => (c.skipped ? rowsOf(c.skipped) : null),
        render: (c) => (c.skipped == null ? h('span', { class: 'faint' }, DASH) : rowsOf(c.skipped) ? h('span', { class: 'warn' }, num(rowsOf(c.skipped))) : h('span', { class: 'faint' }, '0')) },
      { key: 'why', label: 'Why skipped', render: (c) => {
        const e = Object.entries(c.skipped || {}).filter(([, n]) => n);
        return e.length ? h('span', { class: 'im-whys' }, e.map(([k, n]) => h('span', { class: 'im-badval', title: reasonWords(k)[1] }, reasonWords(k)[0], ` ${num(n)}`))) : h('span', { class: 'faint' }, c.skipped ? 'none' : DASH);
      } },
      { key: 'drives', label: 'Drives', num: true, sort: true, render: (c) => num(c.drives) },
      { key: 'charges', label: 'Charges', num: true, sort: true, render: (c) => num(c.charges) },
    ],
    csv: { get columns() {
      const done = !!S.report?.imported;
      return [
        { label: 'TeslaMate car number', value: (c) => c.tm_car },
        { label: 'In the backup', value: (c) => c.name || '' },
        { label: 'Went to', value: (c) => goesTo(c.match) },
        { label: 'Went to (car id)', value: (c) => (c.match && c.match !== 'new' && c.match !== 'leave_out' ? c.match : '') },
        { label: 'Read (rows; empty when left out)', value: (c) => c.read },
        done ? { label: 'Imported (rows)', value: (c) => c.imported } : { label: 'To import (rows, not stored yet)', value: (c) => (c.read == null ? null : c.read - rowsOf(c.skipped)) },
        { label: 'Skipped (rows)', value: (c) => (c.skipped ? rowsOf(c.skipped) : null) },
        { label: 'Skipped by reason (reason: rows)', value: (c) => Object.entries(c.skipped || {}).map(([k, n]) => `${reasonWords(k)[0]}: ${n}`).join('; ') },
        { label: 'Drives (count)', value: (c) => c.drives },
        { label: 'Charges (count)', value: (c) => c.charges },
      ];
    } },
    empty: () => emptyState({ icon: 'car', title: 'No cars in the report', text: 'The report lists each car of the backup once the whole file has been checked.' }),
  });
  function paintReport() {
    const j = S.job, rep = j?.report || null;
    S.report = rep;
    canWrap.hidden = true;
    if (rep && rep.kind === CAN) { paintCanReport(rep); return; }
    if (!rep) {
      replace(reportBox, emptyState({ icon: 'book', title: j ? 'No report yet' : 'No import yet', text: j ? 'The report appears once the whole file has been checked (pass 1).' : 'Each import gets a report: rows read, imported and skipped per table, and why.' }));
      skipBox.hidden = true;
      tablesTable.setRows([]); reasonsTable.setRows([]); carsTable.setRows([]);
      reportTables.hidden = true; carsWrap.hidden = true;
      return;
    }
    reportTables.hidden = false; carsWrap.hidden = !(rep.cars || []).length;
    const names = Object.keys(rep.tables || {});
    const rows = names.map((name, i) => {
      const t = rep.tables[name];
      const skipped = Object.values(t.skipped || {}).reduce((a, b) => a + b, 0);
      return { name, order: i, read: t.read, imported: t.imported, would: t.read - skipped, skipped, bad: Object.entries(t.bad_values || {}) };
    });
    const reasons = names.flatMap((name) => Object.entries(rep.tables[name].skipped || {}).map(([reason, n]) => ({ table: name, reason, n })));
    const tot = rows.reduce((a, r) => ({ read: a.read + r.read, imp: a.imp + (rep.imported ? r.imported : r.would), sk: a.sk + r.skipped, bad: a.bad + r.bad.reduce((x, [, n]) => x + n, 0) }), { read: 0, imp: 0, sk: 0, bad: 0 });
    replace(reportBox,
      h('p', { class: 'im-report-what' }, rep.imported ? 'What the import stored.' : 'From the check: what the import will store. Nothing is stored yet.'),
      figures([
        metric('Rows read', num(tot.read), null, { explain: 'Every row of every table the import uses' }),
        metric(rep.imported ? 'Rows imported' : 'Rows to import', num(tot.imp), null, { explain: rep.imported ? 'Stored and marked as imported' : 'Read minus skipped' }),
        metric('Rows skipped', num(tot.sk), null, { explain: tot.sk ? 'Each with its reason below' : 'None: every row passed' }),
        metric('Values cleared', num(tot.bad), null, { explain: 'A value out of range was left blank; the row was kept' }),
        metric('History from', rep.span ? date(rep.span.from_ms, tz) : null, null, { size: 'sm', why: 'The file holds no dated rows', explain: rep.span ? `to ${date(rep.span.to_ms, tz)}: ${dur(rep.span.to_ms - rep.span.from_ms)} of TeslaMate history` : null }),
      ], 'im-figs'));
    skipBox.hidden = !tot.sk;
    replace(skipBox, icon('warn', 'ico--sm'), h('div', null,
      h('p', null, h('b', null, `${num(tot.sk)} rows ${rep.imported ? 'were' : 'will be'} skipped.`), ' Skipping a row did not stop the import; every other row is in.'),
      h('p', null, 'A drive or charge built around a skipped row can come out a little short (fewer track points, no energy figure). The report keeps only how many rows were skipped and why, never what they held, so it is still here after a restart.')));
    tablesTable.setRows(rows);
    reasonsTable.setRows(reasons);
    carsTable.setRows(rep.cars || []);
  }
  /* ---------------------------------------------------------------- a CAN export's report */
  const keyStatus = (k) => KEY_STATUS[k.status] || [k.status, 'plain'];
  const keysTable = dataTable(ctx, {
    title: 'Logs per car key', icon: 'car', rows: [], csvName: 'import-can-keys', label: 'Logs read, imported, already here and refused per car key',
    note: 'A car key is the CAN Bridge\'s install on one car. A key a car has goes to that car; a new key gets a car the way a new key\'s first upload does (Settings → Cars).',
    sort: { key: 'key', dir: 'asc' },
    columns: [
      { key: 'key', label: 'Car key', sort: true, render: (k) => h('span', { class: 'mono' }, k.key) },
      { key: 'car', label: 'Goes to', sort: true, value: (k) => k.car_name || keyStatus(k)[0],
        render: (k) => (k.car ? h('a', { class: 'link', href: buildUrl('car', { car: k.car }) }, k.car_name || k.car) : tag(keyStatus(k)[0], keyStatus(k)[1])),
        csv: (k) => k.car_name || keyStatus(k)[0] },
      { key: 'logs', label: 'Logs', num: true, sort: true, render: (k) => num(k.logs) },
      { key: 'imported', label: 'Imported', num: true, sort: true, value: (k) => (S.report?.imported ? k.imported : k.logs - k.already_here - k.refused),
        render: (k) => (S.report?.imported ? num(k.imported) : h('span', { class: 'faint', title: 'From the check: what the import would store' }, num(k.logs - k.already_here - k.refused), ' to import')) },
      { key: 'already_here', label: 'Already here', num: true, sort: true, render: (k) => num(k.already_here) },
      { key: 'refused', label: 'Refused', num: true, sort: true, render: (k) => (k.refused ? h('span', { class: 'warn' }, num(k.refused)) : h('span', { class: 'faint' }, '0')) },
      { key: 'chain', label: 'Chain', value: (k) => (k.chain_gaps || 0) + (k.chain_forks || 0),
        render: (k) => (k.chain_gaps || k.chain_forks
          ? h('span', { class: 'warn', title: 'Like a live upload: a gap is a log the chain names that is not here; a fork two logs at one place' }, [k.chain_gaps ? `${num(k.chain_gaps)} gap${k.chain_gaps === 1 ? '' : 's'}` : null, k.chain_forks ? `${num(k.chain_forks)} fork${k.chain_forks === 1 ? '' : 's'}` : null].filter(Boolean).join(', '))
          : h('span', { class: 'faint' }, 'whole')),
        csv: (k) => `${k.chain_gaps || 0} gaps, ${k.chain_forks || 0} forks` },
      { key: 'first_ms', label: 'Logs from', sort: true, render: (k) => (k.first_ms ? `${date(k.first_ms, tz)} – ${date(k.last_ms, tz)}` : DASH), csv: false },
    ],
    empty: () => emptyState({ icon: 'car', title: 'No car keys', text: 'The zip holds no CAN Bridge log.' }),
  });
  const refusedTable = dataTable(ctx, {
    title: 'Refused logs', icon: 'warn', rows: [], csvName: 'import-can-refused', label: 'Logs that were refused, with why',
    note: 'A refused log is never stored: only logs signed by the car\'s CAN Bridge and unchanged since are taken. The first 100 are named; all are counted.',
    sort: { key: 'name', dir: 'asc' },
    columns: [
      { key: 'name', label: 'Log', sort: true, render: (r) => h('span', { class: 'mono im-name' }, r.name) },
      { key: 'reason', label: 'Why', sort: true, render: (r) => h('span', null, h('b', null, canReason(r.reason)[0]), h('span', { class: 'im-tdesc' }, canReason(r.reason)[1])), csv: (r) => canReason(r.reason)[0] },
      { key: 'code', label: 'Code', render: (r) => h('span', { class: 'mono faint' }, r.reason), csv: (r) => r.reason },
    ],
    empty: () => emptyState({ icon: 'check', title: 'No log was refused', text: 'Every log of the zip is signed by its car\'s CAN Bridge and unchanged since.' }),
  });
  const canWrap = h('div', { class: 'im-col im-can', hidden: true }, keysTable.el, refusedTable.el);
  function paintCanReport(rep) {
    const L = rep.logs || {};
    reportTables.hidden = true; carsWrap.hidden = true; canWrap.hidden = false;
    tablesTable.setRows([]); reasonsTable.setRows([]); carsTable.setRows([]);
    const took = rep.imported ? L.imported : L.to_import;
    replace(reportBox,
      h('p', { class: 'im-report-what' }, rep.imported ? 'What the import stored: the car\'s own signed CAN logs, as if the CAN Bridge had uploaded them.' : 'From the check: what the import will store. Nothing is stored yet.'),
      figures([
        metric('Logs read', num(L.read), null, { explain: `${bytes(L.bytes)} of CAN Bridge logs in the zip` }),
        metric(rep.imported ? 'Logs imported' : 'Logs to import', num(took), null, { explain: 'Signed by the car\'s CAN Bridge and unchanged since' }),
        metric('Already here', num(L.already_here), null, { explain: 'The same log (name and SHA-256) is here already: not stored twice' }),
        metric('Refused', num(L.refused), null, { explain: L.refused ? 'Never stored; each with its reason below' : 'None: every log checked out' }),
        metric('Logs from', rep.span?.from_ms ? date(rep.span.from_ms, tz) : null, null, { size: 'sm', why: 'The zip holds no log', explain: rep.span?.from_ms ? `to ${date(rep.span.to_ms, tz)}` : null }),
      ], 'im-figs'));
    skipBox.hidden = !L.refused;
    const by = Object.entries(rep.refused || {}).filter(([, n]) => n);
    replace(skipBox, icon('warn', 'ico--sm'), h('div', null,
      h('p', null, h('b', null, `${num(L.refused)} log${L.refused === 1 ? '' : 's'} ${rep.imported ? 'were' : 'will be'} refused`), ': ', by.map(([k, n]) => `${canReason(k)[0]} ${num(n)}`).join(', '), '.'),
      h('p', null, 'A refused log is never stored, so it cannot make up history. Every other log is imported.')));
    keysTable.setRows(rep.cars || []);
    refusedTable.setRows(rep.refused_logs || []);
  }
  const reportTables = h('div', { class: 'im-two' }, tablesTable.el, reasonsTable.el);
  const carsWrap = h('div', { class: 'im-cars' }, carsTable.el);
  const reportPanel = panel({ title: 'Report', icon: 'book', cls: 'im-report-panel' }, reportBox, skipBox);

  /* ---------------------------------------------------------------- overlap with CAN history */
  async function loadOverlaps() {
    // every car the admin sees (any number): imported history stays when its import record is removed,
    // so the overlaps are read from the cars, never from the import records
    S.overlapErr = null;
    const lists = await Promise.all(S.cars.map(async (c) => {
      try {
        const r = await ctx.api.list(`cars/${enc(c.id)}/overlaps`, { limit: 200 }).all({ max: 5000 });
        return r.map((o) => ({ ...o, car: c.id }));
      } catch (e) { if (e?.name !== 'AbortError') S.overlapErr = e; return []; }
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
      flash(`${carName(o.car)}: ${shown === 'can' ? 'CAN' : 'TeslaMate'} history is shown for that span on every page.`);
    } catch (e) {
      if (!ctx.alive()) return;
      flash(`Could not switch: ${errorWords(e).text}`, { kind: 'error' });
    }
    overlapTable.setRows(S.overlaps);
  }
  const zoneOf = (o) => S.cars.find((c) => c.id === o.car)?.time_zone || tz;
  const side = (x) => (x ? `${num(x.drives)} drives, ${num(x.charges)} charges` : DASH);
  // the Timeline is a car page: link a real car's (one with an overlap, else one an import brought history to, else the first)
  const timelineSlot = h('span', { class: 'im-tl' });
  function paintTimelineLink() {
    const done = S.jobs.filter((j) => j.status === 'done').flatMap((j) => (j.report?.cars || []).map((c) => c.match));
    const car = S.cars.find((c) => c.id === S.overlaps[0]?.car) || S.cars.find((c) => done.includes(c.id)) || S.cars[0];
    replace(timelineSlot, car
      ? h('a', { class: 'link', href: buildUrl('timeline', { car: car.id }) }, S.cars.length > 1 ? `Timeline of ${car.name}` : 'Timeline')
      : 'Timeline of each car');
  }
  const overlapTable = dataTable(ctx, {
    title: 'Spans with both histories', icon: 'timeline', rows: [], csvName: 'import-overlaps', label: 'Spans where imported and CAN history overlap',
    note: 'Where TeslaMate and the CAN Bridge both recorded the same time, only one side is shown on Drives, Charges, Timeline, Statistics and Travel: CAN at first, since it is measured. Neither side is deleted; switch a span to show the other.',
    sort: { key: 'start_ms', dir: 'desc' },
    columns: [
      { key: 'car', label: 'Car', sort: true, value: (o) => carName(o.car), render: (o) => h('b', null, carName(o.car)), csv: (o) => carName(o.car) },
      { key: 'start_ms', label: 'From', sort: true, render: (o) => dateTime(o.start_ms, zoneOf(o)), csv: { label: 'From (ISO 8601, car\'s time zone)', value: (o) => toIsoZoned(o.start_ms, zoneOf(o)) } },
      { key: 'end_ms', label: 'To', sort: true, render: (o) => dateTime(o.end_ms, zoneOf(o)), csv: { label: 'To (ISO 8601, car\'s time zone)', value: (o) => toIsoZoned(o.end_ms, zoneOf(o)) } },
      { key: 'len', label: 'Length', num: true, sort: true, value: (o) => o.end_ms - o.start_ms, render: (o) => dur(o.end_ms - o.start_ms), csv: { label: 'Length (s)', value: (o) => Math.round((o.end_ms - o.start_ms) / 1000) } },
      { key: 'can', label: 'CAN has', render: (o) => side(o.can), csv: { label: 'CAN has (drives, charges)', value: (o) => side(o.can) } },
      { key: 'teslamate', label: 'TeslaMate has', render: (o) => side(o.teslamate), csv: { label: 'TeslaMate has (drives, charges)', value: (o) => side(o.teslamate) } },
      { key: 'shown', label: 'Shown', sort: true, csv: { label: 'Shown (can or teslamate)', value: (o) => o.shown }, render: (o) => h('div', { class: 'seg im-seg', role: 'group', 'aria-label': `Which history is shown for ${carName(o.car)} from ${dateTime(o.start_ms, zoneOf(o))}` },
        [['can', 'CAN'], ['teslamate', 'TeslaMate']].map(([v, l]) => h('button', { class: 'seg-btn', type: 'button', 'aria-pressed': String(o.shown === v), onclick: () => flip(o, v) }, l))) },
      { key: 'go', label: 'See it', csv: false, render: (o) => h('a', { class: 'link nowrap', href: buildUrl('timeline', { car: o.car, period: { from: o.start_ms, to: o.end_ms } }) }, 'Timeline', icon('chev-r', 'ico--sm')) },
    ],
    empty: () => (S.overlapErr
      ? emptyState({ icon: 'warn', warn: true, title: 'Overlaps could not be read', text: `${errorWords(S.overlapErr).text} Reload the page to try again.` })
      : emptyState({ icon: 'check', title: 'No overlap', text: S.cars.length
        ? `No span of ${S.cars.length === 1 ? 'the car' : `the ${num(S.cars.length)} cars`} has both imported and CAN history, so every span shows the one history it has. Imported history that ends before the CAN Bridge started recording never overlaps.`
        : 'There is no car yet, so there is nothing to overlap.' })),
  });
  const overlapPanel = panel({ title: 'Overlap with CAN history', icon: 'timeline', cls: 'im-overlap', cover: ['web.import.docs-overlap', 'web.import.docs-backfill', 'faq.no-backfill'] },
    h('ul', { class: 'im-facts' },
      h('li', { cover: 'web.import.docs-overlap' }, icon('timeline', 'ico--sm'), h('div', null, h('b', null, 'Every span is imported, and overlaps are kept. '),
        'History is imported for all of its time, also where the CAN Bridge was recording too. Those spans are listed here with the side that is shown, and a switch.')),
      h('li', { cover: 'web.import.docs-backfill' }, icon('pin', 'ico--sm'), h('div', null, h('b', null, 'Addresses and elevation come with the backup. '),
        'A TeslaMate backup holds its own addresses and heights, so imported drives have them at once. A stop without an address gets one from the server\'s offline address lookup in the minutes after the import; it shows its coordinates until then.')),
      h('li', { cover: 'faq.no-backfill' }, icon('info', 'ico--sm'), h('div', null, h('b', null, 'Time nobody recorded cannot be fetched later. '),
        'Tesla keeps no history to download. The CAN Bridge keeps its logs and uploads them when it can, so its own gaps fill themselves; time before it, or a spell when nothing recorded, shows as unknown on the ',
        timelineSlot, '. Only an import from another logger, such as TeslaMate, fills it.'))),
    overlapTable.el);

  /* ---------------------------------------------------------------- every import */
  const historyTable = dataTable(ctx, {
    title: 'Imports', icon: 'logs', rows: [], csvName: 'imports', label: 'Every import, newest first',
    note: 'Choose one to see its progress and report. The history an import brought in stays when its record is removed.',
    sort: { key: 'started_ms', dir: 'desc' },
    rowHref: (j) => buildUrl('import', { query: { import: j.id } }),
    columns: [
      { key: 'name', label: 'File', sort: true, render: (j) => h('span', { class: 'row im-car' }, h('span', { class: 'mono im-name' }, j.name), j.id === S.job?.id ? tag('shown above', 'live') : null), csv: (j) => j.name },
      { key: 'status', label: 'State', sort: true, render: (j) => h('span', { class: ['im-st', `im-st--${STATUS[j.status]?.tone || 'plain'}`] }, STATUS[j.status]?.label || j.status), csv: { label: 'State', value: (j) => STATUS[j.status]?.label || j.status } },
      { key: 'bytes_total', label: 'Size', num: true, sort: true, render: (j) => bytes(j.bytes_total), csv: { label: 'Size (bytes)', value: (j) => j.bytes_total } },
      { key: 'started_ms', label: 'Started', sort: true, render: (j) => dateTime(j.started_ms, tz), csv: { label: `Started (ISO 8601, ${tz})`, value: (j) => toIsoZoned(j.started_ms, tz) } },
      { key: 'finished_ms', label: 'Finished', sort: true, render: (j) => (j.finished_ms ? agoEl(j.finished_ms, tz) : h('span', { class: 'faint' }, UNFINISHED.includes(j.status) ? 'not yet' : DASH)), csv: { label: `Finished (ISO 8601, ${tz}; empty if not yet)`, value: (j) => toIsoZoned(j.finished_ms, tz) } },
    ],
    empty: () => emptyState({ icon: 'logs', title: 'No imports yet', text: 'Each file you upload is listed here with its state.' }),
  });

  /* ---------------------------------------------------------------- refresh */
  function setJob(j) {
    S.jobs = S.jobs.map((x) => (x.id === j.id ? { ...x, ...j } : x));
    if (S.job?.id === j.id) S.job = { ...S.job, ...j };
  }
  let busy = false, lastRead = 0, tick = 0;
  async function refreshJob() {
    const j = S.job;
    if (!j || busy) return;
    busy = true;
    const was = j.status;
    try {
      const fresh = await ctx.api.get(`imports/${enc(j.id)}`);
      if (!ctx.alive()) return;
      setJob(fresh);
      lastRead = ctx.api.now();
      if (fresh.status !== was || ['checking', 'ready'].includes(fresh.status)) await loadMatch();
      if (fresh.status === 'done' && was !== 'done') { await loadJobs(); S.cars = await ctx.api.list('cars', { limit: 500 }).all({ max: 10000 }); pick(); await loadOverlaps(); flash(isCan(fresh) ? 'Import done. The logs are in; their drives and charges show as they are decoded.' : 'Import done. The history shows on every page, marked as imported.'); }
      if (fresh.status === 'failed' && was !== 'failed') flash('The import failed; see Upload for why.', { kind: 'error' });
    } catch (e) {
      if (!ctx.alive()) return;
      if (e?.code === 'no_such_import') { S.jobs = S.jobs.filter((x) => x.id !== j.id); S.job = null; pick(); }
    } finally { busy = false; }
    if (ctx.alive()) paintAll();
  }
  // only while a job moves on the server; nothing is read once it is ready, done or failed
  ctx.every(2000, () => {
    const j = S.job;
    tick++;
    if (!j || up.active) return;
    if (j.status === 'checking' || j.status === 'importing') refreshJob();
    else if (j.status === 'uploading' && tick % 5 === 0) refreshJob(); // perhaps uploading from another tab
  });

  const ageEl = progressPanel.querySelector('.im-age');
  function paintAll() {
    paintSteps(); paintUpload(); paintProgress(); paintMatch(); paintReport();
    historyTable.setRows(S.jobs);
    overlapTable.setRows(S.overlaps);
    paintTimelineLink();
    if (ageEl) replace(ageEl, lastRead ? h('span', { class: 'setting-note' }, 'read ', agoEl(lastRead, tz)) : null);
  }

  try { await Promise.all([loadMatch(), loadOverlaps()]); } catch (e) { if (!ctx.alive()) return; ctx.fail(e); return; }
  if (!ctx.alive()) return;
  lastRead = ctx.api.now();

  replace(ctx.view, h('div', { class: 'im' }, head,
    h('div', { class: 'im-top' },
      h('div', { class: 'im-col' }, uploadPanel, historyTable.el),
      h('div', { class: 'im-col' }, progressPanel)),
    matchWrap,
    reportPanel,
    carsWrap,
    reportTables,
    canWrap,
    overlapPanel));
  paintAll();
  ctx.ready();
}
