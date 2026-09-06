// p34 :: the analytics kernels measured where they actually ship — inside WebAssembly.
//
// `bench/roadmap/p33-accel-kernel.md` measured these kernels natively and said plainly that it was
// not a browser number: wasm has bounds-checked memory, no SIMD, and 32-bit pointers. This closes
// that limit by running the same seven kernels through the real 6,342-byte artifact, driven exactly
// as onegrid's host drives it — bump-allocating above `og_heap_base()`.
//
// It reports throughput AND checks correctness in this tier, because a fast wrong answer is the
// failure mode that matters: the native pass cannot catch a defect that only exists at 32-bit
// pointer width, which is precisely the gap `p33` left open.

import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
// Optional path argument so two artifacts can be compared -- notably the default build against a
// `-C target-feature=+simd128` one, which is the follow-up p34 named and did not run.
const WASM = process.argv[2] ?? join(root, 'target/wasm32-unknown-unknown/release/index_accel.wasm');

const N = 1_000_000;
const SLOTS = 4096;

const wasm = await readFile(WASM);
const { instance } = await WebAssembly.instantiate(wasm, {});
const e = instance.exports;

let failed = 0;
const check = (ok, msg) => {
  console.log(`  ${ok ? 'PASS' : 'FAIL'}  ${msg}`);
  if (!ok) failed++;
};

console.log('p34-accel-wasm :: the analytics kernels, measured inside WebAssembly');
console.log(`  artifact ${wasm.byteLength.toLocaleString()} bytes, ABI version ${e.og_abi_version()}`);

// ---- Host-owned bump allocator, above og_heap_base ------------------------------------------
// This is the contract: the module never allocates, the host owns everything above heap_base.
let brk = e.og_heap_base();
const need = 8 * N + 4 * N * 3 + 2 * Math.ceil(N / 8) + SLOTS * 16 + 4096;
const havePages = e.memory.buffer.byteLength / 65536;
const wantPages = Math.ceil((brk + need) / 65536) + 8;
if (wantPages > havePages) e.memory.grow(wantPages - havePages);
console.log(`  memory ${havePages} -> ${e.memory.buffer.byteLength / 65536} pages, heap_base ${brk.toLocaleString()}`);

// Every allocation is 8-byte aligned: an f64 view at an unaligned offset throws in JS and reads
// garbage in some engines. Growing FIRST means no view created below is ever detached.
const alloc = (bytes) => {
  const p = (brk + 7) & ~7;
  brk = p + bytes;
  return p;
};

const valuePtr = alloc(8 * N);
const presencePtr = alloc(Math.ceil(N / 8));
const maskPtr = alloc(Math.ceil(N / 8));
const permPtr = alloc(4 * N);
const scratchPtr = alloc(4 * N);
const outIPtr = alloc(4 * N);
const slotKeyPtr = alloc(8 * SLOTS);
const slotStatePtr = alloc(4 * SLOTS);
const slotCodePtr = alloc(4 * SLOTS);
const outFPtr = alloc(64);

const f64 = new Float64Array(e.memory.buffer);
const i32 = new Int32Array(e.memory.buffer);
const u8 = new Uint8Array(e.memory.buffer);

// ---- Data: small integers dominate so ties and duplicates are common, specials salted in -----
let seed = 0x1234abcd;
const rand = () => {
  seed ^= seed << 13;
  seed ^= seed >>> 17;
  seed ^= seed << 5;
  return seed >>> 0;
};

for (let i = 0; i < N; i++) {
  const r = rand() % 32;
  let v;
  if (r === 0) v = NaN;
  else if (r === 1) v = -0;
  else if (r === 3) v = Infinity;
  else if (r === 4) v = -Infinity;
  else v = (rand() % 50) - 25;
  f64[valuePtr / 8 + i] = v;
  // A row is missing when its validity bit is clear OR its value is NaN; the host folds both.
  const present = rand() % 8 !== 0 && !Number.isNaN(v);
  const byte = presencePtr + (i >> 3);
  if (present) u8[byte] |= 1 << (i & 7);
  else u8[byte] &= ~(1 << (i & 7));
}

