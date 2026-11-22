// Import (/settings/import): bring history in from TeslaMate's own backup file, or bring SMO logs in from
// Sentry Six Manager's CAN export (a .zip of signed CAN Bridge logs, which become the car's own SMO data, not
// imported rows; there are no cars to match and its report counts logs per car key). Horizon shows it as a
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
import { srcChip, sourcesOf } from '../core/sources.js';

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
  one_file_only: 'A TeslaMate backup or a CAN export is imported on its own: discard this import and upload that file alone. Only TeslaFi\'s CSVs can go up several at once.',
  not_a_teslafi_export: 'One of the files is not a TeslaFi export (a TeslaFi CSV, .csv.gz, or a .zip of them). Discard this import and upload the TeslaFi files without it.',
  not_a_can_export: 'The zip is not a CAN export from Sentry Six Manager: it has no manifest naming a car key and no CAN Bridge log. Make the export again in Sentry Six Manager (Logs, with ids.csv) and upload that zip.',
  unreadable: 'The zip cannot be read: it is damaged, encrypted, packed in a way the server does not read, or one entry unpacks to far more than its size. Make the export again.',
  read_failed: 'The server could not read the uploaded file back. Try the upload again.',
  store_failed: 'A log could not be stored (for example the disk is full). Logs stored before stay; free some space and start it again.',
  stopped: 'The import was stopped. Logs stored before stay; start it again to bring in the rest.',
  interrupted: 'The server restarted part way through. Logs stored before stay; start it again and it skips them.',
  internal_error: 'Something went wrong inside the import. Logs stored before stay. Try again; if it keeps failing, keep the file for a bug report.',
};
// Sentry Six Manager's CAN export: why a log was refused, and what a car key's logs go to
const REFUSED = {
  unsigned: ['Not signed', 'The log has no CAN Bridge signature, so it cannot be trusted as the car\'s own data.'],
  failed_check: ['Changed since signed', 'The log is not what was signed (or was signed for another name or car key).'],
  signed_by_another_key: ['Another signing key', 'Signed, but not by the key this car key is paired with. Trust the new key in Car settings first.'],
  no_sidecar: ['No sidecar', 'The log came without its .json sidecar, which holds the signature.'],
  bad_sidecar: ['Sidecar unreadable', 'The sidecar is damaged or not a CAN Bridge sidecar.'],
  different_file_here: ['Another file of that name', 'A different log with the same name is stored here already.'],
  being_uploaded: ['Being uploaded now', 'The CAN Bridge is uploading this log right now; that upload finishes it.'],
  too_large: ['Too large', 'Bigger than the server takes for one log.'],
  car_full: ['Car has too many logs', 'The car has reached the number of logs the server keeps.'],
};
const refusedWords = (c) => REFUSED[c] || [String(c).replace(/[_-]/g, ' '), 'The server\'s own reason code.'];
const KEY_STATUS = {
  car: ['Goes to a car', 'grn'], new_car: ['A new car', 'cyan'], new_key: ['A new car key', 'cyan'], only_car: ['The only car here', 'cyan'],
  waiting: ['Waiting in Car settings', 'org'], working: ['Being decoded', 'cyan'],
};
const isCan = (j) => !!j && (j.kind === 'sentry_manager_can' || j.report?.kind === 'sentry_manager_can' || (!j.kind && (j.compression === 'zip' || /\.zip$/i.test(j.name || ''))));
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
// null is a gap (the file has no such count), never 0: a total adds only the numbers present, and is a gap when none is
const add = (...vs) => { const p = vs.filter((v) => v != null && !Number.isNaN(v)); return p.length ? p.reduce((a, b) => a + b, 0) : null; };
const sum = (o) => (o == null ? null : Object.keys(o).length ? add(...Object.values(o)) : 0);
const less = (a, b) => (a == null ? null : a - (b || 0));
const gap = () => h('span', { class: 'faint' }, DASH);
const cnt = (v) => (v == null ? gap() : num(v));
const ctr = (v) => (v == null ? { text: gap() } : { value: v, kind: 'count' });
const kindName = (j) => (isCan(j) ? 'CAN export' : (j.kind || j.report?.kind) === 'teslafi' ? 'TeslaFi export' : 'TeslaMate backup');
const STEPS = [['choose', 'Choose'], ['uploading', 'Upload'], ['checking', 'Check'], ['ready', 'Match cars'], ['importing', 'Import'], ['done', 'Done']];
// a CAN export has no cars to match: once checked it waits at Import
const CAN_STEPS = STEPS.filter(([k]) => k !== 'ready');
const ALL_ITEMS = ['web.import.start', 'web.import.discard', 'web.import.start-error', 'web.import.run-error', 'web.import.files.col-status', 'web.import.restart-note',
  'web.import.rejected-count', 'web.import.rejected.col-reason', 'web.import.docs-overlap', 'web.import.docs-backfill', 'faq.no-backfill'];

