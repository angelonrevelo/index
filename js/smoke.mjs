// CI smoke test: the WASM artifact must load, BUILD an index, and answer a query with no corpus
// on disk and no Rust build step.
//
// `js/demo.mjs` is the richer proof but needs the profstopick checkout, which CI does not have.
// This one is self-contained. It used to assert only the ABI surface and the refusal behaviour,
// on the belief that a JavaScript host could not build an index — but `idx_build_new`/`_add`/
// `_finish` are exported precisely so it can, and that path had no host-side test at all. It is
// also the only code that exercises the NUL-separated wire format, so a change to that separator
// could break every host while all 103 Rust tests stayed green.

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
  'idx_build_new', 'idx_build_add', 'idx_build_finish', 'idx_build_free', 'idx_serialize',
  'idx_build_facet', 'idx_search_facet', 'idx_search_facet_all', 'idx_facet_tally',
  'idx_facet_count', 'idx_facet_slot_count',
  'idx_build_numeric', 'idx_search_range', 'idx_range_tally', 'idx_numeric_slot_count',
  'idx_search_sorted',
  'idx_searcher_new', 'idx_searcher_push', 'idx_searcher_close', 'idx_searcher_search',
  'idx_searcher_search_facet_all', 'idx_searcher_facet_tally', 'idx_searcher_delete',
  'idx_searcher_doc_count', 'idx_searcher_segment_count', 'idx_searcher_live_count',
  'idx_searcher_needs_compaction', 'idx_searcher_search_range', 'idx_searcher_search_sorted',
  'idx_searcher_result_ptr', 'idx_searcher_result_len', 'idx_highlight',
  'idx_search_clause', 'idx_search_page',
  'idx_searcher_search_clause', 'idx_searcher_search_page',
  'idx_build_position', 'idx_search_phrase', 'idx_searcher_search_phrase',
  'idx_build_key', 'idx_doc_of_key', 'idx_key_of', 'idx_keyed_count',
  'idx_searcher_doc_of_key', 'idx_searcher_delete_key', 'idx_searcher_key_of',
  'idx_searcher_has_key',
];
check(want.every((k) => k in e), `all ${want.length} ABI symbols are exported`);
check(e.idx_abi_version() === 14, 'ABI version is 14');

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

// ---- Build an index from JavaScript, then search it -------------------------------------------
//
// This is the claim the project makes to a host application: no upload, no service, no Rust in the
// build. Bring your own rows, get exact ranked search over them in-process.
const enc = new TextEncoder();

// Every helper re-reads `e.memory.buffer`. `idx_alloc` can call `memory.grow()`, which DETACHES
// every existing ArrayBuffer view — a view cached across an allocation reads zeroes silently.
const put = (s) => {
  const b = enc.encode(s);
  const ptr = u32(e.idx_alloc(b.length));
  new Uint8Array(e.memory.buffer, ptr, b.length).set(b);
  return [ptr, b.length];
};

const SEP = '\0'; // the wire separator, matching `idx_build_new` / `idx_build_add`
const [sp, sl] = put(['name:3:0.4', 'brand:1:0.6'].join(SEP));
const builder = u32(e.idx_build_new(sp, sl));
e.idx_free(sp, sl);
check(builder !== 0, 'idx_build_new accepts a NUL-separated field spec');

const row = [
  ['Colgate Total Toothpaste 150g', 'Colgate'],
  ['Safeguard Pure White Soap 135g', 'Safeguard'],
  ['Lucky Me Pancit Canton 60g', 'Lucky Me'],
  ['Bear Brand Fortified Milk 320g', 'Bear Brand'],
];
let added = 0;
for (const r of row) {
  const [dp, dl] = put(r.join(SEP));
  const ord = u32(e.idx_build_add(builder, dp, dl));
  e.idx_free(dp, dl);
  if (ord !== 0xffffffff) added++;
}
check(added === row.length, `all ${row.length} documents were accepted`);

const h = u32(e.idx_build_finish(builder));
check(h !== 0, 'idx_build_finish returns a searchable handle');
check(u32(e.idx_doc_count(h)) === row.length, 'doc_count matches what was added');
check(u32(e.idx_term_count(h)) > 0, 'the term dictionary is non-empty');

