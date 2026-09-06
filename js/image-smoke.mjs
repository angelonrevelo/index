// CI smoke test for the IMAGE tier through the C ABI — the p62 row.
//
// `js/smoke.mjs` proves the text engine works with no server and no Rust build step. This one
// proves the same thing for images, which is where the claim actually bites: `docs/research/image.md`
// §1 finds that every alive FOSS photo system (Immich, LibrePhotos, PhotoPrism) needs a server to
// answer a question about your own files, and §2 finds the browser is exactly where they fail. An
// image tier only Rust could call would forfeit that argument at the one place it matters.
//
// Three things are asserted here that a Rust test cannot assert:
//
//   1. The embedding really does cross as a flat f32 buffer in linear memory, written by the host,
//      never as JSON. A 512-d embedding is 2 KB raw and ~6 KB as JSON that must then be parsed.
//   2. The fused ranking a JavaScript host reads back is the ranking JavaScript computed itself.
//      The expected order below is derived from cosine similarity in JS, not copied from a run.
//   3. A facet filter is NOT leaked by the vector arm. That is the p50 defect: a soft arm knows
//      nothing about facets, so it must be FILTERED, not trusted, and a fan-out design routinely
//      skips the step. The corpus below is arranged so the single nearest neighbour of the probe
//      is a document the filter EXCLUDES — if the filter were leaked, it would rank first.
//
// And it reports the linear-memory HIGH-WATER MARK, because `docs/research/portability.md` §1
// records that an mmap-style format does not fail to port to WASM, it silently reads the whole
// index into linear memory — and a vector column is the largest thing this engine has ever put
// there. Latency alone would hide the constraint that actually binds in a browser.

import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const wasmPath = join(root, 'target/wasm32-unknown-unknown/release/index_wasm.wasm');
const wasm = await readFile(wasmPath);
const { instance } = await WebAssembly.instantiate(wasm, {});
const e = instance.exports;

let failed = 0;
const check = (ok, msg) => {
  console.log(`  ${ok ? 'PASS' : 'FAIL'}  ${msg}`);
  if (!ok) failed++;
};

// WASM has no unsigned return type: every u32 arrives in JS as a signed i32, so u32::MAX shows up
// as -1. Coerce at every boundary or the error sentinel stops being recognisable.
const u32 = (n) => n >>> 0;
const ERR = 0xffffffff;

// Linear memory only ever grows, so the current byte length IS the high-water mark. Sampled after
// each phase anyway, so a regression can be attributed to a phase rather than to the whole run.
let highWater = 0;
const mark = (label) => {
  const at = e.memory.buffer.byteLength;
  if (at > highWater) highWater = at;
  return [label, at];
};
const phase = [];
phase.push(mark('module instantiated'));

// ---- The ABI surface ---------------------------------------------------------------------------

const want = [
  'idx_image_new', 'idx_image_push_vector', 'idx_image_add', 'idx_image_build',
  'idx_image_doc_count', 'idx_image_search_vector', 'idx_image_search_fused',
  'idx_image_hash_near', 'idx_image_why', 'idx_image_result_ptr', 'idx_image_result_len',
  'idx_image_free',
];
check(want.every((k) => k in e), `all ${want.length} idx_image_* symbols are exported`);
check(e.idx_abi_version() === 12, 'ABI version is 12');

// No wasm-bindgen, asserted rather than assumed: its shims appear as imports, and this module
// declares none at all. If the row ever grows a bindgen dependency, this line fails first.
check(WebAssembly.Module.imports(new WebAssembly.Module(wasm)).length === 0,
  'the artifact imports nothing — no wasm-bindgen shim, no JS glue');

// ---- The corpus --------------------------------------------------------------------------------
//
// Six images. `camera` is a facet, `year` is a numeric column, and the embeddings are hand-picked
// so that the nearest neighbour of the probe (doc 1, an iPhone shot) is EXCLUDED by the Canon
// filter used below. That is what makes the leak test falsifiable rather than decorative.

const DIM = 8;
const SEP = '\0';
const SPEC = ['caption:3:0.4', 'camera:1:0.6:f', 'year:0:0.6:n'].join(SEP);

