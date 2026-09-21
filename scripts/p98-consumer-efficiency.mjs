// Measure p98 planner on the two consumer corpora: profstopick names vs the
// shipped prefix matcher, and presyo gold listings with Filipino alias queries.
//
// Usage (from repo root, after wasm is built):
//   node scripts/p98-consumer-efficiency.mjs

import { readFileSync, existsSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { performance } from 'node:perf_hooks';
import { SearchIndex } from '../js/index.mjs';
import { fold as foldMatcher, matchFolded as matchMatcher } from '../js/profstopick-match.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const wasm = readFileSync(join(root, 'target/wasm32-unknown-unknown/release/index_wasm.wasm'));

const ntime = (fn, n = 200) => {
  fn();
  const t = [];
  for (let i = 0; i < n; i++) {
    const a = performance.now();
    fn();
    t.push((performance.now() - a) * 1e3);
  }
  t.sort((x, y) => x - y);
  return { p50: t[Math.floor(t.length * 0.5)], p99: t[Math.floor(t.length * 0.99)] };
};

const prof = async () => {
  const hit = [
    { label: 'Pastrana, Allan J.', sublabel: 'English' },
    { label: 'Rigor, Adrian', sublabel: 'Information Systems' },
    { label: 'Magpantay, Andre', sublabel: 'Mathematics' },
    { label: 'Medina, Erron', sublabel: 'Political Science' },
    { label: 'Rojas, Nina Rosario L.', sublabel: 'Chemistry' },
    { label: 'Tan, Romeo', sublabel: 'Management' },
    { label: 'Cruz, Jacob', sublabel: 'Philosophy' },
    { label: 'Jacob, Precious', sublabel: 'Biology' },
    { label: 'Peña-Reyes, Ser Percival K.', sublabel: 'Economics' },
    { label: 'MATH 30.23', sublabel: 'Mathematical Analysis I' },
  ];
  const query = [
    'allan pastrana',
    'Adrian Rigor',
    'pena',
    'math 30.23',
    'jacob c',
    'Rigr Adrian',
    'colgte',
  ];
  const folded = foldMatcher(hit.map((h) => h.label));
  const ix = await SearchIndex.build(
    wasm,
    [
      { name: 'label', boost: 3, b: 0.4 },
      { name: 'sublabel', boost: 1, b: 0.6 },
    ],
    hit.map((h) => [h.label, h.sublabel]),
    { position: true, key: 0 },
  );
  const rank1 = (list, q, want) => {
    const top = list(q);
    return top.length > 0 && String(top[0]).toLowerCase().includes(want);
  };
  let matcherHit = 0;
  let engineHit = 0;
  for (const q of query) {
    const m = matchMatcher(folded, q, 5).map((r) => r.label ?? r);
    const e = ix.search(q, { k: 5, prefix: true }).map((h) => hit[h.doc]?.label ?? '');
    const want = q.split(/\s+/)[0].toLowerCase().slice(0, 4);
    if (rank1(() => m, q, want) || m.some((x) => String(x).toLowerCase().includes(want))) matcherHit++;
    if (e.some((x) => String(x).toLowerCase().includes(want))) engineHit++;
  }
  const mt = ntime(() => {
    for (const q of query) matchMatcher(folded, q, 10);
  });
  const et = ntime(() => {
    for (const q of query) ix.search(q, { k: 10, prefix: true });
  });
  return {
    corpus: 'profstopick contract names',
    query_count: query.length,
    matcher_hit: matcherHit,
    engine_hit: engineHit,
    matcher_p50_us: mt.p50,
    matcher_p99_us: mt.p99,
    engine_p50_us: et.p50,
    engine_p99_us: et.p99,
  };
};

const presyo = async () => {
  const goldPath = join(root, '..', 'presyo', 'tests', 'fixtures', 'recall-gold-cases.json');
  if (!existsSync(goldPath)) {
    return { skipped: 'presyo gold fixture missing' };
  }
  const gold = JSON.parse(readFileSync(goldPath, 'utf8'));
  const doc = [];
  const query = [];
  for (const c of gold.clusters.slice(0, 200)) {
    const listing = c.listings ?? [];
    if (listing.length < 2) continue;
    const q = listing[0].raw_name;
    for (const L of listing.slice(1)) {
      doc.push([String(c.gold_product_id), L.raw_name, c.gold_brand ?? '', `${L.raw_name} ${c.gold_brand ?? ''}`]);
    }
    query.push({ q, id: String(c.gold_product_id) });
  }
  const ix = await SearchIndex.build(
    wasm,
    [
      { name: 'id', boost: 0, b: 0.6 },
      { name: 'name', boost: 3, b: 0.4 },
      { name: 'brand', boost: 1, b: 0.6 },
      { name: 'text', boost: 1, b: 0.4 },
    ],
    doc,
    { position: true, key: 0 },
  );
  let hit1 = 0;
  let hit10 = 0;
  for (const { q, id } of query) {
    const got = ix.search(q, { k: 10 }).map((h) => doc[h.doc][0]);
    if (got[0] === id) hit1++;
    if (got.includes(id)) hit10++;
  }
  const filipino = [
    ['bigas', 'rice'],
    ['gatas', 'milk'],
    ['sabon', 'soap'],
    ['gamot sa ubo', 'cough'],
  ];
  const grocery = await SearchIndex.build(
    wasm,
    [{ name: 'name', boost: 1, b: 0.4 }],
    [
      ['Jasmine Rice 5kg'],
      ['Bear Brand Powdered Milk 300g'],
      ['Tide Laundry Soap 1000g'],
      ['Solmux Cough Medicine 120ml'],
      ['Bear Brand Powdered Milk 800g'],
    ],
    { position: true },
  );
  const filipino_hit = filipino.map(([q]) => ({
    q,
    rank1: grocery.search(q, { k: 1 })[0]?.doc ?? null,
  }));
  const size300 = grocery.search('300g', { k: 10 }).map((h) => h.doc);
  const t = ntime(() => {
    for (const { q } of query.slice(0, 15)) ix.search(q, { k: 10 });
  }, 50);
  return {
    corpus: 'presyo gold 200 clusters + Filipino alias fixture',
    gold_query: query.length,
    gold_hit1: hit1,
    gold_hit10: hit10,
    gold_hit1_pct: ((100 * hit1) / query.length).toFixed(1),
    gold_hit10_pct: ((100 * hit10) / query.length).toFixed(1),
    filipino_hit,
    size_300g_docs: size300,
    engine_p50_us: t.p50,
    engine_p99_us: t.p99,
  };
};

const out = {
  profstopick: await prof(),
  presyo: await presyo(),
};
console.log(JSON.stringify(out, null, 2));
