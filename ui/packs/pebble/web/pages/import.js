// Sentry Mate, "Pebble" UI pack: Import (/settings/import).
//
// Two kinds of file: TeslaMate's history from its own backup file, or the CAN Bridge logs Sentry Six Manager
// already has (its CAN export bundle, a .zip: each log as the CAN Bridge signed it, checked like an upload and
// stored as the car's own SMO data, never doubled). Numbered step cards: pick the file (it goes up in chunks over
// HTTPS, each with its SHA-256, so a stopped upload goes on from the last chunk the server has), the check (the
// whole file is read before anything is stored), match the cars and start, the import, done. Only the current
// card carries a button and its output under it. Below: the report as number tiles (rows read, imported,
// skipped and why, values left blank, per table and per car), the spans where imported and CAN history overlap
// with a switch for the side shown, a tile of what to know, and the earlier imports.
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
import { sectionHead, count, nums } from '../core/bits.js';
import { srcPill } from '../core/source.js';
import { group, infoRow, pickRow, said } from '../core/settings.js';
import { progressBar, callout, bytes } from './move-parts.js';

export const id = 'import';
export const css = true;
export const shape = [['full', 'h1'], [2, 'h3'], [2, 'h3'], [1, 'h1'], [1, 'h1'], [1, 'h1'], [1, 'h1']];
export const title = () => 'Import';

