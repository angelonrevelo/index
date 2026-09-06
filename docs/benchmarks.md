# Benchmarks

> Every number here is reproducible from a command in this repo, on real documents, and **includes
> the ones that fail.** Where a bar is missed it is left red and labelled.

This file exists because of something the field survey found rather than because benchmarks are
nice to have. From [`research/landscape.md`](research/landscape.md) §2:

> **Almost nobody in the embedded category publishes p50/p99 at 100 k / 1 M / 10 M docs.** Only
> Quickwit and LanceDB Enterprise have real percentile tables. Any latency grid you see for
> Tantivy, Bleve, MiniSearch, Orama, FlexSearch or Pagefind is **reconstructed, not published**.
> That absence is itself an opportunity.

And §7.3, on the other absence:

> **No search product publishes an end-to-end p99 index-visibility number.** Not Debezium, not
> Algolia, not Meilisearch.

So: the grid, and the staleness contract.

**Machine.** One Windows workstation, Samsung MZALQ512HALU NVMe. **Every query number here is
single-threaded**; since `p69`/`p73` the *build* is threaded, which is why the `build` column above
carries a staleness note and the query columns do not.
`rdtsc` timing with calibrated overhead subtraction (`crates/index-bench/src/timer.rs`). Numbers
were taken while a parallel build was sometimes running on the same box — where that mattered,
arms are timed **interleaved in one process** rather than compared across runs, and it is said so.

---

## 1. The scaling grid — real documents, real vocabulary

> ### ⚠ This grid predates `p69`, `p73`, `p75` and `p79`, all of which moved it
>
> The engine has changed underneath these numbers, in the two columns they are read for. Stating it
> here rather than quietly leaving the table wrong:
>
> - **`p69` (delta-varint postings) roughly halved the `bytes` and `B/doc` columns.** Re-measured on
>   the same bin at the rungs it covered: **100 K 106.4 → 60.0**, **250 K 105.4 → 54.2**,
>   **500 K 101.3 → 51.5 B/doc**. The 1 M–8 M rungs are **not** re-measured. A 10 M index should now
>   be estimated at roughly **500 MB, not 910 MB**.
> - **`p69` also improved the tail** it was expected to charge for: **typo p99 at 250 K
>   5,183 → 3,286 us**, at 500 K 5,601 → 5,392 us. The 5 ms bar's crossing point therefore moved
>   **up** from ~250 K, and the "holds to ~250 K" verdict below is now **conservative** rather than
>   current.
> - **`p73`, `p75` and `p79` together cut the `build` column by ~3.6x.** On the `scale` bin's
>   million recombined documents, measured on this tree: **18,436 → 5,166 ms**, with the serialized
>   bytes identical at every rung throughout. `p73` threaded the build, `p75` rewrote the tokenizer
>   byte-wise, `p79` deleted the per-token allocation. The `build` figures below are single-threaded
>   pre-`p73` numbers and are now an upper bound by roughly that factor.
>
>   *Both figures are a minimum over repeated runs on a workstation running its owner's normal
>   desktop applications — this machine never presents a genuinely quiet box, and a minimum is the
>   right estimator when the only available noise adds time.*
>
> **Why it is not simply re-run here.** The corpus is a live pull from presyo's `raw_product`;
> `bench/fixture/presyo-1m.tsv` is not committed. Re-timing it also requires an otherwise-idle box,
> and this grid is worth exactly one authoritative re-run once the current work lands — a benchmark
> taken next to three running builds is not a measurement.


**Corpus:** 8,290,639 real product names from presyo's `raw_product` and `extraction_insight`.
Queries are real product names drawn from the indexed slice, then corrupted by transposing one
adjacent character pair. `k = 10`.

| documents | terms | build | bytes | B/doc | exact p50 | typo p50 | **typo p99** | 5 ms bar |
|---|---|---|---|---|---|---|---|---|
| 100,000 | 49,540 | 0.9 s | 10.6 MB | 106.4 | — | 329 us | **2,057 us** | **PASS** |
| 250,000 | 84,637 | 2.7 s | 26.4 MB | 105.4 | — | 644 us | **5,183 us** | *just over* |
| 500,000 | 116,923 | 5.8 s | 50.7 MB | 101.3 | — | 766 us | 5,601 us | FAIL |
| 1,000,000 | 151,866 | 9.0 s | 94.4 MB | 94.4 | — | 928 us | 6,947 us | FAIL |
| 2,000,000 | 356,062 | 18.7 s | 183.7 MB | 91.8 | — | 1,204 us | 10,683 us | FAIL |
| 4,000,000 | 1,234,225 | 44.4 s | 374.5 MB | 93.6 | — | 1,815 us | 17,958 us | FAIL |
| **8,000,000** | **1,263,840** | **84.2 s** | **729.4 MB** | **91.2** | — | **3,024 us** | **33,619 us** | FAIL |