// `k` hits of 12 bytes: doc u32, score f32, typo_bucket u32, all little-endian.
const HIT_BYTE = 12;
const topDoc = (n) => {
  const rp = u32(e.idx_result_ptr(h));
  const dv = new DataView(e.memory.buffer, rp, n * HIT_BYTE);
  return dv.getUint32(0, true);
};

const search = (q) => {
  const [qp, ql] = put(q);
  const n = u32(e.idx_search(h, qp, ql, 5, 0));
  e.idx_free(qp, ql);
  return n;
};

let n = search('Colgate Toothpaste');
check(n > 0 && n !== 0xffffffff, 'an exact query returns hits');
check(topDoc(n) === 0, 'the Colgate document ranks first on an exact query');

// The error-correction claim, from JavaScript, against an index JavaScript just built.
n = search('Colgte Toothpaest');
check(n > 0 && n !== 0xffffffff, 'a query with two typos still returns hits');
check(topDoc(n) === 0, 'the Colgate document still ranks first with two typos');

n = search('Pancit Canton');
check(n > 0 && topDoc(n) === 2, 'a different query ranks its own document first');

// ---- Faceting: the interaction shopping search actually is ------------------------------------
// Rebuild with a facet on the brand field, then filter and count -- the same three calls a
// storefront makes on every keystroke.
const fspec = ['name:3:0.4', 'brand:1:0.6'].join(SEP);
const [fsp, fsl] = put(fspec);
const fb = u32(e.idx_build_new(fsp, fsl));
e.idx_free(fsp, fsl);
check(u32(e.idx_build_facet(fb, 1)) === 1, 'idx_build_facet accepts a valid field');
check(u32(e.idx_build_facet(fb, 99)) === 0, 'an out-of-range facet field is refused, not trapped');
for (const r of [...row, ['Colgate Fresh Gel 100g', 'Colgate']]) {
  const [dp, dl] = put(r.join(SEP));
  e.idx_build_add(fb, dp, dl);
  e.idx_free(dp, dl);
}
const fh = u32(e.idx_build_finish(fb));
check(fh !== 0, 'the faceted index builds');
check(u32(e.idx_facet_slot_count(fh)) === 1, 'one facet slot');
check(u32(e.idx_facet_count(fh, 0)) === 4, 'four distinct brands stored');

const facetSearch = (q, v) => {
  const [qp, ql] = put(q);
  const [vp, vl] = put(v);
  const n = u32(e.idx_search_facet(fh, qp, ql, 10, 0, vp, vl));
  e.idx_free(qp, ql);
  e.idx_free(vp, vl);
  return n;
};
check(facetSearch('Colgate', 'Colgate') === 2, 'filtered search returns both Colgate rows');
check(facetSearch('Colgate', 'Nestle') === 0, 'an unknown facet value returns nothing, not everything');

