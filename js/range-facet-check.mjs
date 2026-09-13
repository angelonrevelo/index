// Faceted and numeric-range queries through the range tier, answered identically to a full open.
//
// The range tier used to answer one question: ranked text search. A storefront's filter bar is
// four more -- a facet clause, a facet tally, a price range, a price histogram -- and a host that can
// only read byte ranges had to fall back to fetching the whole file for any of them. This builds a
// synthetic index WITH a facet and a numeric column in-process, then for every query and filter:
//
//   1. EXACT   RangeIndex.searchClause / facetTally / searchRange / rangeTally return exactly what
//              the full-open exports (`idx_search_clause`, `idx_facet_tally`, `idx_search_range`,
//              `idx_range_tally`) return on the same bytes -- same docs, scores, buckets, labels.
//   2. COST    what is resident at open vs fetched per query, measured on this index AND on the
//              committed `profstopick.idx`, and that each filtered call reads exactly one plan.
//   3. REFUSE  a filter over a query the plan did not cover is refused, never under-counted.
//
//   node js/range-facet-check.mjs            (INDEX_WASM overrides the module path)

import { readFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { RangeIndex, clauseSpec, decodeFacetTally } from './opfs.mjs';
import { SearchIndex } from './index.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const wasm = await readFile(process.env.INDEX_WASM ?? join(here, 'index_wasm.wasm'));

let failed = 0;
const check = (ok, msg) => {
  console.log(`  ${ok ? 'PASS' : 'FAIL'}  ${msg}`);
  if (!ok) failed++;
};

const u32 = (n) => n >>> 0;
const ERR = 0xffffffff;
const HIT_BYTE = 12;
const SEP = '\0';

// ---- A raw full-open host: the reference every range answer is compared against ----------------
const { instance } = await WebAssembly.instantiate(wasm, {});
const e = instance.exports;
const enc = new TextEncoder();
const put = (bytes) => {
  const b = typeof bytes === 'string' ? enc.encode(bytes) : bytes;
  const p = u32(e.idx_alloc(b.length || 1));
  if (b.length) new Uint8Array(e.memory.buffer, p, b.length).set(b);
  return [p, b.length];
};
const free = ([p, l]) => e.idx_free(p, l || 1);

// ---- Build: name / brand (facet) / size (numeric), same shape as the Rust fixture ----------------
const BRAND = ['Colgate', 'Nescafe', 'Bear Brand', 'Lucky Me', 'Oral B', 'Milo'];
const KIND = ['Toothpaste', 'Coffee', 'Powdered Milk', 'Instant Noodle'];
const spec = put(['name:3:0.4', 'brand:1:0.6', 'size:0:0.6'].join(SEP));
const builder = u32(e.idx_build_new(...spec));
free(spec);
check(u32(e.idx_build_facet(builder, 1)) === 1 && u32(e.idx_build_numeric(builder, 2)) === 1, 'builder declares brand as a facet and size as numeric');
for (let i = 0; i < 2000; i++) {
  const grams = 50 + (i % 40) * 5;
  const row = [`${BRAND[i % BRAND.length]} ${KIND[i % KIND.length]} ${grams}g`, BRAND[i % BRAND.length], i % 29 === 0 ? 'n/a' : String(grams)];
  const d = put(row.join(SEP));
  e.idx_build_add(builder, ...d);
  free(d);
}
const built = u32(e.idx_build_finish(builder));
const size = e.idx_serialize(built);
const file = new Uint8Array(e.memory.buffer, u32(e.idx_result_ptr(built)), size).slice();
e.idx_close(built);

const fileHandle = put(file);
const full = u32(e.idx_open(...fileHandle));
free(fileHandle);
check(full !== 0, `the serialized index reopens whole (${file.length.toLocaleString()} B)`);

const fullHits = (n) => {
  if (n === ERR) return 'ERR';
  const view = new DataView(e.memory.buffer, u32(e.idx_result_ptr(full)), n * HIT_BYTE);
  return Array.from({ length: n }, (_, i) => ({
    doc: view.getUint32(i * HIT_BYTE, true),
    score: view.getFloat32(i * HIT_BYTE + 4, true),
    typoBucket: view.getUint32(i * HIT_BYTE + 8, true),
    label: null,
  }));
};
const withQuery = (q, f) => {
  const qb = put(q);
  try {
    return f(...qb);
  } finally {
    free(qb);
  }
};
const reference = {
  searchClause: (q, clause, k, offset) =>
    withQuery(q, (qp, ql) => {
      const s = put(clauseSpec(clause));
      try {
        return fullHits(u32(e.idx_search_clause(full, qp, ql, k, offset, ...s)));
      } finally {
        free(s);
      }
    }),
  facetTally: (q, slot) =>
    withQuery(q, (qp, ql) => {
      const n = u32(e.idx_facet_tally(full, qp, ql, slot));
      return n === ERR ? 'ERR' : decodeFacetTally(e, u32(e.idx_result_ptr(full)), n);
    }),
  searchRange: (q, slot, lo, hi, k) => withQuery(q, (qp, ql) => fullHits(u32(e.idx_search_range(full, qp, ql, k, slot, lo, hi)))),
  rangeTally: (q, slot, edge) =>
    withQuery(q, (qp, ql) => {
      const b = new Uint8Array(edge.length * 8);
      edge.forEach((x, i) => new DataView(b.buffer).setFloat64(i * 8, x, true));
      const ep = put(b);
      try {
        const n = u32(e.idx_range_tally(full, qp, ql, slot, ep[0], edge.length));
        if (n === ERR) return 'ERR';
        const p = u32(e.idx_result_ptr(full));
        return Array.from(new Uint32Array(e.memory.buffer.slice(p, p + n * 4)));
      } finally {
        free(ep);
      }
    }),
};

// ---- An in-memory reader that counts every byte and every read ----------------------------------
const tally = { byte: 0, read: 0 };
const read = async (span) => {
  tally.read++;
  const total = span.reduce((t, [, l]) => t + l, 0);
  const out = new Uint8Array(total);
  let at = 0;
  for (const [o, l] of span) {
    out.set(file.subarray(o, o + l), at);
    at += l;
  }
  tally.byte += total;
  return out;
};

console.log('range-facet :: facet clauses, tallies and numeric ranges through the range tier');
const ranged = await RangeIndex.open(wasm, read);
const openByte = tally.byte;
const openRead = tally.read;
check(ranged.docCount === 2000, `range open reads ${openByte.toLocaleString()} B in ${openRead} reads and knows all 2,000 docs`);

// ---- 1. EXACT -----------------------------------------------------------------------------------
const QUERY = ['toothpaste', 'colgaye', 'coffee 100g', 'powdered milk', 'bearbrand', 'milo', 'lucky me noodle', 'zzzznothing'];
const CLAUSE = [
  [{ slot: 0, value: ['Colgate'] }],
  [{ slot: 0, value: ['Colgate', 'Milo'] }],
  [{ slot: 0, value: ['Nescafe'], exclude: true }],
  [{ slot: 0, value: ['Nobody'] }],
  [],
];
const RANGE = [[0, 100, 200], [0, -1e308, 1e308], [0, 150, 150], [7, 0, 1]];
const EDGE = [0, 100, 150, 250, 400];

let compared = 0;
let exact = 0;
let nonempty = 0;
let perCallRead = true;
const same = async (tag, rangeCall, want) => {
  const before = tally.read;
  let got;
  try {
    got = await rangeCall();
  } catch (err) {
    got = /refused/.test(String(err)) ? 'ERR' : (() => { throw err; })();
  }
  if (tally.read - before !== 1) perCallRead = false;
  compared++;
  if (JSON.stringify(got) === JSON.stringify(want)) exact++;
  else console.log(`    mismatch: ${tag}`);
  if (Array.isArray(got) && got.length > 0 && got.some((x) => (typeof x === 'number' ? x > 0 : true))) nonempty++;
};

const queryByte = tally.byte;
for (const q of QUERY) {
  for (const clause of CLAUSE) {
    for (const [k, offset] of [[10, 0], [5, 3]]) {
      await same(`clause ${q} ${JSON.stringify(clause)} k${k}+${offset}`, () => ranged.searchClause(q, clause, { k, offset }), reference.searchClause(q, clause, k, offset));
    }
  }
  for (const slot of [0, 3]) await same(`facetTally ${q} ${slot}`, () => ranged.facetTally(q, slot), reference.facetTally(q, slot));
  for (const [slot, lo, hi] of RANGE) await same(`range ${q} ${slot} [${lo},${hi})`, () => ranged.searchRange(q, slot, lo, hi, { k: 10 }), reference.searchRange(q, slot, lo, hi, 10));
  for (const slot of [0, 7]) await same(`rangeTally ${q} ${slot}`, () => ranged.rangeTally(q, slot, EDGE), reference.rangeTally(q, slot, EDGE));
}
check(exact === compared, `${exact}/${compared} filtered range answers identical to the full open`);
check(nonempty > 40, `the comparison is not vacuous: ${nonempty} non-empty answers`);
check(perCallRead, 'every filtered call reads exactly one plan -- no facet or numeric bytes owed per query');

// ---- 3. REFUSE ----------------------------------------------------------------------------------
{
  // A reader that, once armed, returns one byte fewer than the plan asked for -- a truncated range
  // response. Every filtered entry point must refuse rather than answer from a short list. (A reader
  // returning the RIGHT length of the WRONG bytes is content corruption, which no plan check can see;
  // that is the transport's job. Two queries whose lists happen to encode to the same length --
  // "toothpaste" and "coffee" here, 500 docs each -- are why this test is not written that way.
  // Planning one query and answering another is refused in the module; the Rust test
  // `filtered_range_bad_input_returns_sentinels_rather_than_trapping` pins that.)
  let short = false;
  const truncating = await RangeIndex.open(wasm, async (span) => {
    const bytes = await read(span);
    return short && bytes.length > 0 ? bytes.subarray(0, bytes.length - 1) : bytes;
  });
  short = true;
  let refusal = 0;
  for (const call of [
    () => truncating.searchClause('toothpaste', [{ slot: 0, value: ['Colgate'] }]),
    () => truncating.facetTally('toothpaste', 0),
    () => truncating.searchRange('toothpaste', 0, 100, 200),
    () => truncating.rangeTally('toothpaste', 0, EDGE),
  ]) {
    try {
      await call();
    } catch (err) {
      if (/refused/.test(String(err))) refusal++;
    }
  }
  check(refusal === 4, `a truncated range response is refused by all four filtered calls (${refusal}/4), never answered`);
  truncating.close();
}

// ---- 2. COST ------------------------------------------------------------------------------------
const NAME = ['meta', 'schema', 'alias', 'dict', 'posting_offset', 'posting', 'doc_len', 'prior', 'first_term', 'deleted', 'expansion', 'facet_label', 'facet_id', 'numeric_field', 'numeric_value', 'position_at', 'position', 'doc_key'];
const HEAD_BYTE = 8 + 18 * 16;
// Must mirror `is_resident` in crates/index-wasm: per query = posting; never = positions + doc_key.
const layout = (bytes) => {
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const len = NAME.map((_, i) => Number(dv.getBigUint64(8 + i * 16 + 8, true)));
  const pick = (...name) => name.reduce((t, n) => t + len[NAME.indexOf(n)], 0);
  const perQuery = pick('posting');
  const never = pick('position_at', 'position', 'doc_key');
  return {
    file: bytes.length,
    resident: len.reduce((t, x) => t + x, 0) - perQuery - never,
    filter: pick('facet_label', 'facet_id', 'numeric_field', 'numeric_value'),
    perQuery,
    docKey: pick('doc_key'),
  };
};
const pct = (n, of) => `${((100 * n) / Math.max(1, of)).toFixed(1)}%`;
const row = (tag, l) =>
  console.log(
    `  ${tag.padEnd(18)} ${String(l.file).padStart(10)} ${`${l.resident} ${pct(l.resident, l.file)}`.padStart(17)} ${String(l.filter).padStart(13)} ${`${l.perQuery} ${pct(l.perQuery, l.file)}`.padStart(17)} ${`${l.docKey} ${pct(l.docKey, l.file)}`.padStart(17)}`,
  );
console.log('\n  --- where every byte lives: resident at open / posting per query / doc_key never ---');
console.log(`  ${'index'.padEnd(18)} ${'file'.padStart(10)} ${'resident (open)'.padStart(17)} ${'facet+numeric'.padStart(13)} ${'posting (query)'.padStart(17)} ${'doc_key (never)'.padStart(17)}`);
const synth = layout(file);
row('synthetic 2K', synth);
const fixture = join(here, 'profstopick.idx');
if (existsSync(fixture)) row('profstopick.idx', layout(new Uint8Array(await readFile(fixture))));
else console.log('  profstopick.idx    (not present -- run emit-artifact and copy it into js/)');
console.log(`  filtered queries: ${(tally.byte - queryByte).toLocaleString()} B read for ${compared} calls (posting plans only)`);
check(synth.filter > 0, 'the synthetic index carries facet and numeric bytes');
check(openByte === HEAD_BYTE + synth.resident, `the open reads the head + resident sections exactly (${openByte} B), facet and numeric included`);

// ---- 4. SCALE: any other index, e.g. a keyed 400K-product catalogue (read-only) -------------------
// INDEX_IDX=<file.idx> INDEX_QUERY=<one query per line> node js/range-facet-check.mjs
if (process.env.INDEX_IDX) {
  const bigFile = new Uint8Array(await readFile(process.env.INDEX_IDX));
  const bigQuery = process.env.INDEX_QUERY
    ? (await readFile(process.env.INDEX_QUERY, 'utf8')).split(/\r?\n/).filter(Boolean)
    : ['a'];
  const big = layout(bigFile);
  row('INDEX_IDX', big);

  const bigTally = { byte: 0, read: 0 };
  const bigRanged = await RangeIndex.open(wasm, async (span) => {
    bigTally.read++;
    const total = span.reduce((t, [, l]) => t + l, 0);
    const out = new Uint8Array(total);
    let at = 0;
    for (const [o, l] of span) {
      out.set(bigFile.subarray(o, o + l), at);
      at += l;
    }
    bigTally.byte += total;
    return out;
  });
  const bigOpen = bigTally.byte;
  check(bigOpen === HEAD_BYTE + big.resident, `large open reads head + resident exactly: ${bigOpen.toLocaleString()} B = ${pct(bigOpen, big.file)} of ${big.file.toLocaleString()} B (doc_key ${big.docKey.toLocaleString()} B never read)`);

  const whole = await SearchIndex.open(wasm, bigFile);
  // A second raw instance for the full-open tally reference on the large file.
  const { instance: bi } = await WebAssembly.instantiate(wasm, {});
  const be = bi.exports;
  const bp = u32(be.idx_alloc(bigFile.length));
  new Uint8Array(be.memory.buffer, bp, bigFile.length).set(bigFile);
  const bigFull = u32(be.idx_open(bp, bigFile.length));
  be.idx_free(bp, bigFile.length);
  const bigFacetSlot = u32(be.idx_facet_slot_count(bigFull)) > 0;

  let bigExact = 0;
  let bigCompared = 0;
  const queryStart = bigTally.byte;
  for (const q of bigQuery) {
    bigCompared++;
    const want = whole.search(q, { k: 10 }).map(({ doc, score, typoBucket }) => ({ doc, score, typoBucket }));
    const got = (await bigRanged.search(q, { k: 10 })).map(({ doc, score, typoBucket }) => ({ doc, score, typoBucket }));
    if (JSON.stringify(got) === JSON.stringify(want)) bigExact++;
    else console.log(`    large mismatch (search): ${q}`);
    if (bigFacetSlot) {
      bigCompared++;
      const qb = enc.encode(q);
      const qp = u32(be.idx_alloc(qb.length || 1));
      new Uint8Array(be.memory.buffer, qp, qb.length).set(qb);
      const n = u32(be.idx_facet_tally(bigFull, qp, qb.length, 0));
      be.idx_free(qp, qb.length || 1);
      const wantTally = n === ERR ? 'ERR' : decodeFacetTally(be, u32(be.idx_result_ptr(bigFull)), n);
      const gotTally = await bigRanged.facetTally(q, 0);
      if (JSON.stringify(gotTally) === JSON.stringify(wantTally)) bigExact++;
      else console.log(`    large mismatch (facetTally): ${q}`);
    }
  }
  const perQuery = bigTally.byte - queryStart;
  check(bigExact === bigCompared, `large index: ${bigExact}/${bigCompared} answers (search${bigFacetSlot ? ' + facet tally' : ''}) identical to a full open`);
  const oldOpen = HEAD_BYTE + big.resident + big.docKey;
  console.log(`  large index, ${bigQuery.length} queries: open ${bigOpen.toLocaleString()} B (${pct(bigOpen, big.file)}; was ${oldOpen.toLocaleString()} B / ${pct(oldOpen, big.file)} with doc_key resident) + queries ${perQuery.toLocaleString()} B (${pct(perQuery, big.file)}, uncached) = ${pct(bigOpen + perQuery, big.file)} of the file`);
  be.idx_close(bigFull);
  bigRanged.close();
}

e.idx_close(full);
ranged.close();
console.log(`\nOVERALL: ${failed === 0 ? 'PASS' : `FAIL (${failed})`}`);
process.exit(failed === 0 ? 0 : 1);