```sh
# 10 M of real text does not exist in this estate; the ladder runs to what is real.
INDEX_MILLION_TSV=presyo-8m.tsv cargo run -p index-bench --release --bin real-million
```

**Read it this way:**

- **The 5 ms typo-p99 bar holds to ~250,000 documents.** Not 1 M, which is where `p7` set it. Two
  independent corpora agree: this ladder crosses between 100 K and 250 K, and presyo's real
  241,677-product catalogue measures **4.54 ms — PASS**.
- **Capacity is not the ceiling.** 8.29 M builds in 84 s at 91 B/doc and nothing degrades
  non-linearly. 10 M extrapolates to ~910 MB.
- **The median is the number nobody quotes and it is the good one**: under a millisecond to a
  million, 3 ms at eight million.
- **The tail tracks VOCABULARY, not documents.** 250 K → 500 K doubles the corpus for +8 % tail;
  2 M → 4 M costs +68 % because vocabulary jumps 3.5x. That is `p5`'s finding at production scale.

### The recombination correction

`scale` builds its ladder by recombining 61,467 real school records, which holds vocabulary **fixed**
at 41,069 terms while multiplying documents:

| 1 M documents | typo p99 | vocabulary |
|---|---|---|
| `scale`, recombined | 13.58 ms | 41,069, fixed |
| `real-million`, real | **6.95 ms** | 151,866, grown |

Recombination **overstates the tail ~2x**. It did not invent the failure — the bar is missed either
way — but any number derived from it is pessimistic and labelled as such. See
[`p55`](../bench/roadmap/p55-real-million.md).

### The one lever, priced

`Index::search_capped(q, k, cap)` limits dictionary expansions per token. At 8 M documents:

| cap | typo p99 | top-10 identical | rank-1 identical |
|---|---|---|---|
| **16 (default, exact)** | 32,501 us | 100 % | 100 % |
| 8 | 25,405 us | 98.90 % | 99.30 % |
| 4 | 23,678 us | 95.95 % | 97.35 % |
| 2 | 15,253 us | 90.10 % | 93.20 % |

**Nothing reaches 5 ms at this scale.** Reported so an adopter can price the trade rather than
discover it.

---

## 2. The staleness contract

Nobody publishes this. Here it is.

**Question:** if you feed the index a change stream instead of rebuilding, how far does it drift
from a rebuild of the same rows — and how fast?

**Method:** 8,000 operations (insert / update / delete, seeded and mixed) against a model of the
table, on 13,000 real business names. Membership is compared exactly; ranking is compared against a
full rebuild. `cargo run -p index-bench --release --bin cdc-equivalence`

### Membership — exact, and gated

| | |
|---|---|
| operations applied | 8,000 |
| keys resolving wrongly | **0** |
| final model rows vs index live keys | **14,288 = 14,288** |

**Membership is exact at every checkpoint.** No scoring is involved, so there is no tolerance here:
a live row set that disagrees with the source of truth is a bug, not drift.

### Ranking — measured, not gated

| operations | deleted | segment skew | `needs_compaction` | rank-1 vs rebuild | top-10 overlap |
|---|---|---|---|---|---|
| 400 | 1.7 % | 2.3 % | no | **92.7 %** | 90.0 % |
| 1,200 | 5.1 % | 6.5 % | no | 90.3 % | 87.2 % |
| 2,400 | 9.5 % | 12.2 % | no | 89.7 % | 85.6 % |
| 4,400 | 15.8 % | 20.3 % | **YES** | ~90 % | ~84 % |
| 8,000 | 24.8 % | 31.6 % | YES | **88.7 %** | 82.0 % |

**The contract, stated plainly:**

1. **Membership never drifts.** A row present in the source is present in the index, and a deleted
   row is gone, operation for operation.
2. **Ranking drifts by ~7–8 points of rank-1 agreement over 8,000 operations**, and then roughly
   stops — the decay is front-loaded, not compounding.
3. **Both causes are removed by one rebuild**, and `index stat` tells you when: `needs_compaction`
   fires at 20 % deleted or 20 % skew.
4. The residual ~8 % at the *first* delta is **not segmentation** — it is that a deleted row still
   counts toward document frequency until a rebuild, exactly as Lucene behaves between merges.

Before [`p52`](../bench/roadmap/p52-collection-statistics.md) the decay was 9 points and compounding
because each segment scored IDF against its own corpus. Collection-wide statistics flattened it.

---

## 3. Segmentation cost

Same rows, split into N segments, against a monolithic index. presyo, 241,789 real products.

