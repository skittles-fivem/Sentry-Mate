// Sentry Mate, "Logbook" UI pack: Import (/settings/import).
//
// Bringing in the history TeslaMate recorded, from TeslaMate's own backup file, the polls TeslaFi recorded, from
// TeslaFi's data download (its monthly CSVs: one, several in a .zip, or gzipped; the owner confirms the time zone
// and the units they are read in before the import starts), or the CAN Bridge logs
// Sentry Six Manager already has, from its CAN export bundle (a .zip: no cars to match, since each log
// names its car key; its report counts logs imported, already here and refused). An admin chooses the
// file; it goes up in chunks over HTTPS, each with its SHA-256, so an upload that stops goes on from the
// last chunk the server has. The server reads the whole file once (the check) before it stores anything,
// then the cars in the file are matched to cars here (same VIN prefix, a new car, or left out), and the
// import runs. The report counts rows per table and per car, with the reason for every skipped count
// (counts only: never a row's values or positions). Where imported and CAN history cover the same time,
// each span is listed with the side that is shown and a switch to show the other; where a TeslaMate and a
// TeslaFi import cover the same time, one import is shown there, with a switch between the two.
//
// API (admins): GET / POST imports, GET / PATCH / DELETE imports/{import}, GET / PUT
// imports/{import}/cars, POST imports/{import}/start, GET / PATCH cars/{car}/overlaps, GET cars.
//
// Progress: there is no live event for an import, so while a job is being checked or imported (or
// uploaded from another tab, or waits checked for its cars to be matched) the page reads that one job
// again every few seconds, and stops as soon as the job is done or failed, or the page is left.

import { h, replace } from '../shared/dom.js';
import * as api from '../shared/api.js';
import * as state from '../shared/state.js';
import { format } from '../shared/units.js';
import { formatDateTime, formatDate, formatDuration } from '../shared/time.js';
import { column, timeColumn } from '../shared/csv.js';
import { pageHead, section, figure, figures, flag, button, icon, empty, errorView, errorWords } from '../core/ui.js';
import { labelOf } from '../core/sources.js';
import { choice, field, carOptions } from '../core/form.js';
import { dataTable } from '../core/table.js';
import { confirmDialog } from '../core/dialog.js';
import { zoneShort, count } from '../core/format.js';

export const id = 'import';
export const css = true;
export const title = () => 'Import';

const UNFINISHED = ['uploading', 'checking', 'ready', 'importing'];
const MOVING = ['checking', 'importing'];
const CHUNK_DEFAULT = 1024 * 1024 - 1024;
const POLL_MS = 2500;

/* ------------------------------------------------------------------ words */

const STATUS = {
  uploading: { word: 'Uploading', kind: 'live', text: 'The files go up to the server in chunks, up to three at a time. Keep this page open until the upload is done.' },
  checking: { word: 'Checking', kind: 'live', text: 'Pass 1 of 2: the server reads the whole file and checks every row. Nothing is stored yet.' },
  ready: { word: 'Checked, waiting for you', kind: 'wait', text: 'The whole file was read and checked. Match its cars below, then start the import.' },
  importing: { word: 'Importing', kind: 'live', text: 'Pass 2 of 2: the checked rows are being stored.' },
  done: { word: 'Done', kind: 'ok', text: 'The history is in. It shows on every page, marked as imported.' },
  failed: { word: 'Failed', kind: 'bad', text: 'The import stopped. Rows of a failed check are never stored.' },
};
const statusWord = (s) => STATUS[s]?.word || s;
const CAN = 'sentry_manager_can';
const isCan = (j) => j?.kind === CAN || j?.report?.kind === CAN;
const TF = 'teslafi';
const isTf = (j) => j?.kind === TF || j?.report?.kind === TF;
const kindWord = (j) => (isCan(j) ? 'Sentry Six Manager CAN export' : isTf(j) ? 'TeslaFi data download' : 'TeslaMate backup');
const siteWord = (j) => (isTf(j) ? 'TeslaFi' : 'TeslaMate');
const SITE_NAME = { teslamate: 'TeslaMate', teslafi: 'TeslaFi' };
// the zones this browser knows, for the time zone a TeslaFi file is read in
const ZONES = (() => { try { return Intl.supportedValuesOf('timeZone'); } catch { return []; } })();
const UNIT_FROM = { detected: 'worked out from the file', default: 'nothing in the file tells: the default', setting: 'chosen at the start' };
const ZONE_FROM = { car: 'the time zone of the car it is matched to', default: 'the server’s default time zone', setting: 'chosen at the start' };
// the same states of a CAN export, where they differ
const CAN_STATUS = {
  checking: 'Pass 1 of 2: the server reads every log of the zip and checks its signature, as it checks an upload. Nothing is stored yet.',
  ready: 'The whole zip was read and checked. Each log goes to the car its car key belongs to; start the import when the report below looks right.',
  importing: 'Pass 2 of 2: each log is stored and checked as if the CAN Bridge had uploaded it, then decoded into drives and charges.',
  done: 'The logs are in, as the car’s own CAN data. Drives, charges and the timeline are built from them as from any upload.',
  failed: 'The import stopped. Logs stored before it stopped stay (checked and signed); running the zip again skips them.',
};
const statusText = (j) => (isCan(j) && CAN_STATUS[j.status]) || STATUS[j.status]?.text || '';

// why a log of a CAN export was refused, in words
const CAN_REASONS = {
  unsigned: ['Not signed', 'The log has no valid CAN Bridge signature, so nothing shows it came from the car. It is not stored.'],
  failed_check: ['Changed since it was signed', 'Its bytes, name or length are not the ones the CAN Bridge signed: it was changed or damaged.'],
  signed_by_another_key: ['Another signing key', 'Signed by a key that is not the one this car key is paired with. Trust that key in Settings: Cars first if it is the car’s new one.'],
  no_sidecar: ['No sidecar', 'The zip has the log without its .log.xz.json beside it, which holds the signature.'],
  bad_sidecar: ['Sidecar unreadable', 'Its sidecar is not JSON, or over 1 MB.'],
  different_file_here: ['Another log of that name', 'A different log with the same name is already here; the one here is kept.'],
  being_uploaded: ['Being uploaded', 'The CAN Bridge is uploading this log now; that upload brings it.'],
  too_large: ['Too large', 'A log can be at most 1 GB.'],
  car_full: ['Car full', 'The car already holds the most logs a car can keep.'],
};
const canReason = (code) => CAN_REASONS[code] || [String(code).replace(/[_-]/g, ' '), 'A reason the server gave by its code.'];
const KEY_STATUS = { car: 'Goes to its car', new_car: 'A new car', only_car: 'The one car here', new_key: 'A new car key', waiting: 'Waiting for an admin (Settings: Cars)', working: 'Being decoded' };

// TeslaMate's tables, in words
const TABLES = {
  cars: ['Cars', 'One row per car in the backup.'],
  drives: ['Drives', 'Trips from start to stop.'],
  charging_processes: ['Charges', 'Charging sessions with their energy and cost.'],
  positions: ['Track points', 'Positions recorded along the drives.'],
  updates: ['Software updates', 'The versions each car installed, with their dates.'],
  geofences: ['Places', 'Named areas. They become places here where no place covers them yet.'],
  addresses: ['Addresses', 'Street addresses of the spots the cars stopped at.'],
  states: ['Car states', 'Online, asleep and offline spells.'],
  car_settings: ['Car settings', 'Each car’s TeslaMate settings, such as free Supercharging.'],
  charges: ['Charge readings', 'The readings taken during each charge: its power curve.'],
};
const tableName = (t) => TABLES[t]?.[0] || t.replace(/_/g, ' ');
const tableNote = (t) => TABLES[t]?.[1] || 'A table of the backup.';

// why rows were skipped (the whole row) or a value was left blank (one column), and what it means
const REASONS = {
  duplicate: ['The same row twice', 'The file holds this row twice (same TeslaMate id). The first copy is kept, so nothing is lost.'],
  no_energy: ['No energy recorded', 'A charge that recorded no energy added. It would show as 0 kWh, so it is left out.'],
  no_position: ['No coordinates', 'A track point without latitude or longitude. Its drive keeps every other point, so the map line skips it.'],
  bad_time: ['Time cannot be read', 'The row has no time, or one that is not a real date.'],
  other_car: ['For a car left out', 'The row belongs to a car you chose to leave out.'],
  out_of_range: ['Values out of range', 'Every value of the row failed its check (for example a negative distance).'],
  // the codes the server's import gives
  'already-imported': ['Imported before', 'An earlier import took this row (or left its car out for good), so it is not stored twice.'],
  open: ['Not finished in the backup', 'A drive, charge, state or update that was still going when the backup was made. A later backup brings it in once it has an end.'],
  'unknown-car': ['Car not in the backup', 'The row names a car the backup’s car list does not have.'],
  'car-left-out': ['For a car left out', 'The row belongs to a car you chose to leave out.'],
  'unknown-charge': ['Charge not in the backup', 'A charge reading whose charge the backup does not have, or did not keep.'],
  'bad-value': ['A needed value cannot be read', 'A value the row needs is missing or unreadable; the blanks below say which.'],
  'wrong-field-count': ['Broken row', 'The row does not have as many values as its table has columns.'],
  'too-long': ['Row too long', 'The row is longer than any real TeslaMate row, so it is not read.'],
  // the codes of a TeslaFi file
  already_imported: ['Imported before', 'An earlier import already took the time of this poll, so it is not stored twice.'],
  out_of_order: ['Out of order', 'A poll older than the one before it in the file.'],
  dst_gap: ['A time the clocks skip', 'A wall time that does not exist in the chosen zone (the hour the clocks go forward).'],
  car_left_out: ['For a car left out', 'The poll belongs to a car you chose to leave out.'],
  too_many_cars: ['Too many cars', 'More than 64 cars in one upload: the polls of the rest are not read.'],
  wrong_field_count: ['Broken row', 'The line does not have as many values as the file has columns.'],
  too_long: ['Line too long', 'A line over 1 MB is not read.'],
};
const BLANKS = {
  cost: ['Cost left blank', 'A cost that could not be read (no number, or no currency). The charge is kept with no cost; you can type one on its page.'],
  elevation: ['Elevation left blank', 'A height out of any real range. The track point is kept without it.'],
};
const reasonWords = (code) => REASONS[code] || [code.replace(/_/g, ' '), 'A reason the server gave by its code.'];
const blankWords = (col) => BLANKS[col] || [`${col.replace(/_/g, ' ')} left blank`, 'A value that failed its check; the row is kept without it.'];

// why a check or an import failed, and what to do
const FAILURES = {
  not_a_backup: 'The file is not a TeslaMate backup. Make one with TeslaMate’s database backup (its whole database, written by pg_dump), as it is or compressed with gzip or xz, and upload that file.',
  no_car: 'The file holds no car. A backup made before TeslaMate first saw a car has nothing to import.',
  mixed_cars: 'The file holds rows for cars it does not describe. Make a fresh, whole backup and upload it.',
  truncated: 'The file ends part-way through: it was cut short when it was made or copied. Copy it again and upload the new copy.',
  not_a_can_export: 'This zip is not a Sentry Six Manager CAN export. In Sentry Six Manager, download the CAN export bundle (Nerd panel, Logs + ids.csv, the car’s key, From before its first log to now) and upload that .zip as it is. A TeslaMate backup is uploaded as the .sql file itself (or .sql.gz, .sql.xz), not zipped.',
  unreadable: 'The file cannot be read as a zip: it is damaged, cut short, encrypted or packed with a method other than deflate. Download it again and upload it as it is.',
  read_failed: 'The server could not read the uploaded file back. Upload it again.',
  store_failed: 'The server could not store a log (for example its disk is full). Logs stored before it stay; free some space and start again.',
  one_file_only: 'A TeslaMate backup or a CAN export is imported on its own. Discard this import, then upload that file alone; only TeslaFi’s CSV files can go up several at once.',
  not_a_teslafi_export: 'Several files go up together only as one TeslaFi export, and one of them is not a TeslaFi CSV (the server names it below). Discard this import, then upload the TeslaFi files without it.',
  stopped: 'The import was stopped. Logs stored before it stopped stay; start it again to bring the rest.',
  not_a_teslafi_export: 'No TeslaFi data file is in this upload. In TeslaFi, open Settings, Account, Advanced, “Download TeslaFi Data” and upload the monthly CSVs it gives (TeslaFi<month><year>.csv), one, several in a .zip, or gzipped.',
  teslafi_summary: 'This is TeslaFi’s list of drives or charges (one row per drive, no positions). Upload the monthly files of Settings, Account, Advanced, “Download TeslaFi Data” instead.',
};

