// Import (/settings/import): bring history in from TeslaMate's own backup file, TeslaFi's data download (its
// monthly CSVs, alone or in a .zip), or the CAN Bridge logs Sentry Six Manager already has (its CAN export
// bundle, a .zip). TeslaFi writes local times without a zone and its own units, so before a TeslaFi import starts
// the owner confirms the time zone and the units (sent with Start). An admin
// chooses the file; it goes up in chunks over HTTPS (each with its SHA-256, so it can stop and go on
// from the last chunk the server has), the server checks the whole file before it stores anything,
// the imported cars are matched to cars here (by VIN prefix, a new car, or left out), and then the
// import runs. The report counts rows per table and says why each skipped row was skipped (counts
// only, never row values or positions). Where imported and CAN history cover the same time, the
// overlap is listed with the side that is shown and a switch. A CAN export has no cars to match (its
// logs name their car key); its report counts logs: imported, already here, and refused with why.
//
// Progress: the job shown follows `job` events of kind import with its id (live events, polling only while the
// live stream is down: then, while it is being checked or imported, or uploaded from another tab, the page reads
// that one job again every few seconds, and stops as soon as it is ready, done or failed). Its figures change in
// place; the rest of the page is drawn again only when the job's state changes.

import { h, replace, setVars } from '../shared/dom.js';
import { buildUrl, preferredCar } from '../shared/state.js';
import { toIsoZoned } from '../shared/time.js';
import { format } from '../shared/units.js';
import { icon } from '../core/icons.js';
import { soldParts, soldOptions } from '../core/carswitch.js';
import { pageHead, panel, metric, figures, kv, tag, pill, note } from '../core/page.js';
import { dataTable } from '../core/table.js';
import { emptyState, errorWords } from '../core/states.js';
import { flash } from '../core/flash.js';
import { confirmDialog } from '../core/dialog.js';
import { dateTime, date, time, dur, num, agoEl, DASH } from '../core/fmt.js';
import { moveSummary, waiting, holdWhy, HOLD_ACTIONS, holdSaid, counted as nOf } from '../shared/rehome.js';
import { srcMark } from '../core/source.js';
import { follow } from '../shared/live.js';
import { morphChildren } from '../core/morph.js';

export const id = 'import';
export const css = true;
export const title = () => 'Import';

const enc = encodeURIComponent;
const bytes = (n) => (n == null ? DASH : format(n, 'bytes'));
const UNFINISHED = ['uploading', 'checking', 'ready', 'importing'];
const CAN = 'sentry_manager_can';
const isCan = (j) => j?.kind === CAN || j?.report?.kind === CAN;
const TF = 'teslafi';
const isTf = (j) => j?.kind === TF || j?.report?.kind === TF;
const KIND_WORDS = { teslamate: 'TeslaMate backup', teslafi: 'TeslaFi data download', [CAN]: 'Sentry Six Manager CAN export' };
// IANA zones this browser knows (for the TeslaFi time zone box); the server takes any IANA name
const ZONES = (() => { try { return Intl.supportedValuesOf('timeZone'); } catch { return []; } })();
const FROM_WORDS = { detected: 'found in the file', default: 'the usual when the file does not tell', setting: 'your choice', car: 'the matched car\'s zone' };

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
// TeslaFi's report: what the polls become
const TF_TABLES = {
  positions: ['Positions', 'Polls with a place or a battery figure.'],
  drives: ['Drives', 'From the gear: D or R, or moving, until P, ten minutes standing, a charge or 15 minutes without a poll.'],
  charging_processes: ['Charges', 'While TeslaFi says Charging; AC or DC from its fast-charger columns.'],
  states: ['Car states', 'Online and asleep from TeslaFi\'s state; offline is unknown, never sleep.'],
  updates: ['Software updates', 'Where the version TeslaFi recorded changes.'],
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
  already_imported: ['Already imported', 'An earlier import of TeslaFi\'s data covers this time; it is not stored twice.'],
  out_of_order: ['Out of order', 'A poll earlier than the one before it in the same file.'],
  dst_gap: ['Clock skipped', 'A wall time that does not exist in the chosen zone (the hour the clocks go forward).'],
  car_left_out: ['For a car left out', 'The poll belongs to a car you chose to leave out.'],
  too_many_cars: ['Too many cars', 'Past 64 cars in one upload.'],
  wrong_field_count: ['Wrong number of fields', 'The line has not as many fields as the header.'],
  too_long: ['Line too long', 'A line over 1 MB.'],
};
const reasonWords = (code) => REASONS[code] || [code.replace(/[_-]/g, ' '), 'Not one of the usual reasons; the code is the server\'s own.'];