| segments | p50 | typo p99 | broad overlap | selective rank-1 | selective overlap |
|---|---|---|---|---|---|
| 1 (control) | 18 us | 3,206 us | 100 % | 100 % | 100 % |
| 2 | 39 us | 9,593 us | 84.45 % | **99.80 %** | 94.98 % |
| 5 | 134 us | 14,239 us | 84.41 % | 99.20 % | 93.62 % |
| 10 | 615 us | 16,990 us | 85.80 % | 99.40 % | 93.70 % |
| 25 | 2,939 us | 38,965 us | 87.01 % | 99.00 % | 92.60 % |
| 50 | 5,592 us | 29,898 us | 84.94 % | **99.40 %** | 92.38 % |

**Latency is the real cost — 18 us to 5,592 us, ~310x at 50 segments.** A query runs against every
segment. Keep segment counts in single digits; `needs_compaction` enforces it.

**Ranking quality is now flat in segment count** (99.0–99.8 % rank-1 at every depth) because of
collection-wide statistics. Before `p52` it degraded from 98.0 % to 96.2 % and broad overlap sat at
51–64 %.

Collection-wide statistics themselves cost **+13 % to +25 % on p50** at the recommended segment
counts and **~2x on the typo tail** — a second dictionary expansion per segment, timed interleaved.
`Searcher::set_collection_stat(false)` trades it back.

---

## 4. The browser tier

`node js/opfs-check.mjs` — a real Chromium, the same `.idx` and `.wasm` the server uses.

| | |
|---|---|
| first visit, over HTTP | 17.7 ms, 220,470 B (386,043 B before `p69`) |
| **second visit, from OPFS** | **2.8 ms** |
| network calls during `search()` | **0** |
| **per query, in-tab** | **0.086 ms** |
| localhost round trip, *empty work* | 2.300 ms |
| **speed-up over a server that does nothing** | **~27x** |
| bytes to learn the file's layout | **296** (0.077 % of the file) |

The index it replaces is profstopick's 2,505,813-byte JSON shard, which occupies **95.6 % of the
5 MB localStorage quota**; this is **8.8 %** of it, in a quota measured in gigabytes.

The localhost comparison is deliberately the *friendliest possible* server: no TLS, no queue, no
work, no distance. A deployed search service is strictly slower than that floor.

---

## 5. Correctness gates

These run on every change and are not performance numbers — they are the reason the performance
numbers are allowed to exist.

| gate | what it proves | result |
|---|---|---|
| `pool-audit` | pruned search == brute force, on 6 real query sets | **0.00 % disagreement in all 6** |
| `real-corpus` | recall on two production corpora | **100 % recall@10**, 99.8 % hit@1 |
| `cdc-equivalence` | change stream converges on a rebuild | membership exact over 8,000 ops |
| `phrase-cost` | phrase hits verified against raw text by an arm sharing no code | **980 hits, 0 wrong** |
| `facet-shop` | the two sort arms agree | 336 queries x 2 directions, **0 disagreements** |
| `cli-smoke` | `build`/`apply`/`search` over a pipe | applied stream == rebuild |
| `opfs-check` | index persists and answers offline in a real browser | PASS |

---

## 6. What fails, collected

Nothing here is hidden elsewhere in this file.

- **The 5 ms typo-p99 bar fails above ~250 K documents** — 6.9 ms at 1 M, 33.6 ms at 8 M. `p7` set it
  at 1 M; that was 4x too optimistic. Red and unraised for five documents running.
- **Prefix bleed is worse than `LIKE`.** On sisia's `CHEM 10x` problem the engine returns **1,391
  spurious rows against `LIKE`'s 295**. Better fuzzy matching means more permissive matching, and
  nobody has fixed it.
- **Ranking drifts under a change stream** — 92.7 % → 88.7 % rank-1 over 8,000 operations. Exact on
  membership, approximate on order, until a rebuild.
- **Segmentation costs ~310x p50 at 50 segments.** Usable only in single digits.
- **Every query is single-threaded.** Nothing shards a query across cores. `p73` threaded the
  *build* (1.89x at a million, bytes identical); the query path is untouched by it.
- **10 M is unmeasured** because 10 M of real text does not exist in this estate, and recombining to
  reach it would report a number ~2x worse than reality.
- **Learned-expansion queries still score per segment** — the expansion table stores term ids, not
  text, so those fall back to local document frequency.

---

## Reproducing

```sh
cargo test --workspace                                        # 268 tests
cargo run -p index-bench --release --bin real-corpus          # recall, two production corpora
cargo run -p index-bench --release --bin pool-audit           # pruning == brute force
cargo run -p index-bench --release --bin cdc-equivalence      # the staleness contract
cargo run -p index-bench --release --bin segment-scale        # segmentation cost
cargo run -p index-bench --release --bin real-million         # the scaling grid (needs the export)
bash scripts/cli-smoke.sh                                     # the CLI, end to end
node js/opfs-check.mjs                                        # the browser tier (needs playwright)
```

The 8 M corpus is one pipe from any Postgres that has the rows — `real-million` prints the exact
command when the file is absent.
