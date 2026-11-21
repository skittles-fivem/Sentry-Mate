// Sentry Mate, "Logbook" UI pack: Import (/settings/import).
//
// Bringing in the history TeslaMate recorded, from TeslaMate's own backup file, or the CAN Bridge logs
// Sentry Six Manager already has, from its CAN export bundle (a .zip: no cars to match, since each log
// names its car key; its report counts logs imported, already here and refused). An admin chooses the
// file; it goes up in chunks over HTTPS, each with its SHA-256, so an upload that stops goes on from the
// last chunk the server has. The server reads the whole file once (the check) before it stores anything,
// then the cars in the file are matched to cars here (same VIN prefix, a new car, or left out), and the
// import runs. The report counts rows per table and per car, with the reason for every skipped count
// (counts only: never a row's values or positions). Where imported and CAN history cover the same time,
// each span is listed with the side that is shown and a switch to show the other.
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
import { choice, field } from '../core/form.js';
import { dataTable } from '../core/table.js';
import { confirmDialog } from '../core/dialog.js';
import { zoneShort } from '../core/format.js';

export const id = 'import';
export const css = true;
export const title = () => 'Import';

const UNFINISHED = ['uploading', 'checking', 'ready', 'importing'];
const MOVING = ['checking', 'importing'];
const CHUNK_DEFAULT = 1024 * 1024 - 1024;
const POLL_MS = 2500;

/* ------------------------------------------------------------------ words */