// a refused Start, in words
function startWords(e) {
  switch (e?.code) {
    case 'not_ready': return 'The file is not checked yet. The import can start once the check is done.';
    case 'cars_unmatched': return 'Some cars in the file have no match yet. Choose a car here, “Add as a new car” or “Leave out” for each one.';
    case 'import_running': return 'Another import is already running, perhaps from another browser tab. Reload this page to see it.';
    case 'no_such_import': return 'This import was discarded, perhaps from another browser tab. Reload this page.';
    default: return errorWords(e).text;
  }
}

const n = (x) => format(x, 'count');
const bytes = (x) => (x == null ? '—' : format(x, 'bytes'));
const pct = (a, b) => (b > 0 ? Math.max(0, Math.min(100, (a / b) * 100)) : 0);
const hex = (buf) => Array.from(new Uint8Array(buf), (b) => b.toString(16).padStart(2, '0')).join('');
const sum = (o) => Object.values(o || {}).reduce((a, b) => a + (b || 0), 0);
// a count the file does not have is null: a gap (the dash, faint), never 0
const DASH = '\u2014';
const gap = () => h('span', { class: 'nil', title: 'Not in the file' }, DASH);
const nn = (x) => (x == null ? gap() : n(x));
// a total of the numbers present; a gap when there is none
const addUp = (xs) => xs.reduce((a, b) => (b == null ? a : (a ?? 0) + b), null);
const sumOf = (o) => (o == null ? null : typeof o === 'number' ? o : addUp(Object.values(o)));
const isCar = (m) => typeof m === 'string' && m !== 'new' && m !== 'leave_out';
const wait = (ms, signal) => new Promise((res) => { const t = setTimeout(res, ms); signal?.addEventListener('abort', () => { clearTimeout(t); res(); }, { once: true }); });

/* ====================================================================== the page */

