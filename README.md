# index

An **embedded retrieval engine**: the goal is to give any app Algolia-grade search over the data it
already has — in its own process, on its own infrastructure, from one index format that serves a
browser, a server, an edge worker and a database.

See [ROADMAP.md](ROADMAP.md) for the plan, [docs/adoption.md](docs/adoption.md) for what shipping it
into each app would take, [CHANGELOG.md](CHANGELOG.md) for what shipped,
[docs/research/](docs/research/) for the evidence every roadmap row rests on, and
[docs/roadmap-rejected.md](docs/roadmap-rejected.md) for what was deliberately ruled out.

## Any database, over a pipe

**If your database can print rows, `index` can index them** — no driver, no connection string, no
dialect. Every database already ships a client that prints CSV, TSV or JSON, and every
change-data-capture tool already emits newline-delimited JSON, so all of them reduce to one pipe.

```sh
# Postgres
psql -c "COPY (SELECT sku,name,brand FROM product) TO STDOUT (FORMAT csv, HEADER)" \
  | index build -d data/ --schema 'sku:0:0.6,name:3:0.4,brand:1:0.6' --key sku --facet brand

# ...and the same tool for everything else
sqlite3 -header -csv app.db 'SELECT ...'   | index build -d data/ ...
mysql -B -e 'SELECT ...'                   | index build -d data/ --tsv ...
mongoexport --type json                    | index build -d data/ --jsonl ...
curl -s /api/product | jq -c '.[]'         | index build -d data/ --jsonl ...

# ...or skip the client entirely: the dump the database already wrote IS the pipe
index build -d data/ --sql --table product   --schema 'sku:0:0.6,name:3:0.4,brand:1:0.6' --key sku --facet brand < dump.sql

index search -d data/ 'colgaye tothpaste'  # typo-corrected, from the shell
```

**Keep it current from whatever already emits changes**, rather than rebuilding:

```sh
pg_recvlogical -S idx -f - --start -P wal2json | jq -c '...' \
  | index apply -d data/ --jsonl --key after.sku,before.sku
```

`apply` takes Debezium and wal2json operation words as they come, resolves the key from fallback
paths (`after.sku,before.sku`), and **collapses the stream per key** — so a row inserted and deleted
in one stream ends up absent, whatever order the records arrive in.

**The claim is measured, not asserted.** `cdc-equivalence` runs 8,000 inserts, updates and deletes
against a model of the table and requires the index to agree on membership *exactly*:
**0 keys wrong, 14,288 = 14,288.** Ranking drifts as segments and tombstones accumulate — 93 % rank-1
agreement with a full rebuild after the first delta, 84 % after 8,000 operations — which is
[measured and explained](bench/roadmap/p50-change-stream.md) rather than hidden, and is what
`index stat` reports as `needs compaction`.

**Tested against every database in the estate, not just a fixture.** 122 databases across five
machines — Windows, macOS and three Linux hosts over Tailscale — and three engines (PostgreSQL
16/17/18, SQLite, DuckDB): **86 indexed, 36 verified empty, 0 failures, 2,226,041 rows**, every one
built by the local binary from rows streamed over SSH. Typo tolerance held on 86 of 86 probes.
It also found a real bug — thirteen SQLite databases aborted the build because real `TEXT` columns
are not always valid UTF-8 — which is now repaired, counted and reported.
See [`p51`](bench/roadmap/p51-database-sweep.md) and
[the raw results](bench/evidence/p51-database-sweep.tsv).

**The price, stated up front: the pipe is the integration.** Credentials, pagination and restarts
belong to whoever runs the command; the tool cannot resume a stream it did not start. That buys
zero dependencies and a database nobody here has heard of working on day one.

## In the browser, with no server in the query path

`docs/research/landscape.md` §7.5 found a position nobody occupies, and found that it is
**architectural rather than neglected**: Tantivy's WASM RFC has been open since 2019 and is
read-only, LanceDB closed WASM as *"not planned"*, Orama's Rust core has no wasm32 target, and
Meilisearch and Elasticsearch are servers with no "run it in the tab" mode to add.

The same `.idx` the server and the CLI use, persisted to the browser's own filesystem:

```sh
node js/opfs-check.mjs      # a real Chromium, the real artifact
```