const doc = [
  { caption: 'beach sunset waves',   camera: 'Canon EOS R5', year: '2019',
    vec: [0.90, 0.20, 0.10, 0.00, 0.00, 0.10, 0.00, 0.00], dhash: 0x0123456789abcdefn },
  { caption: 'beach umbrella sand',  camera: 'iPhone 15',    year: '2020',
    vec: [0.95, 0.25, 0.05, 0.00, 0.00, 0.00, 0.00, 0.00], dhash: 0x0123456789abcdecn },
  { caption: 'mountain snow peak',   camera: 'Canon EOS R5', year: '2021',
    vec: [0.10, 0.90, 0.20, 0.00, 0.00, 0.00, 0.00, 0.00], dhash: 0xffffffffffffffffn },
  { caption: 'city street night',    camera: 'iPhone 15',    year: '2018',
    vec: [0.00, 0.10, 0.95, 0.20, 0.00, 0.00, 0.00, 0.00], dhash: 0x0000000000000000n },
  { caption: 'beach dog running',    camera: 'Pixel 8',      year: '2022',
    vec: [0.80, 0.30, 0.20, 0.10, 0.00, 0.00, 0.00, 0.00], dhash: 0xaaaaaaaaaaaaaaaan },
  { caption: 'forest trail mist',    camera: 'Canon EOS R5', year: '2017',
    vec: [0.20, 0.20, 0.90, 0.10, 0.00, 0.00, 0.00, 0.00], dhash: 0x5555555555555555n },
];
const probe = [1.0, 0.2, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];

// ---- Host-side helpers -------------------------------------------------------------------------
//
// Every helper re-derives its view from `e.memory.buffer`. `idx_alloc` can call `memory.grow()`,
// which DETACHES every existing view — even `grow(0)` — and a view cached across an allocation
// reads zeroes with no error at all.

const enc = new TextEncoder();
const put = (s) => {
  const b = enc.encode(s);
  const ptr = u32(e.idx_alloc(b.length || 1));
  if (ptr === 0) throw new Error('idx_alloc failed');
  if (b.length) new Uint8Array(e.memory.buffer, ptr, b.length).set(b);
  return [ptr, b.length];
};

/** Copy an f32 array into linear memory. This is the whole point of the row: no JSON, one copy. */
const putF32 = (v) => {
  const ptr = u32(e.idx_alloc(v.length * 4));
  if (ptr === 0) throw new Error('idx_alloc failed for an embedding');
  new Float32Array(e.memory.buffer, ptr, v.length).set(v);
  return [ptr, v.length];
};

const putF64 = (v) => {
  const ptr = u32(e.idx_alloc(v.length * 8));
  new Float64Array(e.memory.buffer, ptr, v.length).set(v);
  return [ptr, v.length];
};

const HIT = 12;  // u32 doc, f32 score, u32 why
const NEAR = 8;  // u32 doc, u32 distance

const readHit = (h, n) => {
  if (n === 0 || n === ERR) return [];
  const dv = new DataView(e.memory.buffer, u32(e.idx_image_result_ptr(h)), n * HIT);
  return Array.from({ length: n }, (_, i) => ({
    doc: dv.getUint32(i * HIT, true),
    score: dv.getFloat32(i * HIT + 4, true),
    why: dv.getUint32(i * HIT + 8, true),
  }));
};

const readNear = (h, n) => {
  if (n === 0 || n === ERR) return [];
  const dv = new DataView(e.memory.buffer, u32(e.idx_image_result_ptr(h)), n * NEAR);
  return Array.from({ length: n }, (_, i) => ({
    doc: dv.getUint32(i * NEAR, true),
    distance: dv.getUint32(i * NEAR + 4, true),
  }));
};

// A raw pointer read as Float32Array must be 4-byte aligned, and idx_alloc asks its allocator for
// alignment 1. It happens to return more; asserting it here means a future allocator change that
// broke the fast path would be caught by this file rather than by a host in production.
{
  const [p] = putF32(probe);
  check(p % 4 === 0, 'idx_alloc returns 4-byte-aligned memory, so a host may write Float32Array directly');
  e.idx_free(p, probe.length * 4);
}

