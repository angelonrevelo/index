// End-to-end proof that the engine works outside Rust.
//
// Loads the WASM module and a real index built from profstopick's Ateneo registrar snapshot, then
// answers queries — including the typo'd, name-shaped queries whose failure that application
// measured in production: 109 of 267 real searches returning nothing on 2026-08-17.
//
//   cargo build -p index-wasm --release --target wasm32-unknown-unknown
//   cargo run -p index-bench --release --bin emit-artifact
//   node js/demo.mjs
//
// Exits non-zero if any assertion fails, so it is usable as a check and not just a script.

import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { SearchIndex } from './index.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const WASM = join(root, 'target/wasm32-unknown-unknown/release/index_wasm.wasm');
const IDX = join(root, 'artifact/profstopick.idx');
const LABEL = join(root, 'artifact/profstopick.label.json');

let failed = 0;
const check = (ok, msg) => {
  console.log(`  ${ok ? 'PASS' : 'FAIL'}  ${msg}`);
  if (!ok) failed++;
};

const wasm = await readFile(WASM).catch(() => null);
if (!wasm) {
  console.error(`missing ${WASM}\nrun: cargo build -p index-wasm --release --target wasm32-unknown-unknown`);
  process.exit(2);
}
const idxBytes = await readFile(IDX).catch(() => null);
if (!idxBytes) {
  console.error(`missing ${IDX}\nrun: cargo run -p index-bench --release --bin emit-artifact`);
  process.exit(2);
}
const label = JSON.parse(await readFile(LABEL, 'utf8'));

console.log('index-text in Node, through WASM — no wasm-bindgen, no native build\n');
console.log(`  wasm module   ${wasm.length.toLocaleString()} bytes`);
console.log(`  index file    ${idxBytes.length.toLocaleString()} bytes`);

const t0 = performance.now();
const ix = await SearchIndex.open(wasm, idxBytes, { label });
const openMs = performance.now() - t0;

console.log(`  documents     ${ix.docCount.toLocaleString()}`);
console.log(`  terms         ${ix.termCount.toLocaleString()}`);
console.log(`  open + parse  ${openMs.toFixed(1)} ms\n`);

// A real professor from the corpus, and the ways a student actually mistypes a name.
const target = label[0];
console.log(`querying for: ${JSON.stringify(target)}\n`);

const cases = [
  ['exact name', target],
  ['surname only', target.split(',')[0]],
  ['lowercase', target.toLowerCase()],
  // One deleted character — the class that returns nothing without edit distance.
  ['one deletion', target.slice(0, 3) + target.slice(4)],
  // Two adjacent characters swapped.
  ['transposition', (() => {
    const c = [...target];
    if (c.length > 4) [c[2], c[3]] = [c[3], c[2]];
    return c.join('');
  })()],
];

for (const [name, q] of cases) {
  const t = performance.now();
  const hit = ix.search(q, { k: 3 });
  const ms = performance.now() - t;
  const top = hit[0];
  console.log(`  ${name.padEnd(14)} ${JSON.stringify(q)}`);
  console.log(
    `  ${''.padEnd(14)} -> ${top ? `${JSON.stringify(top.label)} (bucket ${top.typoBucket}, ${ms.toFixed(2)} ms)` : 'NO RESULTS'}`,
  );
  check(top?.label === target, `${name} finds the right professor`);
  console.log();
}

// Typeahead: the last token is treated as a prefix.
const partial = target.slice(0, 5);
const ta = ix.search(partial, { k: 5, prefix: true });
console.log(`  typeahead      ${JSON.stringify(partial)} -> ${ta.length} hit(s)`);
check(ta.some((h) => h.label === target), 'typeahead reaches the professor from a 5-char prefix');

// Nonsense must return nothing rather than everything.
check(ix.search('qzxwv unmatched nonsense', { k: 5 }).length === 0, 'a nonsense query returns nothing');

// Latency over the whole corpus, from JavaScript.
const sample = label.slice(0, 500);
const t1 = performance.now();
for (const q of sample) ix.search(q, { k: 10 });
const per = ((performance.now() - t1) / sample.length) * 1000;
console.log(`\n  ${sample.length} real name queries: ${per.toFixed(0)} us each, called from JS`);
check(per < 5000, 'mean query under 5 ms from JavaScript');

ix.close();
console.log(`\n${failed === 0 ? 'OVERALL: PASS' : `OVERALL: FAIL (${failed})`}`);
process.exit(failed === 0 ? 0 : 1);