// Tally wire format: u32 len, UTF-8 bytes, u32 count -- decoded exactly as a host must.
const [tqp, tql] = put('Colgate');
const tn = u32(e.idx_facet_tally(fh, tqp, tql, 0));
e.idx_free(tqp, tql);
check(tn >= 1, 'idx_facet_tally returns entries');
{
  const raw = new Uint8Array(e.memory.buffer, u32(e.idx_result_ptr(fh)), u32(e.idx_result_len(fh)));
  const dv = new DataView(raw.buffer, raw.byteOffset, raw.byteLength);
  let at = 0;
  let first = null;
  for (let i = 0; i < tn; i++) {
    const len = dv.getUint32(at, true); at += 4;
    const label = new TextDecoder().decode(raw.subarray(at, at + len)); at += len;
    const count = dv.getUint32(at, true); at += 4;
    if (i === 0) first = [label, count];
  }
  check(at === raw.byteLength, 'the tally encoding is self-delimiting');
  check(first?.[0] === 'Colgate' && first?.[1] === 2, 'Colgate tallies 2, ranked first');
}
// Conjunctive filtering, with the spec parsed inside the module.
{
  const [cqp, cql] = put('Colgate');
  const [csp, csl] = put('0=Colgate');
  check(u32(e.idx_search_facet_all(fh, cqp, cql, 10, csp, csl)) === 2, 'conjunctive spec filters');
  e.idx_free(csp, csl);
  const [bsp, bsl] = put('notanumber=Colgate');
  check(
    u32(e.idx_search_facet_all(fh, cqp, cql, 10, bsp, bsl)) === 0xffffffff,
    'a malformed filter spec errors rather than matching everything',
  );
  e.idx_free(bsp, bsl);
  e.idx_free(cqp, cql);
}
// ---- Numeric ranges: the other half of a filter bar -----------------------------------------
{
  const nspec = ['name:3:0.4', 'brand:1:0.6', 'size:0:0.6'].join(SEP);
  const [nsp, nsl] = put(nspec);
  const nb = u32(e.idx_build_new(nsp, nsl));
  e.idx_free(nsp, nsl);
  check(u32(e.idx_build_numeric(nb, 2)) === 1, 'idx_build_numeric accepts a valid field');
  for (const r of [
    ['Colgate Toothpaste Small', 'Colgate', '50'],
    ['Colgate Toothpaste Medium', 'Colgate', '100'],
    ['Colgate Toothpaste Large', 'Colgate', '150'],
    ['Colgate Toothpaste Family', 'Colgate', '300'],
    ['Colgate Toothpaste Sample', 'Colgate', 'not a number'],
  ]) {
    const [dp, dl] = put(r.join(SEP));
    e.idx_build_add(nb, dp, dl);
    e.idx_free(dp, dl);
  }
  const nh = u32(e.idx_build_finish(nb));
  check(u32(e.idx_numeric_slot_count(nh)) === 1, 'one numeric column');

  const [nqp, nql] = put('Colgate Toothpaste');
  check(u32(e.idx_search_range(nh, nqp, nql, 10, 0, 100, 300)) === 2,
    'a half-open range excludes the value sitting exactly on the upper bound');
  check(u32(e.idx_search_range(nh, nqp, nql, 10, 0, -1e308, 1e308)) === 4,
    'the unparseable row is in no range at all');

  // Histogram edges are passed as f64 through linear memory.
  const edge = [0, 100, 200, 400];
  const ep = u32(e.idx_alloc(edge.length * 8));
  new Float64Array(e.memory.buffer, ep, edge.length).set(edge);
  const bn = u32(e.idx_range_tally(nh, nqp, nql, 0, ep, edge.length));
  check(bn === 3, 'the histogram returns one count per bucket');
  const count = Array.from(new Uint32Array(e.memory.buffer, u32(e.idx_result_ptr(nh)), bn));
  check(String(count) === '1,2,1', 'buckets partition without double counting the boundary');
  check(count.reduce((a, b) => a + b, 0) === 4, 'the absent value is counted nowhere');
  // Sort by value, which is a different ordering from relevance.
  const sortDoc = (asc) => {
    const n = u32(e.idx_search_sorted(nh, nqp, nql, 10, 0, asc ? 1 : 0));
    const dv = new DataView(e.memory.buffer, u32(e.idx_result_ptr(nh)), n * HIT_BYTE);
    return Array.from({ length: n }, (_, i) => dv.getUint32(i * HIT_BYTE, true));
  };
  const asc = sortDoc(true);
  check(String(asc) === '0,1,2,3', 'ascending sort is by value: 50,100,150,300');
  check(String(sortDoc(false)) === String([...asc].reverse()), 'descending is the reverse');
  check(asc.length === 4, 'the row with no numeric value is excluded from the order');
  e.idx_free(ep, edge.length * 8);
  e.idx_free(nqp, nql);
  e.idx_close(nh);
}
e.idx_close(fh);

// Serialize, reopen, and confirm the reopened index answers identically — this is the
// "build once, cache the bytes, ship them to the browser" path.
const slen = u32(e.idx_serialize(h));
check(slen > 0, 'idx_serialize returns a byte length');
// Copy OUT before allocating again: the next `idx_alloc` may grow memory and detach this view.
const blob = new Uint8Array(e.memory.buffer, u32(e.idx_result_ptr(h)), slen).slice();
const bp = u32(e.idx_alloc(blob.length));
new Uint8Array(e.memory.buffer, bp, blob.length).set(blob);
const h2 = u32(e.idx_open(bp, blob.length));
e.idx_free(bp, blob.length);
check(h2 !== 0, 'the serialized bytes reopen as an index');
check(u32(e.idx_doc_count(h2)) === row.length, 'the reopened index has the same doc count');