export async function mount(ctx) {
  const tz = ctx.tz;
  const zone = zoneShort(tz);
  const crumbs = h('nav', { class: 'crumbs', 'aria-label': 'Breadcrumb', cover: 'web.import.breadcrumb' },
    h('a', { href: state.buildUrl('fleet') }, 'All cars'), h('span', { class: 'crumb-sep', 'aria-hidden': 'true' }, '/'),
    h('a', { href: state.buildUrl('settings-cars') }, 'Settings'), h('span', { class: 'crumb-sep', 'aria-hidden': 'true' }, '/'),
    h('span', { 'aria-current': 'page' }, 'Import'));
  const head = pageHead({
    eyebrow: crumbs, title: 'Import history',
    deck: 'Bring in the drives, charges, updates and places TeslaMate recorded, from TeslaMate’s own backup file; the polls TeslaFi recorded, from TeslaFi’s data download (its monthly CSV files); or the CAN Bridge logs Sentry Six Manager already has, from its CAN export bundle (.zip). The whole file is checked before anything is stored, and running the same file again stores nothing twice.',
  });

  if (!ctx.me || ctx.me.role !== 'admin') {
    ctx.main.append(head, section({ title: 'Import' },
      empty({ title: 'Only an admin can import history.', text: 'An import adds drives and charges to cars, so it needs an admin account. Ask an admin to upload the TeslaMate backup or TeslaFi’s data download; the imported history then shows on every page, marked as imported.' })));
    ctx.ready();
    return {};
  }

  const S = { jobs: [], cars: [], carsMore: false, job: null, match: null, matchErr: null, overlaps: new Map(), currency: '', tf: null };
  const up = { files: new Map(), active: false, paused: false, error: null, t0: 0, b0: 0, ctl: null };

  const [jobs, carsPage] = await Promise.all([
    ctx.api.list('imports', { limit: 100 }).all({ max: 500 }),
    ctx.api.page('cars', { limit: 500 }),
  ]);
  S.jobs = jobs;
  S.cars = carsPage.items || [];
  S.carsMore = !!carsPage.next;
  const carById = new Map(S.cars.map((c) => [c.id, c]));
  const carName = (cid) => carById.get(cid)?.name || cid;

  function pickJob() {
    const want = ctx.query?.import;
    S.job = (want && S.jobs.find((j) => j.id === want)) || S.jobs.find((j) => UNFINISHED.includes(j.status)) || S.jobs[0] || null;
  }
  pickJob();
  const putJob = (j) => {
    const i = S.jobs.findIndex((x) => x.id === j.id);
    if (i >= 0) S.jobs[i] = { ...S.jobs[i], ...j }; else S.jobs.unshift(j);
    if (S.job && S.job.id === j.id) S.job = { ...S.job, ...j };
  };
  const running = () => S.jobs.find((j) => UNFINISHED.includes(j.status)) || null;

  /* ---------------------------------------------------------------- the steps */
  const STEPS = [['uploading', 'Upload'], ['checking', 'Check'], ['ready', 'Match cars'], ['importing', 'Import'], ['done', 'Done']];
  const stepLabel = (k, label, j) => (k === 'ready' && isCan(j) ? 'Review' : label);
  const stepsEl = h('ol', { class: 'im-steps', 'aria-label': 'Steps of an import' });
  function paintSteps() {
    const j = S.job && (UNFINISHED.includes(S.job.status) || S.job.status === 'failed' || (S.job.status === 'done' && S.job.id === ctx.query?.import)) ? S.job : null;
    let at = -1;
    if (j) at = j.status === 'failed' ? (j.checked_ms ? 3 : j.uploaded_bytes >= j.bytes_total ? 1 : 0) : STEPS.findIndex(([k]) => k === j.status);
    replace(stepsEl, STEPS.map(([k, label0], i) => {
      const label = stepLabel(k, label0, j);
      const st = !j ? 'later' : j.status === 'done' || i < at ? 'done' : i === at ? (j.status === 'failed' ? 'failed' : 'now') : 'later';
      return h('li', { class: ['im-step', `is-${st}`], 'aria-current': st === 'now' ? 'step' : null },
        h('span', { class: 'im-step-n', 'aria-hidden': 'true' }, st === 'done' ? icon('check') : st === 'failed' ? icon('close') : String(i + 1)),
        h('span', { class: 'im-step-l' }, label,
          h('span', { class: 'sr-only' }, st === 'done' ? ' (done)' : st === 'now' ? ' (now)' : st === 'failed' ? ' (failed here)' : '')));
    }));
  }

  /* ---------------------------------------------------------------- choose and upload */
  const fileInput = h('input', {
    class: 'im-file-input', type: 'file', multiple: true,
    accept: '.sql,.gz,.xz,.zip,.csv,.dump,.bck,.bak,.txt,application/sql,application/gzip,application/x-xz,application/zip,text/csv',
    'aria-label': 'TeslaMate backup, TeslaFi data download or Sentry Six Manager CAN export',
    onchange: () => { const fs = [...(fileInput.files || [])]; fileInput.value = ''; if (fs.length) chose(fs); },
  });
  const drop = h('label', { class: 'im-drop' }, fileInput,
    h('span', { class: 'im-drop-main' }, 'Choose the files, or drop them here'),
    h('span', { class: 'im-drop-sub' }, 'The .sql file of a TeslaMate database backup, as it is or compressed (.gz or .xz); TeslaFi’s data download (its monthly .csv files: several at once are fine, such as all of its months, and they become one import; or a .zip of them, or gzipped); or the .zip of Sentry Six Manager’s CAN export bundle. Up to 64 GB a file. They go over this HTTPS connection to your own server and nowhere else.'));
  const onDrag = (e) => { e.preventDefault(); drop.classList.add('is-over'); };
  const offDrag = () => drop.classList.remove('is-over');
  const onDrop = (e) => { e.preventDefault(); offDrag(); const fs = [...(e.dataTransfer?.files || [])]; if (fs.length) chose(fs); };
  drop.addEventListener('dragover', onDrag);
  drop.addEventListener('dragleave', offDrag);
  drop.addEventListener('drop', onDrop);

  const upErr = h('p', { class: 'form-error im-err', role: 'alert', hidden: true });
  function showUpErr(text, action = null) { replace(upErr, text ? [text, action ? [' ', action] : null] : null); upErr.hidden = !text; }

  // a job's files: the job view's list, or (an older one-file job) the job itself as file 0
  const filesOf = (j) => (j?.files?.length ? j.files : j ? [{ index: 0, name: j.name, bytes_total: j.bytes_total, uploaded_bytes: j.uploaded_bytes || 0, upload_url: null }] : []);
  const fileUrl = (j, f) => f.upload_url || (f.index === 0 && !j.files?.length ? api.apiPath`imports/${j.id}` : api.apiPath`imports/${j.id}/files/${f.index}`);
  const left = (j) => filesOf(j).filter((f) => (f.uploaded_bytes || 0) < f.bytes_total);
  // answers of files sent at the same time can arrive out of order: keep each file's furthest offset
  function putUpload(ans, index, off) {
    const old = filesOf(S.job);
    const fresh = ans?.files?.length ? ans.files : old;
    const files = fresh.map((f) => {
      const o = old.find((x) => x.index === f.index);
      let u = Math.max(f.uploaded_bytes || 0, o?.uploaded_bytes || 0);
      if (f.index === index) u = Math.max(u, off);
      return { ...o, ...f, uploaded_bytes: Math.min(u, f.bytes_total) };
    });
    // an older answer arriving late does not take the job back to uploading
    const status = S.job.status !== 'uploading' && ans?.status === 'uploading' ? S.job.status : (ans?.status || S.job.status);
    putJob({ ...ans, status, files, uploaded_bytes: files.reduce((a, f) => a + f.uploaded_bytes, 0) });
  }

  async function chose(list) {
    showUpErr(null);
    const j = running();
    if (j && j.status === 'uploading' && !up.active) {
      // going on: each chosen file to the job's file of that name and size
      const files = filesOf(j);
      const got = new Map(up.files);
      const strange = [];
      for (const f of list) {
        const m = files.find((x) => x.name === f.name && x.bytes_total === f.size);
        if (m) got.set(m.index, f); else strange.push(f.name);
      }
      const need = left(j);
      if (!need.some((x) => got.has(x.index))) {
        showUpErr(files.length > 1
          ? `Those are not the files this upload began with (${need.map((x) => `${x.name}, ${bytes(x.bytes_total)}`).join('; ')}). Choose those files to go on, or discard this upload to start with other files.`
          : `That is not the file this upload began with (${j.name}, ${bytes(j.bytes_total)}). Choose that file to go on, or discard this upload to start with another file.`);
        return;
      }
      if (strange.length) showUpErr(`Not part of this upload, so left aside: ${strange.join(', ')}.`);
      up.files = got;
      S.job = j;
      upload();
      return;
    }
    if (j) { showUpErr('An import is not finished yet. Finish it, or discard it, before you upload other files.'); return; }
    try {
      const created = await ctx.api.post('imports', { files: list.map((f) => ({ name: f.name, bytes_total: f.size })) });
      if (!ctx.alive()) return;
      putJob(created);
      S.job = S.jobs.find((x) => x.id === created.id);
      S.match = null;
      up.files = new Map(filesOf(S.job).map((f) => [f.index, list[f.index]]).filter(([, f]) => f));
      ctx.setQuery({ import: created.id });
      paintAll();
      upload();
    } catch (e) {
      if (!ctx.alive() || api.isAbort(e)) return;
      if (e.code === 'import_running') {
        showUpErr('Another import is not finished yet, perhaps from another browser tab.', button('Show it', { onclick: () => refreshJobs(true), kind: 'quiet', cls: 'button-small' }));
      } else showUpErr(e.fields ? Object.values(e.fields).join(' ') : errorWords(e).text);
    }
  }

  // one file, its chunks one after another; false when the whole upload should stop
  async function sendFile(index, file, signal, max) {
    let tries = 0;
    let f = filesOf(S.job).find((x) => x.index === index);
    let off = f?.uploaded_bytes || 0;
    while (off < file.size && ctx.alive() && !up.paused && !signal.aborted) {
      try {
        const buf = await file.slice(off, Math.min(file.size, off + max)).arrayBuffer();
        const sha = hex(await crypto.subtle.digest('SHA-256', buf));
        const ans = await ctx.api.patch(fileUrl(S.job, f), buf, {
          signal,
          headers: { 'Content-Type': 'application/offset+octet-stream', 'Upload-Offset': String(off), 'Upload-Length': String(file.size), 'Upload-SHA256': sha },
        });
        if (!ctx.alive()) return false;
        off = ans?.offset ?? off + buf.byteLength;
        putUpload(ans, index, off);
        f = filesOf(S.job).find((x) => x.index === index) || f;
        tries = 0;
        paintProgress();
      } catch (e) {
        if (!ctx.alive() || api.isAbort(e) || up.paused) return false;
        if (e.code === 'wrong_offset' || e.code === 'not_uploading') {
          // the server knows best where the upload is
          try {
            const j = await ctx.api.get(api.apiPath`imports/${S.job.id}`);
            putJob(j);
            f = filesOf(S.job).find((x) => x.index === index) || f;
            off = f.uploaded_bytes || 0;
            if (j.status !== 'uploading') return false;
            continue;
          } catch (e2) { if (api.isAbort(e2)) return false; }
        }
        tries++;
        const again = e.code === 'checksum_mismatch' || e.status === 0 || e.status >= 500 || e.status === 429;
        if (again && tries <= 5) { await wait(Math.min(16000, 1000 * 2 ** (tries - 1)), signal); continue; }
        up.error = e.code === 'checksum_mismatch' ? `A chunk of ${file.name} kept arriving damaged. Check the connection, then go on: the upload resumes where it stopped.`
          : e.status === 0 ? 'The server stopped answering. The upload resumes where it stopped when you go on.'
            : e.code === 'no_such_import' ? 'This upload was discarded, perhaps from another browser tab.'
              : (e.fields ? Object.values(e.fields).join(' ') : errorWords(e).text);
        return false;
      }
    }
    return true;
  }

  async function upload() {
    if (up.active || !up.files.size || !S.job) return;
    if (!globalThis.crypto?.subtle) { showUpErr('This browser cannot work out the chunks’ SHA-256 here. Open Sentry Mate over HTTPS and try again.'); return; }
    let job = S.job;
    up.active = true; up.paused = false; up.error = null;
    up.t0 = performance.now(); up.b0 = job.uploaded_bytes || 0;
    up.ctl = new AbortController();
    const ctl = up.ctl;
    ctx.onCleanup(() => ctl.abort());
    paintAll();
    const max = job.chunk_max_bytes || CHUNK_DEFAULT;
    // up to 3 files at the same time, each file's chunks in order
    const queue = left(job).filter((f) => up.files.has(f.index)).map((f) => f.index);
    const worker = async () => {
      while (queue.length && ctx.alive() && !up.paused && !ctl.signal.aborted) {
        const index = queue.shift();
        const ok = await sendFile(index, up.files.get(index), ctl.signal, max);
        if (!ok) { ctl.abort(); return; }
      }
    };
    await Promise.all(Array.from({ length: Math.min(3, queue.length) }, worker));
    up.active = false;
    up.ctl = null;
    if (!ctx.alive()) return;
    job = S.job;
    if (left(job).length || job.status === 'uploading') {
      // a pause or a break can leave a chunk the client gave up on but the server took: read where the
      // server really is, so the bytes and the status shown are its own
      paintAll();
      await syncJob(job.id);
      if (!ctx.alive() || up.active) return;
    }
    if (S.job?.id === job.id && S.job.status !== 'uploading') {
      if (up.files.size) { up.files = new Map(); up.error = null; flashNext(filesOf(S.job).length > 1 ? 'Every file is up. The server is checking them now; you can leave this page.' : 'The whole file is up. The server is checking it now; you can leave this page.'); }
      if (S.job.status === 'ready' && !S.match) loadMatch();
    } else if (S.job?.id === job.id && !up.error && !up.paused) {
      // the files chosen are up, others of this upload are still to choose
      for (const f of filesOf(S.job)) if ((f.uploaded_bytes || 0) >= f.bytes_total) up.files.delete(f.index);
    }
    paintAll();
    follow();
  }
  async function syncJob(jid) {
    try {
      const fresh = await ctx.api.get(api.apiPath`imports/${jid}`);
      if (!ctx.alive()) return;
      putJob(fresh);
    } catch (e) {
      if (!ctx.alive() || api.isAbort(e)) return;
      if (e.code === 'no_such_import') {
        S.jobs = S.jobs.filter((x) => x.id !== jid);
        if (S.job?.id === jid) { up.files = new Map(); S.match = null; pickJob(); }
        ctx.flash('This upload was discarded, perhaps from another browser tab.', { kind: 'info' });
      }
    }
  }
  function flashNext(text) { ctx.flash(text, { kind: 'info' }); }
  function pauseUpload() { up.paused = true; up.ctl?.abort(); paintAll(); }

  /* ---------------------------------------------------------------- following a job */
  let pollTimer = null;
  ctx.onCleanup(() => clearTimeout(pollTimer));
  function follow() {
    clearTimeout(pollTimer);
    const j = S.job;
    if (!j) return;
    const otherTabUpload = j.status === 'uploading' && !up.active && !up.files.size;
    // a job waiting in 'ready' is read too (more slowly): another tab can match its cars or discard it
    if (!MOVING.includes(j.status) && !otherTabUpload && j.status !== 'ready') return;
    pollTimer = setTimeout(async () => {
      if (!ctx.alive() || S.job?.id !== j.id) return;
      try {
        const was = S.job?.status;
        const wasToMatch = S.job?.cars_to_match;
        const fresh = await ctx.api.get(api.apiPath`imports/${j.id}`);
        if (!ctx.alive() || S.job?.id !== j.id) return;
        putJob(fresh);
        if (fresh.status === 'ready' && was === 'ready' && fresh.cars_to_match !== wasToMatch) {
          // matched (or changed) elsewhere: read the matches again and repaint Start
          await loadMatch();
          if (!ctx.alive()) return;
          paintJob();
        } else if (fresh.status !== was) {
          if (fresh.status === 'ready') ctx.flash(isCan(fresh) ? 'The zip is checked. See its report, then start the import.' : 'The file is checked. Match its cars, then start the import.', { kind: 'info' });
          if (fresh.status === 'done') { ctx.flash(isCan(fresh) ? 'The import is done. Its logs are decoded into drives and charges as uploads are.' : 'The import is done. The history shows on every page.'); loadOverlaps(); }
          if (fresh.status === 'failed') ctx.flash('The import failed. The reason is on this page.', { kind: 'error' });
          if (['checking', 'ready'].includes(fresh.status) && !S.match) loadMatch();
          paintAll();
        } else paintProgress();
      } catch (e) {
        if (!ctx.alive() || api.isAbort(e)) return;
        if (e.code === 'no_such_import') {
          S.jobs = S.jobs.filter((x) => x.id !== j.id);
          S.match = null;
          if (ctx.query?.import === j.id) ctx.setQuery({ import: null });
          pickJob();
          if (S.job && ['checking', 'ready'].includes(S.job.status)) loadMatch();
          ctx.flash('This import was discarded, perhaps from another browser tab.', { kind: 'info' });
          paintAll();
          follow();
          return;
        }
      }
      follow();
    }, otherTabUpload || j.status === 'ready' ? POLL_MS * 2 : POLL_MS);
  }

  async function refreshJobs(pickRunning = false) {
    try {
      S.jobs = await ctx.api.list('imports', { limit: 100 }).all({ max: 500 });
      if (!ctx.alive()) return;
      if (pickRunning) { const r = running(); if (r) ctx.setQuery({ import: r.id }); S.job = r || S.job; }
      else pickJob();
      S.match = null;
      showUpErr(null);
      if (S.job && ['checking', 'ready'].includes(S.job.status)) await loadMatch();
      paintAll();
      follow();
    } catch (e) { if (!api.isAbort(e)) ctx.flash('The imports could not be read again: ' + errorWords(e).text, { kind: 'error' }); }
  }

  /* ---------------------------------------------------------------- the current job */
  const jobBox = h('div', { class: 'im-job' });
  const progressBox = h('div', { class: 'im-progress', cover: 'web.import.files.col-status', 'aria-live': 'polite' });

  function bar(done, total, label) {
    const p = pct(done, total);
    return h('div', { class: 'im-bar', role: 'progressbar', 'aria-label': label, 'aria-valuemin': '0', 'aria-valuemax': '100', 'aria-valuenow': String(Math.round(p)) },
      h('span', { class: 'im-bar-fill', vars: { '--w': `${p.toFixed(2)}%` } }));
  }

  function paintProgress() {
    const j = S.job;
    if (!j || !UNFINISHED.concat('failed', 'done').includes(j.status)) { replace(progressBox); return; }
    const st = STATUS[j.status] || { word: j.status, kind: null, text: '' };
    const lines = [];
    lines.push(h('p', { class: 'im-status-line' },
      h('span', { class: ['im-status', `is-${st.kind}`] }, h('span', { class: 'im-status-dot', 'aria-hidden': 'true' }), st.word),
      j.pass ? h('span', { class: 'im-pass' }, `pass ${j.pass} of 2`) : null,
      h('span', { class: 'im-file' }, j.name)));
    lines.push(h('p', { class: 'im-status-text' }, statusText(j)));
    if (j.status === 'uploading') {
      const done = j.uploaded_bytes || 0;
      let speed = null;
      if (up.active) {
        const s = (performance.now() - up.t0) / 1000;
        if (s > 2 && done > up.b0) speed = (done - up.b0) / s;
      }
      lines.push(bar(done, j.bytes_total, 'Uploaded'));
      lines.push(h('p', { class: 'im-bytes' },
        h('strong', null, bytes(done)), ` of ${bytes(j.bytes_total)} uploaded (${format(pct(done, j.bytes_total), 'percent', { digits: 0 })})`,
        speed ? ` · ${bytes(speed)} a second, about ${formatDuration(((j.bytes_total - done) / speed) * 1000)} left` : null));
      const files = filesOf(j);
      if (files.length > 1) {
        lines.push(h('ul', { class: 'im-up-files', 'aria-label': 'Each file' }, files.map((f) => {
          const fd = f.uploaded_bytes || 0;
          const whole = fd >= f.bytes_total;
          return h('li', { class: 'im-bytes' },
            h('strong', null, f.name), ` · ${bytes(fd)} of ${bytes(f.bytes_total)} (${format(pct(fd, f.bytes_total), 'percent', { digits: 0 })})`,
            whole ? h('span', { class: 'im-quiet' }, ' · done') : !up.files.has(f.index) && !up.active ? h('span', { class: 'im-quiet' }, ' · to choose again') : null);
        })));
      }
      const need = left(j);
      const many = need.length > 1;
      const acts = [];
      if (up.active) acts.push(button('Pause the upload', { onclick: pauseUpload, kind: 'quiet' }));
      else if (up.files.size) acts.push(button('Go on with the upload', { onclick: upload, kind: 'primary' }));
      if (!up.active && need.some((f) => !up.files.has(f.index))) {
        acts.push(h('label', { class: ['button', up.files.size ? 'button-quiet' : 'button-primary', 'im-resume'] }, many ? 'Choose the same files to go on' : 'Choose the same file to go on',
          h('input', { class: 'im-file-input', type: 'file', multiple: true, 'aria-label': many ? `Choose the files of ${j.name} again to go on` : `Choose ${need[0]?.name || j.name} again to go on`, onchange: (e) => { const fs = [...(e.target.files || [])]; e.target.value = ''; if (fs.length) chose(fs); } })));
      }
      lines.push(h('div', { class: 'im-acts' }, acts));
      const missing = need.filter((f) => !up.files.has(f.index));
      if (!up.active && missing.length) {
        lines.push(h('p', { class: 'note' }, `This upload stopped at ${bytes(done)}. Choose ${missing.map((f) => `${f.name} (${bytes(f.bytes_total)})`).join(', ')} again and ${missing.length > 1 ? 'each goes' : 'it goes'} on from there: the chunks already on the server are not sent twice.`));
      }
      if (up.error) lines.push(h('p', { class: 'form-error', role: 'alert' }, up.error));
    } else if (MOVING.includes(j.status)) {
      lines.push(bar(j.bytes_read, j.bytes_total, j.status === 'checking' ? 'Checked' : 'Imported'));
      lines.push(h('p', { class: 'im-bytes' }, h('strong', null, bytes(j.bytes_read)), ` of ${bytes(j.bytes_total)} read in this pass (${format(pct(j.bytes_read, j.bytes_total), 'percent', { digits: 0 })})`));
    } else {
      lines.push(h('dl', { class: 'im-facts' },
        h('dt', null, 'File'), h('dd', null, `${j.name} · ${bytes(j.bytes_total)}${j.compression && j.compression !== 'plain' ? ` · ${j.compression}` : ''}`),
        j.kind ? [h('dt', null, 'Kind'), h('dd', null, kindWord(j))] : null,
        j.started_ms ? [h('dt', null, 'Upload began'), h('dd', null, formatDateTime(j.started_ms, tz, { now: api.now() }), ` ${zone}`)] : null,
        j.checked_ms ? [h('dt', null, 'Checked'), h('dd', null, formatDateTime(j.checked_ms, tz, { now: api.now() }), ` ${zone}`)] : null,
        j.finished_ms ? [h('dt', null, j.status === 'failed' ? 'Stopped' : 'Finished'), h('dd', null, formatDateTime(j.finished_ms, tz, { now: api.now() }), ` ${zone}`,
          j.started_ms ? h('span', { class: 'im-quiet' }, ` · ${formatDuration(j.finished_ms - j.started_ms)} from upload to end`) : null)] : null));
    }
    replace(progressBox, lines);
  }

  const restartNote = h('div', { class: 'im-aside-note', cover: 'web.import.restart-note' },
    h('p', { class: 'label' }, 'Safe to leave, safe to repeat'),
    h('p', null, 'Once the file is up, the server does the rest: you can close this page and come back. An upload that stops goes on from the last chunk the server has.'),
    h('p', null, 'If the server restarts part-way, nothing is left half-stored: the check stores nothing, and the import is run again from the start of the file, skipping every row that is already in. Running the same file twice never stores a drive or charge twice.'));

  /* ---------------------------------------------------------------- failed */
  function failedBox(j) {
    const code = j.error?.code;
    return h('div', { class: 'im-failed' },
      h('p', { class: 'im-failed-title' }, j.checked_ms ? 'The import stopped part-way.' : 'The file did not pass its check.'),
      h('p', null, FAILURES[code] || 'The server gave this reason:'),
      j.error?.message ? h('p', { class: 'im-failed-msg' }, h('span', { class: 'im-quiet' }, 'Server: '), j.error.message, code ? h('span', { class: 'im-code' }, code) : null) : null,
      h('p', { class: 'im-quiet' }, j.checked_ms
        ? (isCan(j) ? 'Logs already stored stay, checked and signed. Discard this import, then upload the zip again: logs already here are skipped.'
          : 'Rows already stored stay and show as imported. Discard this import, then upload the file again: rows already in are skipped.')
        : 'Nothing from this file was stored. Discard this import, then upload a good file.'));
  }

  /* ---------------------------------------------------------------- car matching */
  const matchBox = h('div', { class: 'im-match' });
  async function loadMatch() {
    const j = S.job;
    if (!j) return;
    try {
      const ans = await ctx.api.get(api.apiPath`imports/${j.id}/cars`);
      if (!ctx.alive() || S.job?.id !== j.id) return;
      S.match = ans.items || [];
      S.matchErr = null;
    } catch (e) {
      if (api.isAbort(e)) return;
      S.match = null;
      S.matchErr = e.code === 'not_checked' ? null : e;
    }
    paintMatch();
  }

  // The choices for one car of the file: the suggestion first, the other cars here (the sold ones under their
  // own "Sold cars" group), then a new car or leaving it out.
  function matchOptions(row) {
    const taken = new Set((S.match || []).filter((x) => x.tm_car !== row.tm_car && isCar(x.match)).map((x) => x.match));
    const opt = (v, t) => h('option', { value: v, selected: String(v) === String(row.match ?? '') }, t);
    const sug = row.suggested && carById.get(row.suggested);
    return [
      row.match == null ? opt('', 'Choose…') : null,
      sug ? opt(sug.id, `${sug.name} (same VIN prefix)`) : null,
      carOptions(S.cars.filter((c) => c.id !== row.suggested), { option: (c) => opt(c.id, `${c.name}${taken.has(c.id) ? ' (already matched)' : ''}`) }),
      isCar(row.match) && !carById.has(row.match) ? opt(row.match, row.match) : null,
      opt('new', 'Add as a new car'), opt('leave_out', 'Leave out'),
    ];
  }

  async function setMatch(row, value, sel) {
    if (!value) return;
    const before = row.match;
    sel.disabled = true;
    try {
      const ans = await ctx.api.put(api.apiPath`imports/${S.job.id}/cars`, { items: [{ tm_car: row.tm_car, match: value }] });
      if (!ctx.alive()) return;
      S.match = ans.items || S.match;
      S.matchErr = null;
      const j = await ctx.api.get(api.apiPath`imports/${S.job.id}`);
      if (!ctx.alive()) return;
      putJob(j);
      paintAll();
    } catch (e) {
      if (!ctx.alive() || api.isAbort(e)) return;
      row.match = before;
      S.matchErr = { row: row.tm_car, text: e.fields ? Object.values(e.fields).join(' ') : e.code === 'not_matchable' ? 'The cars can be matched only while the file is checked and the import has not started.' : errorWords(e).text };
      paintMatch();
    }
  }

  function paintMatch() {
    const j = S.job;
    if (!j || !['checking', 'ready'].includes(j.status) || isCan(j)) { replace(matchBox); return; }
    if (!S.match) {
      replace(matchBox, S.matchErr ? errorView(S.matchErr, { retry: loadMatch, eyebrow: 'Cars in the file' }) : h('p', { class: 'note' }, 'The cars in the file show here once the whole file is up.'));
      return;
    }
    const rows = S.match;
    const unmatched = rows.filter((r) => r.match == null).length;
    const table = dataTable(ctx, {
      rows, itemName: ['car in the file', 'cars in the file'], label: `Cars in the ${isTf(j) ? 'file' : 'backup'} and their match here`,
      caption: unmatched ? `${n(unmatched)} still to match.` : 'Every car has its match.',
      csv: { filename: 'import-cars', columns: [column(isTf(j) ? 'Car in the file' : 'Car in the backup', 'name'), column('VIN prefix', 'vin_prefix'), column('Drives', 'drives', 'count'), column('Charges', 'charges', 'count'), timeColumn('First', 'first_ms', tz), timeColumn('Last', 'last_ms', tz), column('Match', 'match')] },
      columns: [
        { key: 'name', label: isTf(j) ? 'Car in the file' : 'Car in the backup', th: true, cell: (r) => h('span', null, h('span', { class: 'cell-main' }, r.name || `${siteWord(j)} car ${r.tm_car}`), h('span', { class: 'cell-sub' }, `VIN prefix ${r.vin_prefix || 'unknown'} · ${siteWord(j)} car ${r.tm_car}`)), value: (r) => r.name },
        { key: 'drives', label: 'Drives', num: true, cell: (r) => n(r.drives), value: (r) => r.drives },
        { key: 'charges', label: 'Charges', num: true, cell: (r) => n(r.charges), value: (r) => r.charges },
        { key: 'span', label: `Recorded (${zone})`, cell: (r) => (r.first_ms ? `${formatDate(r.first_ms, tz, { weekday: false, year: true })} to ${formatDate(r.last_ms, tz, { weekday: false, year: true })}` : '—'), value: (r) => r.first_ms },
        {
          key: 'match', label: 'Becomes', cls: 'im-match-cell',
          cell: (r) => {
            const sel = h('select', { class: 'field-select im-select', 'aria-label': `Match for ${r.name || r.tm_car}`, disabled: j.status !== 'checking' && j.status !== 'ready' },
              matchOptions(r));
            sel.addEventListener('change', () => setMatch(r, sel.value, sel));
            const note = r.match == null ? h('span', { class: 'cell-sub is-warn' }, 'Not matched yet')
              : r.match === 'new' ? h('span', { class: 'cell-sub' }, 'A new car is made with this name and VIN prefix.')
                : r.match === 'leave_out' ? h('span', { class: 'cell-sub' }, 'Nothing of this car is imported.')
                  : h('span', { class: 'cell-sub' }, r.match === r.suggested ? 'Same VIN prefix as this car. ' : 'Not the VIN prefix of this car. ',
                    h('a', { href: state.buildUrl('settings-cars', { car: r.match }) }, 'Its settings'));
            const err = S.matchErr?.row === r.tm_car ? h('span', { class: 'field-error', role: 'alert' }, S.matchErr.text) : null;
            return h('span', { class: 'im-match-wrap' }, sel, note, err);
          },
          value: (r) => r.match,
        },
      ],
    });
    replace(matchBox,
      h('h3', { class: 'im-sub' }, 'Match the cars'),
      h('p', { class: 'im-lead' }, 'Each car in the file goes to one car here. A car with the same VIN prefix is matched for you; choose another car, add it as a new car, or leave it out.'),
      S.carsMore ? h('p', { class: 'note' }, `The list shows the first ${n(S.cars.length)} cars. A car further down can be renamed in Settings: Cars to find it, or add the imported car as a new car.`) : null,
      table.el);
    table.load();
  }

  /* ---------------------------------------------------------------- start and discard */
  const startErr = h('p', { class: 'form-error', role: 'alert', hidden: true, cover: 'web.import.start-error' });
  // the failure of the job shown: filled while that job has failed, empty and hidden otherwise
  const failRegion = h('div', { class: 'im-fail-region', role: 'alert', cover: 'web.import.run-error' });
  async function startImport(btn) {
    startErr.hidden = true;
    btn.disabled = true;
    try {
      // TeslaMate keeps no currency: the one its charge costs and place prices are in, when the owner names it
      const cur = String(S.currency || '').trim().toUpperCase();
      let body = cur ? { currency: cur } : {};
      if (isTf(S.job)) {
        // TeslaFi's times have no zone and its units are only worked out: the owner's confirmed choices go along
        const t = tfChoices(S.job);
        const zoneName = String(t.zone || '').trim();
        if (!zoneName) { replace(startErr, 'Type the time zone TeslaFi shows its times in (TeslaFi: Settings, Account), such as America/Chicago.'); startErr.hidden = false; btn.disabled = false; return; }
        body = { time_zone: zoneName, distance_unit: t.distance, temperature_unit: t.temperature };
      }
      const j = await ctx.api.post(api.apiPath`imports/${S.job.id}/start`, body);
      if (!ctx.alive()) return;
      putJob(j);
      ctx.flash('The import has started. You can leave this page; it carries on.', { kind: 'info' });
      paintAll();
      follow();
    } catch (e) {
      if (!ctx.alive() || api.isAbort(e)) return;
      btn.disabled = false;
      replace(startErr, e?.fields ? Object.values(e.fields).join(' ') : startWords(e));
      startErr.hidden = false;
    }
  }

  async function discard(j = S.job) {
    if (!j) return;
    const what = j.status === 'done' ? 'This removes the import and its report from the list. The history it brought in stays.'
      : j.status === 'importing' ? 'This stops the import. Rows already stored stay and show as imported; running the file again later skips them.'
        : j.status === 'uploading' ? `This throws away the ${bytes(j.uploaded_bytes)} uploaded so far. Nothing has been stored.`
          : 'This throws away the uploaded file and its check. Nothing from it has been stored.';
    const ok = await confirmDialog({ title: j.status === 'done' ? 'Remove this import from the list?' : 'Discard this import?', text: what, yes: j.status === 'done' ? 'Remove' : 'Discard', danger: true });
    if (!ok || !ctx.alive()) return;
    up.paused = true; up.ctl?.abort(); up.files = new Map(); up.error = null;
    try {
      await ctx.api.del(api.apiPath`imports/${j.id}`);
      if (!ctx.alive()) return;
      S.jobs = S.jobs.filter((x) => x.id !== j.id);
      if (S.job?.id === j.id) { S.match = null; ctx.setQuery({ import: null }); pickJob(); }
      if (S.job && UNFINISHED.includes(S.job.status) && ['checking', 'ready'].includes(S.job.status)) loadMatch();
      ctx.flash(j.status === 'done' ? 'Import removed from the list. Its history stays.' : 'Import discarded.');
      paintAll();
      follow();
    } catch (e) {
      if (!ctx.alive() || api.isAbort(e)) return;
      ctx.flash('Not discarded: ' + errorWords(e).text, { kind: 'error' });
    }
  }

  /* ---------------------------------------------------------------- a TeslaFi file: its time zone and units */
  // TeslaFi writes wall-clock time without a zone and its units are worked out from the first rows, so the owner
  // confirms both before the import: what the check used is filled in, and what is chosen goes along with Start.
  function tfChoices(j) {
    if (!S.tf || S.tf.id !== j.id) {
      const r = j.report || {};
      S.tf = { id: j.id, zone: null, distance: r.units?.distance === 'km' ? 'km' : 'mi', temperature: r.units?.temperature === 'f' ? 'f' : 'c' };
    }
    // until the owner types one: the zone of the car the file is matched to, else the one the check read it in
    if (S.tf.zone == null) {
      const r = j.report || {};
      const m = (S.match || []).find((x) => isCar(x.match));
      return { ...S.tf, zone: (r.time_zone?.from !== 'setting' && m && carById.get(m.match)?.time_zone) || r.time_zone?.name || '' };
    }
    return S.tf;
  }
  function tfStep(j, can) {
    const t = tfChoices(j);
    const r = j.report || {};
    const zf = field({
      label: 'Time zone of TeslaFi’s times', name: 'tf_zone', value: t.zone, cls: 'im-tf-zone',
      hint: ['TeslaFi writes local time with no zone. Use the zone in TeslaFi’s own settings (Settings, Account), such as America/Chicago. ',
        r.time_zone?.name ? `The check read the file in ${r.time_zone.name.replace(/_/g, ' ')} (${ZONE_FROM[r.time_zone.from] || r.time_zone.from}).` : ''],
      attrs: { list: 'im-tf-zones', autocomplete: 'off', spellcheck: 'false', disabled: !can },
    });
    zf.append(h('datalist', { id: 'im-tf-zones' }, ZONES.map((z) => h('option', { value: z }))));
    zf.control.addEventListener('input', () => { S.tf.zone = zf.control.value; });
    const dist = choice({ label: 'Distances in the file', name: 'tf_distance', value: t.distance, options: [['mi', 'Miles'], ['km', 'Kilometres']], onchange: (v) => { S.tf.distance = v; } });
    const temp = choice({ label: 'Temperatures in the file', name: 'tf_temperature', value: t.temperature, options: [['c', '°C'], ['f', '°F']], onchange: (v) => { S.tf.temperature = v; } });
    if (!can) for (const i of [...dist.querySelectorAll('input, button'), ...temp.querySelectorAll('input, button')]) i.disabled = true;
    const u = r.units || {};
    return h('div', { class: 'im-tf-step' },
      h('p', { class: 'im-sub' }, 'Before the import: time zone and units'),
      zf,
      h('div', { class: 'im-tf-units' }, dist, temp),
      h('p', { class: 'im-quiet' }, 'TeslaFi’s base columns are in miles and °C whatever the car shows; its display columns follow the owner’s units. ',
        u.distance ? `The check read distances in ${u.distance === 'km' ? 'kilometres' : 'miles'} (${UNIT_FROM[u.distance_from] || u.distance_from}) and temperatures in ${u.temperature === 'f' ? '°F' : '°C'} (${UNIT_FROM[u.temperature_from] || u.temperature_from}). ` : '',
        'Imported in the wrong zone or units? Import the same files again with the right ones: what they stored is replaced.'));
  }

  function paintJob() {
    const j = S.job;
    const r = running();
    const shown = j && (UNFINISHED.includes(j.status) || j.status === 'failed' || (j.status === 'done' && j.id === ctx.query?.import));
    const parts = [stepsEl];
    if (!r) parts.push(drop, upErr);
    else if (!shown || j.id !== r.id) parts.push(h('p', { class: 'note' }, 'An import is not finished. ', h('a', { href: state.buildUrl('import', { query: { import: r.id } }), onclick: (e) => { e.preventDefault(); S.job = r; S.match = null; ctx.setQuery({ import: r.id }); if (['checking', 'ready'].includes(r.status)) loadMatch(); paintAll(); follow(); } }, `Show ${r.name}`)));
    replace(failRegion, shown && j.status === 'failed' ? failedBox(j) : null);
    failRegion.hidden = !(shown && j.status === 'failed');
    if (shown) parts.push(progressBox);
    // a refused file (or any other upload message) shows also while an import is unfinished
    if (r) parts.push(upErr);
    parts.push(failRegion);
    if (shown) parts.push(matchBox);

    // Start: there whenever no import runs, so it says what it waits for
    const unmatched = shown ? (j.cars_to_match ?? (S.match ? S.match.filter((x) => x.match == null).length : null)) : null;
    const can = shown && j.status === 'ready' && unmatched === 0;
    const why = !shown || !['uploading', 'checking', 'ready'].includes(j.status) ? (j?.status === 'importing' ? 'The import is running.' : 'Choose a backup file above. Start becomes possible once it is up and checked and its cars are matched.')
      : j.status === 'uploading' ? 'Start becomes possible once the whole file is up and checked.'
        : j.status === 'checking' ? 'Start becomes possible once the check is done; you can match the cars meanwhile.'
          : unmatched ? `Match ${n(unmatched)} more ${unmatched === 1 ? 'car' : 'cars'} first.`
            : isCan(j) ? 'The checked logs are stored and decoded as uploads are, each for the car of its car key. You can leave this page meanwhile.'
              : 'The checked rows are stored for the matched cars. It takes a few minutes per gigabyte, and you can leave this page meanwhile.';
    if (!shown || j.status !== 'importing') {
      const btn = button('Start the import', { kind: 'primary', disabled: !can, cover: 'web.import.start' });
      btn.addEventListener('click', () => startImport(btn));
      const cur = field({ label: 'Currency of TeslaMate’s costs', name: 'currency', value: S.currency || '', cls: 'im-currency',
        hint: 'A code such as EUR. TeslaMate does not store one; without it, imported charges keep no cost and its places no price.',
        attrs: { maxlength: 3, autocomplete: 'off', spellcheck: 'false', disabled: !can, placeholder: 'EUR' } });
      cur.control.addEventListener('input', () => { S.currency = cur.control.value; });
      parts.push(h('div', { class: 'im-start' }, isCan(j) ? null : isTf(j) ? tfStep(j, can) : cur, h('div', { class: 'im-start-row' }, btn, h('p', { class: 'im-quiet' }, why)), startErr));
    }
    if (shown) {
      const discardBtn = button(j.status === 'done' ? 'Remove from the list' : 'Discard this import', { kind: 'danger', cls: 'button-small', onclick: () => discard(j), cover: 'web.import.discard' });
      parts.push(h('div', { class: 'im-discard' }, discardBtn,
        h('span', { class: 'im-quiet' }, j.status === 'done' ? 'The history it brought in stays.' : 'You can discard at any time. History already imported stays.')));
    }
    replace(jobBox, parts);
    paintSteps();
    paintProgress();
    paintMatch();
  }

  /* ---------------------------------------------------------------- the report */
  const reportBox = h('div', { class: 'im-report' });
  function paintReport() {
    const j = S.job?.report ? S.job : S.jobs.find((x) => x.report && x.status === 'done');
    if (!j || !j.report) {
      replace(reportBox, empty({ title: 'No report yet.', text: 'A report shows here once a file is checked: rows read per table, rows imported, how many rows were skipped and why.', cover: ['web.import.rejected-count', 'web.import.rejected.col-reason'] }));
      return;
    }
    const rep = j.report;
    const imported = rep.imported !== false && j.status === 'done';
    if (rep.kind === CAN) { paintCanReport(j, rep, imported); return; }
    if (rep.kind === TF) { paintTfReport(j, rep, imported); return; }
    const tables = Object.entries(rep.tables || {}).map(([name, t0]) => {
      const t = t0 || {};
      const has = t.read != null;
      const skipped = has ? sum(t.skipped) : sumOf(t.skipped);
      return { name, read: t.read ?? null, imported: imported ? t.imported ?? null : null, would: has ? t.read - (skipped || 0) : null, skipped, blank: has ? sum(t.bad_values) : sumOf(t.bad_values), closed: t.closed_at_last_reading ?? null, t };
    });
    const tot = (k) => addUp(tables.map((r) => r[k]));
    // old charges or states TeslaMate never ended: taken, ending at their last reading (in read and imported, not skipped)
    const closedRows = tables.filter((r) => r.closed);
    const closedLine = closedRows.map((r) => {
      const [one, many] = { charging_processes: ['charge', 'charges'], states: ['state', 'states'] }[r.name] || ['row', 'rows'];
      return `${count(r.closed, one, many)} TeslaMate never ended ${imported ? (r.closed === 1 ? 'was' : 'were') : 'will be'} taken, ending at ${r.closed === 1 ? 'its' : 'their'} last reading.`;
    }).join(' ');
    const skippedRows = [];
    for (const r of tables) {
      for (const [code, cnt] of Object.entries(r.t.skipped || {})) if (cnt) skippedRows.push({ table: r.name, kind: 'skipped', code, count: cnt, words: reasonWords(code) });
      for (const [col, cnt] of Object.entries(r.t.bad_values || {})) if (cnt) skippedRows.push({ table: r.name, kind: 'blank', code: col, count: cnt, words: blankWords(col) });
    }
    const skippedTotal = tot('skipped');
    const fileName = `import-${j.id}`;

    const figs = figures([
      figure('Rows read', nn(tot('read')), 'Every row of every table this import uses, read in the check.'),
      figure(imported ? 'Rows imported' : 'Rows to import', nn(imported ? tot('imported') : tot('would')), imported ? 'Stored and shown, marked as imported.' : 'What the import will store once it is started.'),
      figure('Rows skipped', nn(skippedTotal), skippedTotal ? 'Each has its reason below. The import went on past them.' : 'No row had to be skipped.', { cover: 'web.import.rejected-count', cls: skippedTotal ? 'im-fig-warn' : null }),
      figure('Values left blank', nn(tot('blank')), 'Rows kept with one value left out, each listed below.'),
      rep.span ? figure('History in the file', `${formatDate(rep.span.from_ms, tz, { weekday: false, year: true })} to ${formatDate(rep.span.to_ms, tz, { weekday: false, year: true })}`, `${formatDuration(rep.span.to_ms - rep.span.from_ms, { style: 'long' })} of TeslaMate history (${zone}).`) : null,
    ].filter(Boolean), { cls: 'im-figs' });

    const tTable = dataTable(ctx, {
      rows: tables, itemName: ['table', 'tables'], label: 'Rows per table',
      csv: { filename: `${fileName}-tables`, columns: [column('Table', 'name'), column('Read', 'read', 'count'), column(imported ? 'Imported' : 'To import', imported ? 'imported' : 'would', 'count'), ...(closedRows.length ? [column('Closed at last reading', 'closed', 'count')] : []), column('Skipped', 'skipped', 'count'), column('Values left blank', 'blank', 'count')] },
      columns: [
        { key: 'name', label: 'Table', th: true, cell: (r) => h('span', null, h('span', { class: 'cell-main' }, tableName(r.name)), h('span', { class: 'cell-sub' }, tableNote(r.name))), value: (r) => r.name },
        { key: 'read', label: 'Read', num: true, sort: true, cell: (r) => nn(r.read), value: (r) => r.read },
        { key: 'imported', label: imported ? 'Imported' : 'To import', num: true, sort: true, cell: (r) => nn(imported ? r.imported : r.would), value: (r) => (imported ? r.imported : r.would) },
        closedRows.length ? { key: 'closed', label: 'Closed at last reading', num: true, sort: true, cell: (r) => nn(r.closed), value: (r) => r.closed } : null,
        { key: 'skipped', label: 'Skipped', num: true, sort: true, cell: (r) => (r.skipped ? h('span', { class: 'is-warn' }, n(r.skipped)) : nn(r.skipped)), value: (r) => r.skipped },
        { key: 'blank', label: 'Values left blank', num: true, sort: true, cell: (r) => nn(r.blank), value: (r) => r.blank },
      ].filter(Boolean),
    });

    const sTable = skippedRows.length ? dataTable(ctx, {
      rows: skippedRows, itemName: ['reason', 'reasons'], label: 'Why rows were skipped or values left blank', cover: 'web.import.rejected.col-reason',
      csv: { filename: `${fileName}-skipped`, columns: [column('Table', 'table'), column('What', 'kind'), column('Reason code', 'code'), column('Reason', 'reason'), column('Rows', 'count', 'count')] },
      columns: [
        { key: 'table', label: 'Table', th: true, cell: (r) => tableName(r.table), value: (r) => r.table },
        { key: 'kind', label: 'What happened', cell: (r) => (r.kind === 'skipped' ? flag('Row skipped', { kind: 'warn' }) : flag('Value left blank', { kind: 'estimated' })), value: (r) => r.kind },
        { key: 'reason', label: 'Reason', cls: 'im-reason', cell: (r) => h('span', null, h('span', { class: 'cell-main' }, r.words[0]), h('span', { class: 'cell-sub' }, r.words[1])), value: (r) => r.words[0] },
        { key: 'count', label: 'Rows', num: true, sort: true, cell: (r) => n(r.count), value: (r) => r.count },
      ],
    }) : null;
    if (sTable) for (const r of skippedRows) r.reason = r.words[0];

    // a car left out is not in the report (the server leaves it out too): only the cars that count
    const cars = (rep.cars || []).filter((c) => c.match !== 'leave_out').map((c) => ({ ...c, here: isCar(c.match) ? carName(c.match) : c.match === 'new' ? 'A new car' : 'Not matched' }));
    const carRows = cars.some((c) => 'rows' in c);
    const cTable = cars.length ? dataTable(ctx, {
      rows: cars, itemName: ['car', 'cars'], label: 'Per car',
      csv: { filename: `${fileName}-cars`, columns: [column('TeslaMate car', 'tm_car'), column('Name', 'name'), column('Car here', 'here'), carRows ? column('Rows', 'rows', 'count') : null, column('Drives', 'drives', 'count'), column('Charges', 'charges', 'count')].filter(Boolean) },
      columns: [
        { key: 'tm', label: 'TeslaMate car', th: true, cell: (c) => (c.name ? h('span', null, h('span', { class: 'cell-main' }, c.name), h('span', { class: 'cell-sub' }, `TeslaMate car ${c.tm_car}`)) : `Car ${c.tm_car}`), value: (c) => c.name || c.tm_car },
        { key: 'here', label: 'Car here', cell: (c) => (isCar(c.match) ? h('a', { href: state.buildUrl('timeline', { car: c.match, period: rep.span ? { from: rep.span.from_ms, to: rep.span.to_ms } : null }) }, c.here) : c.here), value: (c) => c.here },
        carRows ? { key: 'rows', label: 'Rows', num: true, cell: (c) => nn(c.rows), value: (c) => c.rows } : null,
        { key: 'drives', label: imported ? 'Drives imported' : 'Drives', num: true, cell: (c) => nn(c.drives), value: (c) => c.drives },
        { key: 'charges', label: imported ? 'Charges imported' : 'Charges', num: true, cell: (c) => nn(c.charges), value: (c) => c.charges },
      ].filter(Boolean),
    }) : null;

    // several files of one TeslaFi export: what each one gave
    const uploads = Array.isArray(rep.uploads) && rep.uploads.length ? rep.uploads.map((u, i) => ({ ...u, i, skipped: sumOf(u.rows_skipped) })) : null;
    const fTable = uploads ? dataTable(ctx, {
      rows: uploads, itemName: ['file', 'files'], label: 'Rows per file',
      csv: { filename: `${fileName}-files`, columns: [column('File', 'name'), column('Rows read', 'rows_read', 'count'), column('Rows used', 'rows_used', 'count'), column('Rows skipped', 'skipped', 'count')] },
      columns: [
        { key: 'name', label: 'File', th: true, cell: (u) => h('span', null, h('span', { class: 'cell-main' }, u.name), u.compression && u.compression !== 'plain' ? h('span', { class: 'cell-sub' }, `${u.compression} · ${bytes(u.packed_bytes)}`) : null), value: (u) => u.i },
        { key: 'read', label: 'Rows read', num: true, sort: true, cell: (u) => nn(u.rows_read), value: (u) => u.rows_read },
        { key: 'used', label: 'Used', num: true, sort: true, cell: (u) => nn(u.rows_used), value: (u) => u.rows_used },
        { key: 'skipped', label: 'Skipped', num: true, sort: true, cell: (u) => (u.skipped ? h('span', { class: 'is-warn' }, n(u.skipped)) : nn(u.skipped)), value: (u) => u.skipped },
      ],
    }) : null;

    replace(reportBox,
      h('p', { class: 'im-lead' }, h('span', { class: 'im-report-of' }, j.name), ' · ', statusWord(j.status),
        j.finished_ms ? ` ${formatDateTime(j.finished_ms, tz, { now: api.now() })} ${zone}` : j.checked_ms ? ` since ${formatDateTime(j.checked_ms, tz, { now: api.now() })} ${zone}` : null),
      figs,
      closedLine ? h('p', { class: 'im-lead' }, closedLine) : null,
      skippedTotal ? h('div', { class: 'im-skip-note' },
        h('p', null, `${n(skippedTotal)} ${skippedTotal === 1 ? 'row was' : 'rows were'} skipped. That did not stop the import; every other row went in.`),
        h('p', null, 'A drive or charge built around a skipped row can come out with less detail (a missing track point leaves a straight line on the map). The report keeps only counts and reasons, never a row’s values or positions, so it can stay after the import.')) : null,
      h('div', { class: 'im-report-grid' },
        h('div', null, fTable ? [h('h3', { class: 'im-sub' }, 'Files'), fTable.el] : null, h('h3', { class: 'im-sub' }, 'Per table'), tTable.el),
        h('div', null,
          h('h3', { class: 'im-sub' }, 'Why rows were skipped'),
          sTable ? sTable.el : empty({ title: 'Nothing was skipped.', text: 'Every row passed its checks.', cover: 'web.import.rejected.col-reason' }),
          cTable ? [h('h3', { class: 'im-sub' }, 'Per car'), cTable.el] : null)));
    fTable?.load();
    tTable.load();
    sTable?.load();
    cTable?.load();
  }

  /* ---------------------------------------------------------------- a TeslaFi file's report */
  const TF_FOUND = [
    ['drives', 'Drives', 'From the gear: D or R (or moving), to P, a charge, 10 minutes standing still or 15 without a poll.'],
    ['charges', 'Charges', 'While TeslaFi says “Charging”; AC or DC from its fast-charger columns. No cost: TeslaFi’s files keep none.'],
    ['states', 'Car states', 'Online and asleep spells; “offline” is unknown, never sleep.'],
    ['updates', 'Software updates', 'Where the car’s version changes between polls.'],
    ['positions', 'Positions', 'Every poll with a place or a battery figure.'],
    ['charge_points', 'Charge-curve points', 'The polls taken while charging.'],
  ];
  function paintTfReport(j, rep, imported) {
    const R = rep.rows || {};
    const F = rep.found || {};
    const fileName = `import-${j.id}`;
    const day = (ms) => formatDate(ms, tz, { weekday: false, year: true });
    const skipped = sum(R.skipped);
    const u = rep.units || {};
    const zoneInfo = rep.time_zone || {};
    const filesSkipped = Object.entries(rep.files_skipped || {});
    const figs = figures([
      figure('Files read', n(rep.files ?? 0), filesSkipped.length ? `${n(filesSkipped.length)} other ${filesSkipped.length === 1 ? 'file' : 'files'} passed over (below).` : `${bytes(rep.unpacked_bytes)} of TeslaFi’s polls${rep.compression && rep.compression !== 'plain' ? `, ${rep.compression}` : ''}.`),
      figure('Polls read', n(R.read ?? 0), `${n(R.used ?? 0)} used${R.windows_1252 ? `, ${n(R.windows_1252)} read as Windows-1252 text` : ''}.`),
      figure('Polls skipped', n(skipped), skipped ? 'Each has its reason below; the rest went on.' : 'No poll had to be skipped.', { cover: 'web.import.rejected-count', cls: skipped ? 'im-fig-warn' : null }),
      figure(imported ? 'Drives and charges imported' : 'Drives and charges found', `${n(F.drives ?? 0)} · ${n(F.charges ?? 0)}`, imported ? 'Stored, marked “Imported from TeslaFi”.' : 'What the import will store once it is started.'),
      rep.span?.from_ms ? figure('History in the files', `${day(rep.span.from_ms)} to ${day(rep.span.to_ms)}`, `${formatDuration(rep.span.to_ms - rep.span.from_ms, { style: 'long' })} of TeslaFi polls (${zone}).`) : null,
    ].filter(Boolean), { cls: 'im-figs' });
    const readIn = h('dl', { class: 'im-facts im-tf-read' },
      h('dt', null, 'Time zone'), h('dd', null, zoneInfo.name ? zoneInfo.name.replace(/_/g, ' ') : 'not known',
        zoneInfo.from ? h('span', { class: 'im-quiet' }, ` · ${ZONE_FROM[zoneInfo.from] || zoneInfo.from}`) : null,
        zoneInfo.dst_repeated_rows ? h('span', { class: 'im-quiet' }, ` · ${n(zoneInfo.dst_repeated_rows)} polls in the hour the clocks went back, read in their order`) : null),
      h('dt', null, 'Distances'), h('dd', null, u.distance === 'km' ? 'Kilometres' : u.distance === 'mi' ? 'Miles' : 'not known', u.distance_from ? h('span', { class: 'im-quiet' }, ` · ${UNIT_FROM[u.distance_from] || u.distance_from}`) : null),
      h('dt', null, 'Temperatures'), h('dd', null, u.temperature === 'f' ? '°F' : u.temperature === 'c' ? '°C' : 'not known', u.temperature_from ? h('span', { class: 'im-quiet' }, ` · ${UNIT_FROM[u.temperature_from] || u.temperature_from}`) : null),
      (rep.missing_columns || []).length ? [h('dt', null, 'Columns missing'), h('dd', null, h('span', { class: 'mono' }, rep.missing_columns.join(', ')), h('span', { class: 'im-quiet' }, ' · read as empty'))] : null,
      rep.unknown_columns ? [h('dt', null, 'Columns passed over'), h('dd', null, `${n(rep.unknown_columns)} not used by Sentry Mate`)] : null);
    const found = TF_FOUND.map(([k, name, note]) => ({ k, name, note, count: F[k] ?? 0 }));
    const fTable = dataTable(ctx, {
      rows: found, itemName: ['kind', 'kinds'], label: imported ? 'What was imported' : 'What the files give',
      csv: { filename: `${fileName}-found`, columns: [column('What', 'name'), column(imported ? 'Imported' : 'Found', 'count', 'count')] },
      columns: [
        { key: 'name', label: 'What', th: true, cell: (r) => h('span', null, h('span', { class: 'cell-main' }, r.name), h('span', { class: 'cell-sub' }, r.note)), value: (r) => r.name },
        { key: 'count', label: imported ? 'Imported' : 'Found', num: true, cell: (r) => n(r.count), value: (r) => r.count },
      ],
    });
    const why = [];
    for (const [code, cnt] of Object.entries(R.skipped || {})) if (cnt) why.push({ kind: 'skipped', code, count: cnt, words: reasonWords(code) });
    for (const [col, cnt] of Object.entries(R.bad_values || {})) if (cnt) why.push({ kind: 'blank', code: col, count: cnt, words: blankWords(col) });
    for (const r of why) r.reason = r.words[0];
    const sTable = why.length ? dataTable(ctx, {
      rows: why, itemName: ['reason', 'reasons'], label: 'Why polls were skipped or values left blank', cover: 'web.import.rejected.col-reason',
      csv: { filename: `${fileName}-skipped`, columns: [column('What', 'kind'), column('Reason code', 'code'), column('Reason', 'reason'), column('Polls', 'count', 'count')] },
      columns: [
        { key: 'kind', label: 'What happened', th: true, cell: (r) => (r.kind === 'skipped' ? flag('Poll skipped', { kind: 'warn' }) : flag('Value left blank', { kind: 'estimated' })), value: (r) => r.kind },
        { key: 'reason', label: 'Reason', cls: 'im-reason', cell: (r) => h('span', null, h('span', { class: 'cell-main' }, r.words[0]), h('span', { class: 'cell-sub' }, r.words[1])), value: (r) => r.words[0] },
        { key: 'count', label: 'Polls', num: true, cell: (r) => n(r.count), value: (r) => r.count },
      ],
    }) : null;
    const cars = (rep.cars || []).filter((c) => c.match !== 'leave_out').map((c) => ({ ...c, here: isCar(c.match) ? carName(c.match) : c.match === 'new' ? 'A new car' : 'Not matched' }));
    const cTable = cars.length ? dataTable(ctx, {
      rows: cars, itemName: ['car', 'cars'], label: 'Per car',
      csv: { filename: `${fileName}-cars`, columns: [column('TeslaFi car', 'name'), column('VIN prefix', 'vin_prefix'), column('Car here', 'here'), column('Polls', 'rows', 'count'), column('Drives', 'drives', 'count'), column('Charges', 'charges', 'count')] },
      columns: [
        { key: 'name', label: 'TeslaFi car', th: true, cell: (c) => h('span', null, h('span', { class: 'cell-main' }, c.name || `Car ${c.tm_car}`), h('span', { class: 'cell-sub' }, `VIN prefix ${c.vin_prefix || 'unknown'}`)), value: (c) => c.name || c.tm_car },
        { key: 'here', label: 'Car here', cell: (c) => (isCar(c.match) ? h('a', { href: state.buildUrl('timeline', { car: c.match, period: rep.span?.from_ms ? { from: rep.span.from_ms, to: rep.span.to_ms } : null }) }, c.here) : c.here), value: (c) => c.here },
        { key: 'rows', label: 'Polls', num: true, cell: (c) => nn(c.rows), value: (c) => c.rows },
        { key: 'dc', label: 'Drives · charges', num: true, cell: (c) => h('span', null, nn(c.drives), ' · ', nn(c.charges)), value: (c) => c.drives },
      ],
    }) : null;
    // the files of the download sent together: what each one gave
    const uploads = Array.isArray(rep.uploads) && rep.uploads.length ? rep.uploads.map((x, i) => ({ ...x, i, skipped: sumOf(x.rows_skipped) })) : null;
    const uTable = uploads ? dataTable(ctx, {
      rows: uploads, itemName: ['file', 'files'], label: 'Polls per file',
      csv: { filename: `${fileName}-files`, columns: [column('File', 'name'), column('Polls read', 'rows_read', 'count'), column('Polls used', 'rows_used', 'count'), column('Polls skipped', 'skipped', 'count')] },
      columns: [
        { key: 'name', label: 'File', th: true, cell: (x) => h('span', null, h('span', { class: 'cell-main' }, x.name), x.compression && x.compression !== 'plain' ? h('span', { class: 'cell-sub' }, `${x.compression} · ${bytes(x.packed_bytes)}`) : null), value: (x) => x.i },
        { key: 'read', label: 'Polls read', num: true, sort: true, cell: (x) => nn(x.rows_read), value: (x) => x.rows_read },
        { key: 'used', label: 'Used', num: true, sort: true, cell: (x) => nn(x.rows_used), value: (x) => x.rows_used },
        { key: 'skipped', label: 'Skipped', num: true, sort: true, cell: (x) => (x.skipped ? h('span', { class: 'is-warn' }, n(x.skipped)) : nn(x.skipped)), value: (x) => x.skipped },
      ],
    }) : null;
    const warnings = rep.warnings || [];
    replace(reportBox,
      h('p', { class: 'im-lead' }, h('span', { class: 'im-report-of' }, j.name), ' · TeslaFi data download · ', statusWord(j.status),
        j.finished_ms ? ` ${formatDateTime(j.finished_ms, tz, { now: api.now() })} ${zone}` : j.checked_ms ? ` since ${formatDateTime(j.checked_ms, tz, { now: api.now() })} ${zone}` : null),
      figs,
      warnings.length ? h('ul', { class: 'im-warnings', role: 'note' }, warnings.map((w) => h('li', null, icon('warning'), h('span', null, w.charAt(0).toUpperCase() + w.slice(1), w.endsWith('.') ? '' : '.')))) : null,
      h('div', { class: 'im-report-grid' },
        h('div', null,
          h('h3', { class: 'im-sub' }, 'Read in'), readIn,
          uTable ? [h('h3', { class: 'im-sub' }, 'Files'), uTable.el] : null,
          h('h3', { class: 'im-sub' }, imported ? 'Imported' : 'Found in the polls'), fTable.el),
        h('div', null,
          h('h3', { class: 'im-sub' }, 'Why polls were skipped'),
          sTable ? sTable.el : empty({ title: 'Nothing was skipped.', text: 'Every poll could be read.', cover: 'web.import.rejected.col-reason' }),
          filesSkipped.length ? [h('h3', { class: 'im-sub' }, 'Files passed over'), h('ul', { class: 'im-files-skipped' }, filesSkipped.map(([name, why2]) => h('li', null, h('span', { class: 'mono' }, name), h('span', { class: 'im-quiet' }, ` · ${why2}`))))] : null,
          cTable ? [h('h3', { class: 'im-sub' }, 'Per car'), cTable.el] : null)));
    uTable?.load();
    fTable.load();
    sTable?.load();
    cTable?.load();
  }

  /* ---------------------------------------------------------------- a CAN export's report */
  function paintCanReport(j, rep, imported) {
    const L = rep.logs || {};
    const took = imported ? L.imported : L.to_import;
    const fileName = `import-${j.id}`;
    const day = (ms) => formatDate(ms, tz, { weekday: false, year: true });
    const figs = figures([
      figure('Logs read', n(L.read), `${bytes(L.bytes)} of CAN Bridge logs in the zip.`),
      figure(imported ? 'Logs imported' : 'Logs to import', n(took), 'Signed by the car’s CAN Bridge and unchanged since: stored as if it had uploaded them.'),
      figure('Already here', n(L.already_here), 'The same log (name and SHA-256) is here already, so it is not stored twice.'),
      figure('Refused', n(L.refused), L.refused ? 'Never stored. Each has its reason below.' : 'Every log checked out.', { cover: 'web.import.rejected-count', cls: L.refused ? 'im-fig-warn' : null }),
      rep.span?.from_ms ? figure('Logs from', `${day(rep.span.from_ms)} to ${day(rep.span.to_ms)}`, `The CAN Bridge’s own recording (${zone}).`) : null,
    ].filter(Boolean), { cls: 'im-figs' });
    const keys = (rep.cars || []).map((k) => ({ ...k, here: k.car ? (k.car_name || k.car) : KEY_STATUS[k.status] || k.status, would: k.logs - k.already_here - k.refused,
      chainText: k.chain_gaps || k.chain_forks ? [k.chain_gaps ? `${n(k.chain_gaps)} ${k.chain_gaps === 1 ? 'gap' : 'gaps'}` : null, k.chain_forks ? `${n(k.chain_forks)} ${k.chain_forks === 1 ? 'fork' : 'forks'}` : null].filter(Boolean).join(', ') : 'Whole' }));
    const kTable = dataTable(ctx, {
      rows: keys, itemName: ['car key', 'car keys'], label: 'Logs per car key',
      csv: { filename: `${fileName}-keys`, columns: [column('Car key', 'key'), column('Goes to', 'here'), column('Logs', 'logs', 'count'), column(imported ? 'Imported' : 'To import', imported ? 'imported' : 'would', 'count'), column('Already here', 'already_here', 'count'), column('Refused', 'refused', 'count'), column('Chain', 'chainText')] },
      columns: [
        { key: 'key', label: 'Car key', th: true, cell: (k) => h('span', null, h('span', { class: 'cell-main mono' }, k.key), h('span', { class: 'cell-sub' }, k.first_ms ? `${day(k.first_ms)} to ${day(k.last_ms)}` : '')), value: (k) => k.key },
        { key: 'here', label: 'Goes to', cell: (k) => (k.car ? h('a', { href: state.buildUrl('timeline', { car: k.car }) }, k.here) : h('span', { class: 'cell-sub is-warn' }, k.here)), value: (k) => k.here },
        { key: 'logs', label: 'Logs', num: true, sort: true, cell: (k) => n(k.logs), value: (k) => k.logs },
        { key: 'imported', label: imported ? 'Imported' : 'To import', num: true, sort: true, cell: (k) => n(imported ? k.imported : k.would), value: (k) => (imported ? k.imported : k.would) },
        { key: 'already_here', label: 'Already here', num: true, sort: true, cell: (k) => n(k.already_here), value: (k) => k.already_here },
        { key: 'refused', label: 'Refused', num: true, sort: true, cell: (k) => (k.refused ? h('span', { class: 'is-warn' }, n(k.refused)) : n(0)), value: (k) => k.refused },
        { key: 'chain', label: 'Chain', cell: (k) => (k.chain_gaps || k.chain_forks ? h('span', { class: 'is-warn', title: 'As for a live upload: a gap is a log the chain names that is not here, a fork two logs in one place' }, k.chainText) : k.chainText), value: (k) => k.chainText },
      ],
    });
    const refused = (rep.refused_logs || []).map((r) => ({ ...r, why: canReason(r.reason)[0] }));
    const rTable = refused.length ? dataTable(ctx, {
      rows: refused, itemName: ['refused log', 'refused logs'], label: 'Logs that were refused, with why', cover: 'web.import.rejected.col-reason',
      csv: { filename: `${fileName}-refused`, columns: [column('Log', 'name'), column('Reason code', 'reason'), column('Reason', 'why')] },
      columns: [
        { key: 'name', label: 'Log', th: true, cell: (r) => h('span', { class: 'mono' }, r.name), value: (r) => r.name },
        { key: 'why', label: 'Why', cls: 'im-reason', cell: (r) => h('span', null, h('span', { class: 'cell-main' }, r.why), h('span', { class: 'cell-sub' }, canReason(r.reason)[1])), value: (r) => r.why },
      ],
    }) : null;
    const by = Object.entries(rep.refused || {}).filter(([, c]) => c);
    replace(reportBox,
      h('p', { class: 'im-lead' }, h('span', { class: 'im-report-of' }, j.name), ' · ', statusWord(j.status),
        j.finished_ms ? ` ${formatDateTime(j.finished_ms, tz, { now: api.now() })} ${zone}` : j.checked_ms ? ` since ${formatDateTime(j.checked_ms, tz, { now: api.now() })} ${zone}` : null),
      figs,
      L.refused ? h('div', { class: 'im-skip-note' },
        h('p', null, `${n(L.refused)} ${L.refused === 1 ? 'log was' : 'logs were'} refused: ${by.map(([k, c]) => `${canReason(k)[0].toLowerCase()} ${n(c)}`).join(', ')}.`),
        h('p', null, 'A refused log is never stored, so it cannot make up history. Every other log is imported. Only the first 100 are named below; all are counted.')) : null,
      h('div', { class: 'im-report-grid' },
        h('div', null, h('h3', { class: 'im-sub' }, 'Per car key'), kTable.el),
        h('div', null,
          h('h3', { class: 'im-sub' }, 'Refused logs'),
          rTable ? rTable.el : empty({ title: 'No log was refused.', text: 'Every log of the zip is signed by its car’s CAN Bridge and unchanged since.', cover: 'web.import.rejected.col-reason' }))));
    kTable.load();
    rTable?.load();
  }

  /* ---------------------------------------------------------------- overlaps: live and imported, and two imports */
  const overlapBox = h('div', { class: 'im-overlaps' });
  function overlapCars() {
    const ids = new Set();
    for (const j of S.jobs) for (const c of j.report?.cars || []) if (isCar(c.match) && j.status === 'done') ids.add(c.match);
    // a car that holds imported history from anywhere (an import discarded from the list keeps its rows)
    for (const c of S.cars) if ((c.sources || []).some((x) => x.kind === 'import')) ids.add(c.id);
    return [...ids].slice(0, 50);
  }
  async function loadOverlaps() {
    const ids = overlapCars();
    await Promise.all(ids.map(async (cid) => {
      try {
        const ans = await ctx.api.get(api.apiPath`cars/${cid}/overlaps`, { limit: 200 });
        S.overlaps.set(cid, { tz: ans.time_zone || carById.get(cid)?.time_zone || tz, items: ans.items || [], err: null });
      } catch (e) {
        if (api.isAbort(e)) return;
        S.overlaps.set(cid, { tz, items: [], err: e });
      }
    }));
    if (ctx.alive()) paintOverlaps();
  }

  // the side shown: the new shown_source (live | import | import:<site>), else the old shown word
  const shownOf = (r) => r.shown_source || (r.shown === 'teslamate' ? (r.kind === 'imports' ? 'import:teslamate' : 'import') : 'live');
  const liveOf = (r) => r.live || r.can || {};
  const importedOf = (r) => r.imported || r.teslamate || {};
  const siteCounts = (r, site) => (r.sites && r.sites[site]) || (site === 'teslamate' ? r.teslamate : null) || { drives: 0, charges: 0 };
  // which live inputs the span's live side has (its sources less the imports), and which import sites it holds
  const liveLabel = (r) => { const l = (r.sources || []).filter((x) => x.kind !== 'import'); return l.length ? labelOf(l) : 'Data from SMO'; };
  const importSites = (r) => {
    const fromSites = Object.entries(r.sites || {}).filter(([, c]) => (c.drives || 0) + (c.charges || 0) > 0).map(([k]) => k);
    const fromSources = (r.sources || []).filter((x) => x.kind === 'import').map((x) => x.site);
    return [...new Set(fromSites.length ? fromSites : fromSources)].filter(Boolean);
  };
  const importLabel = (r) => { const s2 = importSites(r); return s2.length ? `Imported from ${s2.map((x) => SITE_NAME[x] || x).join(' and ')}` : 'Imported'; };
  const counts = (c) => `${n(c.drives ?? 0)} ${c.drives === 1 ? 'drive' : 'drives'} · ${n(c.charges ?? 0)} ${c.charges === 1 ? 'charge' : 'charges'}`;
  const sideWord = (v) => (v === 'live' ? 'live history' : v === 'import:teslafi' ? 'TeslaFi’s import' : v === 'import:teslamate' ? 'TeslaMate’s import' : 'the imported history');
  const REASON = {
    more_drives_and_charges: 'it has more drives and charges in the span',
    more_samples: 'the same drives and charges, and more positions',
    tie: 'a tie, so TeslaMate’s (the older format most owners came from)',
  };
  /** The choices of a span: live or imported, or (two imports) TeslaMate's or TeslaFi's. */
  const OPTS = (r) => (r.kind === 'imports' ? [['import:teslamate', 'TeslaMate'], ['import:teslafi', 'TeslaFi']] : [['live', 'Live'], ['import', 'Imported']]);
  const optOf = (r) => { const v = shownOf(r); return r.kind === 'imports' ? v : v === 'live' ? 'live' : 'import'; };

  async function flip(cid, row, value, ch) {
    const o = S.overlaps.get(cid);
    try {
      // the car's overlap choices carry one version: a stale one is refused (409) with nothing changed
      const ans = await ctx.api.patch(api.apiPath`cars/${cid}/overlaps`, { id: row.id, shown: value, version: row.version });
      if (!ctx.alive()) return;
      if (ans?.items) o.items = ans.items; else row.shown_source = value;
      ctx.flash(`This span now shows ${sideWord(value)}${row.kind === 'imports' ? '' : value === 'live' ? ' (SMO and the Tesla API)' : ''}. Drives, Charges, the Timeline, Statistics and Travel follow it.`);
      paintOverlaps();
    } catch (e) {
      if (!ctx.alive() || api.isAbort(e)) return;
      ch.set(optOf(row));
      if (e.code === 'stale_version' || e.status === 409) {
        // someone switched a span of this car meanwhile: show what is chosen now, and say so
        ctx.flash('Not switched: the spans of this car were changed meanwhile (perhaps in another tab). They are shown as they are now; switch again if you still want to.', { kind: 'error' });
        await loadOverlaps();
        return;
      }
      ctx.flash('Not switched: ' + errorWords(e).text, { kind: 'error' });
    }
  }

  function paintOverlaps() {
    const ids = overlapCars();
    const parts = [];
    if (!ids.length) parts.push(empty({ title: 'No imported history overlaps other history.', text: 'Once an import is done, any span where an imported history and live history (from SMO or the Tesla API) both exist, or where a TeslaMate and a TeslaFi import both have rows, shows here, with a switch for which one to show.' }));
    for (const cid of ids) {
      const o = S.overlaps.get(cid);
      if (!o) { parts.push(h('p', { class: 'note' }, `Reading ${carName(cid)}…`)); continue; }
      const ztz = o.tz;
      const z = zoneShort(ztz);
      parts.push(h('h3', { class: 'im-sub' }, carName(cid), h('span', { class: 'label-note' }, `times in ${z}`)));
      if (o.err) { parts.push(errorView(o.err, { retry: loadOverlaps, eyebrow: 'Overlaps', home: false })); continue; }
      if (!o.items.length) { parts.push(empty({ title: 'No overlap for this car.', text: 'Its imported history covers time nothing else does, so every span has one source.' })); continue; }
      for (const r of o.items) {
        const a = r.kind === 'imports' ? siteCounts(r, 'teslamate') : liveOf(r);
        const b = r.kind === 'imports' ? siteCounts(r, 'teslafi') : importedOf(r);
        Object.assign(r, { a_drives: a.drives, a_charges: a.charges, b_drives: b.drives, b_charges: b.charges });
      }
      const t = dataTable(ctx, {
        rows: o.items, itemName: ['span', 'spans'], label: `Overlaps of ${carName(cid)}`, cls: 'im-ov-table',
        csv: { filename: `overlaps-${cid}`, columns: [column('Kind', 'kind'), timeColumn('Start', 'start_ms', ztz), timeColumn('End', 'end_ms', ztz), column('First side drives', 'a_drives', 'count'), column('First side charges', 'a_charges', 'count'), column('Second side drives', 'b_drives', 'count'), column('Second side charges', 'b_charges', 'count'), { label: 'Live from', value: (r) => (r.kind === 'imports' ? '' : liveLabel(r)) }, { label: 'Shown', value: (r) => shownOf(r) }, { label: 'Shown at first', value: (r) => r.default_source || '' }] },
        columns: [
          {
            key: 'span', label: 'Span', th: true,
            cell: (r) => h('span', null,
              h('span', { class: 'cell-main' }, `${formatDate(r.start_ms, ztz, { weekday: false, year: true })} to ${formatDate(r.end_ms, ztz, { weekday: false, year: true })}`),
              h('span', { class: 'cell-sub' }, r.kind === 'imports' ? 'Two imports: TeslaMate and TeslaFi' : 'Live and imported', ' · ', formatDuration(r.end_ms - r.start_ms, { style: 'long' }))),
            value: (r) => r.start_ms,
          },
          {
            key: 'a', label: 'One side', num: true,
            cell: (r) => h('span', null, h('span', { class: 'cell-main' }, counts(r.kind === 'imports' ? siteCounts(r, 'teslamate') : liveOf(r))), h('span', { class: 'cell-sub' }, r.kind === 'imports' ? 'Imported from TeslaMate' : liveLabel(r))),
            value: (r) => r.a_drives,
          },
          {
            key: 'b', label: 'Other side', num: true,
            cell: (r) => h('span', null, h('span', { class: 'cell-main' }, counts(r.kind === 'imports' ? siteCounts(r, 'teslafi') : importedOf(r))), h('span', { class: 'cell-sub' }, r.kind === 'imports' ? 'Imported from TeslaFi' : importLabel(r))),
            value: (r) => r.b_drives,
          },
          {
            key: 'shown', label: 'Shown', cls: 'im-ov-shown', cell: (r) => {
              const ch = choice({ name: `Shown for ${r.id}`, value: optOf(r), options: OPTS(r), onchange: (v) => flip(cid, r, v, ch) });
              const def = r.default_source;
              const note = def && shownOf(r) !== def ? `Chosen by an admin; at first ${sideWord(def)}.`
                : r.kind === 'imports' ? `Shown at first: ${REASON[r.default_reason] || 'the bigger import'}.` : 'Shown at first: live history wins.';
              return h('span', { class: 'im-ov-choice' }, ch, h('span', { class: 'cell-sub' }, note));
            }, value: (r) => shownOf(r),
          },
          { key: 'see', label: 'See it', cell: (r) => h('a', { class: 'link-arrow', href: state.buildUrl('timeline', { car: cid, period: { from: r.start_ms, to: r.end_ms } }) }, 'Timeline', icon('arrowRight')), value: () => null },
        ],
      });
      parts.push(t.el);
      t.load();
    }
    replace(overlapBox, parts);
  }

  const overlapWords = h('div', { class: 'im-overlap-words' },
    h('p', { cover: 'web.import.docs-overlap' }, 'Imported history is kept whole, also where it covers the same time as live history (SMO or the Tesla API). For each such span the live history is shown at first; the imported rows are hidden, never deleted. Switch a span to “Imported” and Drives, Charges, the Timeline, Statistics and Travel show that side for it instead.'),
    h('p', null, 'Where a TeslaMate import and a TeslaFi import both cover the same days, only one of them is shown there, so no drive shows twice: at first the one with more drives and charges in the span (then more positions, else TeslaMate’s). Switch the span to show the other.'),
    h('p', { cover: 'web.import.docs-backfill' }, 'TeslaMate’s backup carries its own addresses and places, so imported drives and charges keep their street names. Spots it had no address for are filled in by the server’s address lookup after the import, a few at a time, so a big import can show some plain coordinates for a while.'),
    h('p', { cover: 'faq.no-backfill' }, 'Time the CAN Bridge missed is not lost the way a cloud logger’s is: the bridge keeps its logs and sends them late, so most gaps fill by themselves. Time nothing recorded shows as Unknown, never as asleep. An import fills only the time before the CAN Bridge, or spans another logger recorded; Tesla keeps no history that could be fetched.'));

  /* ---------------------------------------------------------------- earlier imports */
  const historyBox = h('div', { class: 'im-history' });
  function paintHistory() {
    if (!S.jobs.length) { replace(historyBox, empty({ title: 'No import yet.', text: 'Uploaded files and their reports are listed here, newest first, each with its status and a button to discard it.', cover: ['web.import.files.col-status', 'web.import.discard'] })); return; }
    const t = dataTable(ctx, {
      rows: S.jobs, itemName: ['import', 'imports'], label: 'Imports',
      csv: { filename: 'imports', columns: [column('File', 'name'), column('Status', 'status'), column('Size', 'bytes_total', 'bytes'), timeColumn('Began', 'started_ms', tz), timeColumn('Finished', 'finished_ms', tz)] },
      rowAttrs: (j) => ({ class: j.id === S.job?.id ? 'is-current' : null }),
      columns: [
        { key: 'name', label: 'File', th: true, cell: (j) => h('span', null, h('span', { class: 'cell-main' }, j.name), h('span', { class: 'cell-sub' }, `${bytes(j.bytes_total)}${j.compression && j.compression !== 'plain' ? ` · ${j.compression}` : ''}`)), value: (j) => j.name },
        { key: 'status', label: 'Status', cover: 'web.import.files.col-status', cell: (j) => h('span', { class: ['im-status', `is-${STATUS[j.status]?.kind}`] }, h('span', { class: 'im-status-dot', 'aria-hidden': 'true' }), statusWord(j.status)), value: (j) => j.status },
        { key: 'started', label: `Began (${zone})`, sort: true, cell: (j) => (j.started_ms ? formatDateTime(j.started_ms, tz, { now: api.now() }) : '—'), value: (j) => j.started_ms },
        { key: 'finished', label: `Finished (${zone})`, sort: true, cell: (j) => (j.finished_ms ? formatDateTime(j.finished_ms, tz, { now: api.now() }) : UNFINISHED.includes(j.status) ? 'Not yet' : '—'), value: (j) => j.finished_ms },
        {
          key: 'open', label: 'Report', cell: (j) => (j.id === S.job?.id ? h('span', { class: 'im-quiet' }, 'Shown on this page')
            : button('Show', { kind: 'quiet', cls: 'button-small', onclick: () => { S.job = j; S.match = null; ctx.setQuery({ import: j.id }); if (['checking', 'ready'].includes(j.status)) loadMatch(); paintAll(); follow(); } })),
          value: () => null,
        },
        {
          key: 'discard', label: 'Discard', cell: (j) => button(j.status === 'done' ? 'Remove' : 'Discard', { kind: 'quiet', cls: 'button-small', onclick: () => discard(j), cover: 'web.import.discard', title: j.status === 'done' ? 'Remove from the list; the history it brought in stays' : 'Discard this import' }),
          value: () => null,
        },
      ],
    });
    replace(historyBox, t.el);
    t.load();
  }

  function paintAll() {
    paintJob();
    paintReport();
    paintHistory();
  }

  /* ---------------------------------------------------------------- layout */
  const howTo = h('div', { class: 'im-aside-note' },
    h('p', { class: 'label' }, 'Which file'),
    h('p', null, 'The backup TeslaMate’s own documentation tells you to make: a dump of its whole database, written by pg_dump. Upload it as it is, or compressed with gzip or xz; the server reads it in two passes and keeps no copy once the import is done.'),
    h('p', null, 'One file can hold any number of cars. Each is matched by the first 11 characters of its VIN; the rest of the VIN is never stored.'));

  ctx.main.append(head,
    section({ title: 'Upload and import', note: 'Admins only · over HTTPS', cls: 'im-sec' },
      h('div', { class: 'im-two' }, jobBox, h('aside', { class: 'im-side' }, howTo, restartNote))),
    section({ title: 'Report', note: 'Counts and reasons only: no row values or positions', cls: 'im-sec' }, reportBox),
    section({ title: 'Overlapping history', note: 'live and imported, and two imports', cls: 'im-sec', id: 'im-overlaps' }, h('div', { class: 'im-two im-two-even' }, overlapWords, overlapBox)),
    section({ title: 'Imports', note: 'Newest first', cls: 'im-sec' }, historyBox));

  if (S.job && ['checking', 'ready'].includes(S.job.status)) await loadMatch();
  paintAll();
  await loadOverlaps();
  follow();
  ctx.ready();
  return {};
}
