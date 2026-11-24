#!/usr/bin/env node
// Bundles and minifies the built-in UI packs for the release build (esbuild, pinned in package-lock.json):
//
//   npm ci --prefix ui/tools/bundle && node ui/tools/bundle/bundle.mjs OUT [--packs instrument]
//
// For each pack in ui/packs/<id>/ it writes OUT/<id>/web/ with only the files that differ from the sources:
//   - the module the pack's index.html starts (core/app.js or core/start.js) and every page (pages/*.js), each at
//     its own path, bundled and minified, the code they share in chunks (core/c-<hash>.js): one level deep like the
//     sources, so the `new URL('../vendor/...', import.meta.url)` of the pack's code still finds its files, and the
//     router's import(`../pages/${id}.js`) still finds each page;
//   - every style sheet of the pack's own (not vendor/) minified, at its own path;
//   - boot.js (the classic script in <head>) minified.
// The server (crates/web/build.rs, with SENTRY_MATE_UI_BUNDLE=OUT) serves these in place of the sources' files;
// everything else (index.html, vendor/, fonts, icons, and the pack's zip download) stays as it is in the sources.
// Each written file and folder gets the newest time of its pack's sources, so a build folder kept between runs
// (.github/build-binary.sh) is not built again when nothing in the packs changed.
// The devserver (ui/devserver/serve.py) and the tests keep serving the sources as they are.
import * as esbuild from 'esbuild';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const PACKS = path.resolve(HERE, '../../packs');

const argv = process.argv.slice(2);
const outArg = argv.find((a) => !a.startsWith('--'));
if (!outArg) {
  console.error('usage: bundle.mjs OUT [--packs a,b]');
  process.exit(2);
}
const out = path.resolve(outArg);
const only = (() => {
  const i = argv.indexOf('--packs');
  return i >= 0 ? new Set(argv[i + 1].split(',')) : null;
})();
// the oldest browsers the packs support (ES2022 modules: top-level await, class fields)
const TARGET = ['chrome109', 'edge109', 'firefox115', 'safari16'];

function walk(dir, base = dir, acc = []) {
  for (const e of fs.readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) walk(p, base, acc);
    else if (e.isFile()) acc.push(path.relative(base, p).split(path.sep).join('/'));
  }
  return acc;
}

// the same files build.rs leaves out
function devOnly(rel) {
  const name = rel.split('/').pop().toLowerCase();
  return name === 'readme.md' || name.startsWith('mock-') || name.startsWith('mock.') || name.includes('.test.')
    || name.includes('.spec.') || rel.split('/').some((s) => s === 'tests' || s === 'test' || s === 'mock' || s.startsWith('.'));
}

function writeIfChanged(file, bytes) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  try {
    if (Buffer.compare(fs.readFileSync(file), bytes) === 0) return;
  } catch { /* new */ }
  fs.writeFileSync(file, bytes);
}

async function bundlePack(id) {
  const web = path.join(PACKS, id, 'web');
  const html = fs.readFileSync(path.join(web, 'index.html'), 'utf8');
  const entry = [...html.matchAll(/<script\b[^>]*>/g)].map((m) => m[0])
    .filter((t) => /type=["']module["']/.test(t)).map((t) => (t.match(/\ssrc=["']\/([^"'?#]+)/) || [])[1]).filter(Boolean);
  if (entry.length !== 1) throw new Error(`${id}: expected one module script in index.html, found ${entry.length}`);
  const files = walk(web).filter((f) => !devOnly(f));
  const pages = files.filter((f) => /^pages\/[^/]+\.js$/.test(f));
  const dest = path.join(out, id, 'web');
  // what this run writes; anything else already in dest (a page or chunk gone since) is removed afterwards
  const wrote = new Set();
  const put = (file, bytes) => { wrote.add(path.relative(dest, file).split(path.sep).join('/')); writeIfChanged(file, bytes); };
  const r = await esbuild.build({
    absWorkingDir: web,
    entryPoints: [...new Set([entry[0], ...pages])],
    outbase: '.',
    outdir: dest,
    bundle: true,
    splitting: true,
    format: 'esm',
    minify: true,
    target: TARGET,
    entryNames: '[dir]/[name]',
    chunkNames: 'core/c-[hash]',
    charset: 'utf8',
    legalComments: 'none',
    logLevel: 'warning',
    // the sources' own (a key given twice: the last one counts, as in the browser)
    logOverride: { 'duplicate-object-key': 'silent' },
    metafile: true,
    // the vendored libraries stay their own files, loaded when needed (as the sources load them, by a relative
    // address that is the same from every output file: all are one folder deep)
    plugins: [{ name: 'vendor-external', setup(b) { b.onResolve({ filter: /\/vendor\// }, (a) => ({ path: a.path, external: true })); } }],
    write: false,
  });
  for (const f of r.outputFiles) put(f.path, Buffer.from(f.contents));
  const css = files.filter((f) => f.endsWith('.css') && !f.startsWith('vendor/'));
  for (const f of css) {
    const src = fs.readFileSync(path.join(web, f), 'utf8');
    const min = await esbuild.transform(src, { loader: 'css', minify: true, target: TARGET, charset: 'utf8', legalComments: 'none' });
    put(path.join(dest, f), Buffer.from(min.code));
  }
  for (const f of files.filter((x) => x === 'boot.js')) {
    const src = fs.readFileSync(path.join(web, f), 'utf8');
    const min = await esbuild.transform(src, { loader: 'js', minify: true, target: TARGET, charset: 'utf8', legalComments: 'none' });
    put(path.join(dest, f), Buffer.from(min.code));
  }
  for (const f of walk(dest)) if (!wrote.has(f)) fs.rmSync(path.join(dest, f));
  // the newest source time on every file and folder written (folders too: a build tool may look at them)
  const newest = Math.max(...files.map((f) => fs.statSync(path.join(web, f)).mtimeMs), fs.statSync(path.join(PACKS, id, 'ui.json')).mtimeMs) / 1000;
  const touch = (p) => fs.utimesSync(p, newest, newest);
  for (const f of walk(dest)) touch(path.join(dest, f));
  const dirs = new Set();
  for (const f of walk(dest)) for (let d = path.dirname(f); d !== '.'; d = path.dirname(d)) dirs.add(d);
  for (const d of [...dirs].sort().reverse()) touch(path.join(dest, d));
  touch(dest);
  touch(path.join(out, id));
  return newest;
  const before = files.filter((f) => f.endsWith('.js') && !f.startsWith('vendor/') && f !== 'boot.js').reduce((s, f) => s + fs.statSync(path.join(web, f)).size, 0);
  const js = r.outputFiles.filter((f) => f.path.endsWith('.js'));
  const after = js.reduce((s, f) => s + f.contents.length, 0);
  console.log(`${id}: ${pages.length} pages and ${entry[0]} -> ${js.length} files (${(after / 1024).toFixed(0)} KiB; the sources' modules ${(before / 1024).toFixed(0)} KiB), ${css.length} style sheets`);
}

const ids = fs.readdirSync(PACKS).filter((d) => fs.existsSync(path.join(PACKS, d, 'ui.json')) && fs.existsSync(path.join(PACKS, d, 'web', 'index.html')))
  .filter((d) => !only || only.has(d)).sort();
fs.mkdirSync(out, { recursive: true });
let newest = 0;
for (const id of ids) newest = Math.max(newest, await bundlePack(id));
fs.utimesSync(out, newest, newest);