// ---- Build ---------------------------------------------------------------------------------------

const [sp, sl] = put(SPEC);
const ix = u32(e.idx_image_new(sp, sl, DIM, 0 /* cosine */));
e.idx_free(sp, sl);
check(ix !== 0, 'idx_image_new accepts a spec with facet and numeric column types');

{
  const [bp, bl] = put('caption:3:0.4\0camera:1:0.6:X');
  check(u32(e.idx_image_new(bp, bl, DIM, 0)) === 0, 'an unrecognised column type is refused, not ignored');
  e.idx_free(bp, bl);
  const [mp, ml] = put(SPEC);
  check(u32(e.idx_image_new(mp, ml, DIM, 99)) === 0, 'an unknown metric is refused');
  e.idx_free(mp, ml);
}

for (const d of doc) {
  const [vp] = putF32(d.vec);
  check(u32(e.idx_image_push_vector(ix, vp, DIM)) === 1, `embedding staged for "${d.caption}"`);
  e.idx_free(vp, DIM * 4);

  const [dp, dl] = put([d.caption, d.camera, d.year].join(SEP));
  const hi = Number(d.dhash >> 32n) >>> 0;
  const lo = Number(d.dhash & 0xffffffffn) >>> 0;
  const ord = u32(e.idx_image_add(ix, dp, dl, 1 /* dhash present */, hi, lo, 0, 0, 0, 0));
  e.idx_free(dp, dl);
  if (ord === ERR) check(false, `idx_image_add accepted "${d.caption}"`);
}
phase.push(mark('six images ingested'));

// The dimension mismatch that means a model was swapped mid-run: refused at the call that can name
// both numbers, never a trap and never a silently dropped vector.
{
  const [wp] = putF32([1, 2, 3]);
  check(u32(e.idx_image_push_vector(ix, wp, 3)) === 0, 'a dimension mismatch is refused, not trapped');
  e.idx_free(wp, 12);
  check(u32(e.idx_image_push_vector(0, wp, DIM)) === 0, 'a null handle returns the refusal sentinel');
  check(u32(e.idx_image_search_fused(0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, -1, 5)) === ERR,
    'a fused search on a null handle returns UINT32_MAX, not a trap');
}
check(u32(e.idx_image_search_vector(ix, 0, DIM, 5, 4)) === ERR, 'a query before build is refused');

check(u32(e.idx_image_build(ix)) === 1, 'idx_image_build finishes the index');
check(u32(e.idx_image_build(ix)) === 0, 'building twice is refused rather than trapping');
check(u32(e.idx_image_doc_count(ix)) === doc.length, 'doc_count matches what was added');
phase.push(mark('index built'));

// ---- The expected ranking, computed in JavaScript ------------------------------------------------
//
// Cosine over the raw embeddings. The column L2-normalises on ingest and the query on search, so
// cosine IS the score, and min-max normalisation inside the fusion is monotone — it cannot reorder
// a single arm. So this order is the order the module must return.

const cos = (a, b) => {
  let dot = 0, na = 0, nb = 0;
  for (let i = 0; i < a.length; i++) { dot += a[i] * b[i]; na += a[i] * a[i]; nb += b[i] * b[i]; }
  return dot / Math.sqrt(na * nb);
};
const byCosine = doc
  .map((d, i) => ({ doc: i, sim: cos(d.vec, probe) }))
  .sort((x, y) => y.sim - x.sim || x.doc - y.doc);

check(new Set(byCosine.map((x) => x.sim.toFixed(6))).size === doc.length,
  'the expected order has no ties, so it is a real assertion rather than an arbitrary one');

// ---- Vector search alone -------------------------------------------------------------------------