export async function mount(ctx) {
  const tz = ctx.tz;
  const trail = h('nav', { class: 'im-trail', 'aria-label': 'Where this page is', cover: 'web.import.breadcrumb' },
    h('a', { href: '/' }, 'All cars'), icon('chev-r', 'i--sm'), h('a', { href: state.buildUrl('settings-server') }, 'Set-up'), icon('chev-r', 'i--sm'), h('span', { 'aria-current': 'page' }, 'Import'));
  const rec = recLamp({ idle: 'Deck ready' });
  const head = plate({ kicker: trail, title: 'Import history', aim: 'Brings in the history TeslaMate recorded, from TeslaMate\'s own backup file, or SMO logs from Sentry Six Manager\'s CAN export (.zip). The whole file is checked before anything is stored, and running it again stores nothing twice.', tools: rec.el });

  if (!ctx.admin) {
    replace(ctx.view, h('div', { class: 'page im' }, head, hud({ title: 'Import', icon: 'lock', tone: 'orange' },
      emptyState({ icon: 'lock', title: 'Only an admin can import', text: 'Importing adds history to cars, so it needs an admin account. Ask an admin to upload the TeslaMate backup or the CAN export; the history then shows on every page with where it came from.' }),
      h('p', { class: 'note', cover: ALL_ITEMS }, icon('info', 'i--sm'), h('span', null, 'An admin sees the upload, its progress, the report and the overlap with CAN history here.')))));
    ctx.ready();
    return;
  }

  const S = { jobs: [], cars: [], job: null, match: [], overlaps: [], overlapErr: null, report: null };
  const up = { files: new Map(), sent: {}, active: false, paused: false, halt: false, error: null, startedAt: 0, sent0: 0 };
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
    const steps = isCan(j) ? CAN_STEPS : STEPS;
    const k = !j ? 'choose' : j.status === 'failed' ? (j.checked_ms ? 'importing' : 'checking') : steps === CAN_STEPS && j.status === 'ready' ? 'importing' : j.status;
    const at = Math.max(0, steps.findIndex(([x]) => x === k));
    replace(stepsSlot, lampSteps(steps, at, { failed: j?.status === 'failed', done: j?.status === 'done', label: 'Steps of an import' }));
  }
  const fileInput = h('input', { class: 'im-file', type: 'file', multiple: true, accept: '.sql,.gz,.xz,.dump,.bak,.txt,.zip,.csv,text/csv,application/sql,application/gzip,application/x-xz,application/zip', 'aria-label': 'TeslaMate backup, TeslaFi CSVs or CAN export files',
    onchange: () => { const fs = [...(fileInput.files || [])]; fileInput.value = ''; if (fs.length) chose(fs); } });
  const slot = h('label', { class: 'im-slot' }, fileInput,
    h('span', { class: 'im-slot-ic' }, icon('upload', 'i--lg')),
    h('span', { class: 'im-slot-text' }, h('span', { class: 'im-slot-main' }, 'Choose the files, or drop them here'),
      h('span', { class: 'im-slot-sub' }, h('span', { class: 'im-kind' }, h('b', null, 'TeslaMate backup: '), 'the .sql file of its database backup, as it is or compressed (.gz, .xz).'),
        h('span', { class: 'im-kind' }, h('b', null, 'CAN export: '), 'the .zip Sentry Six Manager makes of a car key\'s CAN Bridge logs (Logs and ids.csv).'),
        h('span', { class: 'im-kind' }, h('b', null, 'TeslaFi export: '), 'its CSVs, as they are, as .csv.gz or in a .zip.'),
        h('span', { class: 'im-kind' }, 'Several files are fine: drop all of TeslaFi\'s monthly CSVs at once and they become one import. A TeslaMate backup or a CAN export goes up alone. Up to 64 GB a file; the server tells the kinds apart by the files themselves.'))));
  ctx.on(slot, 'dragover', (e) => { e.preventDefault(); slot.classList.add('is-over'); });
  ctx.on(slot, 'dragleave', () => slot.classList.remove('is-over'));
  ctx.on(slot, 'drop', (e) => { e.preventDefault(); slot.classList.remove('is-over'); const fs = [...(e.dataTransfer?.files || [])]; if (fs.length) chose(fs); });
  const upErr = h('p', { class: 'field-error', hidden: true, role: 'alert' });
  const sayUp = (t) => { upErr.hidden = !t; if (t) replace(upErr, icon('warn', 'i--sm'), h('span', null, t)); };

  // the files of a job: every job view has them; an older one-file job is its file 0
  const filesOf = (j) => (j?.files?.length ? j.files : j ? [{ index: 0, name: j.name, bytes_total: j.bytes_total, uploaded_bytes: j.uploaded_bytes || 0, upload_url: `imports/${enc(j.id)}` }] : []);
  const sentOf = (f) => Math.max(f.uploaded_bytes || 0, up.sent[f.index] || 0);
  const fileKey = (n, b) => `${n}\u0000${b}`;

  async function chose(files) {
    sayUp(null);
    const j = S.job;
    if (j && j.status === 'uploading' && !up.active) {
      // carry on: each chosen file goes on from what the server has of the file with its name and size
      const want = new Map(filesOf(j).filter((f) => (f.uploaded_bytes || 0) < f.bytes_total).map((f) => [fileKey(f.name, f.bytes_total), f]));
      const got = new Map();
      for (const file of files) { const f = want.get(fileKey(file.name, file.size)); if (f) got.set(f.index, file); }
      if (!got.size) { sayUp(`${files.length === 1 ? 'That is not a file' : 'None of those is a file'} this upload still needs (${[...want.values()].map((f) => `${f.name}, ${bytes(f.bytes_total)}`).join('; ')}). Choose ${want.size === 1 ? 'that file' : 'those files'} to carry on, or discard this upload to start another.`); return; }
      const still = [...want.values()].filter((f) => !got.has(f.index));
      if (still.length) sayUp(`Carrying on with ${got.size} of ${want.size} files. Still to choose: ${still.map((f) => f.name).join(', ')}.`);
      up.files = got; upload(j); return;
    }
    if (j && UNFINISHED.includes(j.status)) { sayUp('An import is not finished yet. Wait for it, or discard it, before you upload more files.'); return; }
    try {
      const made = await ctx.api.post('imports', { files: files.map((f) => ({ name: f.name, bytes_total: f.size })) });
      if (!ctx.alive()) return;
      up.files = new Map(filesOf(made).map((f, i) => [f.index ?? i, files[i]]));
      up.sent = {};
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

  // one file's chunks, one after another, from what the server has of it
  async function uploadOne(job, f, file, max) {
    let off = f.uploaded_bytes || 0, tries = 0;
    const where = f.upload_url || `imports/${enc(job.id)}/files/${f.index}`;
    while (off < file.size && ctx.alive() && !up.paused && !up.halt) {
      try {
        const buf = await file.slice(off, Math.min(file.size, off + max)).arrayBuffer();
        const sha = hex(await crypto.subtle.digest('SHA-256', buf));
        const r = await ctx.api.patch(where, buf, { headers: { 'Content-Type': 'application/offset+octet-stream', 'Upload-Offset': String(off), 'Upload-Length': String(file.size), 'Upload-SHA256': sha } });
        if (!ctx.alive()) return off;
        tries = 0;
        off = r.offset ?? (r.files?.find((x) => x.index === f.index)?.uploaded_bytes) ?? off + buf.byteLength;
        up.sent[f.index] = Math.max(up.sent[f.index] || 0, off);
        setJob(r);
        paintUploadBar();
      } catch (e) {
        if (!ctx.alive()) return off;
        if (['wrong_offset', 'checksum_mismatch', 'network', 'timeout'].includes(e?.code) && tries < 3) {
          tries++;
          try {
            const fresh = await ctx.api.get(`imports/${enc(job.id)}`); if (!ctx.alive()) return off;
            setJob(fresh);
            const ff = filesOf(fresh).find((x) => x.index === f.index);
            if (ff) { off = ff.uploaded_bytes || 0; up.sent[f.index] = off; }
            if (fresh.status !== 'uploading') break;
          } catch { /* the chunk again */ }
          continue;
        }
        if (e?.code === 'not_uploading') up.halt = true;
        else if (!up.error) up.error = { e, name: f.name, off };
        break;
      }
    }
    return off;
  }

  async function upload(job) {
    if (up.active || !up.files.size) return;
    Object.assign(up, { active: true, paused: false, halt: false, error: null, startedAt: Date.now(), sent0: job.uploaded_bytes || 0 });
    paintAll();
    const max = job.chunk_max_bytes || S.chunkMax || (1024 * 1024 - 1024);
    S.chunkMax = max;
    const queue = filesOf(job).filter((f) => up.files.has(f.index) && (f.uploaded_bytes || 0) < f.bytes_total);
    // up to 3 files at the same time
    const worker = async () => {
      while (queue.length && ctx.alive() && !up.paused && !up.halt && !up.error) {
        const f = queue.shift();
        const file = up.files.get(f.index);
        const off = await uploadOne(job, f, file, max);
        if (off >= file.size) up.files.delete(f.index);
      }
    };
    await Promise.all([0, 1, 2].map(worker));
    if (!ctx.alive()) return;
    up.active = false;
    if (up.error) sayUp(`The upload of ${up.error.name} stopped at ${bytes(up.error.off)}: ${errorWords(up.error.e).text} Choose the files again to carry on from there.`);
    await refreshJob();
  }

  const upBarSlot = h('div', { class: 'im-up' });
  function paintUploadBar() {
    const j = S.job;
    if (!j) return;
    const files = filesOf(j);
    const done = add(...files.map(sentOf)) ?? 0;
    const secs = (Date.now() - up.startedAt) / 1000;
    const rate = up.active && secs > 1 ? (done - up.sent0) / secs : null;
    replace(upBarSlot, bar(share(done, j.bytes_total), { label: 'Uploaded' }),
      h('p', { class: 'im-bar-text' }, h('b', null, bytes(done)), ` of ${bytes(j.bytes_total)} uploaded (${pctText(done, j.bytes_total)})`, files.length > 1 ? ` · ${num(files.filter((f) => sentOf(f) >= f.bytes_total).length)} of ${num(files.length)} files done` : null, rate ? ` · ${bytes(rate)} a second · about ${dur(((j.bytes_total - done) / rate) * 1000)} to go` : null),
      files.length > 1 ? h('ul', { class: 'im-ups', 'aria-label': 'Upload of each file' }, files.map((f) => {
        const n = sentOf(f), whole = n >= f.bytes_total;
        return h('li', { class: ['im-upf', whole && 'is-done'] },
          code(f.name, { long: true }),
          h('span', { class: 'im-upf-n' }, `${bytes(n)} of ${bytes(f.bytes_total)} · ${pctText(n, f.bytes_total)}`),
          whole ? sig('ok', 'Done') : up.files.has(f.index) ? (up.active ? sig('live', 'Sending') : sig('off', 'Chosen')) : sig('off', 'Not chosen'));
      })) : null);
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
    up.paused = true; up.files = new Map(); up.sent = {};
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
        h('p', { class: 'im-tape-name' }, icon('book', 'i--sm'), code(j.name, { long: true }), h('span', { class: 'faint small' }, filesOf(j).length > 1 ? `${num(filesOf(j).length)} files · ${bytes(j.bytes_total)}` : bytes(j.bytes_total))),
        upBarSlot,
        up.active
          ? h('div', { class: 'row' }, btn('Pause', () => { up.paused = true; }, { sm: true, icon: 'x' }), h('span', { class: 'chan-hint' }, 'Sent over HTTPS in chunks of about 1 MB, each with its SHA-256: a chunk that arrives damaged is sent again.'))
          : h('div', { class: 'stack' }, h('p', { class: 'small' }, up.paused ? 'Paused. ' : 'This upload stopped (the page was closed or the connection broke). ', filesOf(j).length > 1 ? `The server has ${bytes(j.uploaded_bytes)} of the files. Choose the same files again (all at once is fine) and each carries on from where it stopped.` : `The server has the first ${bytes(j.uploaded_bytes)}. Choose the same file again to carry on from there.`),
            up.files.size && up.paused ? btn('Carry on', () => upload(S.job), { kind: 'go', sm: true, icon: 'arrow-r' }) : slot)));
    }
    parts.push(upErr);
    if (j?.status === 'failed') {
      const c = j.error?.code;
      replace(failSlot, callout('warn', j.checked_ms ? 'The import failed' : 'The check found a problem', FAILURES[c] || j.error?.message || 'The server gave no reason.',
        FAILURES[c] && j.error?.message ? ` The server says: ${j.error.message}` : '', isCan(j) && j.checked_ms ? ' Discard it, or upload the zip again: logs already here are skipped.' : ' Nothing from this file was stored. Discard it, then upload a good file.'));
      failSlot.hidden = false;
    } else { replace(failSlot); failSlot.hidden = true; }
    parts.push(failSlot);
    replace(deckSlot, parts);
    const unmatched = S.match.filter((c) => c.match == null).length;
    startBtn.disabled = !(j?.status === 'ready' && !unmatched);
    replace(why, !j ? 'Choose a file first.'
      : j.status === 'ready' ? (isCan(j) ? 'Checked: ready to start. A CAN export has no cars to match.' : unmatched ? `Match ${unmatched === 1 ? 'the last imported car' : `${unmatched} imported cars`} below first.` : 'Checked and matched: ready to start.')
        : j.status === 'uploading' || j.status === 'checking' ? 'Possible once the whole file is uploaded and checked.'
          : j.status === 'importing' ? 'Running now.' : j.status === 'done' ? 'This import is done. Upload a file to import again.' : 'This import failed: discard it and upload a good file.');
    discardBtn.disabled = !j;
    replace(discardBtn, icon('x'), h('span', null, j && !open ? 'Remove from list' : 'Discard'));
  }

  /* ---------- the three passes */
  const passSlot = h('div', { class: 'stack', cover: 'web.import.files.col-status' });
  // kind: 'can' (a CAN export), 'tm' (a TeslaMate backup) or null (not known yet)
  const restartNote = (kind = null) => h('p', { class: 'note', cover: 'web.import.restart-note' }, icon('info', 'i--sm'), h('span', null,
    'Safe to leave: close this page or the browser while it runs. If the server restarts, an upload carries on from the last chunk it has once you choose the file again; a check starts over (nothing was stored); an import runs again from the file, and ',
    kind === 'can' ? 'logs already here are known by their name and SHA-256'
      : kind === 'tm' ? 'rows already in are known by their TeslaMate id'
        : 'what is already in is known (a TeslaMate row by its id, a CAN log by its name and SHA-256)',
    ', so nothing is stored twice.'));
  function paintPasses() {
    const j = S.job;
    if (!j) { replace(passSlot, emptyState({ icon: 'upload', title: 'No import yet', text: 'Choose a TeslaMate backup or a CAN export. Its upload, check and import show here as they run.' }), restartNote()); return; }
    const can = isCan(j);
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
      kv([['File', code(j.name, { long: true })], ['Kind', j.kind ? (can ? 'CAN export from Sentry Six Manager' : kindName(j)) : h('span', { class: 'faint' }, can ? 'A zip: known after the check' : 'Known after the check')], ['Size', `${bytes(j.bytes_total)}${j.compression && j.compression !== 'plain' ? ` (${j.compression})` : ''}`],
        ['Started', dateTime(j.started_ms, tz)], ['Checked', j.checked_ms ? dateTime(j.checked_ms, tz) : DASH],
        ['Finished', j.finished_ms ? `${dateTime(j.finished_ms, tz)} · took ${dur(j.finished_ms - j.started_ms)}` : DASH]]),
      h('ol', { class: 'im-passes', 'aria-label': 'State of each pass' },
        pass(1, 'Upload', 'The file goes to the server in chunks.', p1, p1 === 'done' ? `${bytes(j.bytes_total)} uploaded` : null, p1 === 'now' ? share(j.uploaded_bytes, j.bytes_total) : null),
        pass(2, 'Check', can ? 'Every log is checked against its signature and its car key; nothing is stored.' : 'Every row is read and checked; nothing is stored.', p2, read(p2) || (p2 === 'done' && j.checked_ms ? `Checked ${dateTime(j.checked_ms, tz)}` : null), p2 === 'now' ? share(j.bytes_read, j.bytes_total) : null),
        pass(3, 'Import', can ? 'The signed logs are stored and decoded like live uploads: they become the car\'s own SMO data.' : 'The rows are stored and the timeline is built again for the matched cars.', p3, read(p3) || (p3 === 'done' ? `Done ${dateTime(j.finished_ms, tz)}` : p3 === 'skip' ? 'Not run: the check failed.' : null), p3 === 'now' ? share(j.bytes_read, j.bytes_total) : null)),
      restartNote(can ? 'can' : j.kind || j.report?.kind ? 'tm' : null));
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
    paintMatch(); paintDeck(); paintReport();
  }
  const matchList = tracks(ctx, {
    title: 'Match the imported cars', icon: 'car', tone: 'orange', rows: [], label: 'Cars in the backup and where their history goes', csvName: 'import-cars',
    note: 'Each car in the backup goes to a car here, becomes a new car, or is left out. A car here with the same VIN start (the same for every car of a model and factory year) is chosen at first. One car here takes at most one imported car.',
    columns: [
      { key: 'name', label: 'In the backup', lead: true, sort: true, render: (c) => h('span', { class: 'tagline' }, h('b', null, c.name), h('span', { class: 'mono faint' }, `#${c.tm_car}`)), csv: (c) => c.name },
      { key: 'vin_prefix', label: 'VIN start', sort: true, render: (c) => code(c.vin_prefix) },
      { key: 'drives', label: 'Drives', num: true, sort: true, render: (c) => cnt(c.drives) },
      { key: 'charges', label: 'Charges', num: true, sort: true, render: (c) => cnt(c.charges) },
      { key: 'first_ms', label: 'History', sort: true, render: (c) => `${date(c.first_ms, tz, { year: true })} to ${date(c.last_ms, tz, { year: true })}`, csv: (c) => `${toIsoZoned(c.first_ms, tz)} to ${toIsoZoned(c.last_ms, tz)}` },
      { key: 'match', label: 'Goes to', csv: (c) => (c.match === 'new' ? 'a new car' : c.match === 'leave_out' ? 'left out' : c.match ? carName(c.match) : 'not chosen'), render: (c) => {
        const taken = new Set(S.match.filter((x) => x.tm_car !== c.tm_car && x.match && x.match !== 'new' && x.match !== 'leave_out').map((x) => x.match));
        const sel = h('select', { class: 'input im-select', 'aria-label': `Where ${c.name}'s history goes`, disabled: !['ready', 'checking'].includes(S.job?.status), onchange: () => setMatch(c, sel.value) },
          c.match == null ? h('option', { value: '', selected: true, disabled: true }, 'Choose…') : null,
          S.cars.map((x) => h('option', { value: x.id, selected: c.match === x.id, disabled: taken.has(x.id) }, `${x.name}${x.id === c.suggested ? ' (same VIN start)' : ''}${taken.has(x.id) ? ' (taken)' : ''}`)),
          h('option', { value: 'new', selected: c.match === 'new' }, 'Add as a new car'),
          h('option', { value: 'leave_out', selected: c.match === 'leave_out' }, 'Leave out'));
        return h('div', { class: 'row im-goes' }, sel, c.suggested && c.match === c.suggested ? tag('VIN match', 'grn') : null, c.match == null ? tag('Not chosen', 'org') : null);
      } },
    ],
    empty: () => emptyState({ icon: 'car', title: 'No cars to match yet', text: 'The cars in the file are known once the whole file is uploaded.' }),
  });
  const matchWrap = h('div', null, matchList.el);
  function paintMatch() { const show = S.job && !isCan(S.job) && ['checking', 'ready'].includes(S.job.status); matchWrap.hidden = !show; if (show) matchList.setRows(S.match); }

  /* ---------- the report */
  const reportSlot = h('div', { class: 'stack' });
  const tablesList = tracks(ctx, {
    title: 'Rows per table', icon: 'book', rows: [], csvName: 'import-report-tables', label: 'Rows read, imported and skipped per table',
    note: 'Counts only: the report never keeps row values or positions.', sort: { key: 'order', dir: 'asc' },
    columns: [
      { key: 'order', label: 'Table', lead: true, sort: true, value: (r) => r.order, render: (r) => h('span', { class: 'cell' }, h('b', null, tableName(r.name)), h('span', { class: 'cell-sub' }, TABLES[r.name]?.[1] || '')), csv: (r) => r.name },
      { key: 'read', label: 'Read', num: true, sort: true, render: (r) => cnt(r.read) },
      { key: 'imported', label: 'Imported', num: true, sort: true, value: (r) => (S.report?.imported ? r.imported : r.would), render: (r) => (S.report?.imported ? cnt(r.imported) : r.would == null ? gap() : h('span', { class: 'faint', title: 'From the check: what the import would store' }, `${num(r.would)} to import`)), csv: (r) => (S.report?.imported ? r.imported : r.would) ?? '' },
      { key: 'skipped', label: 'Skipped', num: true, sort: true, render: (r) => (r.skipped == null ? gap() : r.skipped ? h('span', { class: 'warn' }, num(r.skipped)) : h('span', { class: 'faint' }, '0')), csv: (r) => r.skipped ?? '' },
      { key: 'bad', label: 'Values cleared', render: (r) => (r.bad.length ? h('span', { class: 'tagline' }, r.bad.map(([col, n]) => h('span', { class: 'im-chip' }, code(col), ` ${num(n)}`))) : h('span', { class: 'faint' }, 'none')), csv: (r) => r.bad.map(([c, n]) => `${c}:${n}`).join('; ') },
    ],
    empty: () => emptyState({ icon: 'book', title: 'No report yet', text: 'The report shows once the whole file has been checked.' }),
  });
  const reasonsList = tracks(ctx, {
    title: 'Why rows were skipped', icon: 'warn', tone: 'orange', rows: [], csvName: 'import-report-skipped', label: 'Skipped rows by reason', cover: 'web.import.rejected.col-reason', sort: { key: 'n', dir: 'desc' },
    columns: [
      { key: 'table', label: 'Table', sort: true, render: (r) => tableName(r.table), csv: (r) => r.table },
      { key: 'reason', label: 'Reason', lead: true, sort: true, render: (r) => h('span', { class: 'cell' }, h('b', null, reasonWords(r.reason)[0]), h('span', { class: 'cell-sub' }, reasonWords(r.reason)[1])), csv: (r) => reasonWords(r.reason)[0] },
      { key: 'code', label: 'Code', render: (r) => code(r.reason), csv: (r) => r.reason },
      { key: 'n', label: 'Rows', num: true, sort: true, render: (r) => cnt(r.n) },
    ],
    empty: () => emptyState({ icon: 'check', title: 'No row was skipped', text: 'Every row of the file passed its checks.' }),
  });
  const goesTo = (m) => (m === 'new' ? 'a new car' : m === 'leave_out' ? 'left out' : m ? carName(m) : 'not matched yet');
  const carsList = tracks(ctx, {
    title: 'Rows per car', icon: 'car', rows: [], csvName: 'import-report-cars', label: 'Rows per imported car', cover: 'web.import.rejected-count',
    note: 'Each car\'s rows over every table. Places belong to no car, so they count only in Rows per table. A car left out is not listed and its rows are not counted.', sort: { key: 'tm_car', dir: 'asc' },
    columns: [
      { key: 'tm_car', label: 'In the backup', lead: true, sort: true, render: (c) => h('span', { class: 'tagline' }, h('b', null, c.name || `Car ${c.tm_car}`), h('span', { class: 'mono faint' }, `#${c.tm_car}`)), csv: (c) => c.name || c.tm_car },
      { key: 'match', label: 'Went to', sort: true, value: (c) => goesTo(c.match), render: (c) => (c.match && c.match !== 'new' && c.match !== 'leave_out' ? h('a', { class: 'plc', href: state.buildUrl('car', { car: c.match }) }, carName(c.match)) : h('span', { class: c.match ? 'faint' : 'warn' }, goesTo(c.match))), csv: (c) => goesTo(c.match) },
      { key: 'read', label: 'Read', num: true, sort: true, render: (c) => cnt(c.read) },
      { key: 'imported', label: 'Imported', num: true, sort: true, value: (c) => (S.report?.imported ? c.imported : less(c.read, sum(c.skipped))),
        render: (c) => (S.report?.imported ? cnt(c.imported) : c.read == null ? gap() : h('span', { class: 'faint' }, `${num(less(c.read, sum(c.skipped)))} to import`)) },
      { key: 'skipped', label: 'Skipped', num: true, sort: true, value: (c) => sum(c.skipped), render: (c) => { const n = sum(c.skipped); return n == null ? gap() : n ? h('span', { class: 'warn' }, num(n)) : h('span', { class: 'faint' }, '0'); }, csv: (c) => sum(c.skipped) ?? '' },
      { key: 'why', label: 'Why skipped', render: (c) => { const e = Object.entries(c.skipped || {}).filter(([, n]) => n); return e.length ? h('span', { class: 'tagline' }, e.map(([k, n]) => h('span', { class: 'im-chip', title: reasonWords(k)[1] }, `${reasonWords(k)[0]} ${num(n)}`))) : c.skipped ? h('span', { class: 'faint' }, 'none') : gap(); }, csv: (c) => Object.entries(c.skipped || {}).map(([k, n]) => `${reasonWords(k)[0]}: ${n}`).join('; ') },
      { key: 'drives', label: 'Drives', num: true, sort: true, render: (c) => cnt(c.drives) },
      { key: 'charges', label: 'Charges', num: true, sort: true, render: (c) => cnt(c.charges) },
    ],
    empty: () => emptyState({ icon: 'car', title: 'No cars in the report', text: 'The report lists each car of the backup once the whole file has been checked.' }),
  });
  // several files in one import: what each file gave
  const filesList = tracks(ctx, {
    title: 'Files', icon: 'book', rows: [], csvName: 'import-report-files', label: 'Rows read, used and skipped per uploaded file', sort: { key: 'order', dir: 'asc' },
    columns: [
      { key: 'order', label: 'File', lead: true, sort: true, value: (f) => f.order, render: (f) => code(f.name, { long: true }), csv: (f) => f.name },
      { key: 'rows_read', label: 'Rows read', num: true, sort: true, render: (f) => cnt(f.rows_read) },
      { key: 'rows_used', label: 'Used', num: true, sort: true, render: (f) => cnt(f.rows_used) },
      { key: 'rows_skipped', label: 'Skipped', num: true, sort: true, render: (f) => (f.rows_skipped == null ? gap() : f.rows_skipped ? h('span', { class: 'warn' }, num(f.rows_skipped)) : h('span', { class: 'faint' }, '0')) },
    ],
    empty: () => emptyState({ icon: 'book', title: 'No files', text: 'The report lists each uploaded file once the files have been checked.' }),
  });
  const reportLists = h('div', { class: 'stack' }, filesList.el, h('div', { class: 'cols' }, tablesList.el, reasonsList.el), carsList.el);

  // a CAN export's report: logs per car key, and the refused logs with their reasons
  const keysList = tracks(ctx, {
    title: 'Logs per car key', icon: 'antenna', rows: [], csvName: 'import-can-keys', label: 'Logs per CAN Bridge car key', sort: { key: 'key', dir: 'asc' },
    note: 'A car key is what a CAN Bridge signs its logs for. Its logs go to the car that has that key; a key no car has is handled as a new key\'s first upload.',
    columns: [
      { key: 'key', label: 'Car key', lead: true, sort: true, render: (k) => code(k.key, { long: true }), csv: (k) => k.key },
      { key: 'status', label: 'Goes to', sort: true, value: (k) => k.car_name || k.status, render: (k) => h('span', { class: 'tagline' }, k.car ? h('a', { class: 'plc', href: state.buildUrl('car', { car: k.car }) }, k.car_name || carName(k.car)) : null, tag(...(KEY_STATUS[k.status] || [k.status, null]))), csv: (k) => k.car_name || KEY_STATUS[k.status]?.[0] || k.status },
      { key: 'logs', label: 'Logs', num: true, sort: true, render: (k) => num(k.logs) },
      { key: 'imported', label: 'Stored', num: true, sort: true, value: (k) => (S.report?.imported ? k.imported : k.logs - k.refused - k.already_here), render: (k) => (S.report?.imported ? num(k.imported) : h('span', { class: 'faint' }, `${num(k.logs - k.refused - k.already_here)} to store`)) },
      { key: 'already_here', label: 'Here already', num: true, sort: true, render: (k) => (k.already_here ? num(k.already_here) : h('span', { class: 'faint' }, '0')) },
      { key: 'refused', label: 'Refused', num: true, sort: true, render: (k) => (k.refused ? h('span', { class: 'warn' }, num(k.refused)) : h('span', { class: 'faint' }, '0')) },
      { key: 'first_ms', label: 'Logs from', sort: true, render: (k) => (k.first_ms ? `${date(k.first_ms, tz, { year: true })} to ${date(k.last_ms, tz, { year: true })}` : DASH), csv: (k) => (k.first_ms ? `${toIsoZoned(k.first_ms, tz)} to ${toIsoZoned(k.last_ms, tz)}` : '') },
      { key: 'chain', label: 'Hash chain', render: (k) => (k.chain_gaps || k.chain_forks ? h('span', { class: 'tagline' }, k.chain_gaps ? tag(`${num(k.chain_gaps)} ${k.chain_gaps === 1 ? 'gap' : 'gaps'}`, 'org') : null, k.chain_forks ? tag(`${num(k.chain_forks)} ${k.chain_forks === 1 ? 'fork' : 'forks'}`, 'red') : null) : sig('ok', 'Whole')), csv: (k) => `gaps ${k.chain_gaps || 0}; forks ${k.chain_forks || 0}` },
    ],
    empty: () => emptyState({ icon: 'antenna', title: 'No car key in the export', text: 'The zip names no CAN Bridge car key.' }),
  });
  const refusedList = tracks(ctx, {
    title: 'Refused logs', icon: 'warn', tone: 'orange', rows: [], csvName: 'import-can-refused', label: 'Logs that were refused, with why', sort: { key: 'name', dir: 'asc' },
    note: 'A refused log is never stored. The first 100 are named here; the counts above cover all of them.',
    columns: [
      { key: 'name', label: 'Log', lead: true, sort: true, render: (r) => code(r.name, { long: true }), csv: (r) => r.name },
      { key: 'reason', label: 'Why', sort: true, render: (r) => h('span', { class: 'cell' }, h('b', null, refusedWords(r.reason)[0]), h('span', { class: 'cell-sub' }, r.message || refusedWords(r.reason)[1])), csv: (r) => refusedWords(r.reason)[0] },
      { key: 'code', label: 'Code', render: (r) => code(r.reason), csv: (r) => r.reason },
    ],
    empty: () => emptyState({ icon: 'check', title: 'No log was refused', text: 'Every log was signed by its car key\'s CAN Bridge and is unchanged since.' }),
  });
  const canLists = h('div', { class: 'stack' }, keysList.el, refusedList.el);
  function paintCanReport(rep) {
    const L = rep.logs || {};
    const refused = Object.entries(rep.refused || {}).filter(([, n]) => n);
    const F = rep.files || {};
    replace(reportSlot,
      h('p', { class: 'small' }, rep.imported ? 'What the import stored. These logs are the car\'s own SMO data now, judged and decoded like live uploads.' : 'From the check: what the import will store. Nothing is stored yet.'),
      counters([
        { label: 'Logs read', ...ctr(L.read), how: 'signed CAN Bridge logs in the zip' },
        { label: rep.imported ? 'Stored' : 'To store', ...ctr(rep.imported ? L.imported : L.to_import), how: 'signed and unchanged' },
        { label: 'Here already', ...ctr(L.already_here), how: 'same name and SHA-256: not stored twice' },
        { label: 'Refused', ...ctr(L.refused), how: L.refused ? 'each with its reason below' : 'every log passed' },
        { label: 'Size', text: bytes(L.bytes), how: L.sidecars_added ? `${num(L.sidecars_added)} sidecars added to logs here` : 'of the logs' },
        { label: 'Logs from', text: rep.span ? dur(rep.span.to_ms - rep.span.from_ms) : DASH, how: rep.span ? `${date(rep.span.from_ms, tz, { year: true })} to ${date(rep.span.to_ms, tz, { year: true })}` : 'no dated logs' },
      ]),
      refused.length ? h('ul', { class: 'im-refused', 'aria-label': 'Refused logs by reason' }, refused.map(([k, n]) => h('li', { class: 'im-chip', title: refusedWords(k)[1] }, `${refusedWords(k)[0]} ${num(n)}`))) : null,
      kv([
        ['Manifest', rep.manifest ? h('span', { class: 'tagline' }, 'car key ', code(rep.manifest.vehicle_key || DASH), rep.manifest.logs != null ? ` · ${num(rep.manifest.logs)} logs` : null) : h('span', { class: 'faint' }, 'none in the zip')],
        ['Files in the zip', `${num(F.entries)} entries${F.candump_text ? ` · ${num(F.candump_text)} candump text copies (not signed, skipped)` : ''}${F.ids_csv ? ' · ids.csv' : ''}${F.sidecars_without_log ? ` · ${num(F.sidecars_without_log)} sidecars without a log` : ''}${F.other ? ` · ${num(F.other)} other files` : ''}`],
      ]),
      (rep.warnings || []).length ? callout('warn', 'Worth a look', ...(rep.warnings || []).map((w, i) => [i ? ' ' : '', typeof w === 'string' ? w : w.message || w.code])) : null);
    keysList.setRows(rep.cars || []);
    refusedList.setRows(rep.refused_logs || []);
  }
  function paintReport() {
    const j = S.job, rep = j?.report || null;
    S.report = rep;
    if (!rep) {
      replace(reportSlot, emptyState({ icon: 'book', title: j ? 'No report yet' : 'No import yet', text: j ? 'The report shows once the whole file has been checked (pass 1).' : 'Each import gets a report: rows (or logs) read, imported and skipped, and why.' }));
      reportLists.hidden = true; canLists.hidden = true;
      tablesList.setRows([]); reasonsList.setRows([]); carsList.setRows([]); filesList.setRows([]); keysList.setRows([]); refusedList.setRows([]);
      return;
    }
    if (rep.kind === 'sentry_manager_can') { reportLists.hidden = true; canLists.hidden = false; paintCanReport(rep); return; }
    canLists.hidden = true;
    reportLists.hidden = false;
    const names = Object.keys(rep.tables || {});
    const rows = names.map((name, i) => { const t = rep.tables[name] || {}; const sk = sum(t.skipped); return { name, order: i, read: t.read ?? null, imported: t.imported ?? null, would: less(t.read, sk), skipped: sk, bad: Object.entries(t.bad_values || {}) }; });
    const reasons = names.flatMap((name) => Object.entries(rep.tables[name]?.skipped || {}).map(([reason, n]) => ({ table: name, reason, n })));
    // the counts add only what the file has: a table it lacks is a gap, and a total of gaps is a gap
    const tot = { read: add(...rows.map((r) => r.read)), imp: add(...rows.map((r) => (rep.imported ? r.imported : r.would))), sk: add(...rows.map((r) => r.skipped)), bad: add(...rows.flatMap((r) => r.bad.map(([, n]) => n))) ?? (rows.some((r) => r.read != null) ? 0 : null) };
    // a car left out is not in the report (the server leaves it out; this makes sure)
    const outs = new Set(S.match.filter((c) => c.match === 'leave_out').map((c) => c.tm_car));
    const cars = (rep.cars || []).filter((c) => c.match !== 'leave_out' && !outs.has(c.tm_car));
    replace(reportSlot,
      h('p', { class: 'small' }, rep.imported ? 'What the import stored.' : 'From the check: what the import will store. Nothing is stored yet.'),
      counters([
        { label: 'Rows read', ...ctr(tot.read), how: 'every row of every table used' },
        { label: rep.imported ? 'Imported' : 'To import', ...ctr(tot.imp), how: rep.imported ? 'stored, marked as imported' : 'read minus skipped' },
        { label: 'Skipped', ...ctr(tot.sk), how: tot.sk ? 'each with its reason below' : 'every row passed' },
        { label: 'Values cleared', ...ctr(tot.bad), how: 'out of range, left blank; row kept' },
        { label: 'History', text: rep.span ? dur(rep.span.to_ms - rep.span.from_ms) : DASH, how: rep.span ? `${date(rep.span.from_ms, tz, { year: true })} to ${date(rep.span.to_ms, tz, { year: true })}` : 'no dated rows' },
      ]),
      tot.sk ? callout('warn', `${num(tot.sk)} rows ${rep.imported ? 'were' : 'will be'} skipped.`, 'Skipping a row did not stop the import; every other row is in. A drive or charge built around a skipped row can come out a little short. The report keeps only how many rows were skipped and why, never what they held.') : null);
    tablesList.setRows(rows); reasonsList.setRows(reasons); carsList.setRows(cars);
    const uploads = Array.isArray(rep.uploads) && rep.uploads.length ? rep.uploads : null;
    filesList.el.hidden = !uploads;
    filesList.setRows(uploads ? uploads.map((f, i) => ({ ...f, order: i })) : []);
  }

  /* ---------- overlap with CAN history */
  async function loadOverlaps() {
    S.overlapErr = null;
    const lists = await Promise.all(S.cars.map(async (c) => {
      try { return (await ctx.api.list(`cars/${enc(c.id)}/overlaps`, { limit: 200 }).all({ max: 5000 })).map((o) => ({ ...o, car: c.id })); } catch (e) { if (e?.name !== 'AbortError') S.overlapErr = e; return []; }
    }));
    S.overlaps = lists.flat().sort((a, b) => b.start_ms - a.start_ms);
  }
  const shownOf = (o) => o.shown_source || (o.shown === 'teslamate' ? 'import:teslamate' : 'live');
  const liveOf = (o) => o.live || o.can;
  const importedOf = (o) => o.imported || o.teslamate;
  async function flip(o, shown) {
    if (shownOf(o) === shown) return;
    try {
      const r = await ctx.api.patch(`cars/${enc(o.car)}/overlaps`, { id: o.id, shown });
      if (!ctx.alive()) return;
      const ch = r.changed || { ...o, shown };
      S.overlaps = S.overlaps.map((x) => (x.car === o.car && x.id === o.id ? { ...x, ...ch, car: o.car } : x));
      rec.saved('Shown history', `${carName(o.car)}: ${shown === 'live' ? 'live (SMO or Tesla API)' : 'imported TeslaMate'} history is shown for that span on every page.`);
    } catch (e) { if (ctx.alive()) toast(`Could not switch: ${errorWords(e).text}`, { kind: 'error' }); }
    overlapList.setRows(S.overlaps);
  }
  const zoneOf = (o) => S.cars.find((c) => c.id === o.car)?.time_zone || tz;
  const side = (x) => (x ? `${num(x.drives)} drives, ${num(x.charges)} charges` : DASH);
  // the inputs of one side of an overlap: live (SMO, Tesla API) or imported
  const srcLine = (o, which) => { const list = sourcesOf(o).filter((x) => (which === 'import') === (x.kind === 'import')); return list.length ? srcChip({ sources: list }) : null; };
  const overlapList = tracks(ctx, {
    title: 'Spans with both histories', icon: 'timeline', rows: [], csvName: 'import-overlaps', label: 'Spans where imported and live history overlap',
    note: 'Where imported TeslaMate history and live data (from SMO or the Tesla API) cover the same time, one side is shown on Drives, Charges, Timeline, Statistics and Travel: live at first. Neither side is deleted; switch a span to show the other.',
    sort: { key: 'start_ms', dir: 'desc' },
    columns: [
      { key: 'car', label: 'Car', lead: true, sort: true, value: (o) => carName(o.car), render: (o) => h('b', null, carName(o.car)), csv: (o) => carName(o.car) },
      { key: 'start_ms', label: 'From', sort: true, render: (o) => dateTime(o.start_ms, zoneOf(o)), csv: (o) => toIsoZoned(o.start_ms, zoneOf(o)) },
      { key: 'end_ms', label: 'To', sort: true, render: (o) => dateTime(o.end_ms, zoneOf(o)), csv: (o) => toIsoZoned(o.end_ms, zoneOf(o)) },
      { key: 'len', label: 'Length', num: true, sort: true, value: (o) => o.end_ms - o.start_ms, render: (o) => dur(o.end_ms - o.start_ms), csv: (o) => Math.round((o.end_ms - o.start_ms) / 1000) },
      { key: 'live', label: 'Live has', render: (o) => h('span', { class: 'cell' }, side(liveOf(o)), srcLine(o, 'live')), csv: (o) => side(liveOf(o)) },
      { key: 'imported', label: 'Imported has', render: (o) => h('span', { class: 'cell' }, side(importedOf(o)), srcLine(o, 'import')), csv: (o) => side(importedOf(o)) },
      { key: 'shown', label: 'Shown', sort: true, value: shownOf, csv: shownOf, render: (o) => h('div', { class: 'seg', role: 'group', 'aria-label': `Which history is shown for ${carName(o.car)} from ${dateTime(o.start_ms, zoneOf(o))}` },
        [['live', 'Live'], ['import:teslamate', 'Imported']].map(([v, l]) => h('button', { class: 'seg-btn', type: 'button', 'aria-pressed': String(shownOf(o) === v), onclick: () => flip(o, v) }, l))) },
      { key: 'go', label: '', cls: 'nowrap', csv: false, render: (o) => h('a', { class: 'plc', href: state.buildUrl('timeline', { car: o.car, period: { from: o.start_ms, to: o.end_ms } }) }, 'Timeline') },
    ],
    empty: () => (S.overlapErr ? emptyState({ icon: 'warn', title: 'Overlaps could not be read', text: `${errorWords(S.overlapErr).text} Load the page again to try once more.` })
      : emptyState({ icon: 'check', title: 'No overlap', text: S.cars.length ? 'No span has both imported and live history, so every span shows the one history it has. Imported history that ends before live data began never overlaps.' : 'There is no car yet, so nothing can overlap.' })),
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
      { key: 'name', label: 'File', lead: true, sort: true, render: (j) => h('span', { class: 'tagline' }, code(j.name, { long: true }), j.id === S.job?.id ? tag('In the deck', 'cyan') : null), csv: (j) => j.name },
      { key: 'kind', label: 'Kind', sort: true, value: (j) => (isCan(j) ? 1 : 0), render: (j) => (j.kind || j.report?.kind ? kindName(j) : h('span', { class: 'faint' }, 'not checked yet')), csv: (j) => j.kind || '' },
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
    canLists,
    band(3, 'Overlap with live history'),
    hud({ title: 'How the two histories meet', icon: 'timeline' },
      h('ul', { class: 'im-facts' },
        h('li', { cover: 'web.import.docs-overlap' }, h('span', { class: 'im-fact-ic' }, icon('timeline', 'i--sm')), h('div', null, h('b', null, 'Every span is imported, and overlaps are kept. '), 'History is imported for all of its time, also where SMO or the Tesla API was recording too. Those spans are listed below with the side that is shown, and a switch.')),
        h('li', { cover: 'web.import.docs-backfill' }, h('span', { class: 'im-fact-ic' }, icon('pin', 'i--sm')), h('div', null, h('b', null, 'Addresses and heights come with the backup. '), 'Imported drives have them at once. A stop without an address gets one from the offline address lookup in the minutes after the import, and shows its position until then.')),
        h('li', { cover: 'faq.no-backfill' }, h('span', { class: 'im-fact-ic' }, icon('info', 'i--sm')), h('div', null, h('b', null, 'Time nobody recorded cannot be fetched later. '), 'Tesla keeps no history to download. The CAN Bridge keeps its logs and uploads them when it can, so its own gaps fill themselves; time before it, or a spell when nothing recorded, shows as unknown on the ', tlLink, '. Only an import from another logger, such as TeslaMate, fills it.')))),
    overlapList.el,
    jobsList.el));
  paintAll();
  ctx.ready();
}
