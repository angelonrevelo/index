// Where the bytes of a WebAssembly artifact actually go.
//
// The browser tier is this engine's unique position, so the module's size is a product number, not
// a build detail -- and it regressed 3.5x (173,700 -> 604,184 bytes) with nobody watching, because
// nothing in the repo ever printed the breakdown. This does.
//
//   node scripts/wasm-section.mjs target/wasm32-unknown-unknown/release/index_wasm.wasm
//   node scripts/wasm-section.mjs <file> --function 20
//
// No dependency, deliberately: `twiggy` and `cargo bloat` would each be a toolchain a contributor
// has to install, and a wasm module is a flat list of length-prefixed sections that a hundred lines
// of hand-rolled LEB128 reads perfectly well. This repo parses its own index format by hand for the
// same reason.
//
// `--function N` needs the `name` custom section, which the shipped build strips. Point it at an
// unstripped build to attribute code bytes to functions:
//
//   RUSTFLAGS="" CARGO_TARGET_DIR=target/unstripped \
//     cargo build -p index-wasm --target wasm32-unknown-unknown --release
//
// Exits non-zero if the file is not a wasm module, so it is usable as a check.

import { readFileSync } from 'node:fs';
import { gzipSync } from 'node:zlib';

const arg = process.argv.slice(2);
const path = arg.find((a) => !a.startsWith('--'));
const topCount = arg.includes('--function') ? Number(arg[arg.indexOf('--function') + 1] || 25) : 0;

if (!path) {
  console.error('usage: node scripts/wasm-section.mjs <file.wasm> [--function N]');
  process.exit(2);
}

const buf = readFileSync(path);
if (buf.length < 8 || buf.readUInt32LE(0) !== 0x6d736100) {
  console.error(`${path}: not a WebAssembly module`);
  process.exit(2);
}

/** Read one unsigned LEB128 at `at`. Returns the value and the offset just past it. */
function leb(at) {
  let value = 0;
  let shift = 0;
  let byte;
  do {
    byte = buf[at++];
    value |= (byte & 0x7f) << shift;
    shift += 7;
  } while (byte & 0x80);
  return [value >>> 0, at];
}

const SECTION_NAME = [
  'custom', 'type', 'import', 'function', 'table', 'memory', 'global',
  'export', 'start', 'element', 'code', 'data', 'data-count', 'tag',
];

const section = [];
let codeBody = 0;
let nameRange = null;

let at = 8;
while (at < buf.length) {
  const id = buf[at];
  const [size, after] = leb(at + 1);
  const body = after;
  const end = body + size;
  let label = SECTION_NAME[id] ?? `id-${id}`;
  if (id === 0) {
    const [len, q] = leb(body);
    const custom = buf.slice(q, q + len).toString('utf8');
    label = `custom:${custom}`;
    if (custom === 'name') nameRange = { body, end };
  }
  if (id === 10) codeBody = body;
  section.push({ label, byte: end - at });
  at = end;
}

const raw = buf.length;
const gzip = gzipSync(buf, { level: 9 }).length;

console.log(`${path}`);
console.log(`  raw ${raw.toLocaleString()} bytes   gzipped ${gzip.toLocaleString()} bytes`);
console.log();
console.log('  section                bytes    share');
for (const s of [...section].sort((a, b) => b.byte - a.byte)) {
  console.log(
    `  ${s.label.padEnd(22)} ${String(s.byte).padStart(8)}   ${((100 * s.byte) / raw).toFixed(1).padStart(5)}%`,
  );
}

if (!topCount) process.exit(0);

// Function bodies. Every body is length-prefixed, so the code section is walkable without
// decoding a single instruction.
const body = [];
{
  const [count, first] = leb(codeBody);
  let p = first;
  for (let i = 0; i < count; i++) {
    const start = p;
    const [size, afterSize] = leb(p);
    body.push({ index: i, byte: size + (afterSize - start) });
    p = afterSize + size;
  }
}

// The name section's subsection 1 is the function-name map: index -> symbol.
const symbol = new Map();
if (nameRange) {
  const [len, afterLen] = leb(nameRange.body);
  let p = afterLen + len;
  while (p < nameRange.end) {
    const kind = buf[p++];
    const [size, afterSize] = leb(p);
    const end = afterSize + size;
    if (kind === 1) {
      let [count, q] = leb(afterSize);
      for (let i = 0; i < count; i++) {
        let index, nameLen;
        [index, q] = leb(q);
        [nameLen, q] = leb(q);
        symbol.set(index, buf.slice(q, q + nameLen).toString('utf8'));
        q += nameLen;
      }
    }
    p = end;
  }
}

console.log();
if (!symbol.size) {
  console.log(`  ${body.length} function bodies; no \`name\` section, so they cannot be attributed.`);
  console.log('  Rebuild with RUSTFLAGS="" into a scratch CARGO_TARGET_DIR to attribute them.');
  process.exit(0);
}

// One bucket per originating crate or runtime facility. `slice::sort` gets its own because it is
// the single largest contributor and the one a code change could actually remove.
const bucket = [
  ['slice::sort', /4core5slice4sort/],
  ['core::fmt', /4core3fmt/],
  ['panic', /panic/],
  ['dlmalloc', /dlmalloc/],
  ['hashbrown', /hashbrown/],
  ['index-text', /index_text/],
  ['index-image', /index_image/],
  ['index-wasm', /index_wasm|^idx_/],
  ['tantivy-fst', /tantivy_fst/],
  ['levenshtein', /levenshtein/],
  ['core/alloc/std', /4core|5alloc|3std|__rust|mem(cpy|set|cmp)/],
];

const tally = new Map();
let codeByte = 0;
for (const f of body) {
  const name = symbol.get(f.index) ?? '';
  codeByte += f.byte;
  const hit = bucket.find(([, re]) => re.test(name));
  const key = hit ? hit[0] : '(other)';
  tally.set(key, (tally.get(key) ?? 0) + f.byte);
}

console.log(`  ${body.length} function bodies, ${codeByte.toLocaleString()} bytes of code`);
console.log();
console.log('  origin                 bytes    share');
for (const [key, byte] of [...tally].sort((a, b) => b[1] - a[1])) {
  console.log(
    `  ${key.padEnd(22)} ${String(byte).padStart(8)}   ${((100 * byte) / codeByte).toFixed(1).padStart(5)}%`,
  );
}

console.log();
console.log(`  largest ${topCount} functions`);
for (const f of [...body].sort((a, b) => b.byte - a.byte).slice(0, topCount)) {
  const name = (symbol.get(f.index) ?? `<function ${f.index}>`).replace(/17h[0-9a-f]{16}E?$/, '');
  console.log(`  ${String(f.byte).padStart(8)}  ${name.slice(0, 120)}`);
}