const [qp2, ql2] = put('Colgte Toothpaest');
const n2 = u32(e.idx_search(h2, qp2, ql2, 5, 0));
e.idx_free(qp2, ql2);
const rp2 = u32(e.idx_result_ptr(h2));
const top2 = new DataView(e.memory.buffer, rp2, n2 * HIT_BYTE).getUint32(0, true);
check(n2 === n && top2 === 0, 'the reopened index answers the typo query identically');

e.idx_close(h2);
e.idx_close(h);

// ---- Filter bar: OR within a clause, AND across, NOT, and paging ------------------------------
{
  const [csp, csl] = put(['name:3:0.4', 'brand:1:0.6'].join(SEP));
  const cb = u32(e.idx_build_new(csp, csl));
  e.idx_free(csp, csl);
  e.idx_build_facet(cb, 1);
  for (const r of [
    ['Colgate Toothpaste Large', 'Colgate'],
    ['Oral B Toothpaste Mini', 'Oral B'],
    ['Aquafresh Toothpaste Twin', 'Aquafresh'],
  ]) {
    const [dp, dl] = put(r.join(SEP));
    e.idx_build_add(cb, dp, dl);
    e.idx_free(dp, dl);
  }
  const ch = u32(e.idx_build_finish(cb));
  const clause = (spec) => {
    const [qp, ql] = put('Toothpaste');
    const [sp2, sl2] = put(spec);
    const n = u32(e.idx_search_clause(ch, qp, ql, 10, 0, sp2, sl2));
    e.idx_free(qp, ql); e.idx_free(sp2, sl2);
    return n;
  };
  check(clause('0=Colgate|Oral B') === 2, 'OR within a clause widens');
  check(clause('0=Colgate') === 1, 'a single value narrows');
  check(clause('0!=Colgate') === 2, 'NOT excludes');
  check(clause('0=Nestle') === 0, 'an all-unknown include matches nothing');
  check(clause('0!=Nestle') === 3, 'an all-unknown exclude excludes nothing');
  check(clause('notanumber=Colgate') === 0xffffffff, 'a malformed spec errors, never matches all');

  const page = (off, k) => {
    const [qp, ql] = put('Toothpaste');
    const n = u32(e.idx_search_page(ch, qp, ql, off, k));
    e.idx_free(qp, ql);
    if (n === 0 || n === 0xffffffff) return [];
    const dv = new DataView(e.memory.buffer, u32(e.idx_result_ptr(ch)), n * HIT_BYTE);
    return Array.from({ length: n }, (_, i) => dv.getUint32(i * HIT_BYTE, true));
  };
  const all = page(0, 3);
  check(String([...page(0, 1), ...page(1, 1), ...page(2, 1)]) === String(all),
    'three pages of 1 equal one request for 3');
  check(page(99, 5).length === 0, 'past the end is empty, not wrapped');
  e.idx_close(ch);
}

// ---- Highlighting: which words matched --------------------------------------------------------
{
  // Its OWN index: `fh` above has already been closed, and calling into a freed handle is a
  // use-after-free inside the module. The first draft of this block did exactly that and hung
  // node with no output -- the module never returned rather than trapping cleanly.
  const [hsp, hsl] = put('name:3:0.4');
  const hb = u32(e.idx_build_new(hsp, hsl));
  e.idx_free(hsp, hsl);
  const [hdp, hdl] = put('Colgate Total Toothpaste 150g');
  e.idx_build_add(hb, hdp, hdl);
  e.idx_free(hdp, hdl);
  const hh = u32(e.idx_build_finish(hb));

  const body = 'Colgate Total Toothpaste 150g';
  const hl = (q) => {
    const [qp, ql] = put(q);
    const [tp, tl] = put(body);
    const n = u32(e.idx_highlight(hh, qp, ql, tp, tl));
    e.idx_free(qp, ql);
    e.idx_free(tp, tl);
    if (n === 0 || n === 0xffffffff) return [];
    const dv = new DataView(e.memory.buffer, u32(e.idx_result_ptr(hh)), n * 8);
    const enc = new TextEncoder().encode(body);
    return Array.from({ length: n }, (_, i) =>
      new TextDecoder().decode(enc.subarray(dv.getUint32(i * 8, true), dv.getUint32(i * 8 + 4, true))));
  };
  check(String(hl('Colgate')) === 'Colgate', 'highlight marks the matching word');
  check(String(hl('Colgte')) === 'Colgate', 'a typo highlights the corrected word');
  check(String(hl('Colgate Toothpaste')) === 'Colgate,Toothpaste', 'multiple spans, in order');
  check(hl('Safeguard').length === 0, 'a term absent from this text marks nothing');
  e.idx_close(hh);
}