const [pp] = putF32(probe);
{
  const n = u32(e.idx_image_search_vector(ix, pp, DIM, doc.length, 4));
  const got = readHit(ix, n);
  check(got.map((h) => h.doc).join() === byCosine.map((x) => x.doc).join(),
    `vector search matches the ordering JS computed: ${byCosine.map((x) => x.doc).join()}`);
  check(got.every((h, i) => Math.abs(h.score - byCosine[i].sim) < 1e-5),
    'and its scores are the cosine similarities, not a rescaling');
  check(got.every((h) => h.why === 2), 'every vector-only hit is attributed to the vector arm alone');
  check(u32(e.idx_image_why(ix, 0)) === 2, 'idx_image_why agrees with the record it decoded');
  check(u32(e.idx_image_why(ix, 99)) === ERR, 'why past the end is UINT32_MAX, not the empty mask 0');
}
phase.push(mark('vector search'));

// ---- The p50 leak test ---------------------------------------------------------------------------
//
// A vector arm knows nothing about facets. If the fused plan trusts it instead of filtering it, the
// nearest neighbour of the probe walks straight past the filter bar. Here that neighbour is doc 1,
// an iPhone shot, and the filter says Canon.

const CANON = doc.map((d, i) => [d, i]).filter(([d]) => d.camera === 'Canon EOS R5').map(([, i]) => i);
const canonExpect = byCosine.filter((x) => CANON.includes(x.doc)).map((x) => x.doc);

check(!CANON.includes(byCosine[0].doc),
  `the nearest neighbour overall (doc ${byCosine[0].doc}) is EXCLUDED by the filter — the leak test can fail`);

{
  const [fp, fl] = put('0=Canon EOS R5');
  const n = u32(e.idx_image_search_fused(ix, 0, 0, fp, fl, 0, 0, pp, DIM, 0, 0, 0, 0, -1, doc.length));
  e.idx_free(fp, fl);
  const got = readHit(ix, n);
  check(got.length === CANON.length, `the filtered fused query returns only the ${CANON.length} Canon rows`);
  check(got.every((h) => CANON.includes(h.doc)),
    'THE VECTOR ARM DOES NOT LEAK THE FACET FILTER — no excluded document appears at any score');
  check(got.map((h) => h.doc).join() === canonExpect.join(),
    `and the surviving order is the JS cosine order restricted to the facet: ${canonExpect.join()}`);
}

// A malformed spec must be an error, never a silent match-everything: that is the dangerous reading.
{
  const [bp, bl] = put('notanumber=Canon EOS R5');
  check(u32(e.idx_image_search_fused(ix, 0, 0, bp, bl, 0, 0, pp, DIM, 0, 0, 0, 0, -1, 5)) === ERR,
    'a malformed filter spec errors rather than matching everything');
  e.idx_free(bp, bl);
}

// The numeric half of the filter bar, half-open lo <= v < hi, passed as one Float64Array of triples.
{
  const [rp, rn] = putF64([0, 2019, 2021]);
  const n = u32(e.idx_image_search_fused(ix, 0, 0, 0, 0, rp, 1, pp, DIM, 0, 0, 0, 0, -1, doc.length));
  const got = readHit(ix, n);
  e.idx_free(rp, rn * 8);
  check(got.map((h) => h.doc).sort().join() === '0,1',
    'a half-open year range excludes the row sitting exactly on the upper bound');
}
phase.push(mark('fused search'));

// ---- Explicability: which signal found it --------------------------------------------------------
//
// Text AND vector together. "beach" is in captions 0, 1 and 4; the probe is nearest 1, 0, 4. A row
// both arms found must carry both bits, or the fused plan's advantage over a fan-out is invisible
// to the application even when it is real.
{
  const [qp, ql] = put('beach');
  const n = u32(e.idx_image_search_fused(ix, qp, ql, 0, 0, 0, 0, pp, DIM, 0, 0, 0, 0, -1, doc.length));
  e.idx_free(qp, ql);
  const got = readHit(ix, n);
  const both = got.filter((h) => (h.why & 1) !== 0 && (h.why & 2) !== 0).map((h) => h.doc);
  check(both.length >= 1, `at least one row is found by BOTH text and vector (docs ${both.join() || 'none'})`);
  check(got[0].why === 3, 'and a two-signal row ranks first — agreement is evidence, not noise');
  check(got.every((h, i) => u32(e.idx_image_why(ix, i)) === h.why),
    'idx_image_why agrees with every decoded record');
}

