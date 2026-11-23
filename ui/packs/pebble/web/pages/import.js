// Sentry Mate, "Pebble" UI pack: Import (/settings/import).
//
// Three kinds of file: TeslaMate's history from its own backup file, TeslaFi's data download (its monthly CSVs, one,
// several at once, a .zip of them, or gzipped: all files picked together become one import; TeslaFi writes wall
// times without a zone and its own units, so the start step asks the owner to confirm the time zone and the units),
// or the CAN Bridge logs Sentry Six Manager already has (its CAN export bundle, a .zip: each log as the CAN Bridge
// signed it, checked like an upload and stored as the car's own SMO data, never doubled). Numbered step cards: pick
// the files (all go into one import; up to 3 go up at a time, each in chunks over HTTPS with its SHA-256, so a
// stopped upload goes on from the last chunk the server has), the check (the whole file is read before anything is
// stored), match the cars and start, the import, done. Only the current card carries a button and its output under
// it. Below: the report as number tiles (rows read, imported, skipped and why, values left blank, per table and per
// car), the spans where two histories overlap (live and imported, or a TeslaMate and a TeslaFi import) with a switch
// for the side shown, a tile of what to know, and the earlier imports. A count the file does not have is null in the
// report and shows as a gap (a faint dash), never as 0; cars left out are not in the report.
//
// API (admins): GET / POST imports, GET / PATCH / DELETE imports/{import}, GET / PUT imports/{import}/cars,
// POST imports/{import}/start, GET / PATCH cars/{car}/overlaps, GET cars. No event says how an import goes, so
// while one is uploaded elsewhere, checked, waiting or imported the page reads that job again every few seconds.

import { h } from '../shared/dom.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { format } from '../shared/units.js';
import { formatDate, formatDateTime, formatDuration } from '../shared/time.js';
import { tile, btn, chip, icon, note, num } from '../core/ui.js';
import { emptyTile, errorWords } from '../core/states.js';
import { confirmSheet } from '../core/sheet.js';
import { field, segmented } from '../core/form.js';
import { sectionHead, count, nums, facts } from '../core/bits.js';
import { srcPill } from '../core/source.js';
import { isValidZone } from '../shared/time.js';
import { group, infoRow, pickRow, said } from '../core/settings.js';
import { moveSummary, waiting, holdWhy, HOLD_ACTIONS, holdSaid, counted as nOf } from '../shared/rehome.js';
import { progressBar, callout, bytes } from './move-parts.js';

export const id = 'import';
export const css = true;
export const shape = [['full', 'h1'], [2, 'h3'], [2, 'h3'], [1, 'h1'], [1, 'h1'], [1, 'h1'], [1, 'h1']];
export const title = () => 'Import';

const UNFINISHED = ['uploading', 'checking', 'ready', 'importing'];
const MOVING = ['checking', 'importing'];
const CHUNK_DEFAULT = 1024 * 1024 - 1024;
const POLL_MS = 2500;
const PARALLEL = 3;

const STATUS = {
  uploading: ['Uploading', 'sky'], checking: ['Checking', 'sky'], ready: ['Waiting for you', 'sun'],
  importing: ['Importing', 'sky'], done: ['Done', 'teal'], failed: ['Failed', 'bad'],
};
const statusChip = (s, cover = null) => chip((STATUS[s] || [s])[0], { tone: (STATUS[s] || [])[1] || null, cover });

const TABLES = {
  cars: 'Cars', drives: 'Drives', charging_processes: 'Charges', positions: 'Track points', updates: 'Software updates',
  geofences: 'Places', addresses: 'Addresses', states: 'Car states', car_settings: 'Car settings', charges: 'Charge readings',
};
const tableName = (t) => TABLES[t] || t.replace(/_/g, ' ');

// Sentry Six Manager's CAN export (a .zip): its logs go in as if the CAN Bridge had uploaded them
const CAN = 'sentry_manager_can';
const isCan = (j) => !!j && (j.kind === CAN || (j.report && j.report.kind === CAN) || (j.kind == null && j.compression === 'zip'));
// TeslaFi's data download: its monthly CSVs of polls
const TF = 'teslafi';
const isTf = (j) => !!j && (j.kind === TF || (j.report && j.report.kind === TF));
const SITE = { teslamate: 'TeslaMate', teslafi: 'TeslaFi' };
const siteOf = (src) => SITE[String(src || '').split(':')[1]] || 'Imported';
const UNIT_FROM = { detected: 'worked out from the file', default: 'nothing in the file tells, so the usual', setting: 'as you set it' };
const ZONE_FROM = { car: 'the zone of the car it is matched to', default: 'the server’s default zone', setting: 'the zone you set' };
const CAN_REASONS = {
  unsigned: ['Not signed', 'The log has no valid CAN Bridge signature, so nothing shows it came from the car. It is not stored.'],
  failed_check: ['Changed since it was signed', 'Its bytes, name or length are not the ones the CAN Bridge signed: it was changed or damaged.'],
  signed_by_another_key: ['Another signing key', 'Signed by a key that is not the one this car key is paired with. Trust that key in Settings: Cars first if it is the car’s new one.'],
  no_sidecar: ['No sidecar', 'The zip has the log without its .log.xz.json beside it, which holds the signature.'],
  bad_sidecar: ['Sidecar unreadable', 'Its sidecar is not JSON, or over 1 MB.'],
  different_file_here: ['Another log of that name', 'A different log with the same name is here already; the one here is kept.'],
  being_uploaded: ['Being uploaded', 'The CAN Bridge is uploading this log now; that upload brings it.'],
  too_large: ['Too large', 'A log can be at most 1 GB.'],
  car_full: ['Car full', 'The car already holds the most logs a car can keep.'],
};
const canReason = (code) => CAN_REASONS[code] || [String(code).replace(/[_-]/g, ' '), 'A reason the server gave by its code.'];
const KEY_STATUS = {
  car: ['Goes to its car', 'teal'], new_car: ['A new car', 'sun'], only_car: ['The one car here', 'teal'],
  new_key: ['New car key', 'sun'], waiting: ['Waiting for an admin', 'sun'], working: ['Being decoded', 'sky'],
};

// why rows were skipped (the whole row) or a value left blank (one column)
const REASONS = {
  'already-imported': ['Imported before', 'An earlier import took this row, so it is not stored twice.'],
  open: ['Not finished in the backup', 'Still going when the backup was made. A later backup brings it in.'],
  'unknown-car': ['Car not in the backup', 'The row names a car the backup does not list.'],
  'car-left-out': ['For a car left out', 'The row belongs to a car you chose to leave out.'],
  'unknown-charge': ['Charge not in the backup', 'A reading whose charge the backup does not have.'],
  'bad-value': ['A needed value cannot be read', 'A value the row needs is missing or unreadable.'],
  'wrong-field-count': ['Broken row', 'The row has more or fewer values than its table has columns.'],
  'too-long': ['Row too long', 'Longer than any real TeslaMate row, so it is not read.'],
  already_imported: ['Imported before', 'An earlier import took this time already, so it is not stored twice.'],
  out_of_order: ['Out of order', 'Earlier than the poll before it in the same file.'],
  dst_gap: ['A time the clocks skip', 'The wall time does not exist in this time zone (the hour the clocks go forward).'],
  car_left_out: ['For a car left out', 'The poll belongs to a car you chose to leave out.'],
  too_many_cars: ['Too many cars', 'Past 64 cars in one upload.'],
  wrong_field_count: ['Broken row', 'The row has more or fewer values than the file has columns.'],
  too_long: ['Row too long', 'A line over 1 MB, so it is not read.'],
  duplicate: ['The same row twice', 'The file holds this row twice. The first copy is kept.'],
  no_energy: ['No energy recorded', 'A charge that added 0 kWh is left out.'],
  no_position: ['No coordinates', 'A track point without a position. The map line skips it.'],
  bad_time: ['Time cannot be read', 'No time, or one that is not a real date.'],
  other_car: ['For a car left out', 'The row belongs to a car you chose to leave out.'],
  out_of_range: ['Values out of range', 'Every value failed its check (a negative distance, say).'],
};
const BLANKS = {
  cost: ['Cost left blank', 'The charge is kept with no cost; you can type one on its page.'],
  elevation: ['Height left blank', 'A height out of any real range. The point is kept without it.'],
};
const reasonWords = (code) => REASONS[code] || [code.replace(/[_-]/g, ' '), 'A reason the server gave by its code.'];
const blankWords = (col) => BLANKS[col] || [`${col.replace(/_/g, ' ')} left blank`, 'The row is kept without this value.'];

const FAILURES = {
  not_a_backup: 'The file is not a TeslaMate backup. Make one with TeslaMate’s database backup (pg_dump of its whole database), as it is or compressed with gzip or xz.',
  no_car: 'The file holds no car. A backup made before TeslaMate saw a car has nothing to import.',
  mixed_cars: 'The file holds rows for cars it does not describe. Make a fresh, whole backup and upload it.',
  truncated: 'The file ends part way: it was cut short when it was made or copied. Copy it again and upload the new copy.',
  not_a_teslafi_export: 'No TeslaFi file is in the upload. In TeslaFi, Settings, Account, Advanced, “Download TeslaFi Data” gives one CSV per car and month: upload one, several in a .zip, or a file gzipped or xz’d.',
  teslafi_summary: 'This is TeslaFi’s list of drives or charges (one row per drive, no positions). Upload TeslaFi’s raw data instead: Settings, Account, Advanced, “Download TeslaFi Data”.',
  not_a_can_export: 'This zip is not a Sentry Six Manager CAN export. In Sentry Six Manager, download the CAN export bundle (Nerd panel, Logs + ids.csv, the car’s key, from before its first log to now) and upload that .zip as it is. A TeslaMate backup goes up as the .sql file itself (or .sql.gz, .sql.xz), not zipped.',
  unreadable: 'The file cannot be read as a zip: it is damaged, cut short, encrypted or packed in a way the server does not read. Download it again and upload it as it is.',
  one_file_only: 'A TeslaMate backup or a CAN export is imported on its own. Discard this import, then upload that file alone; only TeslaFi’s CSVs go up several at a time.',
  read_failed: 'The server could not read the uploaded file back. Upload it again.',
  store_failed: 'The server could not store a log, for example because the disk is full. Logs stored before stay; free some space and start again.',
  stopped: 'The import was stopped. Logs stored before stay; start it again to bring the rest.',
  internal_error: 'Something went wrong in the import itself. Logs stored before stay; start it again, and tell the developers if it happens again.',
};

