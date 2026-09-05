# index — ROADMAP

> **An embedded retrieval engine that gives any app Algolia-grade search over the data it already
> has — in its own process, on its own infrastructure, from one index format that serves a browser,
> a server, an edge worker and a database.**
>
> Edited honestly. Gaps are gaps, not opportunities. No row reaches this file without a falsifiable
> benchmark, and **no row reaches this file without a named consumer whose measured pain it closes.**

**Status (2026-09-05, end of session):** the engine exists, is measured against **all four**
consumer applications on their own contracts and their own data, and runs in Node and in a real
browser.

| App | Their harness | Result |
|---|---|---|
| **profstopick** | `search-name-order.test.mjs` (their file, one import line changed) | **9 / 9** |
| **profstopick** | whole suite | **1,922 / 1,958** — the 4 failures fail identically on untouched `main` |
| **onegrid** | `differential.property.test.ts` + whole `packages/wasm` suite | **294 / 294**, driven by a real compiled module |
| **presyo** | their own `searchProduct` on Postgres, **260,000 rows** | **100 % / 100 %** vs SQL **100 % / 99.4 %** clean |
| **sisia-app** | their catalog `LIKE`, on their own registrar data | out-of-order title words **91.2 % vs 0.0 %** |
| **browser** | headless Chromium 147 | **PASS**, 37 µs/query on the main thread |

Gate: 72 Rust tests, 0 clippy warnings, two WASM artifacts, and `smoke` / `demo` / `browser` /
`real-corpus` / `sisia-catalog` all passing. Adoption plans per app: [`docs/adoption.md`](docs/adoption.md).

**Two things this session could not settle, and neither is unblocked by more engineering here:**

1. **Nothing is deployed.** Everything lives on throwaway worktree branches; no `main` was modified
   and no PR opened, because four shipping repositories are not this project's to change
   unilaterally. `docs/adoption.md` reduces that to a single decision per app.
2. **Billion-query scale is not testable on this machine.** The largest honest corpus available is
   61,467 real rows; everything above is recombination, labelled as such. At 1 M documents the p99
   varies **±0.9 ms across identical runs**, so the 5 ms bar is below this harness's resolution.
   Real scale evidence requires production traffic, which requires (1).

**The blocking engineering item is now built.** `index-text::searcher` adds multi-segment search:
new documents go into a small segment that is cheap to build, queries run across all segments and
merge, and `needs_compaction()` says when the accumulated delta has grown enough to justify
rebuilding from the source of truth. That last point is the design fit — **the application's
database is the source of truth and the index is derived**, so compaction is a rebuild from rows the
app already has, not a merge of segments.

Its cost is measured rather than waved away: BM25 statistics are per-segment (as they are in Lucene,
whose IDF is per-shard), so a **broad** query matching most of the collection can reorder its
near-ties, while a **selective** query still identifies the same document. Both halves are asserted
in `searcher::tests::ranking_skew_is_bounded_for_a_small_delta`.

Before that, the repo was **re-baselined against demand** after a full research
sweep — eleven real apps on this machine surveyed by source inspection, six web-research lanes, one
cross-model X/practitioner sweep, and a crates.io maturity audit. Evidence lives in
[`docs/research/`](docs/research/). The gate is green (16 tests, `cargo test -p index-core`) and the
P0–P2 headline results still reproduce (`beat-btreemap`, 2026-09-05: PLA wins space/p50/p99 on all
four distributions at n=1M).

**The sweep produced one conclusion that reorders the whole project**, and it is not comfortable:

> **The learned-index core solves a problem none of the eleven consumer apps has.**
>
> `index` today maps a sorted `u64` key to a position. The apps' hot paths are *text → ranked
> documents*, *predicate → row set*, and *name → canonical entity*. A faster `u64 → position` map
> appears in none of them. The one place it could plug in — `life`'s `occurred_at` filter — is
> already solved by a Postgres partial index at 2.9 ms.
>
> The literature agrees, from the opposite direction. MountDB (arXiv 2605.23815, May 2026):
> *"adoption in production systems remains limited, partly because learned indexes that support
> concurrency and persistence as effectively as, e.g., the B+-Tree, do not yet exist."* Where PGM
> *is* used there, it is an **SST fence pointer predicting a block**, not a retrieval algorithm.
> Nobody credible uses PGM/ALEX to answer a search query.
>
> Full argument and evidence: [`docs/research/demand.md`](docs/research/demand.md) Finding 0 and
> [`docs/research/claim.md`](docs/research/claim.md) §1.