// ---- Incremental updates: add rows without rebuilding ----------------------------------------
// This is the path an application actually lives on. A full build is O(corpus); adding today's
// hundred new products should not be. New rows go into a small segment that is cheap to build, and
// the searcher queries every segment and merges.
{
  const mk = (rows) => {
    const [sp, sl] = put(['name:3:0.4', 'brand:1:0.6'].join(SEP));
    const b = u32(e.idx_build_new(sp, sl));
    e.idx_free(sp, sl);
    e.idx_build_facet(b, 1);
    for (const r of rows) {
      const [dp, dl] = put(r.join(SEP));
      e.idx_build_add(b, dp, dl);
      e.idx_free(dp, dl);
    }
    return u32(e.idx_build_finish(b));
  };

  // Segment 0 sees Colgate first; segment 1 sees Aquafresh first, so the two intern the same
  // label at different ids. A tally merged on the id would add unrelated brands together.
  const s0 = mk([
    ['Colgate Total Toothpaste 150g', 'Colgate'],
    ['Aquafresh Mini Toothpaste 50g', 'Aquafresh'],
  ]);
  const sr = u32(e.idx_searcher_new(s0));           // CONSUMES s0
  check(sr !== 0, 'idx_searcher_new turns an index into a live collection');
  check(u32(e.idx_searcher_doc_count(sr)) === 2, 'the searcher starts with the base segment');

  const s1 = mk([
    ['Aquafresh Twin Toothpaste 100g', 'Aquafresh'],
    ['Colgate Travel Toothpaste 25g', 'Colgate'],
  ]);
  check(u32(e.idx_searcher_push(sr, s1)) === 1, 'a new segment is appended without rebuilding');
  check(u32(e.idx_searcher_segment_count(sr)) === 2, 'two segments');
  check(u32(e.idx_searcher_doc_count(sr)) === 4, 'ordinals continue across the append');

  const sSearch = (q, k = 10) => {
    const [qp, ql] = put(q);
    const n = u32(e.idx_searcher_search(sr, qp, ql, k, 0));
    e.idx_free(qp, ql);
    if (n === 0 || n === 0xffffffff) return [];
    const dv = new DataView(e.memory.buffer, u32(e.idx_searcher_result_ptr(sr)), n * HIT_BYTE);
    return Array.from({ length: n }, (_, i) => dv.getUint32(i * HIT_BYTE, true));
  };

  check(sSearch('Toothpaste').length === 4, 'a query reaches every segment');
  // The typo lands on a document that lives in the SECOND segment, at a global ordinal.
  const typo = sSearch('Colgte Travel');
  check(typo[0] === 3, 'a typo query finds a row added after the build, at its global ordinal');

  // Faceting across segments, merged by value rather than by interned id.
  const [tqp, tql] = put('Toothpaste');
  const tn = u32(e.idx_searcher_facet_tally(sr, tqp, tql, 0));
  const raw = new Uint8Array(e.memory.buffer, u32(e.idx_searcher_result_ptr(sr)), u32(e.idx_searcher_result_len(sr)));
  const dv = new DataView(raw.buffer, raw.byteOffset, raw.byteLength);
  let at = 0;
  const tally = [];
  for (let i = 0; i < tn; i++) {
    const len = dv.getUint32(at, true); at += 4;
    const label = new TextDecoder().decode(raw.subarray(at, at + len)); at += len;
    tally.push([label, dv.getUint32(at, true)]); at += 4;
  }
  e.idx_free(tqp, tql);
  check(at === raw.byteLength, 'the cross-segment tally encoding is self-delimiting');
  check(
    tally.length === 2 && tally.every(([l, c]) => c === 2) && tally.map((t) => t[0]).sort().join() === 'Aquafresh,Colgate',
    'the tally merges by value across segments (2 and 2), not by interned id',
  );

  const [fqp, fql] = put('Toothpaste');
  const [fsp, fsl] = put('0=Colgate');
  check(
    u32(e.idx_searcher_search_facet_all(sr, fqp, fql, 10, fsp, fsl)) === 2,
    'a conjunctive facet filter spans segments',
  );
  e.idx_free(fsp, fsl);

  // The filter bar and paging ACROSS segments. Both halves of the unknown-value rule are asserted
  // here as well as in Rust, because the host is where a reversed rule would actually be seen: a
  // filter that returns the whole corpus looks exactly like a working search.
  const sClause = (spec, k = 10, off = 0) => {
    const [qp, ql] = put('Toothpaste');
    const [sp, sl] = put(spec);
    const n = u32(e.idx_searcher_search_clause(sr, qp, ql, k, off, sp, sl));
    e.idx_free(qp, ql);
    e.idx_free(sp, sl);
    if (n === 0 || n === 0xffffffff) return n === 0 ? [] : null;
    const d = new DataView(e.memory.buffer, u32(e.idx_searcher_result_ptr(sr)), n * HIT_BYTE);
    return Array.from({ length: n }, (_, i) => d.getUint32(i * HIT_BYTE, true));
  };

  check(sClause('0=Colgate|Aquafresh').length === 4, 'OR within a clause reaches both segments');
  check(sClause('0=Colgate').length === 2, 'AND across clauses narrows to one brand');
  check(sClause('0!=Colgate').length === 2, 'an exclude drops only the excluded brand');
  check(sClause('0=Nestle').length === 0, 'an all-unknown INCLUDE matches nothing');
  check(sClause('0!=Nestle').length === 4, 'an all-unknown EXCLUDE removes nothing');
  check(sClause('notanumber=Colgate') === null, 'a malformed spec errors rather than matching all');

  const sPage = (off, k) => {
    const [qp, ql] = put('Toothpaste');
    const n = u32(e.idx_searcher_search_page(sr, qp, ql, off, k));
    e.idx_free(qp, ql);
    if (n === 0 || n === 0xffffffff) return [];
    const d = new DataView(e.memory.buffer, u32(e.idx_searcher_result_ptr(sr)), n * HIT_BYTE);
    return Array.from({ length: n }, (_, i) => d.getUint32(i * HIT_BYTE, true));
  };

  // Three pages must equal one request for the whole, with nothing on two pages. Across segments
  // this is the claim that matters: the page boundary is global, so a segment's 2nd-best hit can
  // be the page's 1st, and a naive per-segment slice would drop or duplicate it.
  const whole = sPage(0, 4);
  const byPage = [...sPage(0, 2), ...sPage(2, 2)];
  check(byPage.join() === whole.join(), 'pages partition the merged ranking, no gaps or repeats');
  check(new Set(byPage).size === byPage.length, 'no document appears on two pages');
  check(sPage(99, 2).length === 0, 'past the end is empty rather than wrapped');

  check(u32(e.idx_searcher_delete(sr, 3)) === 1, 'a document added after the build can be deleted');
  check(u32(e.idx_searcher_live_count(sr)) === 3, 'live_count drops after a delete');
  check(sSearch('Toothpaste').length === 3, 'the deleted row leaves the results');
  check(u32(e.idx_searcher_needs_compaction(sr)) <= 1, 'needs_compaction answers without trapping');
  e.idx_free(fqp, fql);
  e.idx_searcher_close(sr);
}