// ---- Correctness in THIS tier ----------------------------------------------------------------
// Not a full differential harness — that is `accel-kernel`. These are the properties that would
// break if 32-bit pointer arithmetic went wrong, which is exactly what a native run cannot see.
{
  const present = (i) => (u8[presencePtr + (i >> 3)] & (1 << (i & 7))) !== 0;

  e.og_filter_mask(valuePtr, presencePtr, N, 4, 0, 0, 0, 0, maskPtr); // v > 0
  let want = 0;
  for (let i = 0; i < N; i++) if (present(i) && f64[valuePtr / 8 + i] > 0) want++;
  let got = 0;
  for (let i = 0; i < N; i++) if ((u8[maskPtr + (i >> 3)] & (1 << (i & 7))) !== 0) got++;
  check(got === want, `filter_mask over ${N.toLocaleString()} rows agrees with a JS count (${got})`);

  const written = e.og_aggregate(2, valuePtr, presencePtr, N, 0, -1, slotKeyPtr, slotStatePtr, SLOTS, outFPtr);
  let live = 0;
  for (let i = 0; i < N; i++) if (present(i)) live++;
  check(written === 1 && f64[outFPtr / 8] === live, `aggregate COUNT equals the live-row count (${live})`);

  // top_k must return indices whose values really are the k largest present ones.
  const K = 32;
  const n = e.og_top_k(valuePtr, presencePtr, N, K, 1, 0, outIPtr, scratchPtr);
  let ordered = true;
  for (let i = 1; i < n; i++) {
    const a = f64[valuePtr / 8 + i32[outIPtr / 4 + i - 1]];
    const b = f64[valuePtr / 8 + i32[outIPtr / 4 + i]];
    if (!(a >= b)) ordered = false;
  }
  check(n === K && ordered, `top_k(${K}) returns ${n} indices in descending value order`);
}

// ---- Throughput -------------------------------------------------------------------------------
const time = (fn) => {
  fn(); // warm
  const t0 = process.hrtime.bigint();
  fn();
  return Number(process.hrtime.bigint() - t0) / 1e6;
};

const row = [];
row.push(['filter_mask', time(() => e.og_filter_mask(valuePtr, presencePtr, N, 4, 0, 0, 0, 0, maskPtr))]);
row.push(['aggregate', time(() => e.og_aggregate(0, valuePtr, presencePtr, N, 0, -1, slotKeyPtr, slotStatePtr, SLOTS, outFPtr))]);
row.push(['group_code', time(() => e.og_group_code(valuePtr, presencePtr, N, outIPtr, slotKeyPtr, slotCodePtr, SLOTS))]);
row.push(['top_k(100)', time(() => e.og_top_k(valuePtr, presencePtr, N, 100, 1, 0, outIPtr, scratchPtr))]);
row.push([
  'sort_pass',
  time(() => {
    for (let i = 0; i < N; i++) i32[permPtr / 4 + i] = i;
    e.og_sort_pass(valuePtr, presencePtr, N, 0, 0, permPtr, scratchPtr);
  }),
]);
// bitmap_op touches N/8 bytes, not N values, and is far too fast for a single shot: the native
// arm measured 0.00 ms and printed a six-figure throughput, which is timer noise. Averaged.
const REP = 200;
// Two variants, because the first version of this bench passed `maskPtr` as BOTH the second input
// and the output. Overlapping buffers force a compiler to assume aliasing, which is one of the two
// standard reasons a byte loop fails to vectorise -- so measuring only the aliased form would have
// confounded "wasm has no SIMD" with "this call cannot be vectorised at all".
row.push([
  'bitmap_op',
  time(() => {
    for (let r = 0; r < REP; r++) e.og_bitmap_op(0, presencePtr, maskPtr, N, scratchPtr);
  }) / REP,
]);
row.push([
  'bitmap_op(alias)',
  time(() => {
    for (let r = 0; r < REP; r++) e.og_bitmap_op(0, presencePtr, maskPtr, N, maskPtr);
  }) / REP,
]);

console.log(`\n  --- throughput at ${N.toLocaleString()} rows, inside WebAssembly ---`);
console.log(`  ${'kernel'.padEnd(17)} ${'ms'.padStart(9)} ${'M rows/s'.padStart(12)}`);
for (const [name, ms] of row) {
  console.log(`  ${name.padEnd(17)} ${ms.toFixed(3).padStart(9)} ${(N / ms / 1000).toFixed(1).padStart(12)}`);
}

console.log(`\nOVERALL: ${failed === 0 ? 'PASS' : `FAIL (${failed})`}`);
process.exit(failed === 0 ? 0 : 1);