// why a check or an import failed, in words, and what to do
const FAILURES = {
  not_a_backup: 'The file is not a TeslaMate backup. Make one with TeslaMate\'s database backup (a pg_dump of its database), plain or compressed with gzip or xz, and upload that file.',
  mixed_cars: 'The file holds rows that point at cars the file does not describe. Make a fresh, complete backup and try again.',
  no_car: 'The file holds no car. A backup made before TeslaMate saw a car has nothing to import.',
  truncated: 'The file ends part-way through. It was cut short when it was copied; copy it again.',
  one_file_only: 'A TeslaMate backup / CAN export is imported on its own: upload it alone. Several files at once are for TeslaFi\'s CSVs only.',
  not_a_teslafi_export: 'Several files go up as one TeslaFi import, and one of them is not a TeslaFi export (the server names it below). Discard this import and upload only TeslaFi\'s CSVs (.csv, .csv.gz or a .zip of them), or that other file on its own.',
  not_a_can_export: 'This zip is not a Sentry Six Manager CAN export. In Sentry Six Manager, download the CAN export bundle (Nerd panel, Logs + ids.csv, the car\'s key, From before its first log to now) and upload that .zip as it is. A TeslaMate backup is uploaded as the .sql file itself (or .sql.gz, .sql.xz), not zipped.',
  unreadable: 'The file cannot be read as a zip (it is damaged, cut short, encrypted or packed with a method other than deflate). Download it again and upload it as it is.',
  read_failed: 'The server could not read the uploaded file back. Upload it again.',
  store_failed: 'The server could not store a log (for example the disk is full). Logs stored before it stay; free some space and start again.',
  not_a_teslafi_export: 'No TeslaFi data is in this upload. In TeslaFi, Settings, Account, Advanced, "Download TeslaFi Data" gives one CSV per month: upload one, or several in a .zip.',
  teslafi_summary: 'This is TeslaFi\'s list of drives or charges (one row per drive, no positions), which cannot rebuild the history. Upload the monthly data download instead (Settings, Account, Advanced, "Download TeslaFi Data").',
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
// a count the file does not have is null: a gap (the faint dash), never 0; a sum adds only the numbers present
const sumOf = (list) => { const v = list.filter((x) => x != null); return v.length ? v.reduce((a, b) => a + b, 0) : null; };
const rowsOf = (o) => (o == null ? null : sumOf(Object.values(o)) ?? 0);
const less = (a, b) => (a == null ? null : a - (b ?? 0));
const gap = () => h('span', { class: 'faint', title: 'Not in the file' }, DASH);
const cnt = (n) => (n == null ? gap() : num(n));
const figNum = (n) => (n == null ? null : num(n)); // a metric shows null as its own gap
const counted = (cars) => (cars || []).filter((c) => c.match !== 'leave_out');
const PARALLEL = 3; // files sent at the same time; each file's chunks one after another
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
    sub: ['Admins only', 'TeslaMate backup (plain, gzip or xz), TeslaFi data download (CSV months, several at once, or a .zip) or Sentry Six Manager CAN export (.zip)', 'times in ' + tz],
    purpose: 'Brings in the history TeslaMate or TeslaFi recorded before the CAN Bridge, from TeslaMate\'s own backup file or TeslaFi\'s data download, or the CAN Bridge logs Sentry Six Manager already has, from its CAN export bundle. The whole file is checked before anything is stored, and running it again stores nothing twice.',
  });

  if (!ctx.admin) {
    replace(ctx.view, head, panel({ title: 'Import', icon: 'lock' },
      emptyState({ icon: 'lock', title: 'Only an admin can import', text: 'Importing adds history to cars, so it needs an admin account. Ask an admin to upload the TeslaMate backup; the imported history then shows on every page, marked as imported.' })));
    ctx.ready();
    return;
  }

  const S = { jobs: [], cars: [], job: null, match: [], matchErr: null, overlaps: [], holds: [], holdPick: new Map() };
  // files: the chosen File of each job file (by its index) still to send; sent: each file's offset while sending
  const up = { files: new Map(), sent: new Map(), active: false, paused: false, stop: false, failed: [], startedAt: 0, sentAt0: 0 };

  async function loadJobs() { S.jobs = await ctx.api.list('imports', { limit: 100 }).all({ max: 1000 }); }
  async function loadHolds() { S.holds = (await ctx.api.get('import-holds'))?.items || []; }
  try {
    const [, cars] = await Promise.all([loadJobs(), ctx.api.list('cars', { limit: 500 }).all({ max: 10000 }), loadHolds()]);
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
  const fileInput = h('input', { class: 'im-file-input', type: 'file', multiple: true, accept: '.sql,.gz,.xz,.zip,.dump,.bak,.txt,.csv,application/sql,application/gzip,application/x-xz,application/zip,text/csv', 'aria-label': 'TeslaMate backup, TeslaFi data download or Sentry Six Manager CAN export',
    onchange: () => { const fs = [...(fileInput.files || [])]; fileInput.value = ''; if (fs.length) chose(fs); } });
  const drop = h('label', { class: 'im-drop' }, fileInput,
    h('span', { class: 'im-drop-ic' }, icon('upload')),
    h('span', { class: 'im-drop-main' }, 'Choose the file or files, or drop them here'),
    h('span', { class: 'im-drop-sub' }, 'The .sql file of a TeslaMate database backup, as it is or compressed (.gz, .xz); TeslaFi\'s data download (TeslaFi, Settings, Account, Advanced, "Download TeslaFi Data": one CSV per month, several months at once, or a .zip of them), which become one import; or the .zip of Sentry Six Manager\'s CAN export bundle. A TeslaMate backup or a CAN export goes up on its own. Up to 64 GB a file.'));
  ctx.on(drop, 'dragover', (e) => { e.preventDefault(); drop.classList.add('im-drop--over'); });
  ctx.on(drop, 'dragleave', () => drop.classList.remove('im-drop--over'));
  ctx.on(drop, 'drop', (e) => { e.preventDefault(); drop.classList.remove('im-drop--over'); const fs = [...(e.dataTransfer?.files || [])]; if (fs.length) chose(fs); });

  const upErr = h('p', { class: 'form-error im-err', hidden: true, role: 'alert' });
  function showUpErr(text) { if (text) replace(upErr, icon('warn', 'ico--sm'), text); upErr.hidden = !text; }

  // the files of a job (an older job view without files is its own one file) and where each one's chunks go
  const jobFiles = (j) => (!j ? [] : Array.isArray(j.files) && j.files.length ? j.files
    : [{ index: 0, name: j.name, bytes_total: j.bytes_total, uploaded_bytes: j.uploaded_bytes || 0, upload_url: null }]);
  const fileUrl = (j, f) => f.upload_url || (f.index ? `imports/${enc(j.id)}/files/${f.index}` : `imports/${enc(j.id)}`);
  const sentOf = (f) => (up.sent.has(f.index) ? up.sent.get(f.index) : f.uploaded_bytes || 0);
  const uploadedOf = (j) => (j ? jobFiles(j).reduce((a, f) => a + Math.min(sentOf(f), f.bytes_total || 0), 0) : 0);
  const fileList = (fs) => fs.map((f) => `${f.name} (${bytes(f.bytes_total)})`).join(', ');

  async function chose(files) {
    showUpErr(null);
    const j = S.job;
    if (j && j.status === 'uploading' && !up.active) {
      // go on with an upload that stopped: each chosen file goes to the job file of the same name and size
      const want = jobFiles(j).filter((f) => (f.uploaded_bytes || 0) < f.bytes_total);
      const used = new Set();
      for (const f of want) {
        const m = files.find((x) => !used.has(x) && x.name === f.name && x.size === f.bytes_total);
        if (m) { used.add(m); up.files.set(f.index, m); }
      }
      const missing = want.filter((f) => !up.files.has(f.index));
      if (missing.length === want.length) {
        showUpErr(want.length === 1
          ? `That is not the file this upload was started with (${fileList(want)}). Choose that file to go on, or discard this upload to start with another.`
          : `None of those is a file this upload still needs: ${fileList(want)}. Choose them (all at once is fine) to go on, or discard this upload to start with others.`);
        return;
      }
      if (missing.length) showUpErr(`Still to choose, to finish the upload: ${fileList(missing)}. The others go on now.`);
      upload(j);
      return;
    }
    if (j && UNFINISHED.includes(j.status)) { showUpErr('An import is not finished yet. Wait for it, or discard it, before you upload another file.'); return; }
    try {
      const created = await ctx.api.post('imports', { files: files.map((f) => ({ name: f.name, bytes_total: f.size })) });
      if (!ctx.alive()) return;
      up.files = new Map(); up.sent = new Map();
      jobFiles(created).forEach((f, i) => { if (files[i]) up.files.set(f.index ?? i, files[i]); });
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

  // one file: its chunks one after another, from where the server has it
  async function sendFile(job, f, max) {
    const file = up.files.get(f.index);
    let off = f.uploaded_bytes || 0, tries = 0;
    up.sent.set(f.index, off);
    while (off < file.size && ctx.alive() && !up.paused && !up.stop) {
      try {
        const buf = await file.slice(off, Math.min(file.size, off + max)).arrayBuffer();
        const sum = hex(await crypto.subtle.digest('SHA-256', buf));
        const r = await ctx.api.patch(fileUrl(job, f), buf, { headers: {
          'Content-Type': 'application/offset+octet-stream', 'Upload-Offset': String(off), 'Upload-Length': String(file.size), 'Upload-SHA256': sum } });
        if (!ctx.alive()) return;
        tries = 0;
        off = r.offset ?? r.files?.find((x) => x.index === f.index)?.uploaded_bytes ?? r.uploaded_bytes;
        up.sent.set(f.index, off);
        setJob(r);
        paintUploadProgress();
      } catch (e) {
        if (!ctx.alive()) return;
        if ((e?.code === 'wrong_offset' || e?.code === 'checksum_mismatch' || e?.code === 'network' || e?.code === 'timeout') && tries < 3) {
          tries++;
          try {
            const fresh = await ctx.api.get(`imports/${enc(job.id)}`);
            if (!ctx.alive()) return;
            setJob(fresh);
            if (fresh.status !== 'uploading') { up.stop = true; break; }
            const ff = jobFiles(fresh).find((x) => x.index === f.index);
            if (ff) { off = ff.uploaded_bytes || 0; up.sent.set(f.index, off); }
          } catch { /* try the chunk again */ }
          continue;
        }
        // a discarded import is gone on the server: the upload stops without a word
        if (e?.code === 'not_uploading' || (e?.code === 'no_such_import' && up.discarded)) { up.stop = true; break; }
        up.failed.push({ name: file.name, off, e });
        break;
      }
    }
    if (off >= file.size) up.files.delete(f.index);
  }

  async function upload(job) {
    if (up.active || !up.files.size) return;
    up.active = true; up.paused = false; up.stop = false; up.discarded = false; up.failed = [];
    up.startedAt = Date.now(); up.sentAt0 = uploadedOf(job);
    const max = job.chunk_max_bytes || S.chunkMax || (1024 * 1024 - 1024);
    S.chunkMax = max;
    const queue = jobFiles(job).filter((f) => up.files.has(f.index) && (f.uploaded_bytes || 0) < f.bytes_total);
    // a chosen file the server already has whole needs nothing more
    for (const i of [...up.files.keys()]) if (!queue.some((f) => f.index === i)) up.files.delete(i);
    paintAll();
    const worker = async () => { while (queue.length && ctx.alive() && !up.paused && !up.stop) await sendFile(job, queue.shift(), max); };
    await Promise.all(Array.from({ length: Math.min(PARALLEL, queue.length) }, worker));
    if (!ctx.alive()) return;
    up.active = false;
    up.sent = new Map();
    if (up.failed.length) {
      const f1 = up.failed[0];
      showUpErr(up.failed.length === 1
        ? `The upload of ${f1.name} stopped at ${bytes(f1.off)}: ${errorWords(f1.e).text} Choose the file again to go on from there.`
        : `The upload of ${up.failed.map((x) => `${x.name} (at ${bytes(x.off)})`).join(', ')} stopped: ${errorWords(f1.e).text} Choose the files again to go on from there.`);
    }
    if (up.stop) up.files.clear();
    await refreshJob();
  }

  const upBox = h('div', { class: 'im-up' });
  const upBar = h('div', { class: 'bar im-bar' }, h('i'));
  const upText = h('p', { class: 'im-bar-text' });
  const upFiles = h('div', { class: 'im-upload-now', 'aria-label': 'Each file', role: 'list' });
  function paintUploadProgress() {
    const j = S.job;
    if (!j) return;
    const done = uploadedOf(j);
    setVars(upBar, { '--w': `${pct(done, j.bytes_total).toFixed(1)}%` });
    const secs = (Date.now() - up.startedAt) / 1000;
    const rate = up.active && secs > 1 ? (done - up.sentAt0) / secs : null;
    morphChildren(upText, [h('b', null, bytes(done)), ` of ${bytes(j.bytes_total)} uploaded (${num(pct(done, j.bytes_total), 0)} %)`,
      rate ? ` · ${bytes(rate)} per second · about ${dur(((j.bytes_total - done) / rate) * 1000)} to go` : null]);
    // a line per file once there are several: name, bytes, percent, done
    const fs = jobFiles(j);
    replace(upFiles, fs.length < 2 ? null : fs.map((f) => {
      const n = Math.min(sentOf(f), f.bytes_total), whole = n >= f.bytes_total;
      return h('p', { class: 'im-bar-text', role: 'listitem' }, h('span', { class: 'mono' }, f.name), ` · ${bytes(n)} of ${bytes(f.bytes_total)} (${num(pct(n, f.bytes_total), 0)} %) `,
        whole ? tag('done', 'live') : up.active && up.files.has(f.index) ? (up.sent.has(f.index) ? tag('sending', 'live') : tag('waiting', 'plain')) : null);
    }));
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
      const body = isTf(j) ? tfChoice() : {};
      if (body == null) { startBtn.disabled = false; return; }
      const r = await ctx.api.post(`imports/${enc(j.id)}/start`, body);
      if (!ctx.alive()) return;
      setJob(r);
      flash('Import started. You can leave this page; it goes on without it.');
    } catch (e) {
      if (!ctx.alive()) return;
      replace(startErr, icon('warn', 'ico--sm'), e?.fields ? Object.entries(e.fields).map(([k, v]) => `${k === 'time_zone' ? 'Time zone' : k === 'distance_unit' ? 'Distance' : k === 'temperature_unit' ? 'Temperature' : k}: ${v}`).join(' ') : startWords(e));
      startErr.hidden = false;
      startBtn.disabled = false;   // a failed start can be tried again (refreshJob below sets it right if the job moved on)
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
    up.paused = true; up.discarded = true; up.files.clear();
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
        upBar, upText, upFiles,
        up.active
          ? h('div', { class: 'row' }, h('button', { class: 'btn btn--sm', type: 'button', onclick: () => { up.paused = true; } }, 'Pause'),
            h('span', { class: 'setting-note' }, `Sent over HTTPS in chunks of about 1 MB, each with its SHA-256, so a chunk that arrives damaged is sent again${jobFiles(j).length > 1 ? `; up to ${PARALLEL} files at the same time` : ''}.`))
          : h('div', { class: 'im-resume' },
            h('p', null, up.paused ? 'Paused. ' : 'This upload stopped (the page was closed or the connection broke). ',
              jobFiles(j).length > 1 ? `The server has ${bytes(uploadedOf(j))} of these files. Choose the same files again (all at once is fine) to go on from there; each goes on where it stopped.`
                : `The server has the first ${bytes(j.uploaded_bytes)}. Choose the same file again to go on from there.`),
            up.files.size && up.paused ? h('button', { class: 'btn btn--sm btn--primary', type: 'button', onclick: () => upload(S.job) }, 'Go on') : drop)));
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
      : j.status === 'ready' ? (isTf(j) && !unmatched ? 'Checked and matched: confirm the time zone and units below, then start.' : isCan(j) ? 'Checked: ready to start. Each log goes to the car of its car key.' : unmatched ? `Match ${unmatched === 1 ? 'the last imported car' : `${unmatched} imported cars`} below first.` : 'Checked and matched: ready to start.')
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
    // in place: only the figures that changed are written (an event can come twice a second)
    morphChildren(progBox, [
      h('div', { class: 'im-status' }, pill(S1.label, { tone: S1.tone === 'ok' ? 'live' : S1.tone === 'danger' ? 'warm' : S1.tone, lg: true }), h('p', null, statusText(j))),
      kv([
        ['File', h('span', { class: 'mono im-name' }, j.name), { icon: 'logs' }],
        ['Kind', KIND_WORDS[j.kind] || KIND_WORDS[j.report?.kind] || 'known once checked'],
        ['Size', `${bytes(j.bytes_total)}${j.compression && j.compression !== 'plain' ? ` (${j.compression})` : ''}`],
        ['Started', dateTime(j.started_ms, tz), { icon: 'clock' }],
        ['Checked', j.checked_ms ? dateTime(j.checked_ms, tz) : DASH],
        ['Finished', j.finished_ms ? h('span', null, dateTime(j.finished_ms, tz), ' · took ', dur(j.finished_ms - j.started_ms)) : DASH],
      ]),
      h('ol', { class: 'im-stages', 'aria-label': 'State of each pass' },
        stageRow('Upload', jobFiles(j).length > 1 ? `The ${num(jobFiles(j).length)} files go to the server in chunks.` : 'The file goes to the server in chunks.', up1, up1 === 'done' ? `${bytes(j.bytes_total)} uploaded` : null, up1 === 'now' ? pct(uploadedOf(j), j.bytes_total) : null),
        stageRow('Check (pass 1)', isCan(j) ? 'Every log is read and its signature checked; nothing is stored.' : 'Every row is read and checked; nothing is stored.', ck, read(ck) || (ck === 'done' ? `Checked ${j.checked_ms ? dateTime(j.checked_ms, tz) : ''}` : null), ck === 'now' ? pct(j.bytes_read, j.bytes_total) : null),
        stageRow('Import (pass 2)', isCan(j) ? 'The logs are stored and checked as uploads are, then decoded into drives and charges.' : 'The rows are stored and the timeline is built again for the matched cars.', im, read(im) || (im === 'done' ? `Done ${dateTime(j.finished_ms, tz)}` : im === 'skipped' ? 'Not run: the check failed.' : null), im === 'now' ? pct(j.bytes_read, j.bytes_total) : null)),
      restartNote()]);
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
    paintMatch(); paintUpload(); paintReport();
  }
  const matchTable = dataTable(ctx, {
    title: 'Match the imported cars', icon: 'car', rows: [], label: 'Cars in the file and where their history goes', csvName: 'import-cars',
    note: 'Each car in the backup goes to a car here, becomes a new car, or is left out. A car here with the same VIN prefix (the first 11 characters, the same for every car of a model and factory year) is chosen at first. One car here takes at most one imported car.',
    columns: [
      { key: 'name', label: 'In the backup', sort: true, render: (c) => h('span', { class: 'row im-car' }, h('b', null, c.name), h('span', { class: 'mono faint' }, `#${c.tm_car}`)), csv: (c) => c.name },
      { key: 'vin_prefix', label: 'VIN prefix', sort: true, render: (c) => h('span', { class: 'mono' }, c.vin_prefix) },
      { key: 'drives', label: 'Drives', num: true, sort: true, render: (c) => cnt(c.drives) },
      { key: 'charges', label: 'Charges', num: true, sort: true, render: (c) => cnt(c.charges) },
      { key: 'first_ms', label: 'History', sort: true, render: (c) => `${date(c.first_ms, tz)} – ${date(c.last_ms, tz)}`, csv: false },
      { key: 'match', label: 'Goes to', csv: (c) => c.match ?? 'not chosen', render: (c) => {
        const taken = new Set(S.match.filter((x) => x.tm_car !== c.tm_car && x.match && x.match !== 'new' && x.match !== 'leave_out').map((x) => x.match));
        // sold cars last, in a "Sold cars" group, unless chosen or suggested (same VIN prefix)
        const carOpts = soldParts(S.cars, [c.match, c.suggested]);
        const carOpt = (x) => h('option', { value: x.id, selected: c.match === x.id, disabled: taken.has(x.id) }, `${x.name}${x.id === c.suggested ? ' (same VIN prefix)' : ''}${taken.has(x.id) ? ' (taken)' : ''}`);
        const sel = h('select', { class: 'input select im-select', 'aria-label': `Where ${c.name}'s history goes`, disabled: S.job?.status !== 'ready' && S.job?.status !== 'checking',
          onchange: () => setMatch(c, sel.value) },
        c.match == null ? h('option', { value: '', selected: true, disabled: true }, 'Choose…') : null,
        carOpts.main.map(carOpt),
        h('option', { value: 'new', selected: c.match === 'new' }, 'Add as a new car'),
        h('option', { value: 'leave_out', selected: c.match === 'leave_out' }, 'Leave out'),
        soldOptions(carOpts.sold, carOpt));
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
    paintTfStep();
  }

  /* ---------------------------------------------------------------- TeslaFi: the time zone and units */
  // TeslaFi writes local wall time without a zone and keeps the owner's display units: the check read the file in
  // the zone and units it says (report.time_zone, report.units); the owner confirms or changes them before Start.
  const tfZoneId = 'im-tf-zone';
  const tfZone = h('input', { class: 'input', id: tfZoneId, type: 'text', list: 'im-tf-zones', autocomplete: 'off', spellcheck: 'false', placeholder: 'America/Chicago', 'aria-describedby': 'im-tf-zone-hint im-tf-err' });
  const tfZones = h('datalist', { id: 'im-tf-zones' }, ZONES.map((z) => h('option', { value: z })));
  const tfDist = h('select', { class: 'select', id: 'im-tf-dist' }, h('option', { value: 'mi' }, 'Miles'), h('option', { value: 'km' }, 'Kilometres'));
  const tfTemp = h('select', { class: 'select', id: 'im-tf-temp' }, h('option', { value: 'c' }, '°C'), h('option', { value: 'f' }, '°F'));
  const tfErr = h('p', { class: 'form-error', id: 'im-tf-err', role: 'alert', hidden: true });
  const tfFound = h('div', { class: 'im-tf-found' });
  const tfBox = h('section', { class: 'panel panel-pad im-tf', id: 'teslafi-step', 'aria-label': 'Time zone and units of the TeslaFi data', hidden: true },
    h('div', { class: 'panel-head' }, h('h2', { class: 'panel-title' }, icon('clock'), 'Time zone and units')),
    h('p', { class: 'note' }, 'TeslaFi writes times without their zone, and some of its columns in the units you chose there. Take both from TeslaFi\'s own settings (Settings, Account) as they were when the data was recorded. A wrong zone moves every drive by hours; importing the files again with the right one replaces what this import stores.'),
    tfFound,
    h('div', { class: 'im-tf-fields' },
      h('div', { class: 'form-row im-tf-zone' }, h('label', { class: 'form-label', for: tfZoneId }, 'Time zone'), tfZone, tfZones,
        h('p', { class: 'form-hint', id: 'im-tf-zone-hint' }, 'An IANA name, such as America/Chicago or Europe/Berlin. Type to find one.')),
      h('div', { class: 'form-row' }, h('label', { class: 'form-label', for: 'im-tf-dist' }, 'Distance in'), tfDist),
      h('div', { class: 'form-row' }, h('label', { class: 'form-label', for: 'im-tf-temp' }, 'Temperature in'), tfTemp)),
    tfErr);
  let tfFor = null;
  tfZone.addEventListener('input', () => { tfErr.hidden = true; tfZone.removeAttribute('aria-invalid'); });
  function paintTfStep() {
    const j = S.job;
    const show = !!j && isTf(j) && j.status === 'ready';
    tfBox.hidden = !show;
    if (!show) return;
    const rep = j.report || {};
    const zone = rep.time_zone || {}, u = rep.units || {};
    // fill the boxes once per job, so a choice typed here is not overwritten by a refresh
    if (tfFor !== j.id) {
      tfFor = j.id;
      const car = S.match.find((c) => c.match && c.match !== 'new' && c.match !== 'leave_out');
      tfZone.value = zone.from === 'default' && car ? (S.cars.find((c) => c.id === car.match)?.time_zone || zone.name || '') : zone.name || '';
      tfDist.value = u.distance === 'km' ? 'km' : 'mi';
      tfTemp.value = u.temperature === 'f' ? 'f' : 'c';
    }
    replace(tfFound, h('p', { class: 'im-tf-said' }, 'The check read it in ', h('b', null, zone.name || 'the server\'s zone'), ` (${FROM_WORDS[zone.from] || zone.from || 'not said'}), distances in `,
      h('b', null, u.distance === 'km' ? 'kilometres' : 'miles'), ` (${FROM_WORDS[u.distance_from] || 'not said'}), temperatures in `, h('b', null, u.temperature === 'f' ? '°F' : '°C'), ` (${FROM_WORDS[u.temperature_from] || 'not said'}).`,
      zone.dst_repeated_rows ? ` ${num(zone.dst_repeated_rows)} rows fall in an hour the clocks went back; they are read in their order.` : ''));
  }
  /** Start's body for a TeslaFi import, or null (and the reason shown) when the zone is not one. */
  function tfChoice() {
    const z = tfZone.value.trim();
    if (!z || (ZONES.length && !ZONES.includes(z) && z !== 'UTC')) {
      tfErr.hidden = false; replace(tfErr, icon('warn'), z ? `${z} is not a time zone this browser knows. Pick one from the list, such as America/Chicago.` : 'Choose the time zone TeslaFi shows its times in.');
      tfZone.setAttribute('aria-invalid', 'true'); tfZone.focus();
      return null;
    }
    return { time_zone: z, distance_unit: tfDist.value, temperature_unit: tfTemp.value };
  }

  /* ---------------------------------------------------------------- report */
  const reportBox = h('div', { class: 'im-report' });
  const skipBox = h('div', { class: 'im-skip', role: 'note', cover: 'web.import.rejected-count' });
  // old charges or states TeslaMate never ended, imported ending at their last reading (in read and imported,
  // not skipped): a column only while some table has them
  const closedCol = { key: 'closed', label: 'Closed', num: true, sort: true, title: 'Never ended in TeslaMate: imported ending at their last reading',
    render: (r) => (r.closed == null ? gap() : r.closed ? num(r.closed) : h('span', { class: 'faint' }, '0')) };
  const tableCols = [
    { key: 'order', label: 'Table', sort: true, value: (r) => r.order, render: (r) => h('span', null, h('b', null, r.tf ? TF_TABLES[r.name]?.[0] || tableName(r.name) : tableName(r.name)), h('span', { class: 'im-tdesc' }, (r.tf ? TF_TABLES[r.name]?.[1] : TABLES[r.name]?.[1]) || '')), csv: (r) => r.name },
    { key: 'read', label: 'Read', num: true, sort: true, render: (r) => cnt(r.read) },
    { key: 'imported', label: 'Imported', num: true, sort: true, value: (r) => (S.report?.imported ? r.imported : r.would),
      render: (r) => (S.report?.imported ? cnt(r.imported) : r.would == null ? gap() : h('span', { class: 'faint', title: 'From the check: what the import would store' }, num(r.would), ' to import')), csv: (r) => (S.report?.imported ? r.imported : r.would) },
    closedCol,
    { key: 'skipped', label: 'Skipped', num: true, sort: true, render: (r) => (r.skipped == null ? gap() : r.skipped ? h('span', { class: 'warn' }, num(r.skipped)) : h('span', { class: 'faint' }, '0')) },
    { key: 'bad', label: 'Values cleared', render: (r) => (r.bad.length ? r.bad.map(([col, n]) => h('span', { class: 'im-badval' }, h('span', { class: 'mono' }, col), ` ${num(n)}`)) : h('span', { class: 'faint' }, 'none')),
      csv: (r) => r.bad.map(([c, n]) => `${c}:${n}`).join('; ') },
  ];
  let tablesSort = { key: 'order', dir: 'asc' };
  const tablesTable = dataTable(ctx, {
    title: 'Rows per table', icon: 'book', rows: [], csvName: 'import-report-tables', label: 'Rows read, imported and skipped per table',
    note: 'Counts only: the report never keeps row values or positions.',
    sort: { key: 'order', dir: 'asc' }, keepSort: false,
    columns: tableCols, onSort: (o) => { tablesSort = o; },
    csv: { get columns() {
      const done = !!S.report?.imported;
      return [
        { label: 'Table (TeslaMate name)', value: (r) => r.name },
        { label: 'Table', value: (r) => tableName(r.name) },
        { label: 'Read (rows)', value: (r) => r.read },
        done ? { label: 'Imported (rows)', value: (r) => r.imported } : { label: 'To import (rows, not stored yet)', value: (r) => r.would },
        ...(tableCols.includes(closedCol) ? [{ label: 'Closed at last reading (rows)', value: (r) => r.closed ?? '' }] : []),
        { label: 'Skipped (rows)', value: (r) => r.skipped },
        { label: 'Values cleared (column: rows)', value: (r) => r.bad.map(([c, n]) => `${c}: ${n}`).join('; ') },
      ];
    } },
    empty: () => emptyState({ icon: 'book', title: 'No report yet', text: 'The report appears once the whole file has been checked.' }),
  });
  function setTableRows(rows) {
    const on = rows.some((r) => r.closed), at = tableCols.indexOf(closedCol);
    if (on && at < 0) tableCols.splice(tableCols.findIndex((c) => c.key === 'imported') + 1, 0, closedCol);
    if (!on && at >= 0) tableCols.splice(at, 1);
    if (!on && tablesSort.key === 'closed') tablesSort = { key: 'order', dir: 'asc' };
    tablesTable.setSortKey(tablesSort);
    tablesTable.setRows(rows);
  }
  const reasonsTable = dataTable(ctx, {
    title: 'Why rows were skipped', icon: 'warn', rows: [], csvName: 'import-report-skipped', label: 'Skipped rows by reason',
    cover: 'web.import.rejected.col-reason', sort: { key: 'n', dir: 'desc' },
    columns: [
      { key: 'table', label: 'Table', sort: true, render: (r) => (r.table === 'polls' ? 'TeslaFi polls' : tableName(r.table)), csv: (r) => r.table },
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
      { key: 'match', label: 'Went to', sort: true, value: (c) => goesTo(c.match), render: (c) => (c.match && c.match !== 'new'
        ? h('a', { class: 'link', href: buildUrl('car', { car: c.match }) }, carName(c.match))
        : h('span', { class: c.match ? null : 'warn' }, goesTo(c.match))) },
      { key: 'read', label: 'Read', num: true, sort: true, render: (c) => cnt(c.read) },
      { key: 'imported', label: 'Imported', num: true, sort: true, value: (c) => (S.report?.imported ? c.imported : less(c.read, rowsOf(c.skipped))),
        render: (c) => (S.report?.imported ? cnt(c.imported) : c.read == null ? gap() : h('span', { class: 'faint', title: 'From the check: what the import would store' }, num(less(c.read, rowsOf(c.skipped))), ' to import')) },
      { key: 'skipped', label: 'Skipped', num: true, sort: true, value: (c) => rowsOf(c.skipped),
        render: (c) => (c.skipped == null ? gap() : rowsOf(c.skipped) ? h('span', { class: 'warn' }, num(rowsOf(c.skipped))) : h('span', { class: 'faint' }, '0')) },
      { key: 'why', label: 'Why skipped', render: (c) => {
        const e = Object.entries(c.skipped || {}).filter(([, n]) => n);
        return e.length ? h('span', { class: 'im-whys' }, e.map(([k, n]) => h('span', { class: 'im-badval', title: reasonWords(k)[1] }, reasonWords(k)[0], ` ${num(n)}`))) : c.skipped ? h('span', { class: 'faint' }, 'none') : gap();
      } },
      { key: 'drives', label: 'Drives', num: true, sort: true, render: (c) => cnt(c.drives) },
      { key: 'charges', label: 'Charges', num: true, sort: true, render: (c) => cnt(c.charges) },
    ],
    csv: { get columns() {
      const done = !!S.report?.imported;
      return [
        { label: 'TeslaMate car number', value: (c) => c.tm_car },
        { label: 'In the backup', value: (c) => c.name || '' },
        { label: 'Went to', value: (c) => goesTo(c.match) },
        { label: 'Went to (car id)', value: (c) => (c.match && c.match !== 'new' ? c.match : '') },
        { label: 'Read (rows; empty when the file has none)', value: (c) => c.read },
        done ? { label: 'Imported (rows)', value: (c) => c.imported } : { label: 'To import (rows, not stored yet)', value: (c) => less(c.read, rowsOf(c.skipped)) },
        { label: 'Skipped (rows)', value: (c) => rowsOf(c.skipped) },
        { label: 'Skipped by reason (reason: rows)', value: (c) => Object.entries(c.skipped || {}).map(([k, n]) => `${reasonWords(k)[0]}: ${n}`).join('; ') },
        { label: 'Drives (count)', value: (c) => c.drives },
        { label: 'Charges (count)', value: (c) => c.charges },
      ];
    } },
    empty: () => emptyState({ icon: 'car', title: 'No cars in the report', text: 'The report lists each car of the backup once the whole file has been checked.' }),
  });
  // several files in one import (TeslaFi): what each one gave
  const filesTable = dataTable(ctx, {
    title: 'Files', icon: 'logs', rows: [], csvName: 'import-report-files', label: 'Rows read, used and skipped per uploaded file',
    note: 'Each uploaded file, in the order it went up.',
    sort: { key: 'order', dir: 'asc' },
    columns: [
      { key: 'order', label: 'File', sort: true, value: (u) => u.order, render: (u) => h('span', { class: 'mono im-name' }, u.name), csv: { label: 'File', value: (u) => u.name } },
      { key: 'rows_read', label: 'Rows read', num: true, sort: true, render: (u) => cnt(u.rows_read) },
      { key: 'rows_used', label: 'Used', num: true, sort: true, render: (u) => cnt(u.rows_used) },
      { key: 'rows_skipped', label: 'Skipped', num: true, sort: true, render: (u) => (u.rows_skipped == null ? gap() : u.rows_skipped ? h('span', { class: 'warn' }, num(u.rows_skipped)) : h('span', { class: 'faint' }, '0')) },
    ],
    empty: () => emptyState({ icon: 'logs', title: 'One file', text: 'The import was one file; Rows per table counts it.' }),
  });
  function paintReport() {
    const j = S.job, rep = j?.report || null;
    S.report = rep;
    canWrap.hidden = true;
    if (rep && rep.kind === CAN) { paintCanReport(rep); return; }
    if (rep && rep.kind === TF) { paintTfReport(rep); return; }
    if (!rep) {
      replace(reportBox, emptyState({ icon: 'book', title: j ? 'No report yet' : 'No import yet', text: j ? 'The report appears once the whole file has been checked (pass 1).' : 'Each import gets a report: rows read, imported and skipped per table, and why.' }));
      skipBox.hidden = true;
      setTableRows([]); reasonsTable.setRows([]); carsTable.setRows([]); filesTable.setRows([]);
      reportTables.hidden = true; carsWrap.hidden = true; filesWrap.hidden = true;
      return;
    }
    // a car left out is not in the report (the server leaves it out too): no row, not in a count
    const out = new Set(S.match.filter((c) => c.match === 'leave_out').map((c) => String(c.tm_car)));
    const cars = counted(rep.cars).filter((c) => !out.has(String(c.tm_car)));
    const uploads = Array.isArray(rep.uploads) ? rep.uploads : [];
    reportTables.hidden = false; carsWrap.hidden = !cars.length; filesWrap.hidden = !uploads.length;
    const names = Object.keys(rep.tables || {});
    const rows = names.map((name, i) => {
      const t = rep.tables[name] || {};
      const skipped = rowsOf(t.skipped);
      return { name, order: i, read: t.read ?? null, imported: t.imported ?? null, would: less(t.read, skipped), skipped, closed: t.closed_at_last_reading ?? null, bad: Object.entries(t.bad_values || {}).filter(([, n]) => n != null) };
    });
    const closed = rows.filter((r) => r.closed).map((r) => {
      const [one, many] = { charging_processes: ['charge', 'charges'], states: ['state', 'states'] }[r.name] || ['row', 'rows'];
      return `${num(r.closed)} ${r.closed === 1 ? one : many} TeslaMate never ended ${rep.imported ? (r.closed === 1 ? 'was' : 'were') : 'will be'} taken, ending at ${r.closed === 1 ? 'its' : 'their'} last reading.`;
    });
    const reasons = names.flatMap((name) => Object.entries(rep.tables[name]?.skipped || {}).filter(([, n]) => n != null).map(([reason, n]) => ({ table: name, reason, n })));
    const tot = { read: sumOf(rows.map((r) => r.read)), imp: sumOf(rows.map((r) => (rep.imported ? r.imported : r.would))),
      sk: sumOf(rows.map((r) => r.skipped)), bad: sumOf(rows.flatMap((r) => r.bad.map(([, n]) => n))) };
    replace(reportBox,
      h('p', { class: 'im-report-what' }, rep.imported ? 'What the import stored.' : 'From the check: what the import will store. Nothing is stored yet.', ...closed.map((c) => ` ${c}`)),
      figures([
        metric('Rows read', figNum(tot.read), null, { explain: 'Every row of every table the import uses' }),
        metric(rep.imported ? 'Rows imported' : 'Rows to import', figNum(tot.imp), null, { explain: rep.imported ? 'Stored and marked as imported' : 'Read minus skipped' }),
        metric('Rows skipped', figNum(tot.sk), null, { explain: tot.sk ? 'Each with its reason below' : tot.sk == null ? null : 'None: every row passed' }),
        metric('Values cleared', figNum(tot.bad), null, { explain: 'A value out of range was left blank; the row was kept' }),
        metric('History from', rep.span?.from_ms != null ? date(rep.span.from_ms, tz) : null, null, { size: 'sm', why: 'The file holds no dated rows', explain: rep.span?.from_ms != null ? `to ${date(rep.span.to_ms, tz)}: ${dur(rep.span.to_ms - rep.span.from_ms)} of ${rep.kind === 'teslafi' ? 'TeslaFi' : 'TeslaMate'} history` : null }),
      ], 'im-figs'));
    skipBox.hidden = !tot.sk;
    replace(skipBox, icon('warn', 'ico--sm'), h('div', null,
      h('p', null, h('b', null, `${num(tot.sk)} rows ${rep.imported ? 'were' : 'will be'} skipped.`), ' Skipping a row did not stop the import; every other row is in.'),
      h('p', null, 'A drive or charge built around a skipped row can come out a little short (fewer track points, no energy figure). The report keeps only how many rows were skipped and why, never what they held, so it is still here after a restart.')));
    setTableRows(rows);
    reasonsTable.setRows(reasons);
    carsTable.setRows(cars);
    filesTable.setRows(uploads.map((u, i) => ({ ...u, order: i })));
  }
  /* ---------------------------------------------------------------- TeslaFi's report */
  function paintTfReport(rep) {
    reportTables.hidden = false; carsWrap.hidden = !(rep.cars || []).length;
    const rws = rep.rows || {}, f = rep.found || {};
    const sk = rowsOf(rws.skipped);
    const rows = Object.entries(rep.tables || {}).map(([name, t], i) => ({ name, order: i, read: t.read, imported: t.imported, would: t.found ?? (t.read - rowsOf(t.skipped)), skipped: rowsOf(t.skipped), bad: [], tf: true }));
    const reasons = Object.entries(rws.skipped || {}).filter(([, n]) => n).map(([reason, n]) => ({ table: 'polls', reason, n }));
    const u = rep.units || {}, z = rep.time_zone || {};
    replace(reportBox,
      h('p', { class: 'im-report-what' }, rep.imported ? 'What the import stored from TeslaFi\'s polls.' : 'From the check: what the import will store. Nothing is stored yet.'),
      figures([
        metric('Polls read', num(rws.read), null, { explain: `${num(rep.files)} ${rep.files === 1 ? 'file' : 'files'}, ${bytes(rep.unpacked_bytes)}` }),
        metric('Polls used', num(rws.used), null, { explain: sk ? `${num(sk)} skipped, each with its reason below` : 'None skipped' }),
        metric(rep.imported ? 'Drives and charges' : 'Drives and charges found', `${num(f.drives)} ${f.drives === 1 ? 'drive' : 'drives'}, ${num(f.charges)} ${f.charges === 1 ? 'charge' : 'charges'}`, null, { explain: `${num(f.positions)} positions, ${num(f.states)} state spells, ${num(f.updates)} updates` }),
        metric('Read in', z.name || DASH, null, { size: 'sm', explain: `${u.distance === 'km' ? 'km' : 'miles'} and ${u.temperature === 'f' ? '°F' : '°C'}${z.from ? ` · zone ${FROM_WORDS[z.from] || z.from}` : ''}` }),
        metric('History from', rep.span?.from_ms ? date(rep.span.from_ms, tz) : null, null, { size: 'sm', why: 'No poll was used', explain: rep.span?.from_ms ? `to ${date(rep.span.to_ms, tz)}` : null }),
      ], 'im-figs'),
      Object.keys(rep.files_skipped || {}).length ? h('p', { class: 'setting-note' }, 'Files passed over: ', Object.entries(rep.files_skipped).map(([n, why]) => `${n} (${why})`).join('; '), '.') : null,
      (rep.warnings || []).length ? h('ul', { class: 'im-warn-list' }, rep.warnings.map((w) => h('li', { class: 'warn' }, icon('warn', 'ico--sm'), ' ', w))) : null,
      f.replaced || f.read_again ? h('p', { class: 'setting-note' }, f.replaced ? `${num(f.replaced)} halves of drives or charges over a month's end were joined. ` : '', f.read_again ? `${num(f.read_again)} stretches imported before were read again and replaced.` : '') : null,
      rep.missing_columns?.length ? h('p', { class: 'setting-note' }, 'Columns not in the files (read as empty): ', rep.missing_columns.join(', '), '.') : null);
    skipBox.hidden = !sk;
    replace(skipBox, icon('warn', 'ico--sm'), h('div', null,
      h('p', null, h('b', null, `${num(sk)} polls ${rep.imported ? 'were' : 'will be'} skipped.`), ' Skipping a poll did not stop the import; every other poll is in.'),
      h('p', null, 'The report keeps only how many polls were skipped and why, never what they held.')));
    setTableRows(rows);
    reasonsTable.setRows(reasons);
    // a car's polls: read, stored and skipped as its own `tables.positions` says (a report without it, with one
    // car: the file's own counts; else not known, never "all imported")
    const one = (rep.cars || []).length === 1 ? { imported: rep.rows?.used ?? null, skipped: rep.rows?.skipped ?? null } : null;
    carsTable.setRows((rep.cars || []).map((c) => {
      const p = c.tables?.positions || one;
      return { ...c, read: c.rows, imported: rep.imported && p ? p.imported ?? null : null, skipped: c.rows == null || !p ? null : p.skipped || {} };
    }));
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
    reportTables.hidden = true; carsWrap.hidden = true; filesWrap.hidden = true; canWrap.hidden = false;
    setTableRows([]); reasonsTable.setRows([]); carsTable.setRows([]); filesTable.setRows([]);
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
  const filesWrap = h('div', { class: 'im-cars', hidden: true }, filesTable.el);
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
  // the side shown: live, or an import (import:teslamate, import:teslafi, or import when both sites are in a live
  // span); the old words can and teslamate still read
  const shownOf = (o) => o.shown_source || (o.shown === 'teslamate' ? 'import:teslamate' : 'live');
  const kindOf = (o) => o.kind || 'live';
  const liveOf = (o) => o.live || o.can;
  const importedOf = (o) => o.imported || o.teslamate;
  const SITE = { teslamate: 'TeslaMate', teslafi: 'TeslaFi' };
  const LIVE_WORD = { smo: 'SMO', tesla_api: 'Tesla API', teslafi_api: 'TeslaFi', teslamate_local: 'local TeslaMate' };
  const liveName = (o) => { const k = [...new Set((o.sources || []).filter((x) => x.kind !== 'import').map((x) => LIVE_WORD[x.kind] || x.kind))]; return k.length ? k.join(' + ') : 'Live'; };
  const sitesOf = (o) => (o.sources || []).filter((x) => x.kind === 'import').map((x) => x.site).filter(Boolean);
  const importName = (o) => { const st = [...new Set(sitesOf(o))]; return st.length ? st.map((x) => SITE[x] || x).join(' + ') : 'Imported'; };
  const shownWord = (o, v = shownOf(o)) => (v === 'live' ? `live (${liveName(o)})` : v === 'import:teslamate' ? 'TeslaMate\'s import' : v === 'import:teslafi' ? 'TeslaFi\'s import' : `the import (${importName(o)})`);
  // the two sides of a span: [value, label, counts]
  function sidesOf(o) {
    if (kindOf(o) === 'imports') return [['import:teslamate', 'TeslaMate', o.sites?.teslamate], ['import:teslafi', 'TeslaFi', o.sites?.teslafi]];
    const imp = shownOf(o).startsWith('import') ? shownOf(o) : 'import';
    return [['live', `Live (${liveName(o)})`, liveOf(o)], [imp, `Imported (${importName(o)})`, importedOf(o)]];
  }
  const isShown = (o, v) => shownOf(o) === v || (v.startsWith('import') && shownOf(o).startsWith('import') && kindOf(o) === 'live');
  const REASON = { more_drives_and_charges: 'it has more drives and charges there', more_samples: 'the same drives and charges, more positions', tie: 'the same, so TeslaMate\'s (the older format)' };
  async function flip(o, shown) {
    if (isShown(o, shown)) return;
    try {
      const r = await ctx.api.patch(`cars/${enc(o.car)}/overlaps`, { id: o.id, shown, version: o.version });
      if (!ctx.alive()) return;
      const items = (r.items || []).map((x) => ({ ...x, car: o.car }));
      S.overlaps = items.length ? [...S.overlaps.filter((x) => x.car !== o.car), ...items].sort((a, b) => b.start_ms - a.start_ms)
        : S.overlaps.map((x) => (x.car === o.car && x.id === o.id ? { ...x, ...(r.changed || { shown_source: shown }), car: o.car } : x));
      flash(`${carName(o.car)}: ${shownWord(o, shown)} is shown for that span on every page.`);
    } catch (e) {
      if (!ctx.alive()) return;
      if (e?.code === 'stale_version' || e?.code === 'no_such_overlap') {
        await loadOverlaps();
        if (!ctx.alive()) return;
        flash(e.code === 'stale_version' ? 'Someone else switched an overlap of this car a moment ago, so nothing was changed. The list now shows their choice: switch again if it is still needed.'
          : 'That span is not there any more (an import moved it). The list now shows the spans there are.', { kind: 'info' });
      } else flash(`Could not switch: ${errorWords(e).text}`, { kind: 'error' });
    }
    overlapTable.setRows(S.overlaps);
  }
  const stamp = (ms, z) => h('span', { class: 'cell-text im-stamp' }, h('span', null, date(ms, z, { year: true })), h('span', { class: 'sub' }, time(ms, z)));
  const zoneOf = (o) => S.cars.find((c) => c.id === o.car)?.time_zone || tz;
  const side = (x) => (x ? `${nOf(x.drives, 'drive', num)}, ${nOf(x.charges, 'charge', num)}` : DASH);
  // the Timeline is a car page: link a real car's (one with an overlap, else one an import brought history to, else the first)
  const timelineSlot = h('span', { class: 'im-tl' });
  function paintTimelineLink() {
    const done = S.jobs.filter((j) => j.status === 'done').flatMap((j) => counted(j.report?.cars).map((c) => c.match));
    const car = S.cars.find((c) => c.id === S.overlaps[0]?.car) || S.cars.find((c) => done.includes(c.id)) || S.cars.find((c) => c.id === preferredCar(S.cars));
    replace(timelineSlot, car
      ? h('a', { class: 'link', href: buildUrl('timeline', { car: car.id }) }, S.cars.length > 1 ? `Timeline of ${car.name}` : 'Timeline')
      : 'Timeline of each car');
  }
  const overlapTable = dataTable(ctx, {
    title: 'Spans with two histories', icon: 'timeline', rows: [], csvName: 'import-overlaps', label: 'Spans where two histories overlap',
    note: 'Where an import and a live input (SMO, the Tesla API, TeslaFi) both recorded the same time, only one side is shown on Drives, Charges, Timeline, Statistics and Travel: live at first, since SMO measures. Where TeslaMate\'s and TeslaFi\'s imports both cover a time, one import is shown: at first the one with more there. Neither side is deleted; switch a span to show the other.',
    sort: { key: 'start_ms', dir: 'desc' },
    columns: [
      { key: 'car', label: 'Car', sort: true, value: (o) => carName(o.car), render: (o) => h('b', null, carName(o.car)), csv: (o) => carName(o.car) },
      { key: 'start_ms', label: 'From', sort: true, render: (o) => stamp(o.start_ms, zoneOf(o)), csv: { label: 'From (ISO 8601, car\'s time zone)', value: (o) => toIsoZoned(o.start_ms, zoneOf(o)) } },
      { key: 'end_ms', label: 'To', sort: true, render: (o) => stamp(o.end_ms, zoneOf(o)), csv: { label: 'To (ISO 8601, car\'s time zone)', value: (o) => toIsoZoned(o.end_ms, zoneOf(o)) } },
      { key: 'len', label: 'Length', num: true, sort: true, value: (o) => o.end_ms - o.start_ms, render: (o) => dur(o.end_ms - o.start_ms), csv: { label: 'Length (s)', value: (o) => Math.round((o.end_ms - o.start_ms) / 1000) } },
      { key: 'kind', label: 'Between', sort: true, value: (o) => kindOf(o), render: (o) => h('span', { class: 'cell-text' }, h('span', null, kindOf(o) === 'imports' ? 'Two imports' : 'Live and imported'), h('span', { class: 'sub' }, kindOf(o) === 'imports' ? 'TeslaMate and TeslaFi' : `${liveName(o)} and ${importName(o)}`)),
        csv: { label: 'Between', value: (o) => (kindOf(o) === 'imports' ? 'imports' : 'live') } },
      { key: 'counts', label: 'Each side has', cls: 'im-wide', render: (o) => h('span', { class: 'im-sides' }, sidesOf(o).map(([v, l, n]) => h('span', { class: ['im-side', isShown(o, v) && 'is-shown'] }, h('b', null, l.replace(/ \(.*\)$/, '')), ` ${side(n)}`))),
        csv: { label: 'Each side has (drives, charges)', value: (o) => sidesOf(o).map(([, l, n]) => `${l}: ${side(n)}`).join('; ') } },
      { key: 'shown', label: 'Shown', cls: 'im-wide', sort: true, value: (o) => shownOf(o), csv: { label: 'Shown (live, import, import:teslamate or import:teslafi)', value: (o) => shownOf(o) }, render: (o) => h('div', { class: 'im-shown' },
        ctx.admin ? h('div', { class: 'seg im-seg', role: 'group', 'aria-label': `Which history is shown for ${carName(o.car)} from ${dateTime(o.start_ms, zoneOf(o))}` },
          sidesOf(o).map(([v, l]) => h('button', { class: 'seg-btn', type: 'button', 'aria-pressed': String(isShown(o, v)), onclick: () => flip(o, v) }, l)))
          : h('b', null, shownWord(o)),
        h('span', { class: 'sub' }, o.default_source && !isShown(o, o.default_source) ? `Switched; at first ${shownWord(o, o.default_source)}` : o.default_reason ? `At first: ${REASON[o.default_reason] || o.default_reason}` : 'At first: live, as SMO measures')) },
      { key: 'go', label: 'See it', csv: false, render: (o) => h('a', { class: 'link nowrap', href: buildUrl('timeline', { car: o.car, period: { from: o.start_ms, to: o.end_ms } }) }, 'Timeline', icon('chev-r', 'ico--sm')) },
    ],
    empty: () => (S.overlapErr
      ? emptyState({ icon: 'warn', warn: true, title: 'Overlaps could not be read', text: `${errorWords(S.overlapErr).text} Reload the page to try again.` })
      : emptyState({ icon: 'check', title: 'No overlap', text: S.cars.length
        ? `No span of ${S.cars.length === 1 ? 'the car' : `the ${num(S.cars.length)} cars`} has both imported and live history, so every span shows the one history it has. Imported history that ends before SMO or the Tesla API started recording never overlaps.`
        : 'There is no car yet, so there is nothing to overlap.' })),
  });
  const overlapPanel = panel({ title: 'Overlapping histories', icon: 'timeline', cls: 'im-overlap', id: 'overlaps', cover: ['web.import.docs-overlap', 'web.import.docs-backfill', 'faq.no-backfill'] },
    h('ul', { class: 'im-facts' },
      h('li', { cover: 'web.import.docs-overlap' }, icon('timeline', 'ico--sm'), h('div', null, h('b', null, 'Every span is imported, and overlaps are kept. '),
        'History is imported for all of its time, also where SMO, the Tesla API or TeslaFi was recording too, and where two imports cover the same days. Those spans are listed here with what each side has, the side that is shown, and a switch.')),
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

  /* ---------------------------------------------------------------- rows that need a car */
  const holdErr = h('p', { class: 'form-error im-err', hidden: true, role: 'alert' });
  async function giveCar(x) {
    const want = S.holdPick.get(x.hold) || 'wait';
    holdErr.hidden = true;
    try {
      const out = await ctx.api.post(`import-holds/${enc(x.hold)}`, { match: want });
      if (!ctx.alive()) return;
      S.holdPick.delete(x.hold);
      await Promise.all([loadHolds(), loadJobs()]);
      if (out?.move) S.cars = await ctx.api.list('cars', { limit: 500 }).all({ max: 10000 });
      if (!ctx.alive()) return;
      pick();
      flash(holdSaid(out, carName, num));
    } catch (e) {
      if (!ctx.alive()) return;
      replace(holdErr, icon('warn', 'ico--sm'), errorWords(e).text); holdErr.hidden = false;
    }
    paintAll();
  }
  const holdsTable = dataTable(ctx, {
    title: 'Rows that need a car', icon: 'car', rows: [], csvName: 'import-rows-need-a-car', label: 'Imported rows that wait for a car',
    note: 'Rows an import could not put on a car for sure: they came without a VIN and their odometer or time does not carry on from any car of the file, or an older import put them on a car whose odometer says they are not its own. They are kept, in no car, until you choose one. Nothing is deleted.',
    sort: { key: 'first_ms', dir: 'asc' },
    columns: [
      { key: 'first_ms', label: 'Rows', sort: true, render: (x) => h('span', { class: 'cell-text' }, h('b', null, x.name ? `Named ${x.name}` : 'No name'), h('span', { class: 'sub' }, x.first_ms != null ? `${date(x.first_ms, tz)} to ${date(x.last_ms, tz)}` : DASH)),
        csv: { label: `From (ISO 8601, ${tz})`, value: (x) => toIsoZoned(x.first_ms, tz) } },
      { key: 'rows', label: 'What', num: true, sort: true, render: (x) => h('span', { class: 'cell-text' }, `${num(x.rows)} rows`, h('span', { class: 'sub' }, x.odometer_km ? `${format(x.odometer_km[0], 'odometer')} to ${format(x.odometer_km[1], 'odometer')}` : `${nOf(x.drives, 'drive', num)}, ${nOf(x.charges, 'charge', num)}`)),
        csv: { label: 'Rows', value: (x) => x.rows } },
      { key: 'go', label: 'Goes to', cls: 'im-wide', csv: { label: 'State', value: (x) => x.status }, render: (x) => {
        const cur = S.holdPick.get(x.hold) || (x.status === 'ignored' ? 'ignore' : '');
        const sel = h('select', { class: 'input select im-select', 'aria-label': `Where the rows${x.name ? ` named ${x.name}` : ''} go`, onchange: (e) => { S.holdPick.set(x.hold, e.target.value); } },
          h('option', { value: '', selected: !cur, disabled: true }, x.status === 'ignored' ? 'Left aside: choose a car' : 'Choose a car'),
          S.cars.map((c) => h('option', { value: c.id, selected: cur === c.id }, c.name)),
          HOLD_ACTIONS.map(([v, l]) => h('option', { value: v, selected: cur === v }, v === 'new' && x.name ? `A new car named ${x.name}` : l)));
        return h('span', { class: 'row im-goes' }, sel, h('button', { class: 'btn btn--sm', type: 'button', onclick: () => giveCar(x) }, 'Apply'));
      } },
    ],
    empty: () => emptyState({ icon: 'check', title: 'Every imported row has a car', text: 'Rows an import could not put on a car for sure would be listed here to choose their car.' }),
  });
  // each import whose rows moved or wait for a car, in one line
  const movedList = h('ul', { class: 'im-moved-list', 'aria-label': 'Rows each import moved' });
  const holdsPanel = panel({ title: 'Imported rows and their cars', icon: 'car', cls: 'im-holds', id: 'needs-car' }, movedList, holdErr, holdsTable.el);
  function paintHolds() {
    const lines = S.jobs.map((j) => [j, moveSummary(j, carName, num)]).filter(([, w]) => w);
    replace(movedList, lines.map(([j, w]) => h('li', null, icon('info', 'ico--sm'), h('span', null, h('b', null, j.name), `: ${w}.`))));
    movedList.hidden = !lines.length;
    holdsTable.setRows(waiting(S.holds));
    holdsPanel.hidden = !S.holds.length && !lines.length;
  }

  /* ---------------------------------------------------------------- refresh */
  function setJob(j) {
    S.jobs = S.jobs.map((x) => (x.id === j.id ? { ...x, ...j } : x));
    if (S.job?.id === j.id) S.job = { ...S.job, ...j };
  }
  let busy = false, pending = null, lastRead = 0, tick = 0, matchAt = ctx.api.now();
  // a new state of the job shown, from its `job` event or a read: the figures change in place, and the rest of
  // the page is drawn again when its state changed (one at a time; the newest waiting is taken next)
  async function takeJob(fresh) {
    if (!fresh || !S.job || fresh.id !== S.job.id) return;
    if (busy) { pending = fresh; return; }
    busy = true;
    const was = S.job.status;
    let whole = fresh.status !== was;
    try {
      setJob(fresh);
      lastRead = ctx.api.now();
      // the cars to match: again when the state changed, and every 2 s while it is checked or ready
      if (whole || (['checking', 'ready'].includes(fresh.status) && lastRead - matchAt >= 2000)) { matchAt = lastRead; await loadMatch(); whole = true; }
      if (fresh.status === 'done' && was !== 'done') { await Promise.all([loadJobs(), loadHolds()]); S.cars = await ctx.api.list('cars', { limit: 500 }).all({ max: 10000 }); pick(); await loadOverlaps(); flash(isCan(fresh) ? 'Import done. The logs are in; their drives and charges show as they are decoded.' : 'Import done. The history shows on every page, marked as imported.'); }
      if (fresh.status === 'failed' && was !== 'failed') flash('The import failed; see Upload for why.', { kind: 'error' });
    } catch (e) {
      if (!ctx.alive()) return;
    } finally { busy = false; }
    if (!ctx.alive()) return;
    if (whole) paintAll(); else paintLive();
    if (pending) { const p = pending; pending = null; takeJob(p); }
  }
  async function readJob() {
    const j = S.job;
    if (!j) return null;
    try {
      return await ctx.api.get(`imports/${enc(j.id)}`);
    } catch (e) {
      if (!ctx.alive()) return null;
      if (e?.code === 'no_such_import' && S.job?.id === j.id) { S.jobs = S.jobs.filter((x) => x.id !== j.id); S.job = null; pick(); paintAll(); return null; }
      throw e;
    }
  }
  async function refreshJob() {
    const fresh = await readJob().catch(() => null);
    if (fresh && ctx.alive()) await takeJob(fresh);
  }
  // the job shown, as the server sends it (and read once more after a reconnect)
  const jobLive = follow(ctx.api, {
    type: 'job', match: (d) => !!d && d.kind === 'import' && !!S.job && d.id === S.job.id, pick: (d) => d.status,
    load: () => (up.active ? Promise.resolve(null) : readJob()),
    // the fallback below reads only while the stream is down
    pollMs: () => null,
    onValue: (j) => { if (ctx.alive() && j) takeJob(j); },
  });
  // the fallback, only while the live stream is down and a job moves on the server; nothing is read once it is
  // ready, done or failed
  ctx.every(2000, () => {
    const j = S.job;
    tick++;
    if (!j || up.active || jobLive.live) return;
    if (j.status === 'checking' || j.status === 'importing') refreshJob();
    else if (j.status === 'uploading' && tick % 5 === 0) refreshJob(); // perhaps uploading from another tab
  });

  const ageEl = progressPanel.querySelector('.im-age');
  function paintAge() { if (ageEl) morphChildren(ageEl, lastRead ? h('span', { class: 'setting-note' }, 'read ', agoEl(lastRead, tz)) : []); }
  // the figures of the job under way only (its state is the same)
  function paintLive() {
    paintProgress();
    if (S.job?.status === 'uploading' && !up.active) paintUploadProgress();
    paintAge();
  }
  function paintAll() {
    paintSteps(); paintUpload(); paintProgress(); paintMatch(); paintReport();
    historyTable.setRows(S.jobs);
    paintHolds();
    overlapTable.setRows(S.overlaps);
    paintTimelineLink();
    paintAge();
  }

  try { await Promise.all([loadMatch(), loadOverlaps()]); } catch (e) { if (!ctx.alive()) return; ctx.fail(e); return; }
  if (!ctx.alive()) return;
  lastRead = ctx.api.now();

  replace(ctx.view, h('div', { class: 'im' }, head,
    h('div', { class: 'im-top' },
      h('div', { class: 'im-col' }, uploadPanel, historyTable.el),
      h('div', { class: 'im-col' }, progressPanel)),
    matchWrap,
    tfBox,
    reportPanel,
    filesWrap,
    carsWrap,
    reportTables,
    canWrap,
    holdsPanel,
    overlapPanel));
  paintAll();
  ctx.ready();
}