function startWords(e) {
  switch (e && e.code) {
    case 'not_ready': return 'The file is not checked yet. Start works once the check is done.';
    case 'cars_unmatched': return 'Some cars in the file have no match yet. Pick a car, “Add as a new car” or “Leave out” for each.';
    case 'import_running': return 'Another import is running, perhaps from another tab. Reload this page to see it.';
    case 'no_such_import': return 'This import was discarded, perhaps from another tab. Reload this page.';
    default: return errorWords(e).text;
  }
}

const pct = (a, b) => (b > 0 ? Math.max(0, Math.min(100, (a / b) * 100)) : 0);
const hex = (buf) => Array.from(new Uint8Array(buf), (b) => b.toString(16).padStart(2, '0')).join('');
const sum = (o) => Object.values(o || {}).reduce((a, b) => a + (b || 0), 0);
/** Adds only the numbers present; null when there is none (a gap, never 0). */
const addUp = (vals) => vals.reduce((a, b) => (typeof b === 'number' ? (a ?? 0) + b : a), null);
/** A count as text, or the dash of a gap. */
const cnt = (n) => (n == null ? '—' : count(n));
/** A count on the right of a row; a gap is a faint dash. */
const nSide = (n) => (n == null ? h('span', { class: 'im-n im-gap' }, '—') : h('span', { class: 'im-n' }, count(n)));
/** The files of a job; a job from before several files has its one file at the old address. */
const jobFiles = (j) => (j && Array.isArray(j.files) && j.files.length ? j.files
  : j ? [{ index: 0, name: j.name, bytes_total: j.bytes_total, uploaded_bytes: j.uploaded_bytes || 0, upload_url: api.apiPath`imports/${j.id}` }] : []);
const isCar = (m) => typeof m === 'string' && m !== 'new' && m !== 'leave_out';
const wait = (ms, signal) => new Promise((res) => { const t = setTimeout(res, ms); signal && signal.addEventListener('abort', () => { clearTimeout(t); res(); }, { once: true }); });
const fieldsText = (e) => (e && e.fields ? Object.values(e.fields).join(' ') : errorWords(e).text);

function crumb() {
  return h('nav', { class: 'crumb', 'aria-label': 'Breadcrumb', cover: 'web.import.breadcrumb' },
    h('a', { href: '/', class: 'place-link' }, 'Fleet'), icon('chevronRight'),
    h('a', { href: state.buildUrl('settings-server'), class: 'place-link' }, 'Settings'), icon('chevronRight'),
    h('span', { 'aria-current': 'page' }, 'Import'));
}

/* ====================================================================== the page */