// Text alone: the vector arm dropped, and the why mask says so.
{
  const [qp, ql] = put('mountain');
  const n = u32(e.idx_image_search_fused(ix, qp, ql, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, -1, 5));
  e.idx_free(qp, ql);
  const got = readHit(ix, n);
  check(got.length >= 1 && got[0].doc === 2 && got[0].why === 1,
    'a text-only fused query is attributed to the text arm alone');
}

// ---- Near-duplicates ------------------------------------------------------------------------------
//
// Doc 1's dHash differs from doc 0's in exactly two bits — the re-encode/resize case this column
// exists for. Everything else is far away. A different record layout, so the count is what tells
// the host which decoder to run.
{
  const hi = Number(doc[0].dhash >> 32n) >>> 0;
  const lo = Number(doc[0].dhash & 0xffffffffn) >>> 0;
  const near = readNear(ix, u32(e.idx_image_hash_near(ix, hi, lo, 4)));
  check(near.map((x) => x.doc).join() === '0,1', 'hash_near finds the re-encode within radius 4');
  check(near[0].distance === 0 && near[1].distance === 2, 'and reports the true Hamming distances');
  check(u32(e.idx_image_hash_near(ix, hi, lo, 0)) === 1, 'radius 0 is the exact hash only');
  check(u32(e.idx_image_hash_near(0, hi, lo, 4)) === ERR, 'hash_near on a null handle returns the sentinel');
}

e.idx_free(pp, DIM * 4);
e.idx_image_free(ix);
e.idx_image_free(0); // must not trap
phase.push(mark('teardown'));

// ---- Linear-memory high-water mark ------------------------------------------------------------------
//
// p62 acceptance item 3. `docs/research/portability.md` §1: an mmap-style format does not FAIL to
// port to WASM — it silently reads the whole index into linear memory. A vector column is the
// largest thing this engine has ever put there, so the number that actually binds in a browser is
// not latency, it is address space.

const PAGE = 65536;
const GiB4 = 4 * 1024 ** 3;

// What one vector costs in the column, by tier, for a column that retains the exact f32 originals:
//   binary prefilter  ceil(dim/64) 64-bit words
//   int8 rerank       dim bytes
//   scale             one f32 per vector
//   exact             dim f32
const bytePerVector = (dim) => 8 * Math.ceil(dim / 64) + dim + 4 + 4 * dim;

console.log('');
console.log('  LINEAR MEMORY');
for (const [label, at] of phase) {
  console.log(`    ${String(at).padStart(9)} B  (${String(at / PAGE).padStart(4)} pages)  ${label}`);
}
console.log(`    HIGH-WATER MARK: ${highWater} B (${(highWater / 1024).toFixed(1)} KiB, ${highWater / PAGE} pages)`);
console.log(`    artifact: ${wasm.length} B raw`);
console.log('');
console.log('  32-BIT CEILING — a wasm32 module addresses at most 4 GiB of linear memory, and the');
console.log('  whole index lives there. The vector column alone caps the corpus at:');
for (const dim of [128, 384, 512, 768]) {
  const b = bytePerVector(dim);
  const cap = Math.floor(GiB4 / b);
  console.log(`    dim ${String(dim).padStart(3)}:  ${String(b).padStart(5)} B/vector  ->  ${(cap / 1e6).toFixed(2)} M images`);
}
console.log('  Text postings, facet columns, hashes and the host\'s own JS heap all come out of the');
console.log('  same 4 GiB, so treat these as an upper bound and not a target. At 512-d — the width');
console.log(`  CLIP-class models actually emit — the ceiling is ${(Math.floor(GiB4 / bytePerVector(512)) / 1e6).toFixed(2)} M images, which is the same order`);
console.log('  as the 1.28 M-image corpus docs/research/image.md §8 records rclip ingesting. The');
console.log('  browser is therefore a real deployment target for a personal library and NOT one for');
console.log('  a shared archive; a compact column (binary + int8 only) raises it about 5.7x, and a');
console.log('  64-bit host removes the ceiling entirely.');
console.log('');

console.log(failed === 0 ? 'OVERALL: PASS' : `OVERALL: FAIL (${failed})`);
process.exit(failed === 0 ? 0 : 1);
