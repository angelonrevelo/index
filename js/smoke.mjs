// CI smoke test: the WASM artifact must load and answer a query with no corpus on disk.
//
// `js/demo.mjs` is the richer proof but needs the profstopick checkout, which CI does not have.
// This builds a tiny index in Rust-free JavaScript terms — it cannot, so instead it asserts the
// module's ABI surface and its refusal behaviour, which is what actually breaks silently.

import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const wasm = await readFile(join(root, 'target/wasm32-unknown-unknown/release/index_wasm.wasm'));
const { instance } = await WebAssembly.instantiate(wasm, {});
const e = instance.exports;

let failed = 0;
const check = (ok, msg) => {
  console.log(`  ${ok ? 'PASS' : 'FAIL'}  ${msg}`);
  if (!ok) failed++;
};

const want = [
  'memory', 'idx_abi_version', 'idx_alloc', 'idx_free', 'idx_open', 'idx_close',
  'idx_search', 'idx_result_ptr', 'idx_result_len', 'idx_doc_count', 'idx_term_count',
];
check(want.every((k) => k in e), `all ${want.length} ABI symbols are exported`);
check(e.idx_abi_version() === 2, 'ABI version is 2');

// Allocation round-trips through linear memory.
const u32 = (n) => n >>> 0;

const p = u32(e.idx_alloc(64));
check(p !== 0, 'idx_alloc returns a pointer');
new Uint8Array(e.memory.buffer, p, 4).set([1, 2, 3, 4]);
check(new Uint8Array(e.memory.buffer, p, 4)[3] === 4, 'host can write to linear memory');
e.idx_free(p, 64);

// Garbage must be refused, not trapped on — a trap kills the whole instance.
const bad = u32(e.idx_alloc(8));
new Uint8Array(e.memory.buffer, bad, 8).set([71, 65, 82, 66, 65, 71, 69, 33]);
check(u32(e.idx_open(bad, 8)) === 0, 'garbage bytes are refused rather than trapping');
e.idx_free(bad, 8);
check(u32(e.idx_doc_count(0)) === 0, 'a null handle returns 0 rather than trapping');
// WASM returns i32: u32::MAX arrives as -1 unless coerced. Asserting the raw value here is the
// point — it is how the sentinel silently stopped being recognisable in the first draft.
check(u32(e.idx_search(0, 0, 0, 5, 0)) === 0xffffffff, 'search on a null handle returns the sentinel');

console.log(failed === 0 ? 'OVERALL: PASS' : `OVERALL: FAIL (${failed})`);
process.exit(failed === 0 ? 0 : 1);