| | |
|---|---|
| first visit, over HTTP | 17.7 ms |
| **second visit, from OPFS** | **2.8 ms** |
| **network calls during `search()`** | **0** |
| **per query, in-tab** | **0.086 ms** |
| localhost round trip doing *no work* | 2.300 ms |
| **speed-up over a server that does nothing** | **~27x** |
| bytes read to learn the file's layout | **296** (0.077 % of it) |

It replaces profstopick's 2,505,813-byte JSON shard — **95.6 % of the 5 MB localStorage quota** —
with **8.8 %** of that, in a quota measured in gigabytes. Their production instrumentation measured
**109 of 267 real searches returning nothing** on 2026-08-17, 60 of them name-shaped. **profstopick
has since fixed the name-order half themselves**, with a backtracking token assignment in their own
matcher; what an engine with edit distance adds on top is the *misspelling* half, which a
prefix/substring matcher cannot reach by construction. `js/compare.html` runs both side by side and
shows each winning where it should.

The 296-byte figure is the point of the section table: a browser can open an index far larger than
the tab's memory, which is what an object-store cold tier looks like from the client side.

Full numbers, including what fails: [`docs/benchmarks.md`](docs/benchmarks.md).

## Status

**The project has been re-baselined** after surveying eleven real applications and running six
research lanes. The headline finding is uncomfortable and is stated first:

> **The learned-index core that this repo spent its first phase building solves a problem none of
> the eleven consumer apps has.** Their hot paths are *text → ranked documents*, *predicate → row
> set*, and *name → canonical entity*. A faster `u64 → position` map appears in none of them.

That work is **not deleted — it is re-scoped from thesis to component** (fence pointers, succinct
structures, an adaptive filter column, and a p99 measurement harness that is better than the ones in
the literature). What replaces it as the spine is normalization, a typo-tolerant term dictionary,
BM25F, and a portable index format. Full argument: [ROADMAP.md](ROADMAP.md) and
[docs/research/demand.md](docs/research/demand.md).

**What is proven and reproducing today:**

- A piecewise-linear (PLA) learned index over sorted `u64` keys whose prediction is guaranteed within
  `±epsilon` of the true position (proven by test), with a bounded last-mile search. At ε=16 it
  **beats `std::BTreeMap` on space, p50 and p99** across four distributions at n=1M and n=10M
  (median-of-5, `rdtsc` timer). Reproduce with
  `cargo run -p index-bench --release --bin beat-btreemap`.
- A recursive `PgmIndex` — correct, but measures ≈ the single-level index up to 10M. The
  tail-latency lever is ε, not the top layer.
- A succinct `FmIndex` (BWT + wavelet tree + sparse SA) at ~5.9/9.7/14.6 bits/char at σ=4/26/256,
  with `count`/`locate`/k-mismatch correct.
- `CrackerColumn` — stochastic database cracking, proven to avoid the adversarial case that naive
  cracking fails.
