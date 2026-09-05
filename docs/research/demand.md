# Demand — what eleven real apps on this machine actually need from `index`

> Evidence gathered 2026-09-05 by direct source inspection of `C:\Users\maran\Code\*`.
> Every number here is quoted from a file in one of those repos, not estimated.
> This file is the **demand side** of the project. `ROADMAP.md` may only contain rows that
> a workload in this file justifies.

## Why this file exists

`index` was scoped in June 2026 from the literature down: SOSD, PGM, learned indexes, FM-index.
It never asked what the apps on this machine were actually struggling with. This is that ask,
done properly. The result **contradicts the original thesis** — see [Finding 0](#finding-0).

---

## The roster

Eleven repos carry real retrieval load. Four more were checked and carry none.

| Repo | Corpus | Retrieval today | Measured pain |
|---|---|---|---|
| **presyo** | 2.08 M raw observations → 260 K canonical product, 296 K alias, 388 K FDA spine, 1.9 M latest-price rows, 15 GB prod Postgres | 7-lane `UNION ALL` over `pg_trgm` (`ILIKE`, `<%` word-similarity) + inline `ts_rank`, then a LATERAL price join | 17 s → 400 ms trigram query; 3–10 s home feed; 19,830-brand seq scan per resolution; 2.55 M-edge match run |
| **profstopick** | 11,949 entries (4,038 professor + 7,911 course), one static JSON shard per school | Hand-written 8-tier prefix matcher with backtracking token assignment, in the browser | **109 of 267 real searches returned nothing**; index eats **95.6 % of the 5 MB localStorage quota**; 13.43 ms first keystroke |
| **sisia-app** | 15.8 K sections, 3.7 K courses, 1,959 instructors, 13.7 K papers, 4.5 K news, chunk substrates | `ts_rank_cd` sparse arm + pgvector dense arm + **RRF k=60** + Vertex cross-encoder; SQLite `LIKE` for the catalog | AND→OR recall collapse; `CHEM 10%` bleeding into `CHEM 107`; dense arm is a network RTT on every query |
| **onegrid** | adopter's data; targets **10 M rows** | In-JS `includes/startsWith`, O(rows × leaf_filter_count); ES adapter is the only one with real IR | 1 M-row filter budget 1,500 ms; 10 M distinct ≈ 500 ms, self-labelled interactive-blocking |
| **maphy** | 41,966 barangays, 95,200 POIs, 7.9 M CAF holdings, 50,412 schools | **SQLite FTS5 × 3 channels** (unicode61 BM25 + trigram + category alias) fused by **RRF k=60**; 10-strategy join ladder with muni-scoped Levenshtein ≤ 2 | 1.2 MB / 42 K-row index lazy-loaded; dense channel stubbed empty and honestly labelled so |
| **life** | **634,624 records** | Postgres partial index on `occurred_at`, with a `life doctor index` EXPLAIN gate that fails on `Seq Scan` | **322.7 ms seq scan vs 2.9 ms index scan = 111×** |
| **blead** | **22 M+ barangay-grain vote rows** | SQLite B-tree, offline PSGC rollup | 1.5 GB → 114 MB gzip (13×) just to ship the aggregate |
| **polkadoc** | ~10 K messages, 190 threads | **Pure linear scan** over a typed store, faceted AND filters, no index file | None — and it **explicitly rejected Tantivy**: 0.10 s whole-vault, "measured need is absent" |
| **tunnelmind** | 5 crawled DB schemas | pgvector + NL→SQL, `fuzzy-enum.ts` = Levenshtein + a hand-built Tagalog/English alias table | none on speed |
| **advo** | ~thousands of claims | generated `tsvector` + GIN + `ts_rank`, 3-tier tsquery fallback | none — "a vector index is a dependency this box does not need yet" |
| **hobbycat** | seed-scale SQLite | `title LIKE '%t%' OR description LIKE '%t%' OR summary LIKE '%t%'`, unranked | search is the stated product thesis and is three `LIKE`s |

No retrieval content: `core` (Postgres job-queue ETL), `probe` (two-file recon dump), `browse`
(a `/search` route *contract*, no implementation), `bisbis` (corpus spec unbuilt).

---

## Finding 0 — the learned-index core solves a problem none of these apps has

This is the uncomfortable one and it goes first.

`index` today is a PLA/PGM learned index over **sorted, unique `u64` keys**, plus an FM-index and
adaptive cracking. It is correct, benchmarked, and beats `std::BTreeMap` on space and p50/p99 at
10 M keys. **Not one of the eleven repos above has a workload shaped like that.** Their hot paths are:

- text → ranked document list (presyo, profstopick, sisia, maphy, hobbycat, polkadoc)
- predicate over a column → row set (onegrid, life, blead)
- name → canonical entity (presyo, profstopick, maphy, tunnelmind)

A faster `u64 → position` map appears in none of them. The one place it could plug in — `life`'s
`occurred_at` filter — is already solved by a Postgres partial index at 2.9 ms.

The external evidence agrees. The most honest recent statement of the field, from the MountDB paper
(arXiv 2605.23815, May 2026): *"adoption in production systems remains limited, partly because
learned indexes that support concurrency and persistence as effectively as, e.g., the B+-Tree, do
not yet exist."* Where PGM does get used in that paper it is a **fence pointer** predicting an SST
*block*, not a retrieval algorithm. Nobody credible is using PGM/ALEX as the thing that answers a
search query.

**This does not make the existing work waste.** It makes it a *component*, not the *thesis*:

| Built | Original role | Actual role |
|---|---|---|
| `PlaIndex` / `PgmIndex` | the product | skip-pointer / fence layer over doc-id postings and sorted term-offset arrays |
| `FmIndex` + `WaveletTree` + `BitRank` | compressed substring search | the succinct-structure toolkit the browser-resident index needs; `BitRank` is directly reusable |
| `CrackerColumn` | adaptive database cracking | adaptive filter column for onegrid's O(rows × leaves) predicate scan |
| SOSD harness, `rdtsc` timer, median-of-5 p99 | measure the learned index | **keep verbatim** — the measurement discipline is the most transferable asset in the repo |

The roadmap has to be re-cut around what the apps have, and the ±ε invariant test stays green the
whole time.

## Finding 1 — the house independently converged on RRF k=60 three times

`presyo/apps/api/src/routes/productV2.ts:101` · `sisia-app/apps/api/src/utils/driveHybridSearch.ts`
(`RRF_K=60`, `ARM_LIMIT=50`, `FUSED_LIMIT=25`) · `maphy/scripts/search/query.ts` (k=60, hand-set
arm weights).

Three unconnected codebases, three authors' worth of separate reasoning, same fusion constant.
Reciprocal Rank Fusion at k=60 over independently-ranked arms is not a research question here —
it is the house default, arrived at empirically. **It should be the engine's built-in fusion, not
a configuration option someone has to discover.**

## Finding 2 — normalization is hand-rolled four separate times, and it is the real product

| Repo | File | Size | What it does |
|---|---|---|---|
| presyo | `packages/identifier/src/normalize.ts` | **1,560 lines** | OpenRefine fingerprint, `brandKey`, PH grocery abbreviation table (~40), unit standardization (~35), `canonicalSize` (1.5 L ≡ 1500 ml at 1e-3 relative tolerance), a `pgTrgmTrigrams` that is **byte-compatible with Postgres `similarity()`** |
| profstopick | `src/lib/search-match.ts` + `professor-name.ts` | 308 + 542 | `foldDiacritic` declared "THE SINGLE FOLD IN THIS REPO"; a *second, different* fold (`canonicalName`, `?`-sentinel) for dedup, because mojibake twins must merge while search must not |
| maphy | `apps/web/.../place-index.ts` + `scripts/lib/psgc-join.ts` | — | NFD folding precomputed at load "so per-keystroke search doesn't re-normalize 42 k strings"; 10-strategy join ladder |
| tunnelmind | `src/normalizer/fuzzy-enum.ts` | — | Levenshtein + Tagalog alias table (`single: [solo, unmarried, walang asawa, hindi kasal]`) |

Four implementations of the same primitive, each carrying scar tissue the others lack. profstopick
learned that folding differently from your slug function produces professors nobody can reach.
presyo learned that stripping the size token instead of canonicalizing it costs **−4.6 pp recall**.
maphy learned to precompute the fold. None of them knows what the others learned.

**A shared, tested, PH-aware normalization layer is worth more to these apps than any index
structure.** It is also the piece no off-the-shelf engine ships — Tantivy, Meilisearch and Typesense
all assume you bring your own analyzer, and none of them has heard of `wmkt → wet market`.

## Finding 3 — typo tolerance is the largest unmet need, and no repo has it

- **profstopick has none.** Measured in production 2026-08-17: **109 of 267 searches returned
  nothing, 60 of those name-shaped**, every one cross-checked to a professor already in the corpus.
  A transposed letter finds nothing.
- **presyo delegates it entirely to `pg_trgm`** at the stock `word_similarity_threshold` of 0.6,
  never overridden. Its own frozen benchmark carries seven typo queries (`coka cola`, `colgaye`,
  `nescaffe`, `safeguar`) and seven joined-no-space queries (`cocacola`, `bearbrand`, `luckyme`).
- **sisia delegates it to the dense arm**, which is a Gemini network call on every query, wrapped
  in a try/catch fallback.
- **maphy is the only one with real edit distance**, and it is muni-scoped, capped at ≤ 2, and used
  for an offline join — not for user queries.

Meanwhile the production engines all converged on the same shape: **≤ 2 edits hard-capped, length
gates at ~4 and ~8, first character protected, typo count as a ranking bucket rather than a filter**
(Typesense `min_len_1typo=4`/`min_len_2typo=7`; Meilisearch 1–4 → 0, 5–8 → 1, 9+ → 2, first-char
typo counts double; Algolia `minWordSizefor1Typo=4`/`minWordSizefor2Typos=8`; Lucene `AUTO:3,6`).
And they all fire it **lazily** — Typesense's `typo_tokens_threshold` only expands when exact
matching underdelivers.

`fst` (BurntSushi) already does the hard part: 2.7 bytes/key at 119 K keys, mmap'd so peak RAM is
flat, ED-2 automaton intersection over a **16 M-key / 157 MB** index in 94 ms unanchored. At
profstopick's 11,949 entries, anchored, this is microseconds. The `roadmap-rejected.md` entry
"build our own string/term-dictionary fuzzy index" was **correct to reject building it** — and
wrong to conclude the problem was therefore out of scope. Depending on `fst` and shipping the
*integration* is the move.

One non-negotiable, learned from Algolia's `allowTyposOnNumericTokens`: **numeric tokens must never
be fuzzy-matched.** `300g` matching `800g` is a correctness bug in a price-comparison app that will
present as a relevance bug.

## Finding 4 — a consumer already exists with a ratified C ABI

`onegrid/packages/wasm/` is not a wish. It is a committed design with:

- Six kernels named and specified — `sortIndex`, `filterMask`, `groupKey`, `aggregate`, `bitmapOp`,
  `topK` — "the six functions that dominate `@onegrid/data` on a large grid", typed-array in/out.
- A hand-written raw-pointer C ABI, **deliberately no wasm-bindgen** (`abi.ts`), versioned
  `ACCEL_ABI_VERSION = 1`, operator code tables that must match `crate/src/lib.rs`.
- **JS owns the heap** (`WasmHeap`, bump allocator).
- A real crate: `onegrid-accel`, `crate-type = ["cdylib","rlib"]`, **zero dependencies**,
  `opt-level=3 / lto="fat" / panic="abort" / strip`.
- A capability probe for SIMD/threads (`detect.ts`).
- And the governing rule: *"The JavaScript implementation is the SPECIFICATION… the accelerated
  implementation is only allowed to exist as long as it can be shown identical to it — see
  `assertBackendEquivalent`."*

No `.wasm` is committed. **The socket is cut and nothing is plugged into it.** This is the single
cheapest path from `index` to a real consumer, and it comes with a pre-written equivalence oracle.

The two measured hot spots with no accelerated counterpart today are exactly the ones a search
engine owns: the `contains`/`startsWith` path in `data/src/filter.ts:125-140`, and the
`enumerateDistinct` hash map at ~500 ms for 10 M.

## Finding 5 — three committed benchmarks already exist and can be stolen as acceptance tests

An engine claiming to be better needs a definition of better that predates it. Three exist:

1. **presyo** — `tests/fixture/product_search_contract.json`: a frozen **60-query** workload across
   10 categories (exact, brand_size, joined, typo, prefix, filipino_category, accent_punctuation,
   barcode, no_result, adversarial), pinned to a dated catalog snapshot. Run by
   `scripts/benchmark-product-search.ts`, reporting `hit_at_1`, `hit_at_5`, `mrr_at_10` and
   `latency_ms {p50,p95,p99}`, **failing if `accepted < 57`**. It even documents a self-flattery
   bug it already fixed (metrics denominated only on non-negative-control queries).
2. **profstopick** — `test/search-name-order.test.mjs`, `hero-search.test.mjs`,
   `search-status.test.mjs`, plus the 109/267 zero-result measurement as the recall target and the
   13.43 ms → 0.43 ms keystroke measurement as the latency baseline.
3. **onegrid** — `apps/benchmarks/src/perf-*.spec.ts`: 1 M-row filter < 1,500 ms, sort < 2,000 ms,
   memory sort < 3,000 ms, plus `assertBackendEquivalent` as a correctness oracle.

`index` does not need to invent an acceptance suite. It needs to **not lose** on three that already
exist and were written by someone who wasn't trying to make this project look good.

## Finding 6 — the counter-evidence, recorded on purpose

`polkadoc` considered an inverted index and **rejected it in writing**: whole-vault search is
0.10 s on a pure linear scan over ~10 K messages, so "measured need is absent", and Tantivy
publishes no binary-size or index-overhead figure. `booted` deferred fuzzy search over its process
census because "today it is 5 rows". `advo` declined a vector index because "this box does not need
it yet". `profstopick`'s roadmap opens with:

> *"Postgres is not the problem and never was — every page query measures 2–253 ms and every index
> is used… Anyone who benchmarks query time will conclude, correctly, that these changes were
> unnecessary — because they will be measuring the wrong thing."*

Three of eleven repos looked directly at this project's product and decided they didn't want it.
That is the honest size of the market, and it sets a rule: **`index` must win on a measurement the
app already takes, or it must not be adopted.** The value is concentrated in presyo (scale),
profstopick (browser byte budget + recall), onegrid (kernel ABI) and sisia (sparse arm) — four
repos, not eleven.

---

## What the demand actually asks for, in priority order

1. **A normalization + tokenization layer that knows about Philippine grocery and school text** —
   diacritic fold, unit canonicalization as a *hard partition* not a fuzzy field, abbreviation and
   Filipino↔English alias tables. Four repos already wrote a worse version of this.
2. **A typo-tolerant term dictionary** — `fst` + Levenshtein automaton, ≤ 2 edits, length-gated,
   first-char protected, numerics exempt, fired lazily. Closes profstopick's 109/267 directly.
3. **BM25 over an inverted index**, with the sparse arm shaped to drop into sisia's existing RRF
   fusion (`query in → (id, rank)[] out`) and to replace presyo's 7-lane UNION.
4. **RRF k=60 as a built-in**, not a config option.
5. **A compact binary index format that loads once in a browser** — profstopick's 2.5 MB JSON at
   95.6 % of quota is the constraint, and its `SearchRow` positional tuple is already one step from
   a binary layout.
6. **Predicate + top-k + distinct kernels behind onegrid's existing C ABI.**
7. **Entity resolution as a first-class offline mode** — presyo's Fellegi–Sunter blocking, 17
   interpretable weights, and its honest correction (in-sample 100 % precision → **real
   out-of-sample ≈ 82 %**) is the most sophisticated matching code in the house and is currently
   `MATCH_FS_TIER=shadow` in production.

Everything on this list is text-shaped, string-shaped, or bitmap-shaped. None of it is
`u64 → position`.