const UNFINISHED = ['uploading', 'checking', 'ready', 'importing'];
const MOVING = ['checking', 'importing'];
const CHUNK_DEFAULT = 1024 * 1024 - 1024;
const POLL_MS = 2500;

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
  not_a_can_export: 'This zip is not a Sentry Six Manager CAN export. In Sentry Six Manager, download the CAN export bundle (Nerd panel, Logs + ids.csv, the car’s key, from before its first log to now) and upload that .zip as it is. A TeslaMate backup goes up as the .sql file itself (or .sql.gz, .sql.xz), not zipped.',
  unreadable: 'The file cannot be read as a zip: it is damaged, cut short, encrypted or packed in a way the server does not read. Download it again and upload it as it is.',
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

  const S = { jobs: [], cars: [], carsMore: false, job: null, match: null, matchErr: null, overlaps: new Map(), currency: '' };
  const up = { file: null, active: false, paused: false, error: null, t0: 0, b0: 0, ctl: null };

  const [jobs, carsPage] = await Promise.all([
    ctx.api.list('imports', { limit: 100 }).all({ max: 500 }),
    ctx.api.page('cars', { limit: 500 }),
  ]);
  if (!ctx.alive()) return {};
  S.jobs = jobs;
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
      class: 'im-file', type: 'file', accept: '.sql,.gz,.xz,.zip,.dump,.bck,.bak,.txt,application/sql,application/gzip,application/x-xz,application/zip',
      'aria-label': text, onchange: () => { const f = input.files && input.files[0]; input.value = ''; if (f) chose(f); },
    });
    const zone = h('label', { class: 'im-drop' }, input,
      h('span', { class: 'im-drop-ic', 'aria-hidden': 'true' }, icon('upload')),
      h('span', { class: 'im-drop-main' }, text),
      sub ? h('span', { class: 'im-drop-sub' }, sub) : null);
    zone.addEventListener('dragover', (e) => { e.preventDefault(); zone.classList.add('is-over'); });
    zone.addEventListener('dragleave', () => zone.classList.remove('is-over'));
    zone.addEventListener('drop', (e) => { e.preventDefault(); zone.classList.remove('is-over'); const f = e.dataTransfer && e.dataTransfer.files[0]; if (f) chose(f); });
    return zone;
  }

  async function chose(file) {
    showUpErr(null);
    const j = running();
    if (j && j.status === 'uploading' && !up.active) {
      if (file.size !== j.bytes_total || file.name !== j.name) {
        showUpErr(`That is not the file this upload began with (${j.name}, ${bytes(j.bytes_total)}). Pick that file to go on, or discard this upload first.`);
        return;
      }
      up.file = file; S.job = j;
      upload();
      return;
    }
    if (j) { showUpErr('An import is not finished yet. Finish it or discard it before you upload another file.'); return; }
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
      showUpErr(e.code === 'import_running' ? 'Another import is not finished yet, perhaps from another tab. Reload this page to see it.' : fieldsText(e));
    }
  }

  async function upload() {
    if (up.active || !up.file || !S.job) return;
    if (!(globalThis.crypto && crypto.subtle)) { showUpErr('This browser cannot work out the SHA-256 of each chunk here. Open Sentry Mate over HTTPS and try again.'); return; }
    const file = up.file;
    let job = S.job;
    Object.assign(up, { active: true, paused: false, error: null, t0: performance.now(), b0: job.uploaded_bytes || 0, ctl: new AbortController() });
    const signal = up.ctl.signal;
    ctx.onCleanup(() => up.ctl && up.ctl.abort());
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
        off = (ans && ans.offset) != null ? ans.offset : off + buf.byteLength;
        putJob({ ...ans, uploaded_bytes: off });
        job = S.job;
        tries = 0;
        paintSteps();
      } catch (e) {
        if (!ctx.alive() || api.isAbort(e) || up.paused) break;
        if (e.code === 'wrong_offset' || e.code === 'not_uploading') {
          try { const j = await ctx.api.get(api.apiPath`imports/${job.id}`); putJob(j); job = S.job; off = j.uploaded_bytes; if (j.status !== 'uploading') break; continue; } catch (e2) { if (api.isAbort(e2)) break; }
        }
        tries++;
        const again = e.code === 'checksum_mismatch' || e.status === 0 || e.status >= 500 || e.status === 429;
        if (again && tries <= 5) { await wait(Math.min(16000, 1000 * 2 ** (tries - 1)), signal); continue; }
        up.error = e.code === 'checksum_mismatch' ? 'A chunk kept arriving damaged. Check the connection, then go on: the upload resumes where it stopped.'
          : e.status === 0 ? 'The server stopped answering. Go on when it is back: the upload resumes where it stopped.'
            : e.code === 'no_such_import' ? 'This upload was discarded, perhaps from another tab.' : fieldsText(e);
        break;
      }
    }
    up.active = false;
    up.ctl = null;
    if (!ctx.alive()) return;
    if (off < file.size || (S.job && S.job.status === 'uploading')) {
      paintAll();
      await syncJob(job.id);
      if (!ctx.alive() || up.active) return;
    }
    if (S.job && S.job.id === job.id && S.job.status !== 'uploading') {
      if (up.file) { up.file = null; up.error = null; ctx.toast('The whole file is up. The server checks it now; you can leave this page.', { kind: 'info' }); }
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
        if (S.job && S.job.id === jid) { up.file = null; S.match = null; pickJob(); }
        ctx.toast('This upload was discarded, perhaps from another tab.', { kind: 'info' });
      }
    }
  }
  const pauseUpload = () => { up.paused = true; if (up.ctl) up.ctl.abort(); paintAll(); };

  /* ---------------------------------------------------------------- following a job */
  let pollTimer = null;
  ctx.onCleanup(() => clearTimeout(pollTimer));
  function follow() {
    clearTimeout(pollTimer);
    const j = S.job;
    if (!j) return;
    const elsewhere = j.status === 'uploading' && !up.active && !up.file;
    if (!MOVING.includes(j.status) && !elsewhere && j.status !== 'ready') return;
    pollTimer = setTimeout(async () => {
      if (!ctx.alive() || !S.job || S.job.id !== j.id) return;
      try {
        const was = S.job.status, toMatch = S.job.cars_to_match;
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
        } else paintSteps();
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
    if (sug) opts.push([sug.id, sug.name, 'Same VIN prefix']);
    for (const c of S.cars) if (c.id !== row.suggested) opts.push([c.id, c.name, taken.has(c.id) ? 'Already matched to another car of the file' : null]);
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
      const sub = [`VIN prefix ${r.vin_prefix || 'unknown'}`, `${count(r.drives)} drives`, `${count(r.charges)} charges`,
        r.first_ms ? `${day(r.first_ms)} to ${day(r.last_ms)}` : null].filter(Boolean).join(' · ');
      const row = pickRow({
        name: r.name || `TeslaMate car ${r.tm_car}`, sub, value: r.match == null ? '' : r.match, options: matchOptions(r),
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
      const cur = isCan(S.job) ? '' : String(S.currency || '').trim().toUpperCase();
      putJob(await ctx.api.post(api.apiPath`imports/${S.job.id}/start`, cur ? { currency: cur } : {}));
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
    if (S.job && S.job.id === j.id) { up.paused = true; if (up.ctl) up.ctl.abort(); up.file = null; up.error = null; }
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
  const STEPS = [
    ['pick', 'Pick the file', 'The .sql file of a TeslaMate database backup, plain or compressed (.gz, .xz), or the .zip of Sentry Six Manager’s CAN export. It goes over HTTPS to this server only.'],
    ['check', 'Check', 'The server reads the whole file and checks every row. Nothing is stored yet.'],
    ['match', 'Match the cars, then start', 'Each car of the file goes to a car here, a new car, or is left out. Then Start stores the checked rows.', 'web.import.start'],
    ['import', 'Import', 'The checked rows are stored, a few minutes per gigabyte. You can leave this page.'],
    ['done', 'Done', 'The history shows on every page, marked as imported.'],
  ];
  const stepsBox = h('ol', { class: 'mv-steps im-steps', 'aria-label': 'Steps of an import' });
  const stepsTile = tile({ w: 2, title: 'This import', icon: 'upload', id: 'im-steps', cls: 'im-steps-tile' }, stepsBox, startErr, failRegion);

  function atOf(j) {
    if (!j) return { at: 0, st: 'now' };
    if (j.status === 'failed') return { at: j.checked_ms ? 3 : j.uploaded_bytes >= j.bytes_total ? 1 : 0, st: 'stuck' };
    return { at: { uploading: 0, checking: 1, ready: 2, importing: 3, done: 4 }[j.status] ?? 0, st: j.status === 'done' ? 'done' : 'now' };
  }

  function stepOutput(j, key) {
    if (!j) return [filePicker('Pick the file, or drop it here', 'A TeslaMate backup (.sql, .sql.gz, .sql.xz) or Sentry Six Manager’s CAN export (.zip). Up to 64 GB. Choosing it starts the upload.'), upErr];
    if (j.status === 'failed') return null;
    if (key === 'pick') {
      const done = j.uploaded_bytes || 0;
      let speed = null, eta = null;
      if (up.active) {
        const s = (performance.now() - up.t0) / 1000;
        if (s > 2 && done > up.b0) { speed = (done - up.b0) / s; eta = (j.bytes_total - done) / speed; }
      }
      const acts = up.active ? btn('Pause the upload', { kind: 'soft', icon: 'minus', onclick: pauseUpload })
        : up.file ? btn('Go on with the upload', { kind: 'primary', icon: 'upload', onclick: upload })
          : filePicker(`Pick ${j.name} again to go on`, `It goes on from ${bytes(done)}: chunks already on the server are not sent again.`);
      return [h('p', { class: 'im-file-name' }, icon('file'), h('span', null, j.name)),
        progressBar({ bytes_done: done, bytes_total: j.bytes_total, bytes_per_s: speed, eta_s: eta }, { label: 'Uploaded' }),
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

  function paintSteps() {
    const j = shownJob();
    const { at, st } = atOf(j);
    stepsBox.replaceChildren(...(isCan(j) ? CAN_STEPS : STEPS).map(([key, name, text, cov], i) => {
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
        j.error && j.error.message && FAILURES[code] ? h('p', { class: 'set-mono' }, `${code}: ${j.error.message}`) : null,
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
        h('p', { class: 'set-a' }, 'Imported history is kept whole. Where it overlaps live history (SMO or the Tesla API), the live side is shown at first; the imported rows are hidden, never deleted. Switch a span to Imported below and every page shows that side for it.')),
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
    const imported = rep.imported !== false && j.status === 'done';
    const tables = Object.entries(rep.tables || {}).map(([name, t]) => ({ name, read: t.read || 0, imported: imported ? t.imported || 0 : (t.read || 0) - sum(t.skipped), skipped: sum(t.skipped), blank: sum(t.bad_values), t }));
    const tot = (k) => tables.reduce((a, r) => a + (r[k] || 0), 0);
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
        name: tableName(r.name), sub: `${count(r.read)} read${r.skipped ? ` · ${count(r.skipped)} skipped` : ''}${r.blank ? ` · ${count(r.blank)} left blank` : ''}`,
        side: h('span', { class: 'im-n' }, count(r.imported)),
      }))),
      note(imported ? 'The number on the right is the rows imported.' : 'The number on the right is the rows the import will store.'));

    const cars = rep.cars || [];
    const carTile = cars.length ? tile({ w: 2, title: 'Per car', icon: 'car' },
      group(null, cars.map((c) => {
        const here = isCar(c.match) ? carName(c.match) : c.match === 'new' ? 'a new car' : c.match === 'leave_out' ? 'left out' : 'not matched';
        return infoRow({
          icon: 'car', tone: c.match === 'leave_out' ? null : 'coral', name: c.name || `TeslaMate car ${c.tm_car}`,
          sub: [`Becomes ${here}`, c.match === 'leave_out' ? null : `${count(c.drives)} drives · ${count(c.charges)} charges`].filter(Boolean).join(' · '),
          side: isCar(c.match) ? h('a', { class: 'tile-link', href: state.buildUrl('timeline', { car: c.match, period: rep.span ? { from: rep.span.from_ms, to: rep.span.to_ms } : null }) }, 'Timeline', icon('chevronRight')) : null,
        });
      }))) : null;

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
        note(imported ? 'Imported rows show on every page, marked as imported.' : 'Nothing is stored until the import is started.')),
      reasonTile, tableTile, carTile);
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
          icon: 'key', tone: st[1], name: h('code', { class: 'set-mono' }, k.key),
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
      (rep.refused_logs || []).length ? h('div', { class: 'log', role: 'list' }, rep.refused_logs.slice(0, 100).map((x) => h('div', { class: 'log-row', role: 'listitem' }, h('span', { class: 'log-k' }, x.name), h('span', { class: 'log-v' }, canReason(x.reason)[0])))) : null,
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

  /* ---------------------------------------------------------------- overlaps */
  const overlapBox = h('div', { class: 'contents' });
  const overlapCars = () => {
    const ids = new Set();
    for (const j of S.jobs) if (j.status === 'done') for (const c of (j.report && j.report.cars) || []) if (isCar(c.match)) ids.add(c.match);
    return [...ids].slice(0, 50);
  };
  async function loadOverlaps() {
    await Promise.all(overlapCars().map(async (cid) => {
      try {
        const ans = await ctx.api.get(api.apiPath`cars/${cid}/overlaps`, { limit: 200 });
        S.overlaps.set(cid, { tz: ans.time_zone || (carById.get(cid) || {}).time_zone || tz, items: ans.items || [], err: null });
      } catch (e) {
        if (!api.isAbort(e)) S.overlaps.set(cid, { tz, items: [], err: e });
      }
    }));
    if (ctx.alive()) paintOverlaps();
  }
  async function flip(cid, row, value, seg) {
    try {
      const ans = await ctx.api.patch(api.apiPath`cars/${cid}/overlaps`, { id: row.id, shown: value === 'live' ? 'can' : 'teslamate' });
      if (!ctx.alive()) return;
      const o = S.overlaps.get(cid);
      if (ans && ans.items) o.items = ans.items; else { row.shown_source = value === 'live' ? 'live' : 'import:teslamate'; row.shown = value === 'live' ? 'can' : 'teslamate'; }
      ctx.toast(value === 'live' ? 'This span shows the live history (SMO and the Tesla API) now.' : 'This span shows the imported history now.');
      paintOverlaps();
    } catch (e) {
      if (!ctx.alive() || api.isAbort(e)) return;
      seg.set(shownOf(row));
      ctx.toast(`Not switched: ${errorWords(e).text}`, { kind: 'error' });
    }
  }
  // the side shown: live (SMO, the Tesla API) or imported; older servers say can / teslamate
  const shownOf = (r) => (r.shown_source ? (r.shown_source === 'live' ? 'live' : 'imported') : r.shown === 'teslamate' ? 'imported' : 'live');
  function paintOverlaps() {
    const ids = overlapCars();
    const tiles = ids.map((cid) => {
      const o = S.overlaps.get(cid);
      const head = { w: 2, title: `Overlaps of ${carName(cid)}`, icon: 'timeline', side: S.overlaps.get(cid) && S.overlaps.get(cid).items.length ? chip(`${count(S.overlaps.get(cid).items.length)} ${S.overlaps.get(cid).items.length === 1 ? 'span' : 'spans'}`) : null };
      if (!o) return tile(head, said('info', 'Reading…'));
      if (o.err) return tile(head, said('bad', errorWords(o.err).text), btn('Try again', { kind: 'soft', icon: 'reset', onclick: loadOverlaps }));
      if (!o.items.length) return tile(head, said('ok', 'No overlap: its imported history ends before its live history (SMO, the Tesla API) begins.'));
      const fd = (ms) => formatDate(ms, o.tz, { weekday: false, year: true });
      return tile(head,
        h('div', { class: 'im-spans' }, o.items.map((r) => {
          const seg = segmented({ name: `Shown from ${fd(r.start_ms)}`, value: shownOf(r), options: [['live', 'Live'], ['imported', 'Imported']], onchange: (v) => flip(cid, r, v, seg) });
          const live = r.live || r.can || {}, imp = r.imported || r.teslamate || {};
          return h('div', { class: 'im-span-row' },
            h('div', { class: 'row-text' },
              h('a', { class: 'row-name place-link', href: state.buildUrl('timeline', { car: cid, period: { from: r.start_ms, to: r.end_ms } }) }, `${fd(r.start_ms)} to ${fd(r.end_ms)}`),
              h('span', { class: 'row-sub' }, `Live: ${count(live.drives)} drives, ${count(live.charges)} charges · Imported: ${count(imp.drives)} drives, ${count(imp.charges)} charges`),
              r.sources?.length ? h('span', { class: 'src-line' }, srcPill(r)) : null),
            seg);
        })),
        note(`Where imported and live history (SMO, the Tesla API) cover the same time, pick the side shown. Dates in ${o.tz.replace(/_/g, ' ')} time.`));
    });
    overlapBox.replaceChildren(...(tiles.length ? tiles : [emptyTile({ icon: 'timeline', title: 'No overlaps', text: 'Once an import is done, each span where both histories exist shows here with a switch for the side shown.', w: 2 })]));
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

  function paintAll() { paintSteps(); paintReport(); paintList(); }

  if (S.job && ['checking', 'ready'].includes(S.job.status)) await loadMatch();
  await loadOverlaps();
  if (!ctx.alive()) return {};
  paintAll();
  paintOverlaps();
  ctx.view.replaceChildren(bar, stepsTile, knowTile, reportBox, overlapBox, listBox);
  follow();
  ctx.ready();
  return {};
}
