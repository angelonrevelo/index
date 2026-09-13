// The range tier over REAL HTTP, answered identically to a whole-file open.
//
// `p72` proved the range tier through an OPFS reader in a browser. The tier's actual promise is
// cold storage — a CDN, an object store — and that path had no reader and no test. This serves the
// committed `profstopick.idx` from a local HTTP server that honours `Range` (and counts every byte
// it sends), then checks three things:
//
//   1. EXACT   every query answers the same docs, scores and typo buckets as `SearchIndex.open`.
//   2. COST    bytes and requests for one pass, naive (one request per span, no cache) vs
//              coalesced+cached — the two knobs `range-http.mjs` adds.
//   3. REPEAT  a second pass of the same queries through the cached reader costs zero requests.
//
//   node js/range-http-check.mjs

import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { SearchIndex } from './index.mjs';
import { RangeIndex } from './opfs.mjs';
import { httpRangeReader } from './range-http.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const wasm = await readFile(join(here, 'index_wasm.wasm'));
const file = await readFile(join(here, 'profstopick.idx'));
const label = JSON.parse(await readFile(join(here, 'profstopick.label.json'), 'utf8'));

let failed = 0;
const check = (ok, msg) => {
  console.log(`  ${ok ? 'PASS' : 'FAIL'}  ${msg}`);
  if (!ok) failed++;
};

// ---- A server that speaks single-range `Range`, and tallies what it sent ----------------------
const sent = { byte: 0, request: 0 };
const server = createServer((req, res) => {
  sent.request++;
  const m = /^bytes=(\d+)-(\d+)?$/.exec(req.headers.range ?? '');
  if (!m) {
    sent.byte += file.length;
    res.writeHead(200, { 'content-length': file.length });
    return res.end(file);
  }
  const start = Number(m[1]);
  const end = Math.min(file.length - 1, m[2] === undefined ? file.length - 1 : Number(m[2]));
  const body = file.subarray(start, end + 1);
  sent.byte += body.length;
  res.writeHead(206, {
    'content-length': body.length,
    'content-range': `bytes ${start}-${end}/${file.length}`,
    'accept-ranges': 'bytes',
  });
  res.end(body);
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const url = `http://127.0.0.1:${server.address().port}/profstopick.idx`;

// ---- Queries drawn from the corpus itself: surnames, given names, typos, typeahead -------------
const query = [];
for (let i = 0; i < label.length && query.length < 60; i += Math.max(1, Math.floor(label.length / 60))) {
  const [surname, given = ''] = label[i].toLowerCase().split(',');
  const first = given.trim().split(/\s+/)[0] ?? '';
  const pick = query.length % 4;
  if (pick === 0) query.push({ q: surname, prefix: false });
  else if (pick === 1 && first) query.push({ q: `${first} ${surname}`, prefix: false });
  else if (pick === 2 && surname.length > 4) query.push({ q: surname.slice(0, 2) + surname.slice(3), prefix: false });
  else query.push({ q: surname.slice(0, Math.max(2, surname.length - 2)), prefix: true });
}

const whole = await SearchIndex.open(wasm, file, { label });
const want = query.map(({ q, prefix }) => whole.search(q, { k: 10, prefix }));

async function pass(reader, ranged, tag) {
  let exact = 0;
  let refused = 0;
  const before = { ...sent };
  for (let i = 0; i < query.length; i++) {
    let got;
    try {
      got = await ranged.search(query[i].q, { k: 10, prefix: query[i].prefix });
    } catch (err) {
      if (/refused/.test(String(err))) {
        refused++;
        continue;
      }
      throw err;
    }
    const same =
      got.length === want[i].length &&
      got.every((h, j) => h.doc === want[i][j].doc && h.score === want[i][j].score && h.typoBucket === want[i][j].typoBucket);
    if (same) exact++;
    else console.log(`    mismatch on ${JSON.stringify(query[i])}`);
  }
  return { tag, exact, refused, byte: sent.byte - before.byte, request: sent.request - before.request };
}

console.log('range-http :: the range tier over real HTTP Range requests');
console.log(`  file ${file.length.toLocaleString()} B, ${whole.docCount.toLocaleString()} docs, ${query.length} queries`);

const naive = httpRangeReader(url, { gap: 0, cache: false });
const naiveIx = await RangeIndex.open(wasm, naive.read, { label });
const openNaive = { ...sent };
const a = await pass(naive, naiveIx, 'naive');

sent.byte = 0;
sent.request = 0;
const smart = httpRangeReader(url, { gap: 4096, cache: true });
const smartIx = await RangeIndex.open(wasm, smart.read, { label });
const openSmart = { ...sent };
const b = await pass(smart, smartIx, 'coalesced+cached');
const c = await pass(smart, smartIx, 'repeat pass');

const answered = query.length - a.refused;
check(a.exact === answered, `naive reader: ${a.exact}/${answered} answered queries identical to a whole-file open (${a.refused} refused)`);
check(b.exact === query.length - b.refused, `coalesced+cached reader: ${b.exact}/${query.length - b.refused} identical (${b.refused} refused)`);
check(c.request === 0, `repeat pass through the cache: ${c.request} requests, ${c.byte} bytes`);

const pct = (n) => ((100 * n) / file.length).toFixed(1);
console.log('\n  --- cost of one pass (open + all queries) ---');
console.log(`  ${'reader'.padEnd(20)} ${'requests'.padStart(9)} ${'bytes'.padStart(10)} ${'% file'.padStart(7)}`);
for (const [r, open] of [[a, openNaive], [b, openSmart]]) {
  console.log(
    `  ${r.tag.padEnd(20)} ${String(r.request + open.request).padStart(9)} ${String(r.byte + open.byte).padStart(10)} ${pct(r.byte + open.byte).padStart(7)}`,
  );
}
console.log(`  ${'repeat pass'.padEnd(20)} ${String(c.request).padStart(9)} ${String(c.byte).padStart(10)} ${pct(c.byte).padStart(7)}`);
check(b.request + openSmart.request < a.request + openNaive.request, 'coalescing issues fewer requests than one-per-span');

server.close();
console.log(`\nOVERALL: ${failed === 0 ? 'PASS' : `FAIL (${failed})`}`);
process.exit(failed === 0 ? 0 : 1);
