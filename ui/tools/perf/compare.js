// node compare.js before.json after.json [after2.json ...] -> markdown
const files = process.argv.slice(2).map((f) => require(require('path').resolve(f)));
const key = (r) => `${r.pack}|${r.page}|${r.phase}`;
const maps = files.map((f) => new Map(f.runs.map((r) => [key(r), r])));
const labels = files.map((f) => f.label);
const kib = (b) => (b / 1024).toFixed(0);
const s = (ms) => (ms / 1000).toFixed(1);
const out = [];
out.push(`| pack | page | phase | TTU s (${labels.join(' → ')}) | FCP s | KiB | requests (static+api) | long tasks ms |`);
out.push('|---|---|---|---|---|---|---|---|');
const agg = labels.map(() => ({ cold: { ttu: 0, fcp: 0, b: 0, n: 0 }, warm: { ttu: 0, fcp: 0, b: 0, n: 0 } }));
for (const r0 of files[0].runs) {
  const rs = maps.map((m) => m.get(key(r0)));
  if (rs.some((r) => !r)) continue;
  rs.forEach((r, i) => { const a = agg[i][r.phase]; a.ttu += r.ttu_ms; a.fcp += r.fcp_ms; a.b += r.bytes; a.n++; });
  out.push(`| ${r0.pack} | ${r0.page} | ${r0.phase} | ${rs.map((r) => s(r.ttu_ms)).join(' → ')} | ${rs.map((r) => s(r.fcp_ms)).join(' → ')} | ${rs.map((r) => kib(r.bytes)).join(' → ')} | ${rs.map((r) => `${r.requests_static}+${r.requests_api}`).join(' → ')} | ${rs.map((r) => r.long_tasks_ms).join(' → ')} |`);
}
out.push('');
out.push(`| mean over 6 packs × 5 pages | ${labels.join(' | ')} |`);
out.push(`|---|${labels.map(() => '---').join('|')}|`);
for (const ph of ['cold', 'warm']) {
  for (const [name, f, fmt] of [['TTU s', 'ttu', s], ['FCP s', 'fcp', s], ['KiB', 'b', kib]]) {
    out.push(`| ${ph} ${name} | ${agg.map((a) => fmt(a[ph][f] / a[ph].n)).join(' | ')} |`);
  }
}
console.log(out.join('\n'));