// ---- Phrase queries -----------------------------------------------------------------------
//
// Built so a bag-of-words search CANNOT tell the cases apart: every row contains both words, and
// only adjacency separates them. Anything less and the test would pass on a broken verifier.
{
  const spec = 'name:3:0.4\0brand:1:0.6';
  const row = [
    'Vanilla Ice Cream Tub\0Selecta',
    'Ice Crushed Cream Soda\0Selecta',
    'Cream Ice Bar\0Selecta',
    'Chocolate Ice\0Cream Co', // adjacent only ACROSS the field boundary
  ];
  const mk = (positions) => {
    const [sp, sl] = put(spec);
    const b = u32(e.idx_build_new(sp, sl));
    e.idx_free(sp, sl);
    if (positions) check(u32(e.idx_build_position(b)) === 1, 'positions are accepted before any row');
    for (const r of row) {
      const [dp, dl] = put(r);
      e.idx_build_add(b, dp, dl);
      e.idx_free(dp, dl);
    }
    return u32(e.idx_build_finish(b));
  };

  const withPos = mk(true);
  const noPos = mk(false);
  const docOf = (h, n) => {
    if (n === 0 || n === 0xffffffff) return [];
    const d = new DataView(e.memory.buffer, u32(e.idx_result_ptr(h)), n * HIT_BYTE);
    return Array.from({ length: n }, (_, i) => d.getUint32(i * HIT_BYTE, true));
  };
  const phrase = (h, q) => {
    const [qp, ql] = put(q);
    const n = u32(e.idx_search_phrase(h, qp, ql, 0, 10));
    e.idx_free(qp, ql);
    return docOf(h, n);
  };
  const plain = (h, q) => {
    const [qp, ql] = put(q);
    const n = u32(e.idx_search(h, qp, ql, 10, 0));
    e.idx_free(qp, ql);
    return docOf(h, n);
  };

  check(plain(withPos, 'Ice Cream').length === 4, 'every row matches as a bag of words');
  check(phrase(withPos, 'Ice Cream').join() === '0', 'only the adjacent, in-order, same-field row matches');
  check(phrase(withPos, 'Cream Ice').includes(2), 'the reversed phrase matches the reversed row');
  check(!phrase(withPos, 'Ice Cream').includes(3), 'a phrase must not span a field boundary');
  check(phrase(withPos, 'Ice Sorbet').length === 0, 'an unknown word matches nothing, not everything');
  check(
    phrase(withPos, 'Vanilla').join() === plain(withPos, 'Vanilla').join(),
    'a one-word phrase agrees with a one-word search',
  );

  // The refusal that matters: no positions must mean NO phrase results, not a term search whose
  // rows would be indistinguishable from phrase rows.
  check(phrase(noPos, 'Ice Cream').length === 0, 'no positions, no phrase results');
  check(plain(noPos, 'Ice Cream').length === 4, 'its ordinary search is unaffected');

  e.idx_close(withPos);
  e.idx_close(noPos);
}