**This does not delete the existing work; it re-scopes it from *thesis* to *component*.** See
[Part I](#part-i--what-is-proven). What replaces it is in [Part II](#part-ii--the-spine), and it is
grounded in numbers the apps already measured against themselves.

Benchmark tiers are unchanged: **T1** runnable + deterministic · **T2** live/manual procedure ·
**T3** written acceptance test. Every T1 for a not-yet-built item lives in `bench/roadmap/`
(gate-excluded) and is **promoted into the live gate on build** — that promotion is the red→green
moment.

---

## The five demand facts every row below answers to

From [`docs/research/demand.md`](docs/research/demand.md). These are measurements the app owners
took *before* this project existed, which is what makes them usable as acceptance criteria.

1. **profstopick: 109 of 267 real searches returned nothing** (measured in production 2026-08-17),
   60 of them name-shaped and every one cross-checked to a professor already in the corpus. Its
   search index occupies **95.6 % of the 5 MB localStorage quota**. It has **no typo tolerance at
   all**.
2. **presyo: 2.08 M observations → 260 K canonical products**, served by a 7-lane `pg_trgm` UNION
   with an inline `ts_rank` rescore. Measured pain: a 17 s → 400 ms trigram query, a 3–10 s home
   feed, a 19,830-brand seq scan per resolution. It owns a **frozen 60-query benchmark** with
   `hit@1`, `MRR@10` and p50/p95/p99, failing under 57/60.
3. **onegrid has a ratified C ABI with nothing plugged into it** — six named kernels, versioned
   `ACCEL_ABI_VERSION = 1`, JS-owns-the-heap, zero-dependency crate, and an `assertBackendEquivalent`
   oracle. The socket is cut and empty.
4. **The house independently converged on RRF k=60 three times** (presyo, sisia, maphy) and
   **hand-rolled diacritic normalization four times** (presyo 1,560 lines, profstopick, maphy,
   tunnelmind), each carrying scar tissue the others lack.
5. **Three of eleven repos looked at this product and declined it** — polkadoc rejected Tantivy in
   writing (0.10 s whole-vault linear scan, "measured need is absent"), booted deferred fuzzy search
   ("today it is 5 rows"), advo declined a vector index. **That is the honest size of the market**,
   and it sets the governing rule below.

> **The governing rule: `index` must win on a measurement the consumer already takes, or it must not
> be adopted.** Value is concentrated in four repos — presyo (scale), profstopick (byte budget +
> recall), onegrid (kernel ABI), sisia (sparse arm) — not eleven.

---

## Part I — What is proven

Complete, reproducing, and staying. The **role** column is the change.

| Built | Status | Original role | **Actual role** |
|---|---|---|---|
| `PlaIndex` (optimal convex-hull PLA, exact `i128` geometry) | **DONE** — ±ε invariant proven red→green; beats `BTreeMap` on space/p50/p99 across 4 distributions at 1M and 10M (ε=16) | the product | **skip-pointer / fence layer** over doc-id postings and sorted term-offset arrays |
| `PgmIndex` (recursive) | **DONE, not the lever** — measures ≈ single-level PLA to 10M | the scaling story | kept for larger scale; **no further investment scheduled** |
| `FmIndex` + `WaveletTree` + `BitRank` | **DONE & succinct** — 5.9/9.7/14.6 bits/char at σ=4/26/256 | compressed substring search | the **succinct-structure toolkit** the browser-resident index needs; `BitRank` directly reusable |
| Fuzzy-over-FM viability decision | **DONE, DECIDED** — viable to k≤2 at σ=26, k≤1 at σ=256 | a feature | a **negative result that redirected the work** — and it was right (see P5) |
| `CrackerColumn` (stochastic) | **DONE, proven red→green** — naive *fails* sequential (2.48×), stochastic passes (0.01×) | adaptive core | **adaptive filter column** for onegrid's O(rows × leaves) predicate scan |
| SOSD harness, `rdtsc` clock, median-of-5 p99 | **DONE** | measure the learned index | **keep verbatim — the most transferable asset in the repo.** The literature's blind spot is p99; this harness never had it |

**What the old P3 and P4 became.** Old P3 (spatial/viewport) and old P4 (federated multi-modal) are
**deferred indefinitely, not cancelled**: no repo in the survey has a spatial-range or filtered-ANN
workload that its current stack fails at. maphy is the only spatial consumer and its 42 K-barangay
index is served in 1.2 MB gzipped. They return when a consumer measures a failure. The
dimensionality-decision experiment (`bench/roadmap/p3-sfc-dimensionality.md`) stays on disk because
it settles a question cheaply if the topic ever reopens.

---

## Part II — The spine

Ordered by *consumer pain closed per week of work*, not by architectural elegance. P4→P5→P7→P8 is
the critical path; it delivers profstopick's win with **no infrastructure at all**.

### P3 — Re-baseline (cheap, and it unblocks honest work)

| Item | What it closes | Effort | Benchmark | Status |
|---|---|---|---|---|
| Reframe README/CHANGELOG to the demand-led thesis | The repo currently advertises a thesis its own evidence contradicts; every downstream decision inherits the error | S | T3 → README's "Honest limits" section names the Finding 0 reframe explicitly | **DONE** (this file + `docs/research/`) |
| Close the `pgm-extra` build-vs-buy Triage row | An open decision blocking P0 closure for three months | S | T2 → crates.io maturity audit | **DONE — answer is neither.** `pgm-extra` has **134 downloads in 90 days**, `pgm_index` 80. Nothing to inherit, and per Finding 0 more PGM is not what any consumer needs. See [`build-or-buy.md`](docs/research/build-or-buy.md) |
| CI gate | Tests and benches run only locally; the "green gate" is a claim about one machine | S | T1 → `.github/workflows` running `cargo test -p index-core` + clippy, with `bench/roadmap/` exclusion intact | **not started** (open item since 2026-06-19) |

### P4 — Normalization and tokenization (the actual product) — **SHIPPED**

Four repos hand-rolled a worse version of this ([`demand.md`](docs/research/demand.md) Finding 2).
It is also the piece **no off-the-shelf engine ships** — Tantivy, Meilisearch and Typesense all
assume you bring your own analyzer, and none of them has heard of `wmkt → wet market`.

| Item | What it closes | Effort | Benchmark | Status |
|---|---|---|---|---|
| Unicode fold: NFKC → case fold → NFD-strip-marks, one canonical implementation | profstopick learned the hard way that **folding differently from your slug function produces professors nobody can reach** (`Peña-Reyes` unreachable by `pena`, for all 72 non-ASCII entries) | S | T1 → property test: fold is idempotent, and `fold(x) == fold(y)` iff the two are the same entity in a fixture drawn from all four repos' scar cases | spec |
| **Unit/size canonicalization as a hard partition** | presyo measured that *stripping* the size token instead of canonicalizing it costs **−4.6 pp recall**; and `300g` fuzzy-matching `800g` is a correctness bug in a price-comparison app | M | T1 → `1.5L ≡ 1500ml ≡ "1.5 liters"` at 1e-3 relative tolerance; **and a negative test that `300g` never matches `800g` at any edit distance** | spec |
| PH abbreviation + Filipino↔English alias tables | The research is unambiguous: **there is no production-grade Filipino stemmer** and Tagalog infixation/circumfixion/reduplication defeats Snowball's model. A few hundred curated rows beats anything statistical trainable on available corpora | M | T3 → presyo's `filipino_category` fixture rows (`bigas`, `gatas`, `gamot sa ubo`, `sabon panlaba`) resolve without an LLM in the hot path | spec |
| Adopt `charabia` for multilingual segmentation, layer PH rules on top | Writing a tokenizer is weeks; Meilisearch's is alive (258 K recent downloads, published 2026-08-13) | S | T2 → build-vs-buy audit re-run | spec |

**Dependency policy for this tier:** `index-core` stays dependency-free; the analyzer is a separate
crate. Audited candidates in [`build-or-buy.md`](docs/research/build-or-buy.md).

### P5 — Typo-tolerant term dictionary — **SHIPPED**

The largest unmet need in the survey, and the row with the strongest evidence behind it.

| Item | What it closes | Effort | Benchmark | Status |
|---|---|---|---|---|
| **Feasibility spike** | Whether the byte and latency budgets are reachable at all | S | T1 → [`bench/roadmap/p5-fuzzy-term-feasibility.md`](bench/roadmap/p5-fuzzy-term-feasibility.md) → `fuzzy-term` bin | **DONE — FEASIBLE.** See numbers below |
| Term dictionary + Levenshtein automaton on `tantivy-fst` + `levenshtein_automata` | profstopick's 109/267; presyo's `typo` and `joined` fixture categories | M | promote the spike into the live gate; both reds below must clear | spec |
| The **policy layer** — length gates, first-char protection, numeric-token exemption, lazy firing, typo-as-ranking-bucket | This is where every production engine's actual behaviour lives, and **no crate provides it** | M | T1 → parity table against Typesense/Meilisearch/Algolia documented defaults | spec |
| Double Metaphone over the **brand dictionary only** | Filipino orthography is phonemic: `kolgate`, `nesle`, `kokakola` are phonetic substitutions that edit distance ≥ 2 rejects. < 50 KB for a few thousand brand tokens | S | T3 → the three names above resolve; full-catalogue phonetics stays banned (too lossy) | spec |

**Measured, 2026-09-05** (`INDEX_BENCH_N=11949`, profstopick's actual corpus size):

| | name corpus | product corpus |
|---|---|---|
| distinct tokens | 12,029 | 20,071 |
| **term FST** | **96,547 B (8.03 B/key)** | **109,562 B (5.46 B/key)** |
| exact p50 / p99 | 210 / 760 ns | 200 / 650 ns |
| fuzzy p50 / p99 | 229 µs / **663 µs** | 63 µs / **597 µs** |
| **typo recall** | **135 → 1999 of 2000 (14.8×)** | **628 → 1981 of 2000 (3.2×)** |

> **A 94 KB term dictionary — 3.8 % of the 2.5 MB JSON shard profstopick ships today — takes typo
> recall from 6.8 % to 99.9 % at a p99 of 0.66 ms, which is 1.5 % of one keystroke frame.**

Holds to **861 K distinct terms** (5.85 B/key, fuzzy p99 1.65 ms), an order of magnitude past
presyo's ~296 K aliases. **Two honest reds are open and must not be silently relaxed:** exact p99
1,380 ns vs a 1,000 ns bar at 848 K terms, and 8.03 B/key vs an 8.0 bar on the 12 K name corpus.
**One hypothesis was refuted and is recorded**: prefix-anchoring is *not* a speed lever (−22.7 % to
+9.6 % effect, no consistent sign) — first-character protection is a precision rule, not a
performance one.

**This also corrects a rejected-ideas entry.** `docs/roadmap-rejected.md` rejected *building* a
string-fuzzy index — correctly. It then concluded typo tolerance was out of scope, which does not
follow. `tantivy-fst` (4.2 M recent downloads) and `levenshtein_automata` (4.4 M) are the exact
stack Meilisearch and Tantivy ship. **Depend, don't build; the integration is the product.**

### P6 — The scorer — the one place from-scratch is justified — **SHIPPED** (BM25F, block-max MaxScore, champion lists, RRF)

Everything else in this plan says *buy*. [`relevance.md`](docs/research/relevance.md) §1 is the
exception, and it is narrow and measurable:

- **Tantivy hardcodes `K1 = 1.2; B = 0.75`** as module constants with no public API (issue #2924,
  open).
- Lucene and Tantivy **quantize the field norm to one byte**, so on 3–5-token product titles
  distinct lengths collapse to the same norm and the length signal dies *before* `b` can act.
- BM25F and per-field `b` are mutually exclusive in Elasticsearch's `combined_fields`.

| Item | What it closes | Effort | Benchmark | Status |
|---|---|---|---|---|
| **BM25F with blended IDF and exact `u16` field lengths** | The above — and presyo's corpus is short product titles, exactly the failure case | M | T1 → score parity against Anserini defaults (k1=0.9, b=0.4) on a fixed corpus; **plus a test that two titles of different length get different norms**, which Lucene fails | spec |
| Postings codec: **PEF or Slicing**, never VByte | VByte is Pareto-dominated on both axes (8.81 bits/int at 1,042 Mint/s vs PEF 3.12 at 1,316) | M | T1 → bits/posting + decode Mint/s vs the CSUR table | spec |
| **Block-max MaxScore top-k — not BMW** | Ding & Suel: 8.1× over exhaustive OR, 23.7× after docID reassignment. **But BMW inverts on dense queries** (SPLADE: BMW 681 ms vs OR 553 ms vs MaxScore 220 ms). Lucene and PISA converged on MaxScore independently | L | T1 → documents fully scored, vs exhaustive baseline; must not regress on a dense-query workload | spec |
| Recursive graph bisection docID reordering | The cheapest multiplier found: ~3× on pruned queries and 27 % space, pure preprocessing, zero query-path risk | S | T1 → index bytes + pruned-query latency before/after | spec |
| **RRF k=60 as a built-in, not a config option** | The house converged on it three times independently. Literature: RRF 0.551 vs BM25 0.425 BEIR; a *tuned convex combination* reaches 0.576 | S | T1 → fusion of two ranked arms is exact and order-stable; α exposed, defaulting to RRF | spec |

### P7 — The index format: one file, four transports — **SHIPPED** (range-readable, `u64` offsets, section table)

**The irreversible decision in this project.** Get it wrong and WASM closes permanently.

| Item | What it closes | Effort | Benchmark | Status |
|---|---|---|---|---|
| Chunked, block-addressed format with **`u64` offsets** and an aligned zero-copy body | 32-bit `usize` on wasm32 was **the named blocker that killed Tantivy's browser PR**; it is unfixable after the format ships | L | T1 → round-trip + a test asserting offsets survive > 4 GB | spec |
| **`Directory`-style trait over mmap ∥ HTTP Range ∥ OPFS ∥ object store** | **wasi-libc's `mmap` is a fake** — it "just allocates memory with malloc and reads and writes data with pread and pwrite". An mmap-based index doesn't fail to port; it **silently slurps itself into linear memory.** Lance proves the alternative: exactly one `mmap` reference in the repo, all in test-data generation | L | T1 → identical results across all four backends on one fixture | spec |
| rkyv archive discipline — self-contained shards, root at buffer end, `AlignedVec<16>`, host never picks the address | [rkyv #575](https://github.com/rkyv/rkyv/issues/575) is exactly our scenario: rkyv bytes over wasm via `Uint8Array`, JS returning 2-byte alignment where 4 was needed. **Rust UB does not care that WASM tolerates it** | M | T1 → misaligned-buffer test must fail loudly, never silently | spec |

**Range reads first, mmap as a native optimization layered on.** Building mmap-first and
retrofitting is the documented failure mode. The CIDR'22 paper's own §6 endorses mmap for exactly
our case — *"working set fits in memory and the workload is read-only"* — which is what immutable
segments on one VPS are; that is why it is an optimization worth having, and why it must not be the
base assumption.

### P8 — Browser tier — **SHIPPED except persistence** (runs in Chromium 147 at 37 µs/query; OPFS caching unbuilt)

| Item | What it closes | Effort | Benchmark | Status |
|---|---|---|---|---|
| wasm32 + hand-written raw-pointer C ABI, **no wasm-bindgen**, SIMD128 required | onegrid already ratified this exact ABI shape. `memory.grow()` detaches every JS view — **even `grow(0)`** | M | T1 → `assertBackendEquivalent` against the JS reference | spec |
| **Sharded static index, fetch-only-what-you-touch** (Pagefind model: index chunks separate from result fragments) | profstopick's 95.6 %-of-quota problem — **solved by not shipping the index, not by storing it better.** Pagefind does 10,000 pages in < 300 kB | M | T1 → total bytes fetched for the 60-query fixture; **and offline-after-first-load must still hold** (profstopick's stated acceptance: "load /search, go offline, type gar, still resolves") | spec |
| OPFS worker pager for indexes > 5 MB | Repeat-visit cost | M | T2 → **Safari deletes all script-written storage after 7 days without interaction**, so this is a one-week cache; a rebuild path is mandatory, not optional | spec |

**Non-negotiable constraints inherited from profstopick:** zero network on keystroke; must work fully
offline from cache; **must fold identically to `slugify()` or links break**; must be importable under
Node type-stripping for its tests; must not regress the professor-before-course primary sort.

### P9 — Server tier — **MEASURED** (presyo head-to-head at 260 K; napi-rs binding unbuilt, WASM used instead)

| Item | What it closes | Effort | Benchmark | Status |
|---|---|---|---|---|
| `napi-rs` native module | presyo's `apps/api/src/search/product.ts` is **a single async function, one input shape, one output shape, already the sole entry for REST + v2 + MCP, and already contract-tested** — the cleanest drop-in target in the house | M | T1 → **presyo's own `bench:product-search`**: `hit@1`, `hit@5`, `MRR@10`, p50/p95/p99, failing under 57/60 | spec |
| Sparse-arm shape `query → (id, rank)[]` | sisia's `driveHybridSearch.ts` and `ateneoPageRetrieval.ts` are drop-in-shaped around exactly this contract, and keep their existing dense arm + RRF + reranker | S | T3 → sisia's `retrievalTelemetry` per-stage timing shows the sparse arm faster with equal-or-better recall | spec |

**No sidecar.** Not because the socket is slow (unix domain socket RTT is 7.7 µs, 1–3 % of a
sub-ms query) but because the codec is (`serde_json` 103.59 ms vs rkyv 5.6 ns on 6 MB), and because
presyo's box already runs Node, Nginx and a 15 GB Postgres — Typesense's 380 MB image at 2–3× resident
RAM is the worst possible tenant for it.

### P10 — onegrid kernels — **SHIPPED** (`index-accel`, 6.3 KB, 294/294 on their harness)

| Item | What it closes | Effort | Benchmark | Status |
|---|---|---|---|---|
| `filterMask` + `topK` + `groupKey` behind `ACCEL_ABI_VERSION = 1` | The two measured hot spots with no accelerated counterpart: the `contains`/`startsWith` path (`data/src/filter.ts:125-140`) and `enumerateDistinct` (~500 ms at 10 M) | M | T1 → onegrid's `perf-*.spec.ts` budgets (1 M filter < 1,500 ms, sort < 2,000 ms) **and** `assertBackendEquivalent` | spec |
| `CrackerColumn` as an adaptive filter column | onegrid's filter is O(rows × leaf_filter_count) and re-evaluated per interaction — the workload cracking was designed for | M | T1 → convergence over a repeated-filter session vs full scan | spec |

Onegrid supplies the equivalence oracle: *"the JavaScript implementation is the SPECIFICATION… the
accelerated implementation is only allowed to exist as long as it can be shown identical to it."*

### P11 — Hybrid retrieval

| Item | What it closes | Effort | Benchmark | Status |
|---|---|---|---|---|
| Static-embedding dense arm via `model2vec-rs` (`potion-multilingual-128M`, 101 languages incl. Filipino) | sisia's dense arm is **a Gemini network call on every query**. Eskildsen: *"it doesn't matter that the turbopuffer latency is 8 milliseconds when it takes 300 milliseconds to create a query vector."* Static embeddings encode in **microseconds** | M | T1 → nDCG@10 on presyo's fixture vs lexical-only; **and query-encode latency ≤ 100 µs** | spec |
| Convex-combination fusion with tunable α | RRF 0.551 → tuned linear 0.576 BEIR | S | T1 → both fusions on one judgment set; RRF stays default | spec |
| Optional CPU cross-encoder rerank, flagged, with a latency circuit-breaker | The biggest quality jump available (0.425 → 0.55+) and the most likely thing to break the SLA | M | T1 → **Ettin-17M measures 267 pairs/s on a desktop i7 = 3.7 ms/pair on passages.** Titles are ~15 tokens vs 256 so expect ~10× faster — **that is extrapolation and must be measured before anything depends on it** | spec |

**Cost:** static embeddings give up ~19 % of retrieval quality (potion-retrieval 35.06 vs MiniLM
42.92) for ~500× CPU throughput. At a 10 ms budget with no GPU there is no alternative.
**No learned sparse in v1** — it needs a ~100 M-param query encoder in-process and ~5× index, and the
`seismic` crate that would make it cheap has **16 downloads in 90 days**.

### P12 — Entity resolution (offline mode)

presyo's Fellegi–Sunter matcher is the most sophisticated code in the house — 17 interpretable
weights, a 497-token DF table, a 10 KB frozen model, blocking that turns ~50 B pairs into 2.55 M
edges at 95.7 % pair-completeness — and it is `MATCH_FS_TIER=shadow` in production because its
**real out-of-sample precision measured ≈ 82 % against an in-sample 100 %**.

| Item | What it closes | Effort | Benchmark | Status |
|---|---|---|---|---|
| Blocking + cheap-scorer stages as a library primitive | The WDC benchmark predicts exactly presyo's cliff: fine-tuned matchers score 79.16 seen → **70.24 unseen**, and a grocery catalogue is *permanently* unseen (new SKU weekly) | L | T1 → pair-completeness ≥ 0.95 and candidate-set size, against presyo's gold set | spec |
| Hard partition by canonical size **before** any similarity | Removes the dominant false-positive class and cuts candidate space ~10× | S | T1 → no cross-size pair ever enters the candidate set | spec |

**Do not train a matcher.** Zero-shot GPT-4 holds 89.61 F1 on WDC where Ditto collapses to 48.74
under transfer, at **0.006 ¢/pair** — 100 K pairs ≈ $6. The engineering is blocking; adjudication is
a solved economic problem. `index` owns the blocking, not the LLM call.

### P13 — Research: the honest home for the learned core

| Item | What it closes | Effort | Benchmark | Status |
|---|---|---|---|---|
| **Bounded-tail-latency updatable PLA vs `BTreeMap` and ART on hard SOSD (osm, genome)** | The standing complaint in the literature is **p99.9 under updates** (VLDB 2022, still being papered at SIGMOD 2026). This repo already measures p99 with an `rdtsc` clock and median-of-5 — which is precisely the instrument the papers lack | L | T1 → p50/p99/p99.9 under a mixed read/write workload, against both baselines, on real SOSD data | spec |

This is a **research row, not a product row**, and it is labelled as such. It is also the only
genuinely open question the existing core is positioned to answer better than anyone else.

---

## What we are NOT going to do

See [`docs/roadmap-rejected.md`](docs/roadmap-rejected.md) for the full reasons, plus the 2026-09-05
additions. Summary of the additions:

- **A sidecar process.** The socket is cheap (7.7 µs); the codec is not (103.59 ms vs 5.6 ns), the
  two engines worth running as sidecars (Meilisearch, Typesense) **cannot be embedded at all**, and
  they cost 305 MB RSS / 2–3× resident RAM on a box already running Postgres.
- **A Vercel Edge artifact** (1–4 MB, and `runtime = 'edge'` is unsupported from Next.js 16.3), a
  **Fastly artifact** (50 ms CPU, fresh sandbox per request — no amortization), a **WASM Component**
  (Phase 1; `cargo-component` stale 17 months), **memory64** (10–100 % slower, absent from Safari).
- **Learned sparse (SPLADE) in v1.**
- **Training an entity matcher.**
- **Counterfactual LTR** before there is real traffic and a working loss — SIGIR'24 reproducibility
  found ULTR improves click prediction but not ranking, dominated by choice of loss.
- **Depending on `pgm-extra` (134 recent downloads) or `seismic` (16).**

## Triage — decisions blocked on external info

| Item | What's blocking |
|---|---|
| Whether to ship a `pgrx` extension at all | **RDS no, Supabase no, Neon deprecated with a 2026-09-21 migration deadline.** Viable only where you control the image — which *is* presyo (self-hosted PG16 on a SG VPS). Blocks the general product, not the first customer |
| Licence | The BM25-on-Postgres field fragmented: ParadeDB AGPL vs Tiger's `pg_textsearch` under the permissive PostgreSQL licence. A licence choice here is a positioning choice |
| r-index (BWT-runs) inclusion | Unchanged — only pays off on *repetitive* corpora; needs a target-data-shape decision |
| Governance / IP home | The evidence says the variable that decides whether an embedded library survives an acquisition is **governance, not licence**: Quickwit→Datadog and DuckDB Labs→AWS both kept the OSS healthy under a foundation; **Kuzu→Apple archived the repo ten days later**. A foundation or consortium structure is a decision, not a task |

---

## Positioning — what this engine is for, and who it is against

From [`docs/research/business.md`](docs/research/business.md). Recorded here because it decides
which technical rows matter, not because the repo is a business yet.

**The market moved.** Classic site search is a declining-multiple business — Elastic's monthly
self-serve Cloud grew **+1 %** and it cut ~7 % of staff in Jun 2026; Coveo's **net expansion is
99 %**, i.e. its installed base is contracting; Algolia has not raised since **Jul 2021** and
replaced its CEO in Apr 2026; Klevu no longer exists. Capital went to retrieval-for-agents instead
(Exa $17 M → $2.2 B in 22 months; Tavily acquired for $275 M).

**But the pain that sells is not price — it is sync.** Price complaints come with numbers; sync
complaints come with regret:

> *"I've built a number of systems that run a database and a separate search index… **The hardest
> part by far is keeping the search index in sync with the database.**"* — Simon Willison, 2025-04-09
>
> GitHub lost data from its own issues index in April 2026. **Six-plus 2025–26 products exist purely
> to kill the dual-write/CDC problem; zero exist purely to make search cheaper.**

**The two positions this repo can actually take**, ranked:

1. **Search that cannot drift — the index lives in the transaction.** Don't compete on price or
   recall; compete on *there is no pipeline*. This is a **correctness claim, not a cost claim, and
   correctness claims survive price wars** — which is why it leads. It is also the one thing
   Algolia, Pinecone and turbopuffer architecturally cannot offer. ParadeDB raised **$12 M on this
   pitch with four employees**.
2. **Be embedded, so it works where extensions cannot be installed.** `pg_search` is **not on RDS,
   not on Supabase, and deprecated on Neon with a 2026-09-21 migration deadline**; ParadeDB's own
   founder conceded the boxout (*"We would happily make it available on Azure... if there were a way
   for us to earn a living in doing so"*). **Nearly all managed Postgres runs where pg_search cannot
   go**, and a library linked into the *application* rather than the *database* sidesteps the
   extension-permission wall entirely.
3. **Fix pgvector's operational failures without leaving the database.** *"The Case Against
   pgvector"* (381 points, Oct 2025) is a public specification of what to beat: HNSW builds
   consuming **"10+ GB of RAM" and hours on the production database**, IVFFlat cluster drift, lock
   contention on live inserts, filtered search forcing pre/post-filter tradeoffs, hybrid search
   needing custom glue. **Every one is an architecture problem, not a Postgres problem** - an engine
   that indexes alongside the primary DB without competing for its buffer pool addresses all five.
   The honest ceiling is that thread's own top reply: *"If $64/month seems like a lot to you, just
   use pgvector."*

Reinforcers, not wedges: a **zero cost floor** (every competitor now has a monthly minimum — Pinecone
$50/$500, Weaviate $45/$400, turbopuffer $16/$256/$4,096, Elastic $99, **Vespa $20,000** — and there
is **no Manila region at any vendor**); and **sovereignty by construction** (real at enterprise
scale, but every incumbent's BYOC answer is quote-only, and the PH DPA imposes accountability, not
residency).

**Licence: keep the core permissive.** The repo is already `MIT OR Apache-2.0`. AGPL on a *library*
is what killed Berkeley DB's ecosystem and is currently boxing ParadeDB out of every managed cloud -
precisely the segment position 2 targets. The decisive precedent: **TigerData built `pg_textsearch`
explicitly because "the leading Postgres BM25 extension, ParadeDB, is guarded behind AGPL", and
Microsoft shipped the permissive one on Azure HorizonDB.** A competitor did not out-engineer
ParadeDB; it out-licensed it.

**The failure mode to design against is Realm:** $40 M raised, $39 M exit *below capital*, **100,000
developers and 350 payers.**

---

## The next row, and why it is the next one

**Wire derived expansion into `AliasTable`, and measure what it costs ordinary queries.**

The vocabulary gap that `p15` opened is now largely closed, with evidence at every step:

| step | question | result |
|---|---|---|
| `p15-presyo-catalog` | is there a real gap? | **61.7 %** precision@10 — the project's first unsaturated workload |
| `p16-biasd-entity` | what could an alias table buy? | **+14.9 pt**, and 0.7 % → 95.5 % on disjoint queries *(ceiling, curated aliases)* |
| `p17-presyo-expand` | can aliases be **derived**? | **+23.9 pt held out** — 55.7 % → 79.6 %, no model, no new storage |

And `p17` carries the control that matters: a **random** expansion of the same size costs **35.9
points**, so the ~60-point spread says the derivation is doing the work rather than "longer queries
match more".

**The damage gate has now been run, and it caught something.** A *loose* trigger — expand whenever
the query contains a category's words — fires on **19.7 %** of ordinary product queries and costs
**2.0 points** of exact-product hit@1: `Signature Select Ice Cream Butter Pecan` gets expanded
because it contains *Cream*, and the exact match is buried under the category. The feature would
have made browse better by making search worse.

**The fix is one comparison**: require the query's token set to *equal* a category's rather than
merely contain it. That fires on **zero** product queries, costs **+0.0 points**, and retains the
full +23.9 on genuine category queries.

So the shippable design is measured on both sides: **+23.9 points on browse, 0.0 damage on lookup,
gated by "expand only when the query IS a category".**

**Both of those are now done.** `IndexBuilder::learn_expansion(facet_field, top_k)` puts the
mechanism inside the engine, and the table is derived at build time and serialized with the index —
so an application gets it by naming a facet field, with no pipeline of its own. That is goal #1
("apps don't need to optimize for their data anymore") delivered for this one failure mode.

On presyo's 241,677 products: **71.1 % → 96.6 % precision@10 in-sample (+25.5 pt)**, 145 facet values learned,
+3.4 s of build time, category-query latency 316 µs against 178 µs for an exact product query.
`real-corpus`, `sisia-catalog`, `maphy-place`, `geo-bench` and the WASM smoke test are all unchanged.

**And it generalizes.** `bench/roadmap/p18-blead-industry.md` ran the identical measurement on a
corpus the feature was not designed for — `blead`'s **25,979 real Philippine business names** tagged
with an industry, where nothing in `10K EAST CONCRETE MIX SPECIALIST, INC.` says *Wholesale/Retail*:

| corpus | documents | facet values | plain | learned | Δ |
|---|---|---|---|---|---|
| presyo — grocery products | 241,677 | 145 | 61.7 % | 96.6 % | **+34.9 pt** |
| **blead — business names** | **25,979** | **27** | **64.8 %** | **85.2 %** | **+20.4 pt** |

Label leakage is 13.8 % and 11.8 % respectively, so the two are comparable by construction and the
difference cannot be blamed on an easier label. `Construction` and `Public Admin` go from **0 %** to
80 % and 100 %; MRR reaches **1.000**, meaning every industry has a correct result at rank 1.

**Held out, the claim gets its condition** — and this is the more useful result:

| corpus | in-sample gain | held-out gain | held-out retains | staleness cost |
|---|---|---|---|---|
| presyo — grocery products | +34.9 pt | +23.9 pt | **68 %** | ~11–17 pt |
| **blead — business names** | +20.4 pt | **+6.7 pt** | **33 %** | **13.7 pt** |

The staleness cost is comparable, but **what survives on unseen documents is not**. The mechanism's
value depends on **vocabulary reuse within a facet**: half of `Baking Needs` and the other half
share brands, product types and sizes, so a term learned from one predicts the other; two logistics
firms share almost nothing but legal suffixes.

So the honest claim is narrower than "it generalizes", and more useful: **it generalizes where a
facet's members reuse vocabulary — and an adopter can check that property on their own data before
adopting**, using exactly the in-sample/held-out split these benches run, which needs no labels
beyond the facet they already have.

**The in-sample/held-out distinction is the thing to carry forward, not the 96.6 %.** In-sample is
the deployment condition for products already in the catalogue; `p17`'s held-out 79.6 % is the
condition for products added after the table was built. **The ~17-point gap is the cost of a stale
table**, and it is what decides how often derivation should re-run.

**The brand caveat has now been tested rather than carried.** The catalogue has a `brand_name`
column, so striking every brand token out of the expansion measures the risk directly: brand-free
expansion still captures **43 % of the achievable gain (+14.5 pt)**. The mechanism is not pure brand
memorisation, and a category of entirely unseen brands degrades toward +14.5 rather than to zero.

**And the gain is now sized against a ceiling.** Indexing the category name on every product —
`p15`'s option 2, and close to cheating — reaches **89.5 %**. So:

| | precision@10 | share of achievable gain |
|---|---|---|
| category name alone | 55.7 % | — |
| **derived expansion** | **79.6 %** | **71 %** |
| derived, brands removed | 70.2 % | 43 % |
| category indexed *(upper bound)* | 89.5 % | 100 % |

The residual 9.9 points are **not vocabulary**: even with the label indexed, a product whose *name*
contains the category words outscores one that merely belongs to it. That is BM25 field weighting,
a different problem, and it makes `p15`'s option 3 (dense/hybrid retrieval) a weaker case than when
it was listed.

## Done — the analyzer half of the goal

The stated goal is *"the best search engine **and database engine/analyzer**"*. Retrieval had been
measured to exhaustion; analysis had not been measured at all.

`bench/roadmap/p19-presyo-categorize.md` takes the **50,607 `Uncategorized` products (20.9 % of
presyo's catalogue)** — recorded by `p15` and left alone — and asks the inverse question: not "find
the products in this category" but **"what category is this product?"**

The method needs nothing new: search the name against the products whose category is known and take
a **majority vote over the top-10 neighbours**. kNN classification in which **the index is the
model** — no training, no embeddings, no second system.

| | accuracy | share of predictions |
|---|---|---|
| all predictions | 68.0 % | 100 % |
| **confident only (≥60 % agree)** | **81.0 %** | 68.7 % |

**A confidence threshold is worth 13 points and costs nothing** — the vote share falls out of the
prediction. An application takes the confident 69 % automatically and queues the rest.

**The operating curve is the real deliverable**, not any single number on it:

| threshold | accuracy | share kept |
|---|---|---|
| take everything | 68.0 % | 100 % |
| 0.6 | 81.0 % | 68.7 % |
| 0.8 | 89.5 % | 46.8 % |
| **unanimous** | **95.7 %** | **22.1 %** |

At unanimity the index is right about **95.7 %** of the products it will speak up for — good enough
to apply automatically — and the application chooses the point, because the vote share comes free
with the prediction. Sweeping `k` shows the same shape: confident accuracy climbs 73.8 % → 85.0 %
from k=3 to k=40 while confident share falls 86 % → 50 %, so `k` is a second coverage/precision dial
rather than an accuracy dial.

`learn_expansion` on the classifier's index is an **exact no-op (−0.0 pt)**, predicted from the
design and run anyway — because a mechanism that *had* fired there would have meant the strict
trigger leaks, which is `p17`'s damage gate checked from the opposite side.

Applied to the real hole: **100 % get a proposal, 46 % confidently, at 2,169 products/second.** The
lower confidence there is the *correct* behaviour, not a disappointment — products left
uncategorised are plausibly the ones that were hard to categorise to begin with, and a method
equally sure about both sets would be the suspicious result.

Validation is deliberately conservative: accuracy is measured only on **labelled products withheld
from the index**, never on the unlabelled rows, whose true categories are unknown. Of the 1,284
misses, 188 (14.6 %) name a category sharing a word with the true one — `Spirits` for `Liquor`,
`Fresh Meat` for `Frozen Meat`. Counting those as acceptable would read 72.7 %, and **that number is
not claimed**: whether `Spirits` may stand in for `Liquor` is presyo's call about its own taxonomy,
not this project's.

## Corrected — a benchmark error worth 9.4 points

`bench/roadmap/p20-profstopick-dept.md` went looking for a third corpus and found a defect in this
project's own measurements instead.

> **A field with `boost 0.0` contributes no score, but still lets a document MATCH** — and a match
> earns `typo_bucket` 0, the primary sort key, so it outranks every non-matching document.

`p15`, `p18`, `p19` and `p20` all set the facet field to `boost 0.0` believing it inert for
retrieval. It is score-free, not match-free. Measured on presyo with **no expansion at all**:
name+brand 61.7 % → **71.1 %** once the boost-0.0 category field is present.

`p15` compared an expansion arm carrying that field against a baseline without it. **The corrected
gain is +25.5 points, not +34.9**, and `p15` now shows all three rows. `p18` and `p19` are
unaffected — both of their arms carried the field, so those comparisons were fair.

The feature still works and the gain is still large. The headline was overstated by 9.4 points, and
it was caught by a baseline being implausibly high — the same tell as every previous methodology
error here.

## Also corrected by p15: the scale numbers were generous## Also corrected by p15: the scale numbers were generous

`p7-scale` reaches 250 K and 1 M by recombining 61,467 schools, and flags the caveat. p15 quantifies
it: recombination stalls at **41,069 terms** while a real catalogue of similar size carries
**117,472**, and **typo p99 is 3.08 ms recombined versus 5.72 ms real — a 1.9× understatement.**
The 5 ms bar is missed at a quarter of a million real rows, not at a million.

## Done — deletion

**Shipped 2026-09-05.** `Searcher::delete(global_ordinal)` marks a document deleted in the segment
that owns it; `undelete` restores it; `live_count()` and `deleted_count()` report the split. One bit
per document, empty when nothing is deleted.

This was the last correctness gap against presyo, which soft-merges products via `superseded_by`
(36,260 merged rows) and filters `WHERE superseded_by IS NULL` on public reads — an index that could
not forget kept serving merged duplicates until the next rebuild.

Two decisions are worth keeping:

- **Deletion does not rewrite postings.** A deleted document stops appearing immediately but still
  counts toward document frequency and average field length until a rebuild. That is Lucene's
  behaviour between merges, and it is right here for the same reason: rewriting postings per delete
  turns an O(1) operation into an O(index) one, and compaction in this project is already "rebuild
  from the application's database". The cost is stated rather than hidden — after many deletions,
  IDF reflects a collection larger than the live one, so near-ties can reorder. The result *set* is
  always correct.
- **`needs_compaction()` now counts deletions, not just additions.** A single segment that has
  forgotten a third of its documents has skew 0.0, so a signal watching only skew would report it
  healthy forever. For presyo, deletions are the likelier driver of the two.

## Done — anchored prefix matching

**Shipped 2026-09-05.** `bench/roadmap/p12-maphy-place.md` found the engine losing typeahead to a
hand-rolled `norm.startsWith(q)` scan, with every miss the same shape: a prefix ending part-way
through a second token. `"DEL C"` plans as exact-`del` plus prefix-`c*`; `del` is common and `c*`
matches nearly everything, so `DEL CARMEN` never ranked.

Storing the term id of each document's first token — four bytes per document — restores the
string-level fact that a typeahead is typed from the *start* of a name. Documents that do not
anchor keep `UNANCHORED_KEEP` of their score.

**Typeahead hit@10 94.6 % → 96.4 %**, with `real-corpus` and `sisia-catalog` unchanged. The residual
gap to maphy's 100 % is dominated by genuinely unanswerable prefix collisions (`"SAN A"` covers six
municipalities), and closing it further would be tuning to one fixture.

The correctness discipline is the same one static priors established and is worth stating once for
both: **a scoring factor must be `<= 1`**. Retrieval prunes against upper bounds, so anything above
1 lets a true score exceed the block maxima the pruner trusts and silently drops valid results —
only on corpora large enough for pruning to engage, which is the worst possible place to find out.

## Built, measured, and narrow: static priors

`IndexBuilder::add_with_prior` gives every document a query-independent importance, normalized into
`(0, 1]` so that block-max pruning bounds stay valid **by construction** rather than by patching
every bound, serialized as an eighth section in `IDXTEXT2`, and asserted never to override
`typo_bucket` — an important document matched via a typo must still lose to an exact match on an
unimportant one.

`docs/adoption.md` records that every consumer has such a signal: presyo store trust and price
recency, profstopick rating counts, sisia catalog level, maphy PSGC level.

**It has now been measured twice, and the answer is no.** `bench/roadmap/p13-presyo-prior.md` held
it out on presyo's 500 gold clusters: **+0.00 points**, while demonstrably reordering 1,227 held-out
queries. `bench/roadmap/p14-presyo-broad.md` then tested it on the broad-query workload it was
supposed to be *for*: **−1.4 points** for source quality and **−2.5 points** for popularity.

A prior on presyo does not merely fail to help, it **actively degrades precision** by pulling
well-stocked or clean-named products above ones that actually match the query.

So the claim is narrowed to almost nothing. "Every consumer has an importance signal" is true;
"therefore the engine needs to accept one" does not follow. The feature stays — correct, cheap,
serialized, safe by construction — and **ships off by default**, with two measurements saying so.

## The row after that

**Deletion.** `Searcher` closes additions; it does not close removals. presyo soft-merges products
(`superseded_by`, 36,260 merged rows) and its public reads filter `WHERE superseded_by IS NULL`, so
an index that cannot forget a document will keep serving merged duplicates until the next
compaction. A per-segment deleted-document bitmap, consulted at merge time, is the standard answer
and is small — one bit per document.

## Done — the browser point-in-polygon number

**Shipped 2026-09-05, `bench/roadmap/p11-geo-wasm.md`.** The row read:

> `docs/research/geometry-sota.md` surveyed the geometry
field and found most of it genuinely solved — FlatGeobuf's bbox-over-HTTP, PMTiles tile addressing,
COPC octree range reads, meshopt as the vertex codec. It found exactly one hole with **no published
figure in any language**: point-in-polygon at scale in a browser. No JS-vs-WASM throughput
benchmark, no GPU version, no rasterization version. The only calibration in existence is DuckDB's
native multicore **2.02 M points/s**.

`bench/roadmap/p10-geo-join.md` filled the native half on real maphy geometry — **4.50 M points/s
single-threaded, 28.2× over a naive scan** — and the remaining step is small, because the pieces
already exist: the kernel is written, `crates/index-wasm` already has a hand-written raw-pointer C
ABI, and `js/` already has a host and a real-browser check. **Publishing this number is worth more
than the code**, and it is the first row in this project whose value does not depend on an adopter
saying yes.

The condition on it is stated in `p10` and holds here: design for **fixed-width SIMD and i32
offsets** — relaxed SIMD is Safari-flag-gated and memory64 is Safari-unsupported.

---

**Result: 4.43 M points/s in Node, 3.61 M in real headless Chrome, ~27–32× a JavaScript scan, in a
22,334-byte gzipped module.** The finding worth keeping is that **WASM is at parity with native** —
the same index does 4.50 M points/s in native Rust, so the browser costs about 20 %, not a factor.
`crates/index-geo` is the library, `crates/index-geo-wasm` the binding, `js/geo.mjs` the host, and
the gate now asserts per-point agreement with a JS scan over 53,715 real points.

It also produced the sharpest methodological lesson of the project so far: **two agreeing
implementations that share an input parser agree about the parser, not about the answer.** A
MultiPolygon-vs-holes bug survived a per-point assert because both JS arms shared a loader, and was
caught only by comparing against the independent Rust implementation.

## New problems surfaced during this sweep

- **The project was scoped from the literature down, never from the consumers up.** Nine months of
  correct, well-benchmarked work aimed at a workload none of the eleven apps has. The fix is
  structural, not a one-off: **no row without a named consumer and a measurement that consumer
  already takes.**
- **A rejected-ideas entry produced a wrong conclusion.** Rejecting *building* an FST was right;
  concluding typo tolerance was therefore out of scope did not follow, and cost the largest
  available win nine months. `roadmap-rejected.md` needs a standing question: *does this rejection
  scope out the problem, or only one solution to it?*
- **Fuzzy cost scales with vocabulary, not corpus size** — confirmed independently by the P5 spike
  (0.66 ms at 12 K terms → 2.86 ms at 848 K, document count irrelevant) and by the landscape sweep.
  This inverts the usual sizing intuition.
- **presyo's measured pain is structural to Postgres, not a mistake.** GIN produces a bitmap, never
  an ordered iterator: no ORDER BY pushdown, no LIMIT pushdown, no early exit, and ranking always
  requires a heap fetch per candidate. 12 M rows, `ORDER BY` + `LIMIT 24`: **54 ms → 188,442 ms
  (~3,500×)**. PG17 and PG18 changed nothing for relevance.
- **An mmap-based format does not fail to port to WASM — it silently succeeds and reads the whole
  index into linear memory.** The worst failure mode available, and it decides P7.
- **The first version of the P5 bench measured a 115-token vocabulary** and reported a flattering
  0.03 B/key. Caught only because the number was implausibly good. **A benchmark's corpus is part of
  its claim**; `bench/README.md` should say so.
- **Three of eleven surveyed repos declined this product in writing.** Recorded as market-size
  evidence rather than buried.

## Honest gaps in this revision

- **No customer has asked for this.** Positioning above is derived from other people's published
  pain, not from anyone offering to pay. The four in-house consumers are the only validated demand,
  and three other repos declined the product outright.
- **Reddit and X sentiment is absent from the business research** — both were inaccessible when the
  sweep ran, so developer-sentiment claims rest on the HN API alone.
- **The P5 spike runs on a synthetic corpus.** profstopick's real shard is not committed (only
  `search-index-manifest.json`); generating it needs `DATABASE_URL`. Until then the headline number
  is *representative*, not *actual*.
- **Still no CI** (open since 2026-06-19), so "green gate" means "green on one Windows box".
- **Real SOSD 200M datasets still not downloaded**, so P13 cannot start.
- **The cross-encoder latency figure is an extrapolation** (267 pairs/s on passages → titles ~10×
  faster). No published CPU-INT8 reranker benchmark exists anywhere — filling that gap is cheap and
  citable, and is why P11's rerank row is flagged rather than assumed.
- Minor test gaps carried over: no `FmIndex::locate` test for an absent pattern; no `PgmIndex` ≡
  `PlaIndex` result-equivalence test.