- `index-text`, the retrieval engine itself — analyzer, typo-tolerant term dictionary, BM25F,
  MaxScore top-k and RRF — **measured against two production corpora**. See
  [Headline measurement](#headline-measurement).

```
crates/
  index-core/   PlaIndex (optimal PLA learned index), PgmIndex (recursive),
                CrackerColumn (adaptive database cracking),
                FmIndex (compressed full-text: count/locate + k-mismatch fuzzy),
                data gens + SOSD loader.  Dependency-free, deliberately.
  index-text/   THE ENGINE. analyze (fold, tokenize, unit canonicalization, alias tables),
                dict (FST term dictionary + the typo policy), index (BM25F with exact
                u16 field lengths + block-max MaxScore top-k), format (portable
                range-readable on-disk index), searcher (multi-segment: add and
                delete documents without a rebuild), fuse (RRF k=60, convex
                combination). Static priors, prefix anchoring, and LEARNED QUERY
                EXPANSION from a facet field (+25.5 pt on presyo categories).
  index-geo/    POINT LOCATION. "which polygon contains this point?" as a Hilbert-ordered
                cell index: interior cells answer with ZERO geometry, ocean with a failed
                binary search. forbid(unsafe_code), no dependencies.
  index-image/  THE IMAGE TIER. Perceptual hashes (aHash/dHash/pHash 64-bit, PDQ-shaped
                256-bit), OKLab palette extraction into 77 perceptual colour buckets, a
                hostile-input-safe EXIF/TIFF/JPEG-structure parser, SHA-256 content
                addressing, and a quantised vector column (binary popcount prefilter ->
                int8 rerank -> exact) that fuses with index-text in ONE query plan with
                ONE top-k selection. Ships NO model: an embedding is an INPUT.
  index-wasm/   The binding. 69 symbols, hand-written C ABI (v13, facets/ranges/sort/
                live segments/highlight/clauses/paging/phrases/IMAGE),
                NO wasm-bindgen,
                so ONE artifact serves
                the browser, Node, edge workers, and native FFI (Go/Python/PHP/Ruby).
                `include/index.h` is that ABI as a real C header; `host/python/` is a
                second-language host proving it, in the standard library alone.
  index-geo-wasm/  Same ABI, for index-geo. 77,065 bytes raw / 31,072 gzipped.
  index-accel/  onegrid's ratified AccelModule ABI, implemented: 7 analytics kernels
                verified differentially by `accel-kernel` (2,400 trials, 0 wrong)
                (sort/filter/group/aggregate/bitmap/topK) in 6.3 KB of no_std wasm.
  index-cli/    THE `index` BINARY. Build and keep an index current from ANY database, over
                a pipe -- no driver, no connection string. Hand-written CSV/TSV/JSONL readers,
                so its only dependency is index-text. `index build` / `apply` / `search` / `stat`.
  index-bench/  bins: beat-btreemap   (bytes/key + p50/p99 vs std BTreeMap)
                      crack-converge   (naive vs stochastic cracking convergence)
                      fuzzy-decision   (fuzzy-over-FM viability: latency vs k and σ)
                      fuzzy-term       (typo-tolerant term dictionary feasibility)
                      real-corpus      (the engine vs two PRODUCTION corpora)
                      scale            (5K -> 1M docs on the real DepEd masterlist)
                      emit-artifact    (build a shippable .idx from real app data)
                      sisia-catalog    (sisia's LIKE catalog search vs the engine)
                      sfc-2d           (is a viewport a range scan over an ordered key?)
                      geo-join         (point-in-polygon as an index, on real geometry)
                      maphy-place      (maphy's place search vs the engine; typos vs priors)
                      image-corpus     (the image tier on a real 17,311-file web scrape:
                                        census, cheap-tier cost, dedup, fused correctness)
                      presyo-prior     (do static priors pay? held-out, on presyo gold clusters)
                      presyo-broad     (broad/browse queries labelled from presyo's own taxonomy)
                      presyo-catalog   (241,677 REAL products; category retrieval = the open gap)
                      biasd-entity     (entity linking from nicknames; what aliases are worth)
                      presyo-expand    (derived query expansion, held out, vs a random control)
                      blead-industry   (does learn_expansion generalize? second corpus, 26k firms)
                      presyo-categorize (index as analyzer: kNN category prediction, 81% confident)
                      profstopick-dept (third corpus; found the boost-0.0 matching error)
                      prune-consistency (GREEN: all three pruning sites gated on the
                                        exact eff bound)
                      pool-audit       (does that defect hit REAL corpora? it did — 28% of
                                        presyo product queries; now 0.00% on all six sets)
                      facet-shop       (the whole filter bar on presyo's catalogue, incl. the
                                        two sort arms held to one answer, 336 queries x 2)
                      cdc-equivalence  (does a change stream converge on a rebuild? membership
                                        exact over 8,000 ops; ranking drift measured)
                      phrase-cost      (what positions cost, and phrase hits checked against
                                        raw text — an arm sharing no code with the engine)
                      accel-kernel     (the 7 analytics kernels, differentially)
                      segment-scale    (what incremental segments cost as they accumulate)
                      booted-schema / alec-surface (the house's own databases)
js/
  index.mjs     JavaScript host for the C ABI (no wasm-bindgen)
  accel-bench.mjs  the 7 analytics kernels timed INSIDE wasm: 73-93% of native on five
                of six, 4% on the byte-parallel one (no SIMD). See p34.
  smoke.mjs     98 checks, and the CI gate: builds an index FROM JavaScript, searches it,
                corrects typos, facets, filters by clause, pages, runs phrases, serializes,
                reopens and compares -- no corpus on disk -- then does it again THROUGH
                `index.mjs`, so the shipped host cannot rot unnoticed the way it did in p44
  demo.mjs      end-to-end proof: real index, real typo queries, in Node
  geo.mjs       host for index-geo, plus the pure-JS scan it is measured against
  geo-bench.mjs point-in-polygon throughput in Node; CI gates on per-point agreement
  geo-browser-check.mjs   the same, in a REAL browser over HTTP
bench/fixture/  real maphy geometry: 53,715 POIs, 88 provinces, 2,454 municipal polygons
bench/
  README.md     benchmark methodology (mirrors SOSD + Pizza&Chili)
  roadmap/      falsifiable specs for not-yet-built items (gate-excluded until promoted)
docs/
  integration.md  the engine measured against profstopick's OWN contract test
  research/     the evidence base — demand, relevance, speed, landscape,
                portability, business, practitioner claims, build-or-buy
  roadmap-rejected.md
```

## Headline measurement

`real-corpus` runs `index-text` against two corpora exported from the production databases of two
consumer applications. Full spec and the three defects it caught:
[`bench/roadmap/p6-real-corpus.md`](bench/roadmap/p6-real-corpus.md).

### profstopick — 1,322 real Ateneo professors

Its production measurement on 2026-08-17: **109 of 267 real searches returned nothing.** Its shipped
matcher is 8 tiers of prefix and substring logic with no edit distance. That matcher is reproduced
here as the baseline, so this is a comparison against the real thing.

| | **engine** | baseline (the shipped matcher) |
|---|---|---|
| exact hit@1 | **99.8 %** | 99.8 % |
| **typo hit@10** | **99.9 %** | 14.9 % |
| typo MRR@10 | **0.998** | 0.149 |
| **zero-result rate** | **0.1 %** | 85.1 % |

29,881-byte term dictionary (6.55 B/term), 5 ms build, **p50 42.6 µs / p99 202 µs** on the typo set.

### presyo — 500 gold cross-store product clusters, 1,940 real retailer listings

Given *one store's* raw listing name, find the *same physical product* in a *different store's*
catalogue. The query listing is not indexed, so a hit can only come from another retailer's text.

| | recall@10 | hit@1 | MRR@10 |
|---|---|---|---|
| clean | **100.0 %** | 99.8 % | 0.999 |
| typo | **100.0 %** | 99.0 % | 0.993 |

**Size guard: 0 violations** across the 478 queries that state a mass or volume — whenever a listing
of the requested size exists, the engine returns that size first. 6,780-byte dictionary, 2 ms build,
**p50 33.3 µs / p99 86.5 µs**.

### It satisfies two applications' own contract tests

| App | Their harness | Result |
|---|---|---|
| **profstopick** | `search-name-order.test.mjs`, unmodified but for the import line | **9 / 9** |
| **profstopick** | the whole suite (`npm test`) | **1,922 / 1,958** — the 4 failures fail identically on their untouched `main` |
| **onegrid** | `differential.property.test.ts` + the whole `packages/wasm` suite | **294 / 294**, driven by a real compiled module instead of their JS stand-in |
| **presyo** | their own `searchProduct` on a disposable PostgreSQL, same rows and queries, **at 260 K** | see below |
| **sisia-app** | its own catalog `LIKE` search, on its own registrar data | out-of-order title words **91.2 % vs 0.0 %**; exact-code lookup **tied**, reported as a non-win |
| **browser** | `js/browser.html` in real headless Chromium 147 | **PASS**, 37 us/query on the main thread |

Both live on throwaway git worktree branches; neither app's `main` was touched. Full account:
[`docs/integration.md`](docs/integration.md).

#### profstopick — the search contract

`profstopick`'s `test/search-name-order.test.mjs` is not synthetic. Its header records a production
measurement from 2026-08-17: **158 search hits against 109 misses, a 40.8 % miss rate**, with 60 of
those misses being name-shaped queries whose professor was already in the corpus, stored
surname-first. Six of its nine assertions are explicit *survival* checks.

The engine was run against **that test, unmodified — only the import line redirected**, on a git
worktree branch that leaves `main` untouched:

```
✔ a full name typed first-name-first finds the professor
✔ a partly-typed second token still finds them
✔ the surname-first spelling keeps working
✔ each query token must match a DISTINCT name token
✔ a name that is not in the corpus still returns nothing
✔ the accent fold still reaches a name nobody can type
✔ a course code with punctuation still matches however it is typed
✔ professors still outrank courses
✔ an empty or whitespace query still returns nothing

ℹ tests 9   ℹ pass 9   ℹ fail 0
```

It passed **7 of 9 first**, and both failures produced engine fixes that help every consumer:
**compound splitting** (`math30.23`, and presyo's whole `cocacola`/`bearbrand`/`luckyme` category)
and **enforcing the typo length gate in prefix mode** — a one-character token was falling through to
a distance-1 automaton and matching nearly the entire dictionary. Full account:
[`docs/integration.md`](docs/integration.md).

**Not deployed.** The branch exists to measure, not to ship.

### It runs in a real browser

```
$ node js/browser-check.mjs          # headless Chromium 147, via Playwright
  wasm 244,200 B | index 380,564 B | 1,322 docs | instantiate 52.7 ms | open 16.8 ms
  PASS exact name / surname only / one deletion / transposition / typeahead / nonsense
  PASS 300 real name queries: 37 us each, on the main thread
OVERALL: PASS
```

`js/browser.html` drives the raw C ABI rather than the Node wrapper, so a failure cannot be hidden.
This is the tier profstopick actually ships to, and the only place `instantiateStreaming` and its
hard `application/wasm` MIME requirement are exercised. **Persistence shipped in `p68`** — the index
is stored in OPFS and survives a page navigation, and since `p72` a query can be answered from byte
ranges without reading the whole file. See `js/opfs.mjs` and `js/opfs-check.mjs`.

*The transcript above predates `p69`, which took the index file from 386,043 to 220,470 bytes; the
figures in it are an upper bound. It is not re-run here because Playwright is deliberately not a
dependency of this repo — link a sibling `node_modules` to reproduce it.*

### It runs outside Rust — measured in Node, through WASM

```
$ cargo build -p index-wasm --release --target wasm32-unknown-unknown
$ cargo run -p index-bench --release --bin emit-artifact
$ node js/demo.mjs

  index-text in Node, through WASM — no wasm-bindgen, no native build

    wasm module   559,970 bytes
    index file    220,470 bytes
    documents     1,322
    terms         4,564
    open + parse  20.3 ms

  querying for: "ABACAN, RAPHAEL"

    exact name     "ABACAN, RAPHAEL"
                   -> "ABACAN, RAPHAEL" (bucket 0, 2.49 ms)
    PASS  exact name finds the right professor

    surname only   "ABACAN"
                   -> "ABACAN, RAPHAEL" (bucket 0, 0.06 ms)
    PASS  surname only finds the right professor

    lowercase      "abacan, raphael"
                   -> "ABACAN, RAPHAEL" (bucket 0, 0.05 ms)
    PASS  lowercase finds the right professor

    one deletion   "ABAAN, RAPHAEL"
                   -> "ABACAN, RAPHAEL" (bucket 1, 3.10 ms)
    PASS  one deletion finds the right professor

    transposition  "ABCAAN, RAPHAEL"
                   -> "ABACAN, RAPHAEL" (bucket 1, 0.33 ms)
    PASS  transposition finds the right professor

    typeahead      "ABACA" -> 5 hit(s)
    PASS  typeahead reaches the professor from a 5-char prefix
    PASS  a nonsense query returns nothing

    500 real name queries: 38 us each, called from JS
    PASS  mean query under 5 ms from JavaScript

  OVERALL: PASS
```

**A 174 KB WASM module and a 380 KB index, answering typo'd Filipino name queries in ~20 µs from
JavaScript.** No wasm-bindgen, so the same C ABI serves the browser, Node, an edge worker, and a
native `.so` for Go/Python/PHP/Ruby — one artifact, not one per ecosystem. It is deliberately the
ABI shape onegrid already ratified on this machine, so it drops into a socket that already exists.

### Scale - 61,467 real Philippine schools, extended to 1 M

| docs | build | index bytes | exact p50 | typo p50 | typo p99 | |
|---|---|---|---|---|---|---|
| **61,467** | 223 ms | 5.2 MB | **173 us** | **451 us** | **2.33 ms** | **real** |
| 250,000 | 778 ms | 19.3 MB | 334 us | 672 us | 6.34 ms | recombined |
| 1,000,000 | 3.2 s | 75.6 MB | **642 us** | **1,127 us** | **14.5 ms** | recombined |

**Sub-millisecond exact p50 at a million documents. The typo p99 fails a 5 ms bar above ~250 K**,
and that bar is the one number in this repo that has never been met at a million — see
[`docs/benchmarks.md`](docs/benchmarks.md) for where it does hold and why the ceiling is vocabulary
rather than document count.

`cargo run -p index-bench --release --bin scale` reproduces the table. Numbers are the minimum of
three runs on one Windows workstation; the build column is threaded and the query column is not.
Details and the remaining levers: [`bench/roadmap/p7-scale.md`](bench/roadmap/p7-scale.md).

### And the term dictionary in isolation

`fuzzy-term`, at profstopick's corpus size (11,949 labels / 12,029 distinct tokens): a **94 KB term
dictionary — 3.8 % of the 2.5 MB JSON index that app ships today** — takes typo recall from 6.8 % to
99.9 % at a p99 of 0.66 ms. Holds to 861 K distinct terms. Spec, thresholds, two open reds and one
refuted hypothesis: [`bench/roadmap/p5-fuzzy-term-feasibility.md`](bench/roadmap/p5-fuzzy-term-feasibility.md).

## Configuration

The engine reads **one** environment variable. Everything else it needs is passed in code.

| variable | default | what it does |
|---|---|---|
| `INDEX_PARALLEL` | unset (off) | `1` opts a `Searcher` into spreading its per-segment work across OS threads, for collections that also clear `PARALLEL_WORK_MIN`. Anything else, including unset, stays single-threaded. |

**Threading is off unless asked for, and that is deliberate.** Spawning a thread per core per query
is a bet that the cores are free, and a library embedded in someone else's process cannot check.
Measured on one workstation running its owner's ordinary applications, the threaded path was
**2.2-2.4x slower** than serial at 25 and 50 segments; measured on a quiet box it was **2.9x faster
on the p99**. Both are real, so the default is the safe one. `Searcher::set_parallel(true)` is the
programmatic form for a process that does own its machine, and `Searcher::parallel()` reports the
decision. Full measurement: [`bench/roadmap/p80-parallel-default.md`](bench/roadmap/p80-parallel-default.md).

The benchmark harness reads a further ~20 variables — corpus paths and sweep parameters, none of
which the library itself consults. They are documented in [`bench/README.md`](bench/README.md).

## Build & test

```sh
cargo test --workspace              # engine + learned-index invariants + image tier + CLI
bash scripts/cli-smoke.sh           # the `index` CLI end to end: build, apply, search, over a pipe
node js/opfs-check.mjs              # the OPFS tier: persists in a real browser, answers offline

python -m http.server 8080          # then open /js/compare.html -- profstopick's real matcher,
                                    # this engine, and both merged, side by side on one corpus

cargo run -p index-bench --release --bin real-corpus       # THE ENGINE vs production corpora

./scripts/build-wasm.sh    # ALL FOUR artifacts side by side into dist/, with gzipped sizes
cargo build -p index-wasm  --release --target wasm32-unknown-unknown   # the search binding
cargo build -p index-accel --release --target wasm32-unknown-unknown   # onegrid's kernels
node js/demo.mjs                                           # end-to-end, in Node

cargo run -p index-bench --release --bin beat-btreemap     # learned index vs BTreeMap
cargo run -p index-bench --release --bin crack-converge    # adaptive cracking convergence
cargo run -p index-bench --release --bin fuzzy-decision    # fuzzy-over-FM viability (set INDEX_BENCH_SIGMA)
cargo run -p index-bench --release --bin fuzzy-term        # typo-tolerant term dictionary feasibility
```

### The image tier

```sh
cargo run -p index-bench --release --bin image-corpus       # the whole tier on a real web scrape
cargo run -p index-bench --release --bin video-shot         # video as SHOTS, not frames
node js/image-smoke.mjs                                     # a fused query, in Node
python host/python/index_ffi.py                             # ...and through the C ABI from Python
```

The image tier ships **no model**, so the two measurements that need one are driven by host-side
scripts — which is the architecture, not a workaround: an embedding is an *input*, and a transcode
is the host's decision (`docs/research/image.md` §8, `bench/roadmap/p61-content-address.md`).

```sh
# 1. ask the benchmark which documents it indexes, and in what order
cargo run -p index-bench --release --bin image-corpus -- --emit-manifest manifest.tsv
# 2. embed exactly those, in exactly that order  (needs torch + transformers)
python scripts/embed-corpus.py manifest.tsv embedding.bin
# 3. now p58's recall verdict is real instead of withheld
cargo run -p index-bench --release --bin image-corpus -- --embedding embedding.bin

# p61 acceptance 4: does a lossless transcode round-trip BYTE-EXACT?  (needs `scoop install libjxl`)
python scripts/jxl-roundtrip.py
```

**Without `--embedding`, `image-corpus` refuses to report a recall verdict** rather than quietly
scoring seeded noise. That is deliberate: a recall figure over made-up vectors measures the
arithmetic, not the retrieval. It prints `[HELD]` and says so.

Size overrides: `INDEX_BENCH_N`. Epsilon override: `INDEX_BENCH_EPS`.
Corpus overrides: `INDEX_IMAGE_CORPUS`, `INDEX_VIDEO_CORPUS`.
`real-corpus` reads the two corpora from sibling checkouts; set `INDEX_CORPUS_DIR` to relocate them.
A missing corpus is skipped and announced — the bench never invents data.
`INDEX_DIAG=1` additionally asserts MaxScore's pruned results are identical to exhaustive scoring.

`fuzzy-term` exits non-zero today **on purpose** — it is a `bench/roadmap/` item with two documented
reds, and it is excluded from the gate until the feature ships.

### Windows / no Visual Studio

This machine has no MSVC linker (no Visual Studio / Build Tools), so the project uses Rust's
**GNU toolchain**, which bundles its own linker:

```sh
rustup toolchain install stable-x86_64-pc-windows-gnu --profile minimal
rustup default stable-x86_64-pc-windows-gnu
```

The toolchain is intentionally *not* pinned via `rust-toolchain.toml` — that would break the
default MSVC/Linux/macOS builds elsewhere. Install MSVC Build Tools (C++ workload) if you prefer
the native toolchain.

## Dependency policy

`index-core` is **dependency-free** and stays that way. Benchmark and integration crates may take
dependencies, and every candidate is audited against crates.io download counts before adoption —
see [`docs/research/build-or-buy.md`](docs/research/build-or-buy.md). The rule:

> Reimplementing something with 8-digit recent downloads costs weeks and produces something worse.
> Reimplementing something with 3-digit recent downloads costs weeks and produces something with a
> maintainer.

`index-text` depends on `tantivy-fst` and `levenshtein_automata` — the exact stack Meilisearch and
Tantivy ship, at 4.2 M and 4.4 M recent downloads. `index-bench` adds `serde_json` to read the
production corpora. **`index-core` remains dependency-free.**

## Honest limits (today)

- **The core's original thesis did not survive contact with its own consumers** (see Status). Rows
  are now admitted only with a named consumer and a measurement that consumer already takes.
- The PLA build is the **optimal convex-hull PLA** (O'Rourke / PGM) — minimum ε-bounded segments,
  geometry decided in exact `i128` arithmetic. `build_greedy` is kept as the comparison baseline.
- **ε matters for the tail.** ε=64 loses p99 to `BTreeMap` on irregular data at 10M (the 129-key
  last-mile window thrashes cache); ε=16 wins everywhere. Default is 16.
- **`PgmIndex` is not faster than the single-level `PlaIndex` up to 10M.** Kept because it should
  matter at much larger scale; no further investment is scheduled.
- Latency is measured with **`rdtsc`** (~10 ns granularity here), reported as **median p99 over 5
  passes** (desktop p99 is noisy ±~100 ns). Overhead is calibrated and subtracted. The bytes/key win
  is exact.
- Real SOSD 200M-key datasets aren't bundled; `data.rs` generates distribution stand-ins and
  `beat-btreemap <file>` loads real SOSD binaries via `load_sosd_u64` when present.
- **Fuzzy-over-FM is sharply k-bounded** (measured): viable to k≤2 at σ=26, only k≤1 at σ=256. This
  negative result is what redirected the fuzzy work onto an FST + Levenshtein automaton, which
  measures far better — see [Headline measurement](#headline-measurement).
- **The `fuzzy-term` spike still runs on a synthetic corpus** (`real-corpus` does not — it uses the
  production exports). profstopick's real *shard* is not committed; generating it needs a database.
- **`real-corpus` depends on sibling checkouts being present**, so it is gate-excluded until the
  corpora (or a fixture subset) are vendored here. A gate that silently skips is not a gate.
- **The engine has not been run inside any consumer application yet.** It is measured against their
  data, not wired into their code. A WASM artifact and a C ABI both exist and are exercised from
  JavaScript and Python; no napi-rs binding does.
- **1 M documents still misses the interactive p99 bar.** The bar holds to roughly 250 K real
  documents and not beyond; `p56` measured 8.29 M real rows at a 33 ms typo p99. The ceiling tracks
  vocabulary rather than document count. Postings compression shipped as `p88` (IDXTEXT11, mode-coded
  lists: −37.5 % of the presyo index, −59 % at 1 M, latency at parity); docID reordering and SIMD block
  decode remain unbuilt.
- **Nothing is deployed.** All four integrations are measurements on throwaway worktree branches; no
  `main` was modified and no PR opened. [`docs/adoption.md`](docs/adoption.md) has the per-app plan,
  gate, cost and rollback so that becomes one decision rather than an investigation.
- **Segmented scoring perturbs the order of near-equal documents.** `Searcher` lets documents be
  added without a rebuild, but BM25 statistics are per-segment, so a *broad* query matching most
  of the collection can reorder its near-ties. A *selective* query still identifies the same
  document — that split is asserted in `searcher::tests::ranking_skew_is_bounded_for_a_small_delta`
  rather than assumed. `needs_compaction()` says when to rebuild from the source of truth.
- **The browser tier persists, on one browser.** The index is stored in OPFS and survives a page
  navigation, and a query can be answered from byte ranges without reading the whole file. Only
  Chromium is checked; Safari's `opfs-sahpool` behaviour is untested here, and Safari evicts
  script-written storage after 7 days regardless. Nothing writes to OPFS from the engine side — a
  browser can persist and query an index, but not update one in place.
- **The 260 K presyo run is padded** — only 1,940 rows are real exported data, and it does not
  populate their `search_text` column or their ~296 K aliases.
- **sisia's hybrid path is still untouched** — the `ts_rank_cd` sparse arm fused with pgvector by
  RRF and reranked by Vertex needs a database, a corpus and an API key this machine does not have.
  Its *catalog* search is now measured on its own data (`bench/roadmap/p8-sisia-catalog.md`).
- **presyo's input contract is deliberately unsatisfied.** It pins
  `productSearchToken('Coca-Cola 1.5L') === ['coca','cola','1.5l']`; the analyzer produces `1500ml`,
  because canonicalizing the unit is what makes `1.5L`/`1500ml`/`1.5 liters` one token — worth
  **+4.6 pp recall** by presyo's own measurement. Matching it verbatim would be a regression.
- **The tail at 1 M is characterized, not fixed** — 8.1–8.8 ms typo p99 on a REAL million-row
  corpus against a 5 ms bar (13.6 ms on the recombined one, which overstates it ~1.6x). Three
  independent attacks have now each moved it under 10 %: better seeding (`p27`), capping expansion
  (`p29`), and tightening the pruning bounds (`p47`). It is the cost of enumerating documents the
  bucket-first ranking rule genuinely requires visiting. **The bar is met on real corpora up to roughly 250 K
  documents** — 4.54 ms at presyo's 241,677 products, 1.81 ms at 61,467 real schools — and fails
  above that on real data too, not merely on recombined rows: `p56` measured 8.29 M real rows at
  33 ms. An earlier revision of this line claimed the bar held on *every* corpus of real documents,
  which was true only because the largest real corpus available at the time was 241,677 rows.
- **Compaction is a rebuild.** `Searcher` appends segments and tombstones documents without one,
  but postings are never rewritten, so collection statistics still count deleted rows until the
  application rebuilds from its own database. `Searcher::needs_compaction` says when that matters.
- **Above 61,467 documents the corpus is recombined, not observed.** Real tokens, synthetic
  combinations; it measures posting-list and top-k scaling, not vocabulary growth on new text.
- **CI runs the gate on Linux** (`.github/workflows/gate.yml`: tests, doc tests, clippy, the three
  WASM artifacts, `js/smoke.mjs`, `js/image-smoke.mjs`, `scripts/cli-smoke.sh`, `js/opfs-check.mjs`
  in real Chromium, and per-point geo agreement). **`pool-audit` is NOT in it** — the ranking gate
  that every merge in this repo is checked against runs by hand, because it needs sibling corpora
  that are not vendored here. Nor are `host/python/index_ffi.py`, `js/demo.mjs` or
  `js/accel-bench.mjs`, so "both hosts pass" means one gated host and one checked by hand.
- The FM-index rank `cum` array is u32-per-word (~50 % overhead); `sucds` ships the two-level rank
  that would trim it.