// ---- Application keys: saying WHICH ROW changed ------------------------------------------------
//
// A host receiving "row sku-1 changed" has the key and nothing else. Before p53 it could search but
// never say which row it meant: `idx_searcher_delete` takes a dense ordinal assigned at insertion
// that no database row carries. This is that gap, closed and gated.
{
  const spec = 'sku:0:0.6\0name:3:0.4';
  const mk = (row) => {
    const [sp, sl] = put(spec);
    const b = u32(e.idx_build_new(sp, sl));
    e.idx_free(sp, sl);
    check(u32(e.idx_build_key(b, 0)) === 1, 'the key field is accepted before any row');
    for (const r of row) {
      const [dp, dl] = put(r);
      e.idx_build_add(b, dp, dl);
      e.idx_free(dp, dl);
    }
    return u32(e.idx_build_finish(b));
  };
  const withKey = (k, f) => {
    const [p, n] = put(k);
    const out = f(p, n);
    e.idx_free(p, n);
    return out;
  };

  const base = mk(['sku-1\0Colgate Total Toothpaste 150g', 'sku-2\0Aquafresh Mini 50g']);
  check(u32(e.idx_keyed_count(base)) === 2, 'both rows carry a key');
  check(withKey('sku-1', (p, n) => u32(e.idx_doc_of_key(base, p, n))) === 0, 'a key resolves to its row');
  check(withKey('sku-404', (p, n) => u32(e.idx_doc_of_key(base, p, n))) === 0xffffffff,
    'an unknown key is a sentinel, not a trap');
  const kn = u32(e.idx_key_of(base, 1));
  check(new TextDecoder().decode(new Uint8Array(e.memory.buffer, u32(e.idx_result_ptr(base)), kn)) === 'sku-2',
    'a key reads back out of the result buffer');

  const sk = u32(e.idx_searcher_new(base));           // CONSUMES base
  check(u32(e.idx_searcher_has_key(sk)) === 1, 'the collection is fully keyed');

  // An UPDATE is an append plus a retirement, because segments are immutable.
  e.idx_searcher_push(sk, mk(['sku-1\0Colgate Total Charcoal 200g', 'sku-3\0Oral B Pro 120g']));
  check(u32(e.idx_searcher_doc_count(sk)) === 4, 'ordinals only ever grow');
  check(u32(e.idx_searcher_live_count(sk)) === 3, 'the superseded row was retired, not accumulated');
  check(withKey('sku-1', (p, n) => u32(e.idx_searcher_doc_of_key(sk, p, n))) === 2,
    'the LIVE sku-1 is the new version');
  const gn = u32(e.idx_searcher_key_of(sk, 2));
  check(new TextDecoder().decode(new Uint8Array(e.memory.buffer, u32(e.idx_searcher_result_ptr(sk)), gn)) === 'sku-1',
    'a global ordinal reads back its key');

  // A DELETE, expressed the only way an application can.
  check(withKey('sku-2', (p, n) => u32(e.idx_searcher_delete_key(sk, p, n))) === 1, 'delete by key retires the row');
  check(withKey('sku-2', (p, n) => u32(e.idx_searcher_delete_key(sk, p, n))) === 0,
    'replaying a delete is a no-op, not an error');
  check(withKey('sku-404', (p, n) => u32(e.idx_searcher_delete_key(sk, p, n))) === 0,
    'deleting a key that never existed reports 0');
  check(u32(e.idx_searcher_live_count(sk)) === 2, 'two live rows remain');
  e.idx_searcher_close(sk);
}