const STATUS = {
  uploading: { word: 'Uploading', kind: 'live', text: 'The file is going up to the server in chunks. Keep this page open until the upload is done.' },
  checking: { word: 'Checking', kind: 'live', text: 'Pass 1 of 2: the server reads the whole file and checks every row. Nothing is stored yet.' },
  ready: { word: 'Checked, waiting for you', kind: 'wait', text: 'The whole file was read and checked. Match its cars below, then start the import.' },
  importing: { word: 'Importing', kind: 'live', text: 'Pass 2 of 2: the checked rows are being stored.' },
  done: { word: 'Done', kind: 'ok', text: 'The history is in. It shows on every page, marked as imported.' },
  failed: { word: 'Failed', kind: 'bad', text: 'The import stopped. Rows of a failed check are never stored.' },
};
const statusWord = (s) => STATUS[s]?.word || s;
const CAN = 'sentry_manager_can';
const isCan = (j) => j?.kind === CAN || j?.report?.kind === CAN;
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
  stopped: 'The import was stopped. Logs stored before it stopped stay; start it again to bring the rest.',
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
    deck: 'Bring in the drives, charges, updates and places TeslaMate recorded, from TeslaMate’s own backup file, or the CAN Bridge logs Sentry Six Manager already has, from its CAN export bundle (.zip). The whole file is checked before anything is stored, and running the same file again stores nothing twice.',
  });

  if (!ctx.me || ctx.me.role !== 'admin') {
    ctx.main.append(head, section({ title: 'Import' },
      empty({ title: 'Only an admin can import history.', text: 'An import adds drives and charges to cars, so it needs an admin account. Ask an admin to upload the TeslaMate backup; the imported history then shows on every page, marked as imported.' })));
    ctx.ready();
    return {};
  }

  const S = { jobs: [], cars: [], carsMore: false, job: null, match: null, matchErr: null, overlaps: new Map(), currency: '' };
  const up = { file: null, active: false, paused: false, error: null, t0: 0, b0: 0, ctl: null };

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
    class: 'im-file-input', type: 'file', accept: '.sql,.gz,.xz,.zip,.dump,.bck,.bak,.txt,application/sql,application/gzip,application/x-xz,application/zip',
    'aria-label': 'TeslaMate backup or Sentry Six Manager CAN export',
    onchange: () => { const f = fileInput.files?.[0]; fileInput.value = ''; if (f) chose(f); },
  });
  const drop = h('label', { class: 'im-drop' }, fileInput,
    h('span', { class: 'im-drop-main' }, 'Choose the backup file, or drop it here'),
    h('span', { class: 'im-drop-sub' }, 'The .sql file of a TeslaMate database backup, as it is or compressed (.gz or .xz), or the .zip of Sentry Six Manager’s CAN export bundle, up to 64 GB. It goes over this HTTPS connection to your own server and nowhere else.'));
  const onDrag = (e) => { e.preventDefault(); drop.classList.add('is-over'); };
  const offDrag = () => drop.classList.remove('is-over');
  const onDrop = (e) => { e.preventDefault(); offDrag(); const f = e.dataTransfer?.files?.[0]; if (f) chose(f); };
  drop.addEventListener('dragover', onDrag);
  drop.addEventListener('dragleave', offDrag);
  drop.addEventListener('drop', onDrop);

  const upErr = h('p', { class: 'form-error im-err', role: 'alert', hidden: true });
  function showUpErr(text, action = null) { replace(upErr, text ? [text, action ? [' ', action] : null] : null); upErr.hidden = !text; }

  async function chose(file) {
    showUpErr(null);
    const j = running();
    if (j && j.status === 'uploading' && !up.active) {
      if (file.size !== j.bytes_total || file.name !== j.name) {
        showUpErr(`That is not the file this upload began with (${j.name}, ${bytes(j.bytes_total)}). Choose that file to go on, or discard this upload to start with another file.`);
        return;
      }
      up.file = file;
      S.job = j;
      upload();
      return;
    }
    if (j) { showUpErr('An import is not finished yet. Finish it, or discard it, before you upload another file.'); return; }
    try {
      const created = await ctx.api.post('imports', { name: file.name, bytes_total: file.size });
      if (!ctx.alive()) return;
      putJob(created);
      S.job = S.jobs.find((x) => x.id === created.id);
      S.match = null;
      up.file = file;
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

  async function upload() {
    if (up.active || !up.file || !S.job) return;
    if (!globalThis.crypto?.subtle) { showUpErr('This browser cannot work out the chunks’ SHA-256 here. Open Sentry Mate over HTTPS and try again.'); return; }
    const file = up.file;
    let job = S.job;
    up.active = true; up.paused = false; up.error = null;
    up.t0 = performance.now(); up.b0 = job.uploaded_bytes || 0;
    up.ctl = new AbortController();
    const signal = up.ctl.signal;
    ctx.onCleanup(() => up.ctl?.abort());
    paintAll();
    const max = job.chunk_max_bytes || CHUNK_DEFAULT;
    let off = job.uploaded_bytes || 0, tries = 0;
    while (off < file.size && ctx.alive() && !up.paused) {
      try {
        const buf = await file.slice(off, Math.min(file.size, off + max)).arrayBuffer();
        const sha = hex(await crypto.subtle.digest('SHA-256', buf));
        const ans = await ctx.api.patch(api.apiPath`imports/${job.id}`, buf, {
          signal,
          headers: { 'Content-Type': 'application/offset+octet-stream', 'Upload-Offset': String(off), 'Upload-Length': String(file.size), 'Upload-SHA256': sha },
        });
        if (!ctx.alive()) return;
        off = ans?.offset ?? off + buf.byteLength;
        putJob({ ...ans, uploaded_bytes: off });
        job = S.job;
        tries = 0;
        paintProgress();
      } catch (e) {
        if (!ctx.alive() || api.isAbort(e) || up.paused) break;
        if (e.code === 'wrong_offset' || e.code === 'not_uploading') {
          // the server knows best where the upload is
          try { const j = await ctx.api.get(api.apiPath`imports/${job.id}`); putJob(j); job = S.job; off = j.uploaded_bytes; if (j.status !== 'uploading') break; continue; } catch (e2) { if (api.isAbort(e2)) break; }
        }
        tries++;
        const again = e.code === 'checksum_mismatch' || e.status === 0 || e.status >= 500 || e.status === 429;
        if (again && tries <= 5) { await wait(Math.min(16000, 1000 * 2 ** (tries - 1)), signal); continue; }
        up.error = e.code === 'checksum_mismatch' ? 'A chunk kept arriving damaged. Check the connection, then go on: the upload resumes where it stopped.'
          : e.status === 0 ? 'The server stopped answering. The upload resumes where it stopped when you go on.'
            : e.code === 'no_such_import' ? 'This upload was discarded, perhaps from another browser tab.'
              : (e.fields ? Object.values(e.fields).join(' ') : errorWords(e).text);
        break;
      }
    }
    up.active = false;
    up.ctl = null;
    if (!ctx.alive()) return;
    if (off < file.size || S.job?.status === 'uploading') {
      // a pause or a break can leave a chunk the client gave up on but the server took: read where the
      // server really is, so the bytes and the status shown are its own
      paintAll();
      await syncJob(job.id);
      if (!ctx.alive() || up.active) return;
    }
    if (S.job?.id === job.id && S.job.status !== 'uploading') {
      if (up.file) { up.file = null; up.error = null; flashNext('The whole file is up. The server is checking it now; you can leave this page.'); }
      if (S.job.status === 'ready' && !S.match) loadMatch();
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
        if (S.job?.id === jid) { up.file = null; S.match = null; pickJob(); }
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
    const otherTabUpload = j.status === 'uploading' && !up.active && !up.file;
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
      const acts = [];
      if (up.active) acts.push(button('Pause the upload', { onclick: pauseUpload, kind: 'quiet' }));
      else if (up.file) acts.push(button('Go on with the upload', { onclick: upload, kind: 'primary' }));
      else {
        acts.push(h('label', { class: 'button button-primary im-resume' }, 'Choose the same file to go on',
          h('input', { class: 'im-file-input', type: 'file', 'aria-label': `Choose ${j.name} again to go on`, onchange: (e) => { const f = e.target.files?.[0]; e.target.value = ''; if (f) chose(f); } })));
      }
      lines.push(h('div', { class: 'im-acts' }, acts));
      if (!up.active && !up.file) lines.push(h('p', { class: 'note' }, `This upload stopped at ${bytes(done)}. Choose ${j.name} (${bytes(j.bytes_total)}) again and it goes on from there: the chunks already on the server are not sent twice.`));
      if (up.error) lines.push(h('p', { class: 'form-error', role: 'alert' }, up.error));
    } else if (MOVING.includes(j.status)) {
      lines.push(bar(j.bytes_read, j.bytes_total, j.status === 'checking' ? 'Checked' : 'Imported'));
      lines.push(h('p', { class: 'im-bytes' }, h('strong', null, bytes(j.bytes_read)), ` of ${bytes(j.bytes_total)} read in this pass (${format(pct(j.bytes_read, j.bytes_total), 'percent', { digits: 0 })})`));
    } else {
      lines.push(h('dl', { class: 'im-facts' },
        h('dt', null, 'File'), h('dd', null, `${j.name} · ${bytes(j.bytes_total)}${j.compression && j.compression !== 'plain' ? ` · ${j.compression}` : ''}`),
        j.kind ? [h('dt', null, 'Kind'), h('dd', null, isCan(j) ? 'Sentry Six Manager CAN export' : 'TeslaMate backup')] : null,
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

  function matchOptions(row) {
    const taken = new Set((S.match || []).filter((x) => x.tm_car !== row.tm_car && isCar(x.match)).map((x) => x.match));
    const opts = [];
    if (row.match == null) opts.push(['', 'Choose…']);
    const sug = row.suggested && carById.get(row.suggested);
    if (sug) opts.push([sug.id, `${sug.name} (same VIN prefix)`]);
    for (const c of S.cars) if (c.id !== row.suggested) opts.push([c.id, `${c.name}${taken.has(c.id) ? ' (already matched)' : ''}`]);
    if (isCar(row.match) && !carById.has(row.match)) opts.push([row.match, row.match]);
    opts.push(['new', 'Add as a new car'], ['leave_out', 'Leave out']);
    return opts;
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
      rows, itemName: ['car in the file', 'cars in the file'], label: 'Cars in the backup and their match here',
      caption: unmatched ? `${n(unmatched)} still to match.` : 'Every car has its match.',
      csv: { filename: 'import-cars', columns: [column('Car in the backup', 'name'), column('VIN prefix', 'vin_prefix'), column('Drives', 'drives', 'count'), column('Charges', 'charges', 'count'), timeColumn('First', 'first_ms', tz), timeColumn('Last', 'last_ms', tz), column('Match', 'match')] },
      columns: [
        { key: 'name', label: 'Car in the backup', th: true, cell: (r) => h('span', null, h('span', { class: 'cell-main' }, r.name || `TeslaMate car ${r.tm_car}`), h('span', { class: 'cell-sub' }, `VIN prefix ${r.vin_prefix || 'unknown'} · TeslaMate car ${r.tm_car}`)), value: (r) => r.name },
        { key: 'drives', label: 'Drives', num: true, cell: (r) => n(r.drives), value: (r) => r.drives },
        { key: 'charges', label: 'Charges', num: true, cell: (r) => n(r.charges), value: (r) => r.charges },
        { key: 'span', label: `Recorded (${zone})`, cell: (r) => (r.first_ms ? `${formatDate(r.first_ms, tz, { weekday: false, year: true })} to ${formatDate(r.last_ms, tz, { weekday: false, year: true })}` : '—'), value: (r) => r.first_ms },
        {
          key: 'match', label: 'Becomes', cls: 'im-match-cell',
          cell: (r) => {
            const sel = h('select', { class: 'field-select im-select', 'aria-label': `Match for ${r.name || r.tm_car}`, disabled: j.status !== 'checking' && j.status !== 'ready' },
              matchOptions(r).map(([v, t]) => h('option', { value: v, selected: String(v) === String(r.match ?? '') }, t)));
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
      const j = await ctx.api.post(api.apiPath`imports/${S.job.id}/start`, cur ? { currency: cur } : {});
      if (!ctx.alive()) return;
      putJob(j);
      ctx.flash('The import has started. You can leave this page; it carries on.', { kind: 'info' });
      paintAll();
      follow();
    } catch (e) {
      if (!ctx.alive() || api.isAbort(e)) return;
      btn.disabled = false;
      replace(startErr, startWords(e));
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
    up.paused = true; up.ctl?.abort(); up.file = null; up.error = null;
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
      parts.push(h('div', { class: 'im-start' }, isCan(j) ? null : cur, h('div', { class: 'im-start-row' }, btn, h('p', { class: 'im-quiet' }, why)), startErr));
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
    const tables = Object.entries(rep.tables || {}).map(([name, t]) => ({
      name, read: t.read, imported: imported ? t.imported : null, would: t.read - sum(t.skipped), skipped: sum(t.skipped), blank: sum(t.bad_values), t,
    }));
    const tot = (k) => tables.reduce((a, r) => a + (r[k] || 0), 0);
    const skippedRows = [];
    for (const r of tables) {
      for (const [code, cnt] of Object.entries(r.t.skipped || {})) if (cnt) skippedRows.push({ table: r.name, kind: 'skipped', code, count: cnt, words: reasonWords(code) });
      for (const [col, cnt] of Object.entries(r.t.bad_values || {})) if (cnt) skippedRows.push({ table: r.name, kind: 'blank', code: col, count: cnt, words: blankWords(col) });
    }
    const skippedTotal = tot('skipped');
    const fileName = `import-${j.id}`;

    const figs = figures([
      figure('Rows read', n(tot('read')), 'Every row of every table this import uses, read in the check.'),
      figure(imported ? 'Rows imported' : 'Rows to import', n(imported ? tot('imported') : tot('would')), imported ? 'Stored and shown, marked as imported.' : 'What the import will store once it is started.'),
      figure('Rows skipped', n(skippedTotal), skippedTotal ? 'Each has its reason below. The import went on past them.' : 'No row had to be skipped.', { cover: 'web.import.rejected-count', cls: skippedTotal ? 'im-fig-warn' : null }),
      figure('Values left blank', n(tot('blank')), 'Rows kept with one value left out, each listed below.'),
      rep.span ? figure('History in the file', `${formatDate(rep.span.from_ms, tz, { weekday: false, year: true })} to ${formatDate(rep.span.to_ms, tz, { weekday: false, year: true })}`, `${formatDuration(rep.span.to_ms - rep.span.from_ms, { style: 'long' })} of TeslaMate history (${zone}).`) : null,
    ].filter(Boolean), { cls: 'im-figs' });

    const tTable = dataTable(ctx, {
      rows: tables, itemName: ['table', 'tables'], label: 'Rows per table',
      csv: { filename: `${fileName}-tables`, columns: [column('Table', 'name'), column('Read', 'read', 'count'), column(imported ? 'Imported' : 'To import', imported ? 'imported' : 'would', 'count'), column('Skipped', 'skipped', 'count'), column('Values left blank', 'blank', 'count')] },
      columns: [
        { key: 'name', label: 'Table', th: true, cell: (r) => h('span', null, h('span', { class: 'cell-main' }, tableName(r.name)), h('span', { class: 'cell-sub' }, tableNote(r.name))), value: (r) => r.name },
        { key: 'read', label: 'Read', num: true, sort: true, cell: (r) => n(r.read), value: (r) => r.read },
        { key: 'imported', label: imported ? 'Imported' : 'To import', num: true, sort: true, cell: (r) => n(imported ? r.imported : r.would), value: (r) => (imported ? r.imported : r.would) },
        { key: 'skipped', label: 'Skipped', num: true, sort: true, cell: (r) => (r.skipped ? h('span', { class: 'is-warn' }, n(r.skipped)) : n(0)), value: (r) => r.skipped },
        { key: 'blank', label: 'Values left blank', num: true, sort: true, cell: (r) => n(r.blank), value: (r) => r.blank },
      ],
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

    const cars = (rep.cars || []).map((c) => ({ ...c, here: isCar(c.match) ? carName(c.match) : c.match === 'new' ? 'A new car' : c.match === 'leave_out' ? 'Left out' : 'Not matched' }));
    const cTable = cars.length ? dataTable(ctx, {
      rows: cars, itemName: ['car', 'cars'], label: 'Per car',
      csv: { filename: `${fileName}-cars`, columns: [column('TeslaMate car', 'tm_car'), column('Name', 'name'), column('Car here', 'here'), column('Drives', 'drives', 'count'), column('Charges', 'charges', 'count')] },
      columns: [
        { key: 'tm', label: 'TeslaMate car', th: true, cell: (c) => (c.name ? h('span', null, h('span', { class: 'cell-main' }, c.name), h('span', { class: 'cell-sub' }, `TeslaMate car ${c.tm_car}`)) : `Car ${c.tm_car}`), value: (c) => c.name || c.tm_car },
        { key: 'here', label: 'Car here', cell: (c) => (isCar(c.match) ? h('a', { href: state.buildUrl('timeline', { car: c.match, period: rep.span ? { from: rep.span.from_ms, to: rep.span.to_ms } : null }) }, c.here) : c.here), value: (c) => c.here },
        { key: 'drives', label: imported ? 'Drives imported' : 'Drives', num: true, cell: (c) => (c.match === 'leave_out' ? '—' : n(c.drives)), value: (c) => c.drives },
        { key: 'charges', label: imported ? 'Charges imported' : 'Charges', num: true, cell: (c) => (c.match === 'leave_out' ? '—' : n(c.charges)), value: (c) => c.charges },
      ],
    }) : null;

    replace(reportBox,
      h('p', { class: 'im-lead' }, h('span', { class: 'im-report-of' }, j.name), ' · ', statusWord(j.status),
        j.finished_ms ? ` ${formatDateTime(j.finished_ms, tz, { now: api.now() })} ${zone}` : j.checked_ms ? ` since ${formatDateTime(j.checked_ms, tz, { now: api.now() })} ${zone}` : null),
      figs,
      skippedTotal ? h('div', { class: 'im-skip-note' },
        h('p', null, `${n(skippedTotal)} ${skippedTotal === 1 ? 'row was' : 'rows were'} skipped. That did not stop the import; every other row went in.`),
        h('p', null, 'A drive or charge built around a skipped row can come out with less detail (a missing track point leaves a straight line on the map). The report keeps only counts and reasons, never a row’s values or positions, so it can stay after the import.')) : null,
      h('div', { class: 'im-report-grid' },
        h('div', null, h('h3', { class: 'im-sub' }, 'Per table'), tTable.el),
        h('div', null,
          h('h3', { class: 'im-sub' }, 'Why rows were skipped'),
          sTable ? sTable.el : empty({ title: 'Nothing was skipped.', text: 'Every row passed its checks.', cover: 'web.import.rejected.col-reason' }),
          cTable ? [h('h3', { class: 'im-sub' }, 'Per car'), cTable.el] : null)));
    tTable.load();
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

  /* ---------------------------------------------------------------- overlap with live history */
  const overlapBox = h('div', { class: 'im-overlaps' });
  function overlapCars() {
    const ids = new Set();
    for (const j of S.jobs) for (const c of j.report?.cars || []) if (isCar(c.match) && j.status === 'done') ids.add(c.match);
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

  // the side shown: the new shown_source (live | import:teslamate), else the old shown word
  // which live inputs the span's live side has (its sources less the import)
  const liveLabel = (r) => { const l = (r.sources || []).filter((x) => x.kind !== 'import'); return l.length ? labelOf(l) : 'Data from SMO'; };
  const shownOf = (r) => r.shown_source || (r.shown === 'teslamate' ? 'import:teslamate' : 'live');
  const liveOf = (r) => r.live || r.can || {};
  const importedOf = (r) => r.imported || r.teslamate || {};
  async function flip(cid, row, value, ch) {
    try {
      const ans = await ctx.api.patch(api.apiPath`cars/${cid}/overlaps`, { id: row.id, shown: value });
      if (!ctx.alive()) return;
      const o = S.overlaps.get(cid);
      if (ans?.items) o.items = ans.items; else row.shown_source = value;
      ctx.flash(value === 'live' ? 'This span now shows the live history (SMO and the Tesla API).' : 'This span now shows the imported TeslaMate history.');
      paintOverlaps();
    } catch (e) {
      if (!ctx.alive() || api.isAbort(e)) return;
      ch.set(shownOf(row));
      ctx.flash('Not switched: ' + errorWords(e).text, { kind: 'error' });
    }
  }

  function paintOverlaps() {
    const ids = overlapCars();
    const parts = [];
    if (!ids.length) parts.push(empty({ title: 'No imported history overlaps live history.', text: 'Once an import is done, any span where both an imported history and live history (from SMO or the Tesla API) exist shows here, with a switch for which one to show.' }));
    for (const cid of ids) {
      const o = S.overlaps.get(cid);
      if (!o) { parts.push(h('p', { class: 'note' }, `Reading ${carName(cid)}…`)); continue; }
      const ztz = o.tz;
      const z = zoneShort(ztz);
      parts.push(h('h3', { class: 'im-sub' }, carName(cid), h('span', { class: 'label-note' }, `times in ${z}`)));
      if (o.err) { parts.push(errorView(o.err, { retry: loadOverlaps, eyebrow: 'Overlaps' })); continue; }
      if (!o.items.length) { parts.push(empty({ title: 'No overlap for this car.', text: 'Its imported history ends before its live history begins, so every span has one source.' })); continue; }
      const t = dataTable(ctx, {
        rows: o.items, itemName: ['span', 'spans'], label: `Overlaps of ${carName(cid)}`,
        csv: { filename: `overlaps-${cid}`, columns: [timeColumn('Start', 'start_ms', ztz), timeColumn('End', 'end_ms', ztz), column('Live drives', 'can_drives', 'count'), column('Live charges', 'can_charges', 'count'), column('Imported drives', 'tm_drives', 'count'), column('Imported charges', 'tm_charges', 'count'), { label: 'Live from', value: (r) => liveLabel(r) }, { label: 'Shown', value: (r) => shownOf(r) }] },
        columns: [
          { key: 'span', label: 'Span', th: true, cell: (r) => h('span', null, h('span', { class: 'cell-main' }, `${formatDate(r.start_ms, ztz, { weekday: false, year: true })} to ${formatDate(r.end_ms, ztz, { weekday: false, year: true })}`), h('span', { class: 'cell-sub' }, formatDuration(r.end_ms - r.start_ms, { style: 'long' }))), value: (r) => r.start_ms },
          { key: 'can', label: 'Live', num: true, cell: (r) => h('span', null, h('span', { class: 'cell-main' }, `${n(liveOf(r).drives ?? 0)} drives · ${n(liveOf(r).charges ?? 0)} charges`), h('span', { class: 'cell-sub' }, liveLabel(r))), value: (r) => liveOf(r).drives },
          { key: 'tm', label: 'Imported', num: true, cell: (r) => h('span', null, h('span', { class: 'cell-main' }, `${n(importedOf(r).drives ?? 0)} drives · ${n(importedOf(r).charges ?? 0)} charges`), h('span', { class: 'cell-sub' }, 'Imported from TeslaMate')), value: (r) => importedOf(r).drives },
          {
            key: 'shown', label: 'Shown', cell: (r) => {
              const ch = choice({ name: `Shown for ${r.id}`, value: shownOf(r), options: [['live', 'Live'], ['import:teslamate', 'Imported']], onchange: (v) => flip(cid, r, v, ch) });
              return ch;
            }, value: (r) => shownOf(r),
          },
          { key: 'see', label: 'See it', cell: (r) => h('a', { class: 'link-arrow', href: state.buildUrl('timeline', { car: cid, period: { from: r.start_ms, to: r.end_ms } }) }, 'Timeline', icon('arrowRight')), value: () => null },
        ],
      });
      for (const r of o.items) Object.assign(r, { can_drives: liveOf(r).drives, can_charges: liveOf(r).charges, tm_drives: importedOf(r).drives, tm_charges: importedOf(r).charges });
      parts.push(t.el);
      t.load();
    }
    replace(overlapBox, parts);
  }

  const overlapWords = h('div', { class: 'im-overlap-words' },
    h('p', { cover: 'web.import.docs-overlap' }, 'Imported history is kept whole, also where it covers the same time as live history (SMO or the Tesla API). For each such span the live history is shown at first; the imported rows are hidden, never deleted. Switch a span to “Imported” and Drives, Charges, the Timeline, Statistics and Travel show that side for it instead.'),
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
    section({ title: 'Overlap with live history', cls: 'im-sec' }, h('div', { class: 'im-two im-two-even' }, overlapWords, overlapBox)),
    section({ title: 'Imports', note: 'Newest first', cls: 'im-sec' }, historyBox));

  if (S.job && ['checking', 'ready'].includes(S.job.status)) await loadMatch();
  paintAll();
  await loadOverlaps();
  follow();
  ctx.ready();
  return {};
}
