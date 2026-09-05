// geo-bench — point-in-polygon throughput in a JavaScript runtime.
//
// `docs/research/geometry-sota.md` surveyed the geometry stack and found one hole with **no
// published figure in any language**: point-in-polygon at scale in a browser. No JS-vs-WASM
// throughput benchmark, no GPU version, no rasterization version. The only calibration that exists
// anywhere is DuckDB's native, multicore SPATIAL_JOIN at ~2.02 M points/s.
//
// `bench/roadmap/p10-geo-join.md` filled the native Rust half. This is the JavaScript half, and it
// is the number the survey says nobody has.
//
// Method, stated so the number can be argued with:
//   - Real data both sides: maphy's own POIs and polygons (see bench/fixture/README.md).
//   - The JS baseline is an equivalent crossing-number scan WITH a bounding-box reject, not a
//     strawman, because beating a bad baseline proves nothing.
//   - Every arm must return identical answers for every point. Asserted, not sampled.
//   - Timing is the median of several runs after a warmup, because a single run on a JIT measures
//     compilation as much as computation.
//
//   node js/geo-bench.mjs

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { GeoIndex, scanLocate, NONE } from './geo.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, '..');

const WASM = join(root, 'target/wasm32-unknown-unknown/release/index_geo_wasm.wasm');
const POI = join(root, 'bench/fixture/maphy-poi.csv');
const POLY = process.env.INDEX_BENCH_PROVINCE
  ? join(root, process.env.INDEX_BENCH_PROVINCE)
  : join(root, 'bench/fixture/maphy-municipal.txt');

function loadPoi(path) {
  const text = readFileSync(path, 'utf8');
  const line = text.split('\n');
  const xy = [];
  for (let i = 1; i < line.length; i++) {
    const c = line[i].split(',');
    if (c.length < 2) continue;
    const x = Number(c[0]), y = Number(c[1]);
    if (Number.isFinite(x) && Number.isFinite(y)) xy.push(x, y);
  }
  return Float64Array.from(xy);
}

function loadPolygon(path) {
  const text = readFileSync(path, 'utf8');
  const polygon = [];
  for (const line of text.split('\n')) {
    if (line.startsWith('P\t')) { polygon.push([]); continue; }
    if (!line.startsWith('R\t')) continue;
    const part = line.split('\t');
    // Column 1 is the ring index within its source polygon: 0 = outer, >0 = hole. A MultiPolygon
    // contributes several ring-0 lines, one per island.
    const outer = part[1] === '0';
    const num = part[2].split(' ');
    const pt = new Float64Array(num.length);
    for (let i = 0; i < num.length; i++) pt[i] = Number(num[i]);
    if (pt.length < 8) continue;
    if (polygon.length) polygon[polygon.length - 1].push({ pt, outer });
  }
  return polygon.filter((p) => p.length > 0);
}

function bboxOf(polygon) {
  return polygon.map((p) => {
    let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
    for (const r of p) {
      const ring = r.pt ?? r;
      for (let i = 0; i < ring.length; i += 2) {
        if (ring[i] < x0) x0 = ring[i];
        if (ring[i] > x1) x1 = ring[i];
        if (ring[i + 1] < y0) y0 = ring[i + 1];
        if (ring[i + 1] > y1) y1 = ring[i + 1];
      }
    }
    return [x0, y0, x1, y1];
  });
}

const median = (a) => { const s = [...a].sort((x, y) => x - y); return s[s.length >> 1]; };

function time(fn, runs = 5) {
  fn(); // warmup: the first run measures JIT compilation as much as computation
  const t = [];
  for (let i = 0; i < runs; i++) {
    const t0 = performance.now();
    fn();
    t.push(performance.now() - t0);
  }
  return median(t);
}

const wasm = readFileSync(WASM);
const xy = loadPoi(POI);
const polygon = loadPolygon(POLY);
const bbox = bboxOf(polygon);
const n = xy.length >>> 1;
const vertex = polygon.reduce((a, p) => a + p.reduce((b, r) => b + (r.pt ?? r).length / 2, 0), 0);

console.log('geo-bench :: point-in-polygon throughput in a JavaScript runtime');
console.log(`  runtime: node ${process.version} (${process.platform}/${process.arch})`);
console.log(`  wasm:    ${wasm.length.toLocaleString()} bytes`);
console.log(`  data:    ${polygon.length.toLocaleString()} polygons, ${vertex.toLocaleString()} vertices`
  + `  x  ${n.toLocaleString()} real POIs\n`);

// --- baseline: pure JS scan with a bbox reject.
let truth;
const scanMs = time(() => {
  truth = new Uint32Array(n);
  for (let i = 0; i < n; i++) truth[i] = scanLocate(polygon, bbox, xy[2 * i], xy[2 * i + 1]);
});

const matched = truth.reduce((a, v) => a + (v !== NONE ? 1 : 0), 0);
console.log(`  ${matched.toLocaleString()} of ${n.toLocaleString()} POIs fall inside a polygon`
  + ` (${((100 * matched) / n).toFixed(1)}%)\n`);

const row = [];
row.push(['JS scan (bbox + crossing)', null, scanMs, 1]);

for (const order of [6, 7, 8, 9]) {
  const t0 = performance.now();
  const idx = await GeoIndex.build(wasm, polygon, { order });
  const buildMs = performance.now() - t0;

  let out;
  const qMs = time(() => { out = idx.locateMany(xy); });

  // The whole argument for the index is that it is exact. If it ever disagrees, the timing is
  // worthless -- so this throws rather than printing a match percentage.
  let bad = 0, first = -1;
  for (let i = 0; i < n; i++) {
    if (out[i] !== truth[i]) { bad++; if (first < 0) first = i; }
  }
  if (bad) {
    throw new Error(`WASM index (order ${order}) disagrees with the scan on ${bad} of ${n}`
      + ` points; first at ${first} (${xy[2 * first]}, ${xy[2 * first + 1]}):`
      + ` got ${out[first]} want ${truth[first]}`);
  }

  row.push([`WASM cell index, L=${order}`, buildMs, qMs, scanMs / qMs, idx.cellCount, idx.interiorCount]);
  idx.close();
}

console.log('  --- arms ---');
console.log(`  ${'arm'.padEnd(28)}${'build'.padStart(10)}${'query'.padStart(12)}`
  + `${'points/s'.padStart(14)}${'vs JS scan'.padStart(12)}`);
for (const [name, buildMs, qMs, ratio, cells, interior] of row) {
  const rate = (n / (qMs / 1000));
  let line = `  ${name.padEnd(28)}${(buildMs === null ? '-' : buildMs.toFixed(0) + 'ms').padStart(10)}`
    + `${(qMs.toFixed(2) + 'ms').padStart(12)}`
    + `${(rate >= 1e6 ? (rate / 1e6).toFixed(2) + ' M' : (rate / 1e3).toFixed(0) + ' k').padStart(14)}`
    + `${(ratio.toFixed(1) + 'x').padStart(12)}`;
  if (cells) line += `   cells=${cells.toLocaleString()} (interior ${interior.toLocaleString()})`;
  console.log(line);
}

console.log('\n  Read: every arm returns identical answers for all'
  + ` ${n.toLocaleString()} points -- asserted, not sampled.`);
console.log('  Reference: DuckDB SPATIAL_JOIN ~2.02 M points/s, NATIVE and MULTICORE, 310 polygons');
console.log('  (duckdb.org/2025/08/08/spatial-joins.html). This is single-threaded in a JS runtime.');