// ---- The shipped host module ------------------------------------------------------------------
//
// Everything above instantiates the module DIRECTLY, which is why `js/index.mjs` -- the host an
// application actually imports -- was able to sit at ABI_VERSION 2 while the module reached 9,
// throwing `ABI mismatch` on every call, with the gate fully green. A host that is not gated is
// not shipped; it is only published. This section imports it the way a consumer would.
{
  const { SearchIndex } = await import('./index.mjs');
  const field = [
    { name: 'name', boost: 3, b: 0.4 },
    { name: 'brand', boost: 1, b: 0.6 },
  ];
  const row = [
    ['Colgate Total Toothpaste 150g', 'Colgate'],
    ['Colgate Fresh Gel Toothpaste 100g', 'Colgate'],
    ['Oral B Toothpaste Pro 120g', 'Oral B'],
    ['Aquafresh Mini Toothpaste 50g', 'Aquafresh'],
  ];
  const ix = await SearchIndex.build(wasm, field, row, { label: row.map((r) => r[0]) });

  check(ix.docCount === 4, 'the shipped host builds an index at the current ABI');
  const hit = ix.search('Colgate Toothpaste');
  check(hit.length > 0 && hit[0].label !== null, 'the shipped host decodes hits and labels');

  // The facet field must be declared at build time, so this host's build() cannot filter on one
  // yet -- it exposes no facet hook. Paging needs none, and is checked here in full.
  const whole = ix.searchPage('Toothpaste', 0, 4).map((h) => h.doc);
  const paged = [
    ...ix.searchPage('Toothpaste', 0, 2).map((h) => h.doc),
    ...ix.searchPage('Toothpaste', 2, 2).map((h) => h.doc),
  ];
  check(paged.join() === whole.join(), 'the shipped host pages without gaps or repeats');
  check(ix.searchPage('Toothpaste', 99, 2).length === 0, 'the shipped host returns nothing past the end');

  // An index with no facet slot: a clause naming any value is unsatisfiable, and an exclude naming
  // the same value removes nothing. Both directions, through the host's own encoder.
  check(ix.searchClause('Toothpaste', [{ slot: 0, value: ['Colgate'] }]).length === 0,
    'the shipped host: an unsatisfiable include matches nothing');
  check(ix.searchClause('Toothpaste', [{ slot: 0, value: ['Colgate'], exclude: true }]).length === 4,
    'the shipped host: an all-unknown exclude removes nothing');

  // The shipped host's own phrase path, including the build-time opt-in it has to pass through.
  const px = await SearchIndex.build(wasm, field, row, { position: true });
  check(px.searchPhrase('Colgate Total').length === 1, 'the shipped host answers a phrase');
  check(px.searchPhrase('Total Colgate').length === 0, 'and refuses the words in the wrong order');
  check(ix.searchPhrase('Colgate Total').length === 0, 'an index built without positions refuses it');
  px.close();

  let refused = false;
  try {
    ix.searchClause('Toothpaste', [{ slot: 0, value: ['Colgate|Oral B'] }]);
  } catch {
    refused = true;
  }
  check(refused, "the shipped host refuses a facet value containing '|' rather than splitting it");

  ix.close();
}

console.log(failed === 0 ? 'OVERALL: PASS' : `OVERALL: FAIL (${failed})`);
process.exit(failed === 0 ? 0 : 1);