export async function mount(ctx) {
  const tz = ctx.tz;
  const bar = h('div', { class: 'page-bar w-full' }, crumb());

  if (!ctx.admin) {
    ctx.view.replaceChildren(bar, emptyTile({ icon: 'lock', title: 'Only an admin can import history',
      text: 'An import adds drives and charges to cars, so it needs an admin account. Ask an admin to upload the TeslaMate backup (its history then shows on every page, marked as imported) or Sentry Six Manager’s CAN export (its logs become the car’s own SMO data).' }));
    ctx.ready();
    return {};
  }

  const S = { jobs: [], cars: [], carsMore: false, job: null, match: null, matchErr: null, overlaps: new Map(), currency: '', tf: null };
  // files: the chosen File of each job file still to send, by index; off: the offset each file has reached here
  const up = { files: null, off: new Map(), active: false, paused: false, error: null, t0: 0, b0: 0, ctl: null };
  const fileDone = (f) => Math.max(f.uploaded_bytes || 0, up.off.get(f.index) || 0);
  const upDone = (j) => jobFiles(j).reduce((a, f) => a + fileDone(f), 0);

  const [jobs, carsPage, holds] = await Promise.all([
    ctx.api.list('imports', { limit: 100 }).all({ max: 500 }),
    ctx.api.page('cars', { limit: 500 }),
    ctx.api.get('import-holds'),
  ]);
  if (!ctx.alive()) return {};
  S.jobs = jobs;
  S.holds = holds?.items || [];
  S.cars = carsPage.items || [];
  S.carsMore = !!carsPage.next;
  const carById = new Map(S.cars.map((c) => [c.id, c]));
  const carName = (cid) => (carById.get(cid) || {}).name || cid;
  const when = (ms) => formatDateTime(ms, tz, { now: api.now() });
  const day = (ms) => formatDate(ms, tz, { weekday: false, year: true });

  const pickJob = () => {
    const want = ctx.query && ctx.query.import;
    S.job = (want && S.jobs.find((j) => j.id === want)) || S.jobs.find((j) => UNFINISHED.includes(j.status)) || S.jobs[0] || null;
  };
  pickJob();
  const putJob = (j) => {
    const i = S.jobs.findIndex((x) => x.id === j.id);
    if (i >= 0) S.jobs[i] = { ...S.jobs[i], ...j }; else S.jobs.unshift(j);
    if (S.job && S.job.id === j.id) S.job = { ...S.job, ...j };
  };
  const running = () => S.jobs.find((j) => UNFINISHED.includes(j.status)) || null;
  /** The job the step cards follow: an unfinished or failed one, or a done one asked for by its address. */
  const shownJob = () => {
    const j = S.job;
    return j && (UNFINISHED.includes(j.status) || j.status === 'failed' || (j.status === 'done' && ctx.query && j.id === ctx.query.import)) ? j : null;
  };
  const showJob = (j) => {
    S.job = j; S.match = null;
    ctx.setQuery({ import: j ? j.id : null });
    if (j && ['checking', 'ready'].includes(j.status)) loadMatch();
    paintAll(); follow();
  };

  /* ---------------------------------------------------------------- picking and uploading */
  const upErr = h('p', { class: 'form-error', role: 'alert', hidden: true });
  const showUpErr = (text) => { upErr.textContent = text || ''; upErr.hidden = !text; };

  function filePicker(text, sub) {
    const input = h('input', {
      class: 'im-file', type: 'file', multiple: true, accept: '.sql,.gz,.xz,.zip,.csv,.dump,.bck,.bak,.txt,text/csv,application/sql,application/gzip,application/x-xz,application/zip',
      'aria-label': text, onchange: () => { const fs = [...(input.files || [])]; input.value = ''; if (fs.length) chose(fs); },
    });
    const zone = h('label', { class: 'im-drop' }, input,
      h('span', { class: 'im-drop-ic', 'aria-hidden': 'true' }, icon('upload')),
      h('span', { class: 'im-drop-main' }, text),
      sub ? h('span', { class: 'im-drop-sub' }, sub) : null);
    zone.addEventListener('dragover', (e) => { e.preventDefault(); zone.classList.add('is-over'); });
    zone.addEventListener('dragleave', () => zone.classList.remove('is-over'));
    zone.addEventListener('drop', (e) => { e.preventDefault(); zone.classList.remove('is-over'); const fs = [...((e.dataTransfer && e.dataTransfer.files) || [])]; if (fs.length) chose(fs); });
    return zone;
  }

  async function chose(files) {
    showUpErr(null);
    const j = running();
    if (j && j.status === 'uploading' && !up.active) {
      // going on: each file of the job still to send is matched to a chosen file by name and size
      const left = jobFiles(j).filter((f) => fileDone(f) < f.bytes_total);
      const map = new Map(), missing = [];
      for (const f of left) {
        const file = files.find((x) => x.name === f.name && x.size === f.bytes_total);
        if (file) map.set(f.index, file); else missing.push(f);
      }
      if (missing.length) {
        const names = missing.slice(0, 3).map((f) => `${f.name} (${bytes(f.bytes_total)})`).join(', ') + (missing.length > 3 ? ` and ${count(missing.length - 3)} more` : '');
        showUpErr(`${missing.length === 1 ? 'A file' : `${count(missing.length)} files`} this upload began with ${missing.length === 1 ? 'is' : 'are'} not among those picked: ${names}. Pick ${jobFiles(j).length === 1 ? 'that file' : 'the same files'} to go on, or discard this upload first.`);
        return;
      }
      up.files = map; S.job = j;
      upload();
      return;
    }
    if (j) { showUpErr('An import is not finished yet. Finish it or discard it before you upload more files.'); return; }
    try {
      const created = await ctx.api.post('imports', { files: files.map((f) => ({ name: f.name, bytes_total: f.size })) });
      if (!ctx.alive()) return;
      putJob(created);
      S.job = S.jobs.find((x) => x.id === created.id);
      S.match = null;
      up.off = new Map();
      up.files = new Map(jobFiles(S.job).map((f, i) => [f.index, files.find((x) => x.name === f.name && x.size === f.bytes_total) || files[i]]));
      ctx.setQuery({ import: created.id });
      paintAll();
      upload();
    } catch (e) {
      if (!ctx.alive() || api.isAbort(e)) return;
      showUpErr(e.code === 'import_running' ? 'Another import is not finished yet, perhaps from another tab. Reload this page to see it.' : fieldsText(e));
    }
  }

  async function upload() {
    if (up.active || !up.files || !S.job) return;
    if (!(globalThis.crypto && crypto.subtle)) { showUpErr('This browser cannot work out the SHA-256 of each chunk here. Open Sentry Mate over HTTPS and try again.'); return; }
    const jid = S.job.id;
    Object.assign(up, { active: true, paused: false, error: null, t0: performance.now(), b0: upDone(S.job), ctl: new AbortController() });
    const signal = up.ctl.signal;
    ctx.onCleanup(() => up.ctl && up.ctl.abort());
    paintAll();
    const max = S.job.chunk_max_bytes || CHUNK_DEFAULT;
    let halt = false, gone = false;
    const stop = (text) => { if (!halt) { halt = true; up.error = text; if (up.ctl) up.ctl.abort(); } };
    const queue = jobFiles(S.job).filter((f) => fileDone(f) < f.bytes_total && up.files.get(f.index));
    const cur = (index) => jobFiles(S.job).find((f) => f.index === index);

    // one file: its chunks one after another, from where the server has it
    async function sendFile(jf) {
      const file = up.files.get(jf.index);
      const where = jf.upload_url || api.apiPath`imports/${jid}/files/${jf.index}`;
      let off = fileDone(jf), tries = 0;
      while (off < file.size && ctx.alive() && !up.paused && !halt) {
        try {
          const buf = await file.slice(off, Math.min(file.size, off + max)).arrayBuffer();
          const sha = hex(await crypto.subtle.digest('SHA-256', buf));
          const ans = await ctx.api.patch(where, buf, {
            signal,
            headers: { 'Content-Type': 'application/offset+octet-stream', 'Upload-Offset': String(off), 'Upload-Length': String(file.size), 'Upload-SHA256': sha },
          });
          if (!ctx.alive()) return;
          off = (ans && ans.offset) != null ? ans.offset : off + buf.byteLength;
          up.off.set(jf.index, off);
          if (ans) putJob(ans);
          tries = 0;
          paintSteps();
        } catch (e) {
          if (!ctx.alive() || api.isAbort(e) || up.paused || halt) return;
          if (e.code === 'wrong_offset' || e.code === 'not_uploading') {
            try {
              const j = await ctx.api.get(api.apiPath`imports/${jid}`);
              putJob(j);
              if (j.status !== 'uploading') { gone = true; return; }
              const f = cur(jf.index);
              off = f ? f.uploaded_bytes || 0 : off;
              up.off.set(jf.index, off);
              continue;
            } catch (e2) { if (api.isAbort(e2)) return; }
          }
          tries++;
          const again = e.code === 'checksum_mismatch' || e.status === 0 || e.status >= 500 || e.status === 429;
          if (again && tries <= 5) { await wait(Math.min(16000, 1000 * 2 ** (tries - 1)), signal); continue; }
          stop(e.code === 'checksum_mismatch' ? `A chunk of ${jf.name} kept arriving damaged. Check the connection, then go on: the upload resumes where it stopped.`
            : e.status === 0 ? 'The server stopped answering. Go on when it is back: the upload resumes where it stopped.'
              : e.code === 'no_such_import' ? 'This upload was discarded, perhaps from another tab.' : `${jf.name}: ${fieldsText(e)}`);
          return;
        }
      }
    }
    // up to PARALLEL files at a time
    await Promise.all(Array.from({ length: Math.min(PARALLEL, queue.length) }, async () => {
      while (queue.length && ctx.alive() && !up.paused && !halt && !gone) await sendFile(queue.shift());
    }));
    up.active = false;
    up.ctl = null;
    if (!ctx.alive()) return;
    const left = S.job && S.job.id === jid ? jobFiles(S.job).some((f) => fileDone(f) < f.bytes_total) : false;
    if (left || (S.job && S.job.id === jid && S.job.status === 'uploading')) {
      paintAll();
      await syncJob(jid);
      if (!ctx.alive() || up.active) return;
    }
    if (S.job && S.job.id === jid && S.job.status !== 'uploading') {
      if (up.files) {
        const n = jobFiles(S.job).length;
        up.files = null; up.error = null; up.off = new Map();
        ctx.toast(`${n === 1 ? 'The whole file is' : `All ${count(n)} files are`} up. The server checks ${n === 1 ? 'it' : 'them'} now; you can leave this page.`, { kind: 'info' });
      }
      if (S.job.status === 'ready' && !S.match) loadMatch();
    }
    paintAll();
    follow();
  }

  async function syncJob(jid) {
    try {
      const fresh = await ctx.api.get(api.apiPath`imports/${jid}`);
      if (ctx.alive()) putJob(fresh);
    } catch (e) {
      if (!ctx.alive() || api.isAbort(e)) return;
      if (e.code === 'no_such_import') {
        S.jobs = S.jobs.filter((x) => x.id !== jid);
        if (S.job && S.job.id === jid) { up.files = null; up.off = new Map(); S.match = null; pickJob(); }
        ctx.toast('This upload was discarded, perhaps from another tab.', { kind: 'info' });
      }
    }
  }
  const pauseUpload = () => { up.paused = true; if (up.ctl) up.ctl.abort(); paintAll(); };

  /* ---------------------------------------------------------------- following a job */
  let pollTimer = null;
  ctx.onCleanup(() => clearTimeout(pollTimer));
  // what the step cards show of a job: they are drawn again only when it changes (and not while you type in them)
  const sig = (x) => [x.status, x.bytes_read, x.uploaded_bytes, x.cars_to_match, x.checked_ms].join('|');
  function follow() {
    clearTimeout(pollTimer);
    const j = S.job;
    if (!j) return;
    const elsewhere = j.status === 'uploading' && !up.active && !up.files;
    if (!MOVING.includes(j.status) && !elsewhere && j.status !== 'ready') return;
    pollTimer = setTimeout(async () => {
      if (!ctx.alive() || !S.job || S.job.id !== j.id) return;
      try {
        const was = S.job.status, toMatch = S.job.cars_to_match, before = sig(S.job);
        const fresh = await ctx.api.get(api.apiPath`imports/${j.id}`);
        if (!ctx.alive() || !S.job || S.job.id !== j.id) return;
        putJob(fresh);
        if (fresh.status === 'ready' && was === 'ready' && fresh.cars_to_match !== toMatch) {
          await loadMatch();
        } else if (fresh.status !== was) {
          if (fresh.status === 'ready') ctx.toast(isCan(fresh) ? 'The zip is checked. Start the import when the report looks right.' : 'The file is checked. Match its cars, then start the import.', { kind: 'info' });
          if (fresh.status === 'done') { ctx.toast(isCan(fresh) ? 'The import is done. The logs are in as the car’s own SMO data.' : 'The import is done. The history shows on every page.'); loadOverlaps(); }
          if (fresh.status === 'failed') ctx.toast('The import failed. The reason is on this page.', { kind: 'error' });
          if (['checking', 'ready'].includes(fresh.status) && !S.match && !isCan(fresh)) loadMatch();
          paintAll();
        } else if (sig(fresh) !== before && !(stepsBox.contains(document.activeElement) && document.activeElement.matches('input, select, textarea'))) paintSteps();
      } catch (e) {
        if (!ctx.alive() || api.isAbort(e)) return;
        if (e.code === 'no_such_import') {
          S.jobs = S.jobs.filter((x) => x.id !== j.id);
          ctx.toast('This import was discarded, perhaps from another tab.', { kind: 'info' });
          showJob(null);
          return;
        }
      }
      follow();
    }, elsewhere || j.status === 'ready' ? POLL_MS * 2 : POLL_MS);
  }

  /* ---------------------------------------------------------------- matching the cars */
  async function loadMatch() {
    const j = S.job;
    if (!j || isCan(j)) return;
    try {
      const ans = await ctx.api.get(api.apiPath`imports/${j.id}/cars`);
      if (!ctx.alive() || !S.job || S.job.id !== j.id) return;
      S.match = ans.items || [];
      S.matchErr = null;
    } catch (e) {
      if (api.isAbort(e)) return;
      S.match = null;
      S.matchErr = e.code === 'not_checked' ? null : e;
    }
    if (ctx.alive()) paintSteps();
  }

  function matchOptions(row) {
    const taken = new Set((S.match || []).filter((x) => x.tm_car !== row.tm_car && isCar(x.match)).map((x) => x.match));
    const opts = [];
    if (row.match == null) opts.push(['', 'Not matched yet']);
    const sug = row.suggested && carById.get(row.suggested);
    if (sug) opts.push([sug.id, sug.name, 'Same VIN prefix', state.isSold(sug)]);
    for (const c of S.cars) if (c.id !== row.suggested) opts.push([c.id, c.name, taken.has(c.id) ? 'Already matched to another car of the file' : null, state.isSold(c)]);
    if (isCar(row.match) && !carById.has(row.match)) opts.push([row.match, row.match]);
    opts.push(['new', 'Add as a new car', 'Made with this name and VIN prefix'], ['leave_out', 'Leave out', 'Nothing of this car is imported']);
    return opts;
  }

  async function setMatch(row, value) {
    if (!value) throw new Error('nothing');
    try {
      const ans = await ctx.api.put(api.apiPath`imports/${S.job.id}/cars`, { items: [{ tm_car: row.tm_car, match: value }] });
      if (!ctx.alive()) return;
      S.match = ans.items || S.match;
      S.matchErr = null;
      putJob(await ctx.api.get(api.apiPath`imports/${S.job.id}`));
      if (ctx.alive()) paintSteps();
    } catch (e) {
      if (!ctx.alive() || api.isAbort(e)) throw e;
      S.matchErr = { row: row.tm_car, text: e.code === 'not_matchable' ? 'Cars can be matched only while the file is checked and the import has not started.' : fieldsText(e) };
      paintSteps();
      throw e;
    }
  }

  function matchRows(j) {
    if (!S.match) {
      if (S.matchErr) return said('bad', `The cars in the file could not be read: ${errorWords(S.matchErr).text}`);
      return j.status === 'checking' ? said('info', 'The cars in the file show here as the check finds them.') : null;
    }
    const can = ['checking', 'ready'].includes(j.status);
    const rows = S.match.map((r) => {
      const sub = [`VIN prefix ${r.vin_prefix || 'unknown'}`, `${cnt(r.drives)} drives`, `${cnt(r.charges)} charges`,
        r.first_ms ? `${day(r.first_ms)} to ${day(r.last_ms)}` : null].filter(Boolean).join(' · ');
      const row = pickRow({
        name: r.name || `${isTf(j) ? 'TeslaFi' : 'TeslaMate'} car ${r.tm_car}`, sub, value: r.match == null ? '' : r.match, options: matchOptions(r),
        sheetTitle: `Becomes: ${r.name || `car ${r.tm_car}`}`, disabled: !can,
        sheetNote: 'A car with the same VIN prefix is matched for you. Each car here takes at most one car of the file.',
        onpick: (v) => setMatch(r, v),
      });
      const err = S.matchErr && S.matchErr.row === r.tm_car ? h('p', { class: 'field-error', role: 'alert' }, S.matchErr.text) : null;
      return [row, err];
    });
    return [
      group('Cars in the file', rows),
      S.carsMore ? note(`Only the first ${count(S.cars.length)} cars here are offered. Add a car further down as a new car, or rename it in Settings: Cars.`) : null,
    ];
  }

  /* ---------------------------------------------------------------- start, discard */
  const startErr = h('p', { class: 'form-error', role: 'alert', hidden: true, cover: 'web.import.start-error' });
  const failRegion = h('div', { class: 'im-fail', role: 'alert', hidden: true, cover: 'web.import.run-error' });

  async function startImport(b) {
    startErr.hidden = true;
    b.disabled = true;
    try {
      let body = {};
      if (isTf(S.job)) {
        const t = tfSettings(S.job);
        const zone = String(t.time_zone || '').trim();
        if (!zone || !isValidZone(zone)) {
          b.disabled = false;
          startErr.textContent = 'Type the time zone TeslaFi shows your times in (TeslaFi: Settings, Account), for example America/Chicago. Pick it from the list.';
          startErr.hidden = false;
          return;
        }
        body = { time_zone: zone, distance_unit: t.distance_unit, temperature_unit: t.temperature_unit };
      } else if (!isCan(S.job)) {
        const cur = String(S.currency || '').trim().toUpperCase();
        if (cur) body = { currency: cur };
      }
      putJob(await ctx.api.post(api.apiPath`imports/${S.job.id}/start`, body));
      if (!ctx.alive()) return;
      ctx.toast('The import has started. You can leave this page; it carries on.', { kind: 'info' });
      paintAll();
      follow();
    } catch (e) {
      if (!ctx.alive() || api.isAbort(e)) return;
      b.disabled = false;
      startErr.textContent = startWords(e);
      startErr.hidden = false;
    }
  }

  async function discard(j) {
    if (!j) return;
    const what = j.status === 'done' ? 'The import and its report leave the list. The history it brought in stays.'
      : j.status === 'importing' ? (isCan(j) ? 'The import stops. Logs already stored stay, checked and signed; importing the zip again skips them.' : 'The import stops. Rows already stored stay and show as imported; running the file again skips them.')
        : j.status === 'uploading' ? `The ${bytes(j.uploaded_bytes)} uploaded so far are thrown away. Nothing was stored.`
          : 'The uploaded file and its check are thrown away. Nothing from it was stored.';
    const ok = await confirmSheet({ title: j.status === 'done' ? 'Remove this import from the list?' : 'Discard this import?', text: what, yes: j.status === 'done' ? 'Remove' : 'Discard', danger: true });
    if (!ok || !ctx.alive()) return;
    if (S.job && S.job.id === j.id) { up.paused = true; if (up.ctl) up.ctl.abort(); up.files = null; up.off = new Map(); up.error = null; }
    try {
      await ctx.api.del(api.apiPath`imports/${j.id}`);
      if (!ctx.alive()) return;
      S.jobs = S.jobs.filter((x) => x.id !== j.id);
      ctx.toast(j.status === 'done' ? 'Removed from the list. Its history stays.' : 'Import discarded.');
      if (S.job && S.job.id === j.id) { ctx.setQuery({ import: null }); pickJob(); S.match = null; if (S.job && ['checking', 'ready'].includes(S.job.status)) loadMatch(); }
      paintAll();
      follow();
    } catch (e) {
      if (!ctx.alive() || api.isAbort(e)) return;
      ctx.toast(`Not discarded: ${errorWords(e).text}`, { kind: 'error' });
    }
  }

  /* ---------------------------------------------------------------- the step cards */
  const CAN_STEPS = [
    ['pick', 'Pick the file', 'Sentry Six Manager’s CAN export bundle (.zip), as it downloaded it. It goes over HTTPS to this server only.'],
    ['check', 'Check', 'The server reads every log of the zip and checks its CAN Bridge signature, as it checks an upload. Nothing is stored yet.'],
    ['match', 'Start', 'Each log goes to the car its car key belongs to; there are no cars to match. Start when the report below looks right.', 'web.import.start'],
    ['import', 'Import', 'Each log is stored and checked as if the CAN Bridge had uploaded it, then decoded into drives and charges. You can leave this page.'],
    ['done', 'Done', 'The logs are in as the car’s own SMO data: drives, charges and the timeline come from them as from any upload.'],
  ];
  const TF_STEPS = [
    ['pick', 'Pick the file', 'TeslaFi’s data download: one month’s CSV, several months in one .zip, or a file gzipped or xz’d. It goes over HTTPS to this server only; nothing is sent to TeslaFi.'],
    ['check', 'Check', 'The server reads every poll of the files and works out drives, charges, states and updates. Nothing is stored yet.'],
    ['match', 'Match the car, confirm the time zone and units, then start', 'TeslaFi writes its times without a zone and in your units, so confirm both. Then Start stores the checked rows.', 'web.import.start'],
    ['import', 'Import', 'The checked rows are stored. You can leave this page.'],
    ['done', 'Done', 'The history shows on every page, marked “Imported from TeslaFi”.'],
  ];
  const STEPS = [
    ['pick', 'Pick the files', 'The .sql file of a TeslaMate database backup, plain or compressed (.gz, .xz), TeslaFi’s data download (its monthly .csv files, several at once, or a .zip of them), or the .zip of Sentry Six Manager’s CAN export. They go over HTTPS to this server only.'],
    ['check', 'Check', 'The server reads every file whole and checks every row. Nothing is stored yet.'],
    ['match', 'Match the cars, then start', 'Each car of the file goes to a car here, a new car, or is left out. Then Start stores the checked rows.', 'web.import.start'],
    ['import', 'Import', 'The checked rows are stored, a few minutes per gigabyte. You can leave this page.'],
    ['done', 'Done', 'The history shows on every page, marked as imported.'],
  ];
  const stepsBox = h('ol', { class: 'mv-steps im-steps', 'aria-label': 'Steps of an import' });
  const stepsTile = tile({ w: 2, title: 'This import', icon: 'upload', id: 'im-steps', cls: 'im-steps-tile' }, stepsBox, startErr, failRegion);

  function atOf(j) {
    if (!j) return { at: 0, st: 'now' };
    if (j.status === 'failed') return { at: j.checked_ms ? 3 : upDone(j) >= j.bytes_total ? 1 : 0, st: 'stuck' };
    return { at: { uploading: 0, checking: 1, ready: 2, importing: 3, done: 4 }[j.status] ?? 0, st: j.status === 'done' ? 'done' : 'now' };
  }

  function stepOutput(j, key) {
    if (!j) return [filePicker('Pick the files, or drop them here', 'A TeslaMate backup (.sql, .sql.gz, .sql.xz), TeslaFi’s data download (TeslaFi<month><year>.csv, several months at once, or a .zip of months) or Sentry Six Manager’s CAN export (.zip). Several files are fine: drop all of TeslaFi’s monthly CSVs at once and they become one import. Up to 64 GB each. Choosing them starts the upload.'), upErr];
    if (j.status === 'failed') return null;
    if (key === 'pick') {
      const list = jobFiles(j);
      const done = upDone(j);
      let speed = null, eta = null;
      if (up.active) {
        const s = (performance.now() - up.t0) / 1000;
        if (s > 2 && done > up.b0) { speed = (done - up.b0) / s; eta = (j.bytes_total - done) / speed; }
      }
      const many = list.length > 1;
      const acts = up.active ? btn('Pause the upload', { kind: 'soft', icon: 'minus', onclick: pauseUpload })
        : up.files ? btn('Go on with the upload', { kind: 'primary', icon: 'upload', onclick: upload })
          : filePicker(many ? `Pick the same ${count(list.length)} files again to go on` : `Pick ${j.name} again to go on`,
            `It goes on from ${bytes(done)}: chunks already on the server are not sent again.${many ? ' Files already up need not be picked.' : ''}`);
      const perFile = many ? group(null, list.map((f) => {
        const d = fileDone(f), whole = d >= f.bytes_total;
        return infoRow({
          icon: 'file', tone: whole ? 'teal' : null, name: f.name, cls: 'im-up-row',
          sub: `${bytes(d)} of ${bytes(f.bytes_total)}`,
          side: whole ? chip('Done', { tone: 'teal' }) : h('span', { class: 'im-n' }, `${Math.floor(pct(d, f.bytes_total))} %`),
        });
      })) : null;
      return [h('p', { class: 'im-file-name' }, icon('file'), h('span', null, many ? `${count(list.length)} files` : j.name)),
        progressBar({ bytes_done: done, bytes_total: j.bytes_total, bytes_per_s: speed, eta_s: eta,
          files_total: many ? list.length : 0, files_done: list.filter((f) => fileDone(f) >= f.bytes_total).length }, { label: 'Uploaded' }),
        perFile,
        h('div', { class: 'im-acts' }, acts), upErr,
        up.error ? said('bad', up.error) : null];
    }
    if (key === 'check' || key === 'import') {
      return [progressBar({ bytes_done: j.bytes_read || 0, bytes_total: j.bytes_total }, { label: key === 'check' ? 'Checked' : 'Imported' }),
        key === 'check' && !isCan(j) ? matchRows(j) : null];
    }
    if (key === 'match' && isCan(j)) {
      const b = btn('Start the import', { kind: 'primary', icon: 'arrowRight' });
      b.addEventListener('click', () => startImport(b));
      const L = (j.report && j.report.logs) || {};
      return [h('div', { class: 'im-acts' }, b, h('span', { class: 'im-why' }, `${count(L.to_import || 0)} ${L.to_import === 1 ? 'log' : 'logs'} to store${L.already_here ? `, ${count(L.already_here)} here already` : ''}${L.refused ? `, ${count(L.refused)} refused` : ''}.`))];
    }
    if (key === 'match' && isTf(j)) return tfStart(j);
    if (key === 'match') {
      const unmatched = j.cars_to_match ?? (S.match ? S.match.filter((x) => x.match == null).length : null);
      const can = unmatched === 0;
      const cur = field({ label: 'Currency of TeslaMate’s costs', name: 'currency', value: S.currency || '',
        hint: 'A code such as EUR. TeslaMate stores none; without it, imported charges keep no cost and places no price.',
        attrs: { maxlength: 3, autocomplete: 'off', spellcheck: 'false', placeholder: 'EUR', class: 'im-cur' } });
      cur.control.addEventListener('input', () => { S.currency = cur.control.value; });
      const b = btn('Start the import', { kind: 'primary', icon: 'arrowRight', disabled: !can });
      b.addEventListener('click', () => startImport(b));
      return [matchRows(j), cur,
        h('div', { class: 'im-acts' }, b, h('span', { class: 'im-why' }, can ? 'Every car has its match.' : unmatched ? `Match ${count(unmatched)} more ${unmatched === 1 ? 'car' : 'cars'} first.` : 'Reading the cars…'))];
    }
    if (key === 'done') return null;
    return null;
  }

  /** The time zone and units a TeslaFi import is read in: the owner's choice, else what the check used. */
  function tfSettings(j) {
    if (!S.tf || S.tf.job !== j.id) {
      const rep = j.report || {};
      S.tf = { job: j.id, time_zone: (rep.time_zone && rep.time_zone.name) || (S.cars[0] && S.cars[0].time_zone) || '', distance_unit: (rep.units && rep.units.distance) || 'mi', temperature_unit: (rep.units && rep.units.temperature) || 'c' };
    }
    return S.tf;
  }
  /** TeslaFi's start step: the car, the time zone (asked: the file cannot tell it), the units, Start. */
  function tfStart(j) {
    const unmatched = j.cars_to_match ?? (S.match ? S.match.filter((x) => x.match == null).length : null);
    const can = unmatched === 0;
    const rep = j.report || {};
    const t = tfSettings(j);
    // until the owner types one: the zone of the car it is matched to, else what the check used
    if (!t.typed) {
      const m = (S.match || []).find((x) => isCar(x.match));
      const cz = m && (carById.get(m.match) || {}).time_zone;
      if (cz && (!rep.time_zone || rep.time_zone.from !== 'setting')) t.time_zone = cz;
    }
    const zid = `im-zones-${j.id}`;
    const zf = field({ label: 'TeslaFi’s time zone', name: 'time_zone', value: t.time_zone,
      hint: `TeslaFi writes its times without a zone: they are read in this one. Use the zone in TeslaFi’s Settings, Account.${rep.time_zone && rep.time_zone.name ? ` The check used ${rep.time_zone.name.replace(/_/g, ' ')}, ${ZONE_FROM[rep.time_zone.from] || 'its own'}.` : ''}`,
      attrs: { list: zid, autocomplete: 'off', spellcheck: 'false', placeholder: 'America/Chicago' } });
    zf.append(h('datalist', { id: zid }, (() => { try { return Intl.supportedValuesOf('timeZone'); } catch { return []; } })().map((z) => h('option', { value: z }))));
    zf.control.addEventListener('input', () => { t.time_zone = zf.control.value; t.typed = true; });
    const u = rep.units || {};
    const dist = segmented({ label: 'Distances in the file', name: 'distance_unit', value: t.distance_unit, options: [['mi', 'Miles'], ['km', 'Kilometres']], onchange: (v) => { t.distance_unit = v; } });
    const temp = segmented({ label: 'Temperatures in the file', name: 'temperature_unit', value: t.temperature_unit, options: [['c', '°C'], ['f', '°F']], onchange: (v) => { t.temperature_unit = v; } });
    const b = btn('Start the import', { kind: 'primary', icon: 'arrowRight', disabled: !can });
    b.addEventListener('click', () => startImport(b));
    return [matchRows(j), zf,
      h('div', { class: 'im-units' }, dist, temp),
      note(`The check read ${u.distance === 'km' ? 'kilometres' : 'miles'} (${UNIT_FROM[u.distance_from] || 'its own guess'}) and ${u.temperature === 'f' ? '°F' : '°C'} (${UNIT_FROM[u.temperature_from] || 'its own guess'}). TeslaFi’s base columns are the Tesla API’s: miles and °C. Months imported before in another zone or other units are read again and replaced.`),
      h('div', { class: 'im-acts' }, b, h('span', { class: 'im-why' }, can ? 'The car has its match.' : unmatched ? `Match ${count(unmatched)} more ${unmatched === 1 ? 'car' : 'cars'} first.` : 'Reading the cars…'))];
  }

  function paintSteps() {
    const j = shownJob();
    const { at, st } = atOf(j);
    stepsBox.replaceChildren(...(isCan(j) ? CAN_STEPS : isTf(j) ? TF_STEPS : STEPS).map(([key, name, text, cov], i) => {
      const mine = st === 'done' || i < at ? 'done' : i === at ? st : 'later';
      const word = mine === 'done' ? 'done' : mine === 'now' ? 'now' : mine === 'stuck' ? 'failed' : 'to do';
      const out = mine === 'now' ? stepOutput(j, key) : null;
      return h('li', { class: ['mv-step', `is-${mine}`], 'aria-current': mine === 'now' || mine === 'stuck' ? 'step' : null, cover: cov || null },
        h('span', { class: 'mv-mark', 'aria-hidden': 'true' }, mine === 'done' ? icon('check') : mine === 'stuck' ? icon('warning') : String(i + 1)),
        h('div', { class: 'mv-step-body' },
          h('p', { class: 'mv-step-name' }, name, h('span', { class: 'mv-step-said' }, word)),
          mine !== 'done' ? h('p', { class: 'mv-step-text' }, text) : null,
          out ? h('div', { class: 'mv-step-out' }, out) : null));
    }));
    // the side of the tile: what the job is, and the one way to drop it
    const head = stepsTile.querySelector('.tile-head');
    let side = head.querySelector('.tile-side');
    if (!side) { side = h('div', { class: 'tile-side' }); head.append(side); }
    side.replaceChildren(...(j ? [statusChip(j.status), btn(j.status === 'done' ? 'Remove' : 'Discard', { kind: 'ghost', icon: 'trash', cls: 'btn-sm', onclick: () => discard(j) })] : []));
    // a failed job: why, and what to do
    if (j && j.status === 'failed') {
      const code = j.error && j.error.code;
      failRegion.replaceChildren(callout('bad', j.checked_ms ? 'The import stopped part way.' : 'The file did not pass its check.',
        h('p', null, FAILURES[code] || (j.error && j.error.message) || 'The server gave no reason.'),
        h('p', null, j.checked_ms ? (isCan(j) ? 'Logs already stored stay, checked and signed. Start a new import of the same zip to bring the rest: what is here is skipped.' : 'Rows already stored stay. Discard this import, then upload the file again: rows already in are skipped.') : 'Nothing from this file was stored. Discard this import, then upload a good file.')));
      failRegion.hidden = false;
    } else { failRegion.replaceChildren(); failRegion.hidden = true; }
    // a done job opened by its address: a way to start another
    if (j && j.status === 'done') {
      stepsBox.append(h('li', { class: 'im-again' }, btn('Import another file', { kind: 'primary', icon: 'plus', onclick: () => showJob(null) })));
    }
    // another job runs while this one is shown
    const r = running();
    if (r && (!j || j.id !== r.id)) stepsBox.append(h('li', { class: 'im-again' }, said('info', `${r.name} is not finished.`), btn('Show it', { kind: 'soft', cls: 'btn-sm', onclick: () => showJob(r) })));
  }

  /* ---------------------------------------------------------------- what to know */
  const knowTile = tile({ w: 2, title: 'Good to know', icon: 'info', cls: 'im-know' },
    h('div', { class: 'im-know-list' },
      h('div', { cover: 'web.import.restart-note' },
        h('h3', { class: 'set-q' }, 'Safe to leave, safe to repeat'),
        h('p', { class: 'set-a' }, 'Once the file is up, the server does the rest: close this page and come back. An upload that stops goes on from the last chunk the server has. If the server restarts part way, nothing is left half stored: the import runs again and skips every row already in, so the same file twice never stores a drive twice.')),
      h('div', { cover: 'web.import.docs-overlap' },
        h('h3', { class: 'set-q' }, 'Where both histories cover the same time'),
        h('p', { class: 'set-a' }, 'Imported history is kept whole. Where it overlaps live history (SMO, the Tesla API or TeslaFi), the live side is shown at first; the imported rows are hidden, never deleted. Where a TeslaMate and a TeslaFi import cover the same days, the bigger one is shown at first. Switch a span below and every page shows that side for it.')),
      h('div', null,
        h('h3', { class: 'set-q' }, 'TeslaFi’s data'),
        h('p', { class: 'set-a' }, 'In TeslaFi, Settings, Account, Advanced, “Download TeslaFi Data” gives one CSV per car and month. Upload one month, or several in one .zip. TeslaFi writes local times without a zone, so you confirm the zone (and the units) before the import starts. Importing the same months again adds nothing; in another zone or other units they are read again and replaced.')),
      h('div', null,
        h('h3', { class: 'set-q' }, 'Sentry Six Manager’s CAN export'),
        h('p', { class: 'set-a' }, 'The .zip of its CAN export bundle holds each log as the CAN Bridge signed it. Each one is checked against its signature and the car key’s signing key before it is stored, then counts as the car’s own SMO data, exactly as an upload would. A log already here is never stored twice, so the same zip can go in again.')),
      h('div', { cover: 'web.import.docs-backfill' },
        h('h3', { class: 'set-q' }, 'Addresses'),
        h('p', { class: 'set-a' }, 'The backup carries TeslaMate’s own addresses and places, so imported drives keep their street names. Spots without one are filled in by the server’s address lookup after the import, a few at a time.')),
      h('div', { cover: 'faq.no-backfill' },
        h('h3', { class: 'set-q' }, 'Gaps'),
        h('p', { class: 'set-a' }, 'The CAN Bridge keeps its logs and sends them late, so most gaps fill by themselves. Time nothing recorded shows as Unknown, never as asleep. An import fills only time before the CAN Bridge, or spans another logger recorded: Tesla keeps no history to fetch.'))));

  /* ---------------------------------------------------------------- the report */
  const reportBox = h('div', { class: 'contents' });
  function paintReport() {
    const j = S.job && S.job.report ? S.job : S.jobs.find((x) => x.report && x.status === 'done');
    if (!j || !j.report) {
      reportBox.replaceChildren(
        emptyTile({ icon: 'bars', title: 'No report yet', text: 'Once a file is checked, its report shows here: rows read per table, rows imported, and how many were skipped and why.', cover: ['web.import.rejected-count', 'web.import.rejected.col-reason'] }));
      return;
    }
    const rep = j.report;
    if (isCan(j)) { paintCanReport(j); return; }
    if (isTf(j)) { paintTfReport(j); return; }
    const imported = rep.imported !== false && j.status === 'done';
    // a table or count the file does not have is null: a gap, never 0
    const tables = Object.entries(rep.tables || {}).map(([name, t]) => {
      t = t || {};
      const has = t.read != null;
      const skipped = has || t.skipped ? addUp(Object.values(t.skipped || {})) ?? (has ? 0 : null) : null;
      const blank = has || t.bad_values ? addUp(Object.values(t.bad_values || {})) ?? (has ? 0 : null) : null;
      return { name, read: has ? t.read : null, imported: imported ? t.imported ?? null : has ? t.read - (skipped || 0) : null, skipped, blank, closed: t.closed_at_last_reading || 0, t };
    });
    // old charges and states TeslaMate never ended: taken, ending at their last reading (not skipped)
    const closedWords = (r) => {
      const noun = { charging_processes: ['charge', 'charges'], states: ['state', 'states'] }[r.name] || ['row', 'rows'];
      return `${count(r.closed)} ${noun[r.closed === 1 ? 0 : 1]}`;
    };
    const closedRows = tables.filter((r) => r.closed);
    const one = closedRows.length === 1 && closedRows[0].closed === 1;
    const closedLine = closedRows.length ? h('p', { class: 'set-a' },
      `${closedRows.map(closedWords).join(' and ')} TeslaMate never ended ${imported ? (one ? 'was' : 'were') : 'will be'} taken, ending at ${one ? 'its' : 'their'} last reading.`) : null;
    const tot = (k) => addUp(tables.map((r) => r[k]));
    const reasons = [];
    for (const r of tables) {
      for (const [code, n] of Object.entries(r.t.skipped || {})) if (n) reasons.push({ table: r.name, kind: 'skipped', n, words: reasonWords(code) });
      for (const [col, n] of Object.entries(r.t.bad_values || {})) if (n) reasons.push({ table: r.name, kind: 'blank', n, words: blankWords(col) });
    }
    reasons.sort((a, b) => b.n - a.n);
    const skipped = tot('skipped');
    const sub = [j.name, (STATUS[j.status] || [j.status])[0], j.finished_ms ? when(j.finished_ms) : j.checked_ms ? `checked ${when(j.checked_ms)}` : null].filter(Boolean).join(' · ');

    const reasonTile = tile({ w: 2, title: 'Why rows were skipped', icon: 'warning', cover: 'web.import.rejected.col-reason' },
      reasons.length ? group(null, reasons.map((r) => infoRow({
        name: r.words[0], sub: `${tableName(r.table)} · ${r.words[1]}`, tone: r.kind === 'skipped' ? 'sun' : 'sky', icon: r.kind === 'skipped' ? 'minus' : 'edit',
        side: h('span', { class: 'im-n' }, count(r.n)),
      }))) : said('ok', 'Nothing was skipped: every row passed its checks.'),
      skipped ? note('A skipped row did not stop the import. The report keeps only counts and reasons, never a row’s values or positions.') : null);

    const tableTile = tile({ w: 2, title: 'Per table', icon: 'bars' },
      group(null, tables.map((r) => infoRow({
        name: tableName(r.name), sub: r.read == null ? 'Not in the file' : `${count(r.read)} read${r.skipped ? ` · ${count(r.skipped)} skipped` : ''}${r.blank ? ` · ${count(r.blank)} left blank` : ''}${r.closed ? ` · ${count(r.closed)} closed at last reading` : ''}`,
        side: nSide(r.imported),
      }))),
      note(imported ? 'The number on the right is the rows imported.' : 'The number on the right is the rows the import will store.'));

    // a car left out is not in the report (the server leaves it out too); the matching list still has it
    const cars = (rep.cars || []).filter((c) => c && c.match !== 'leave_out');
    const carTile = cars.length ? tile({ w: 2, title: 'Per car', icon: 'car' },
      group(null, cars.map((c) => {
        const here = isCar(c.match) ? carName(c.match) : c.match === 'new' ? 'a new car' : 'not matched';
        return infoRow({
          icon: 'car', tone: 'coral', name: c.name || `TeslaMate car ${c.tm_car}`,
          sub: [`Becomes ${here}`, `${cnt(c.drives)} drives · ${cnt(c.charges)} charges`].join(' · '),
          side: isCar(c.match) ? h('a', { class: 'tile-link', href: state.buildUrl('timeline', { car: c.match, period: rep.span ? { from: rep.span.from_ms, to: rep.span.to_ms } : null }) }, 'Timeline', icon('chevronRight')) : null,
        });
      }))) : null;

    // several files: what each one gave
    const uploads = Array.isArray(rep.uploads) ? rep.uploads : [];
    const filesTile = uploads.length ? tile({ w: 2, title: 'Files', icon: 'file', side: chip(`${count(uploads.length)} files`) },
      group(null, uploads.map((u) => infoRow({
        icon: 'file', name: u.name, cls: 'im-up-row',
        sub: `${cnt(u.rows_read)} rows read · ${cnt(u.rows_used)} used · ${cnt(u.rows_skipped)} skipped`,
        side: nSide(u.rows_used),
      }))),
      note('The number on the right is the rows used from that file.')) : null;

    reportBox.replaceChildren(
      tile({ w: 2, title: 'Report', icon: 'bars', side: statusChip(j.status), cls: 'im-report' },
        h('p', { class: 'im-report-of' }, sub),
        nums([
          ['Rows read', num(tot('read'), 'count', { size: 'm' })],
          [imported ? 'Rows imported' : 'Rows to import', num(tot('imported'), 'count', { size: 'm' })],
          ['Rows skipped', num(skipped, 'count', { size: 'm', cls: skipped ? 'im-warn' : null }), 'web.import.rejected-count'],
          ['Values left blank', num(tot('blank'), 'count', { size: 'm' })],
        ]),
        rep.span ? h('p', { class: 'set-a' }, h('b', null, `${day(rep.span.from_ms)} to ${day(rep.span.to_ms)}`), `: ${formatDuration(rep.span.to_ms - rep.span.from_ms, { style: 'long' })} of TeslaMate history in the file.`) : null,
        closedLine,
        note(imported ? 'Imported rows show on every page, marked as imported.' : 'Nothing is stored until the import is started.')),
      ...[filesTile, reasonTile, tableTile, carTile].filter(Boolean));
  }

  /** The report of a CAN export: logs read, stored or to store, here already and refused (and why), per car key. */
  function paintCanReport(j) {
    const rep = j.report;
    const imported = rep.imported === true && j.status === 'done';
    const L = rep.logs || {};
    const sub = [j.name, (STATUS[j.status] || [j.status])[0], j.finished_ms ? when(j.finished_ms) : j.checked_ms ? `checked ${when(j.checked_ms)}` : null].filter(Boolean).join(' · ');
    const refused = Object.entries(rep.refused || {}).filter(([, n]) => n).sort((a, b) => b[1] - a[1]);
    const smo = { sources: [{ kind: 'smo', label: 'Data from SMO' }] };
    const reportT = tile({ w: 2, title: 'Report', icon: 'bars', side: statusChip(j.status), cls: 'im-report' },
      h('p', { class: 'im-report-of' }, sub),
      nums([
        ['Logs read', num(L.read || 0, 'count', { size: 'm' })],
        [imported ? 'Logs stored' : 'Logs to store', num(imported ? L.imported || 0 : L.to_import || 0, 'count', { size: 'm' })],
        ['Here already', num(L.already_here || 0, 'count', { size: 'm' })],
        ['Refused', num(L.refused || 0, 'count', { size: 'm', cls: L.refused ? 'im-warn' : null }), 'web.import.rejected-count'],
      ]),
      h('p', { class: 'set-a' }, `${bytes(L.bytes || 0)} of CAN Bridge logs${L.sidecars_added ? `; ${count(L.sidecars_added)} missing sidecars added to logs here` : ''}.`),
      rep.span && rep.span.from_ms ? h('p', { class: 'set-a' }, h('b', null, `${day(rep.span.from_ms)} to ${day(rep.span.to_ms)}`), ': when the logs start.') : null,
      h('p', { class: 'src-line' }, srcPill(smo), h('span', { class: 'note' }, 'Stored logs count as the car’s own data, not as imported rows.')),
      (rep.warnings || []).length ? h('ul', { class: 'set-list' }, rep.warnings.map((w) => h('li', null, typeof w === 'string' ? w : w.message || w.code))) : null,
      note(imported ? 'A log already here (same name and SHA-256) is never stored twice.' : 'Nothing is stored until the import is started.'));
    const keysT = tile({ w: 2, title: 'Per car key', icon: 'key' },
      (rep.cars || []).length ? group(null, rep.cars.map((k) => {
        const st = KEY_STATUS[k.status] || [String(k.status || '').replace(/_/g, ' '), 'muted'];
        const gaps = (k.chain_gaps || 0) + (k.chain_forks || 0);
        return infoRow({
          icon: 'key', tone: st[1], name: h('code', { class: 'set-mono' }, k.key), cls: 'im-key-row',
          sub: [`${count(k.logs)} logs`, imported ? `${count(k.imported)} stored` : `${count(Math.max(0, k.logs - k.already_here - k.refused))} to store`,
            k.already_here ? `${count(k.already_here)} here already` : null, k.refused ? `${count(k.refused)} refused` : null,
            k.car ? `goes to ${k.car_name || carName(k.car)}` : null, k.first_ms ? `${day(k.first_ms)} to ${day(k.last_ms)}` : null,
            gaps ? `hash chain: ${count(k.chain_gaps || 0)} ${k.chain_gaps === 1 ? 'gap' : 'gaps'}, ${count(k.chain_forks || 0)} ${k.chain_forks === 1 ? 'fork' : 'forks'}` : null].filter(Boolean).join(' · '),
          side: chip(st[0], { tone: st[1] }),
        });
      })) : said('info', 'The zip holds no CAN Bridge log.'),
      note('A car key a car has goes to that car; a new one is handled like a new key’s first upload (Settings: Cars).'));
    const refusedT = tile({ w: 2, title: 'Why logs were refused', icon: 'warning', cover: 'web.import.rejected.col-reason' },
      refused.length ? group(null, refused.map(([code, n]) => infoRow({ icon: 'minus', tone: 'sun', name: canReason(code)[0], sub: canReason(code)[1], side: h('span', { class: 'im-n' }, count(n)) })))
        : said('ok', 'No log was refused: every log of the zip is signed by its car’s CAN Bridge and unchanged since.'),
      (rep.refused_logs || []).length ? h('div', { class: 'log im-refused-logs', role: 'list' }, rep.refused_logs.slice(0, 100).map((x) => h('div', { class: 'log-row', role: 'listitem' }, h('span', { class: 'log-k' }, x.name), h('span', { class: 'log-v' }, canReason(x.reason)[0])))) : null,
      refused.length ? note('A refused log is never stored.') : null);
    const f = rep.files || {};
    const filesT = tile({ w: 2, title: 'In the zip', icon: 'file' },
      group(null, [
        infoRow({ name: 'Entries', side: h('span', { class: 'im-n' }, count(f.entries || 0)) }),
        infoRow({ name: 'Candump text copies', sub: 'Not signed, so skipped.', side: h('span', { class: 'im-n' }, count(f.candump_text || 0)) }),
        infoRow({ name: 'ids.csv', side: h('span', { class: 'set-value' }, f.ids_csv ? 'Yes' : 'No') }),
        infoRow({ name: 'Sidecars without a log', side: h('span', { class: 'im-n' }, count(f.sidecars_without_log || 0)) }),
        rep.manifest ? infoRow({ name: 'Manifest', sub: [rep.manifest.vehicle_key ? `car key ${rep.manifest.vehicle_key}` : null, rep.manifest.logs != null ? `${count(rep.manifest.logs)} logs` : null].filter(Boolean).join(' · ') || null, side: h('span', { class: 'set-value' }, 'Yes') })
          : infoRow({ name: 'Manifest', side: h('span', { class: 'set-value' }, 'None') }),
      ]));
    reportBox.replaceChildren(reportT, keysT, refusedT, filesT);
  }

  /** The report of TeslaFi's data: polls read, used and skipped (and why), what they make, the units and zone, per car. */
  function paintTfReport(j) {
    const rep = j.report;
    const imported = rep.imported === true && j.status === 'done';
    const R = rep.rows || {}, F = rep.found || {};
    const sub = [j.name, (STATUS[j.status] || [j.status])[0], j.finished_ms ? when(j.finished_ms) : j.checked_ms ? `checked ${when(j.checked_ms)}` : null].filter(Boolean).join(' · ');
    const skipped = sum(R.skipped);
    const reasons = Object.entries(R.skipped || {}).filter(([, n]) => n).sort((a, b) => b[1] - a[1]);
    const blanks = Object.entries(R.bad_values || {}).filter(([, n]) => n).sort((a, b) => b[1] - a[1]);
    const u = rep.units || {}, z = rep.time_zone || {};
    const tfSrc = { sources: [{ kind: 'import', site: 'teslafi', label: 'Imported from TeslaFi' }] };
    const reportT = tile({ w: 2, title: 'Report', icon: 'bars', side: statusChip(j.status), cls: 'im-report' },
      h('p', { class: 'im-report-of' }, sub),
      nums([
        ['Polls read', num(R.read || 0, 'count', { size: 'm' })],
        [imported ? 'Polls used' : 'Polls to use', num(R.used || 0, 'count', { size: 'm' })],
        ['Polls skipped', num(skipped, 'count', { size: 'm', cls: skipped ? 'im-warn' : null }), 'web.import.rejected-count'],
        ['Values left blank', num(sum(R.bad_values), 'count', { size: 'm' })],
      ]),
      rep.span && rep.span.from_ms ? h('p', { class: 'set-a' }, h('b', null, `${day(rep.span.from_ms)} to ${day(rep.span.to_ms)}`), `: ${formatDuration(rep.span.to_ms - rep.span.from_ms, { style: 'long' })} of TeslaFi polls used.`) : null,
      h('p', { class: 'src-line' }, srcPill(tfSrc), h('span', { class: 'note' }, 'Drive energy is estimated: TeslaFi’s power once a minute, added up.')),
      (rep.warnings || []).length ? h('ul', { class: 'set-list' }, rep.warnings.map((w) => h('li', null, typeof w === 'string' ? w : w.message || w.code))) : null,
      note(imported ? 'Imported rows show on every page, marked “Imported from TeslaFi”. Importing the same months again adds nothing.' : 'Nothing is stored until the import is started.'));
    const foundT = tile({ w: 2, title: imported ? 'What it brought in' : 'What the polls make', icon: 'route' },
      group(null, [
        ['Drives', F.drives], ['Charges', F.charges], ['Car states', F.states], ['Software updates', F.updates], ['Track points', F.positions], ['Charge-curve points', F.charge_points],
      ].map(([k, n]) => infoRow({ name: k, side: h('span', { class: 'im-n' }, count(n)) }))),
      F.replaced ? note(`${count(F.replaced)} halves of drives or charges over a month’s end were joined with their other half.`) : null,
      F.read_again ? said('warn', `${count(F.read_again)} ${F.read_again === 1 ? 'stretch' : 'stretches'} imported before in another zone or other units ${F.read_again === 1 ? 'was' : 'were'} read again: what ${F.read_again === 1 ? 'it' : 'they'} stored is replaced.`) : null);
    const readT = tile({ w: 2, title: 'How it was read', icon: 'clock' },
      facts([
        ['Time zone', z.name ? `${z.name.replace(/_/g, ' ')}, ${ZONE_FROM[z.from] || z.from}` : 'Not known'],
        ['Repeated hour', z.dst_repeated_rows ? `${count(z.dst_repeated_rows)} polls in the hour the clocks go back, read in their order` : 'None'],
        ['Distances', `${u.distance === 'km' ? 'Kilometres' : 'Miles'}, ${UNIT_FROM[u.distance_from] || u.distance_from || '—'}`],
        ['Temperatures', `${u.temperature === 'f' ? '°F' : '°C'}, ${UNIT_FROM[u.temperature_from] || u.temperature_from || '—'}`],
        ['Files', `${count(rep.files || 0)}${Object.keys(rep.files_skipped || {}).length ? `, ${count(Object.keys(rep.files_skipped).length)} passed over` : ''}`],
        ['Size', rep.unpacked_bytes != null ? `${bytes(rep.unpacked_bytes)}${rep.compression && rep.compression !== 'plain' ? ` (${bytes(rep.packed_bytes)} ${rep.compression})` : ''}` : null],
        ['Columns not in the files', (rep.missing_columns || []).length ? rep.missing_columns.join(', ') : 'None'],
        ['Columns passed over', count(rep.unknown_columns || 0)],
        ['Old Windows text', R.windows_1252 ? `${count(R.windows_1252)} rows read as Windows-1252` : null],
      ]),
      Object.keys(rep.files_skipped || {}).length ? h('ul', { class: 'set-list' }, Object.entries(rep.files_skipped).map(([n, why]) => h('li', null, h('code', { class: 'set-mono' }, n), `: ${why}`))) : null);
    const whyT = tile({ w: 2, title: 'Why polls were skipped', icon: 'warning', cover: 'web.import.rejected.col-reason' },
      reasons.length || blanks.length ? group(null, [
        ...reasons.map(([code, n]) => infoRow({ icon: 'minus', tone: 'sun', name: reasonWords(code)[0], sub: reasonWords(code)[1], side: h('span', { class: 'im-n' }, count(n)) })),
        ...blanks.map(([col, n]) => infoRow({ icon: 'edit', tone: 'sky', name: blankWords(col)[0], sub: blankWords(col)[1], side: h('span', { class: 'im-n' }, count(n)) })),
      ]) : said('ok', 'Nothing was skipped: every poll passed its checks.'));
    const cars = rep.cars || [];
    const carT = cars.length ? tile({ w: 2, title: 'Per car', icon: 'car' },
      group(null, cars.map((c) => {
        const here = isCar(c.match) ? carName(c.match) : c.match === 'new' ? 'a new car' : c.match === 'leave_out' ? 'left out' : 'not matched';
        return infoRow({
          icon: 'car', tone: c.match === 'leave_out' ? null : 'coral', name: c.name || `TeslaFi car ${c.tm_car}`,
          sub: [`Becomes ${here}`, c.vin_prefix ? `VIN prefix ${c.vin_prefix}` : null, c.match === 'leave_out' ? null : `${count(c.drives)} drives · ${count(c.charges)} charges`, c.rows != null ? `${count(c.rows)} polls` : null].filter(Boolean).join(' · '),
          side: isCar(c.match) ? h('a', { class: 'tile-link', href: state.buildUrl('timeline', { car: c.match, period: rep.span && rep.span.from_ms ? { from: rep.span.from_ms, to: rep.span.to_ms } : null }) }, 'Timeline', icon('chevronRight')) : null,
        });
      }))) : null;
    reportBox.replaceChildren(reportT, foundT, readT, whyT, carT);
  }

  /* ---------------------------------------------------------------- overlaps */
  const overlapBox = h('div', { class: 'contents' });
  // cars with imported history: those an import here matched, and those whose rows say so
  const overlapCars = () => {
    const ids = new Set();
    for (const j of S.jobs) if (j.status === 'done') for (const c of (j.report && j.report.cars) || []) if (isCar(c.match)) ids.add(c.match);
    for (const c of S.cars) if ((c.sources || []).some((x) => x.kind === 'import')) ids.add(c.id);
    return [...ids].slice(0, 50);
  };
  async function loadOverlaps() {
    await Promise.all(overlapCars().map((cid) => loadOverlapsOf(cid)));
    if (ctx.alive()) paintOverlaps();
  }
  async function loadOverlapsOf(cid) {
    try {
      const all = await ctx.api.list(api.apiPath`cars/${cid}/overlaps`, { limit: 200 }).all({ max: 1000 });
      S.overlaps.set(cid, { tz: (carById.get(cid) || {}).time_zone || tz, items: all, err: null });
    } catch (e) {
      if (!api.isAbort(e)) S.overlaps.set(cid, { tz, items: [], err: e });
    }
  }
  // the side shown now: on a live span 'live' or 'import'; on an imports span the site. Old servers say can / teslamate.
  const shownOf = (r) => {
    if (r.kind === 'imports') return r.shown_source && r.shown_source.startsWith('import:') ? r.shown_source : 'import:teslamate';
    return r.shown_source ? (r.shown_source === 'live' ? 'live' : 'import') : r.shown === 'teslamate' ? 'import' : 'live';
  };
  const SIDE_WORDS = { live: 'the live history (SMO, the Tesla API, TeslaFi)', import: 'the imported history', 'import:teslamate': 'TeslaMate’s import', 'import:teslafi': 'TeslaFi’s import' };
  const REASON_WORDS = { more_drives_and_charges: 'it has more drives and charges there', more_samples: 'the same drives and charges, more positions', tie: 'a tie, so TeslaMate’s' };
  async function flip(cid, row, value, seg) {
    const o = S.overlaps.get(cid);
    try {
      const ans = await ctx.api.patch(api.apiPath`cars/${cid}/overlaps`, { id: row.id, shown: value, version: row.version });
      if (!ctx.alive()) return;
      if (ans && Array.isArray(ans.items)) o.items = ans.items; else await loadOverlapsOf(cid);
      if (!ctx.alive()) return;
      api.invalidate('');
      ctx.toast(`From ${fdOf(o)(row.start_ms)}, every page shows ${SIDE_WORDS[value] || 'that side'} now.`);
      paintOverlaps();
    } catch (e) {
      if (!ctx.alive() || api.isAbort(e)) return;
      if (e && (e.code === 'stale_version' || e.status === 409)) {
        await loadOverlapsOf(cid);
        if (!ctx.alive()) return;
        paintOverlaps();
        ctx.toast('Someone else switched a span of this car a moment ago, so nothing was changed. The list now shows their choice: switch again if it is still needed.', { kind: 'info' });
        return;
      }
      seg.set(shownOf(row));
      ctx.toast(e && e.code === 'not_allowed' ? 'Only an admin can switch the side shown.' : `Not switched: ${errorWords(e).text}`, { kind: 'error' });
    }
  }
  const fdOf = (o) => (ms) => formatDate(ms, o.tz, { weekday: false, year: true });
  function spanRow(cid, o, r) {
    const fd = fdOf(o);
    const isImp = r.kind === 'imports';
    const options = isImp ? [['import:teslamate', 'TeslaMate'], ['import:teslafi', 'TeslaFi']] : [['live', 'Live'], ['import', 'Imported']];
    const seg = segmented({ name: `Shown from ${fd(r.start_ms)} to ${fd(r.end_ms)}`, value: shownOf(r), options, onchange: (v) => flip(cid, r, v, seg) });
    const live = r.live || r.can || {}, imp = r.imported || r.teslamate || {}, sites = r.sites || {};
    const n = (x) => `${count(x && x.drives)} drives, ${count(x && x.charges)} charges`;
    const counts = isImp
      ? [h('span', null, h('b', null, 'TeslaMate: '), n(sites.teslamate)), h('span', null, h('b', null, 'TeslaFi: '), n(sites.teslafi)), live.drives || live.charges ? h('span', null, h('b', null, 'Live, shown over both: '), n(live)) : null]
      : [h('span', null, h('b', null, 'Live: '), n(live)), h('span', null, h('b', null, 'Imported: '), n(imp))];
    const shown = shownOf(r);
    const dflt = r.default_source ? (isImp ? `At first ${siteOf(r.default_source)}’s${REASON_WORDS[r.default_reason] ? `: ${REASON_WORDS[r.default_reason]}` : ''}.` : 'At first the live side.') : null;
    return h('div', { class: ['im-span-row', `is-${r.kind || 'live'}`] },
      h('div', { class: 'row-text' },
        h('span', { class: 'im-span-kind' }, isImp ? 'Two imports' : 'Live and imported'),
        h('a', { class: 'row-name place-link', href: state.buildUrl('timeline', { car: cid, period: { from: r.start_ms, to: r.end_ms } }) }, `${fd(r.start_ms)} to ${fd(r.end_ms)}`),
        h('span', { class: 'row-sub im-span-counts' }, counts),
        h('span', { class: 'row-sub' }, h('b', null, 'Shown: '), shown === 'live' ? 'live' : shown === 'import' ? 'imported' : siteOf(shown), dflt ? ` · ${dflt}` : ''),
        r.sources?.length ? h('span', { class: 'src-line' }, srcPill(r)) : null),
      seg);
  }
  function paintOverlaps() {
    const ids = overlapCars();
    const tiles = ids.map((cid) => {
      const o = S.overlaps.get(cid);
      const head = { w: 2, id: `overlaps-${cid}`, title: `Overlaps of ${carName(cid)}`, icon: 'timeline', side: o && o.items.length ? chip(`${count(o.items.length)} ${o.items.length === 1 ? 'span' : 'spans'}`) : null };
      if (!o) return tile(head, said('info', 'Reading…'));
      if (o.err) return tile(head, said('bad', errorWords(o.err).text), btn('Try again', { kind: 'soft', icon: 'reset', onclick: loadOverlaps }));
      if (!o.items.length) return tile(head, said('ok', 'No overlap: no two histories of this car cover the same time.'));
      return tile(head,
        h('div', { class: 'im-spans' }, o.items.map((r) => spanRow(cid, o, r))),
        note(`Live and imported: pick the side every page shows for that time (live at first). Two imports: where TeslaMate’s and TeslaFi’s both cover the same days, one is shown (live history still wins there). Nothing is deleted; a switch is in the audit log. Dates in ${o.tz.replace(/_/g, ' ')} time.`));
    });
    overlapBox.replaceChildren(...(tiles.length ? tiles : [emptyTile({ icon: 'timeline', title: 'No overlaps', text: 'Once an import is done, each span where two histories cover the same time shows here with a switch for the side shown.', w: 2 })]));
  }

  /* ---------------------------------------------------------------- rows moved, rows that need a car */
  const holdsBox = h('div', { class: 'contents' });
  async function giveCar(x, v) {
    try {
      const out = await ctx.api.post(api.apiPath`import-holds/${x.hold}`, { match: v });
      if (!ctx.alive()) return;
      const [jobs2, holds2] = await Promise.all([ctx.api.list('imports', { limit: 100 }).all({ max: 500 }), ctx.api.get('import-holds')]);
      if (!ctx.alive()) return;
      S.jobs = jobs2;
      S.holds = holds2?.items || [];
      ctx.toast(holdSaid(out, carName, count), { kind: 'info' });
    } catch (e) {
      if (ctx.alive()) ctx.toast(errorWords(e).text, { kind: 'error' });
      throw e;
    } finally { if (ctx.alive()) { paintHolds(); paintList(); } }
  }
  function paintHolds() {
    const lines = S.jobs.map((j) => [j, moveSummary(j, carName, count)]).filter(([, w]) => w);
    const lots = waiting(S.holds);
    if (!lines.length && !S.holds.length) { holdsBox.replaceChildren(); return; }
    const odo = (x) => (x.odometer_km ? `${format(x.odometer_km[0], 'odometer')} to ${format(x.odometer_km[1], 'odometer')}` : null);
    const options = (x) => [...S.cars.map((c) => [c.id, c.name]), ...HOLD_ACTIONS.map(([v, l]) => [v, v === 'new' && x.name ? `A new car named ${x.name}` : l])];
    holdsBox.replaceChildren(sectionHead('Imported rows and their cars', 'Rows an import could not put on a car for sure wait here, kept, until you choose their car. Nothing is deleted.'),
      tile({ w: 'full', title: 'Rows moved', icon: 'car', id: 'needs-car' },
        lines.length ? group(null, lines.map(([j, w]) => infoRow({ icon: 'file', name: j.name, sub: `${w}.`, cls: 'im-job-row' })))
          : said('ok', 'No import moved rows.'),
        lots.length ? group('Rows that need a car', lots.map((x) => pickRow({
          name: x.name ? `Named ${x.name}` : 'Rows without a name',
          sub: [`${count(x.rows)} rows, ${nOf(x.drives, 'drive', count)}`, x.first_ms != null ? `${day(x.first_ms)} to ${day(x.last_ms)}` : null, odo(x)].filter(Boolean).join(' · '),
          value: x.status === 'ignored' ? 'ignore' : 'wait', options: options(x), sheetTitle: 'Where these rows go', sheetNote: holdWhy(x),
          onpick: (v) => giveCar(x, v),
        }))) : said('ok', 'Every imported row has a car.')));
  }

  /* ---------------------------------------------------------------- earlier imports */
  const listBox = h('div', { class: 'contents' });
  function paintList() {
    if (!S.jobs.length) {
      listBox.replaceChildren(sectionHead('Imports'), emptyTile({ icon: 'upload', title: 'No import yet', text: 'Uploaded files and their reports are listed here, newest first, each with its status and a button to discard it.', cover: ['web.import.files.col-status', 'web.import.discard'] }));
      return;
    }
    listBox.replaceChildren(sectionHead('Imports', `Newest first. Times in ${tz.replace(/_/g, ' ')}.`),
      tile({ w: 'full', title: 'Every import', icon: 'file' },
        group(null, S.jobs.map((j) => infoRow({
          icon: 'file', tone: (STATUS[j.status] || [])[1] || null, name: j.name, cls: 'im-job-row',
          sub: [bytes(j.bytes_total), j.compression && j.compression !== 'plain' ? j.compression : null, j.started_ms ? `began ${when(j.started_ms)}` : null,
            j.finished_ms ? `${j.status === 'failed' ? 'stopped' : 'finished'} ${when(j.finished_ms)}` : UNFINISHED.includes(j.status) ? 'not finished' : null].filter(Boolean).join(' · '),
          side: [statusChip(j.status, 'web.import.files.col-status'),
            S.job && j.id === S.job.id && shownJob() ? null : btn('Show', { kind: 'soft', cls: 'btn-sm', onclick: () => showJob(j) }),
            btn(j.status === 'done' ? 'Remove' : 'Discard', { kind: 'ghost', cls: 'btn-sm', cover: 'web.import.discard', title: j.status === 'done' ? 'Remove from the list; its history stays' : 'Discard this import', onclick: () => discard(j) })],
        })))));
  }

  function paintAll() { paintSteps(); paintReport(); paintList(); paintHolds(); }

  if (S.job && ['checking', 'ready'].includes(S.job.status)) await loadMatch();
  await loadOverlaps();
  if (!ctx.alive()) return {};
  paintAll();
  paintOverlaps();
  ctx.view.replaceChildren(bar, stepsTile, knowTile, reportBox, overlapBox, holdsBox, listBox);
  follow();
  ctx.ready();
  return {};
}
