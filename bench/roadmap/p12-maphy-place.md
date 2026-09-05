# P12 — maphy's place search: a typo returns nothing

**Tier:** T1 · **Bin:** `cargo run -p index-bench --release --bin maphy-place`
**Status: MEASURED 2026-09-05; the gap it exposed was then FIXED the same day.**

> **This file was rewritten after its first conclusion turned out to be wrong.** The original
> reported an engine loss on typeahead and attributed it to a missing static prior. Both halves were
> wrong, and the correction is recorded in full below rather than quietly edited out, because the
> wrong version had already been used to reprioritize the roadmap.

## The workload

`maphy/apps/web/src/components/map/lib/place-index.ts` implements `searchPlace()` as a **linear scan
over the whole loaded pool on every keystroke**: score every entry and every alias, collect all
non-zero hits, sort, slice 24. The scoring function is four string predicates:

```text
norm === q                        -> 1000
norm.startsWith(q)                ->  700 - min(len, 99)
some word in norm startsWith(q)   ->  400
norm.includes(q)                  ->  150
otherwise                         ->    0
```

**All four are exact-substring tests, so the score of a misspelling is exactly zero.**

## The corpus, and what is honestly missing

`bench/fixture/maphy-place.txt` — **1,067 real Philippine places** (17 regions, 84 provinces, 966
municipalities with PSGC codes), decoded from maphy's own `municipal_covid19_summary.pmtiles`. That
is the shape of `top.json`.

**maphy's second tier is not on disk.** `barangay.json` is ~42,000 more entries and
`apps/web/public/data/place/` is empty in this checkout, so this measures **the pool that loads
first**, roughly 40× smaller than the shipped worst case.

## Measured

| query class | engine@1 | engine@10 | maphy@1 | maphy@10 |
|---|---|---|---|---|
| exact place name | 100.0 % | 100.0 % | 100.0 % | 100.0 % |
| typeahead (first 5 chars, answerable only) | 76.2 % | 96.4 % | 78.6 % | **100.0 %** |
| **one-character typo** | **89.0 %** | **99.7 %** | 8.9 % | 9.3 % |

| | p50 | p99 |
|---|---|---|
| engine | 27.5 µs | 68.3 µs |
| maphy (linear scan) | 31.3 µs | 70.5 µs |

**maphy returns an empty list for 3,663 of 4,038 typo queries — 90.7 %.**

```
typed "NATIONAL CAPIATL REGION (NCR)"   ->  NATIONAL CAPITAL REGION (NCR)
typed "REGION I (IOLCOS REGION)"        ->  REGION I (ILOCOS REGION)
typed "CORDILLERA ADMINITSRATIVE ..."   ->  CORDILLERA ADMINISTRATIVE REGION (CAR)
```

## The win

**Typo tolerance, 99.7 % against 9.3 %, and it is not close.** A place-name box is exactly where
misspellings happen — the names are long and unfamiliar — and today a single transposed letter
returns an empty list rather than a near miss.

**Latency is a tie** at this pool size (27.5 vs 31.3 µs p50). A linear scan over 1,067 short strings
is not slow, and claiming a win there would be dishonest. The engine's advantage is asymptotic and
should appear at the 42k barangay tier, which is **not measured here**.

## The gap, correctly diagnosed — and closed

Before the fix below, the engine lost typeahead **94.6 % against 100 %**, and the misses said
exactly why:

```
"CAN-A" -> CAN-AVID       "DEL C" -> DEL CARMEN     "DON C" -> DON CARLOS
"DON M" -> DON MARCELINO  "LA CA" -> LA CASTELLANA
```

**Every miss is a prefix that ends part-way through a second token.** The engine tokenizes the
query, so `"DEL C"` becomes exact-`del` plus prefix-`c*` — and `del` is a common token while `c*`
matches almost everything, so the correct document is not ranked. maphy's `norm.startsWith(q)` tests
the **whole normalized string**, which is strictly more precise for this query shape.

That is a real engine gap and a specific one: **in prefix mode the engine loses the string-level
anchoring that a typeahead needs.**

### Built, and it moved the number

`UNANCHORED_KEEP` plus a per-document `first_term` (the term id of the first token of field 0, four
bytes per document) restores the lost fact. In prefix mode, documents whose first token is **not**
one the query's first group expanded to keep only half their score.

| | before anchoring | after |
|---|---|---|
| typeahead hit@10 | 94.6 % | **96.4 %** |
| typeahead hit@1 | 75.7 % | **76.2 %** |

It is applied as a **demotion of the non-anchored**, never a boost of the anchored — the same
discipline the static prior uses, and for the same reason: retrieval prunes against upper bounds, so
a factor above 1 would let a true score exceed the block maxima the pruner trusts and silently drop
valid results on corpora large enough for pruning to engage. A factor `<= 1` leaves every bound
valid and the pruning code untouched. Relative order among non-anchored documents is unchanged, so
the signal only ever promotes documents the user is plausibly typing from the start of.

It fires **only for multi-token prefix queries**. A full query is not necessarily typed from the
start of a name — `"carmen"` should still find `DEL CARMEN` unpenalized — and for a single-token
prefix the term *is* the whole query, so anchoring adds nothing scoring does not already say.

Both production-corpus benches (`real-corpus`, `sisia-catalog`) were re-run and are unchanged, which
is the check that matters for a global scoring change.

The residual 3.6 points are mostly further collisions — `"SAN A"` alone covers SAN AGUSTIN ×2, SAN
ANDRES ×2, SAN ANTONIO ×2 — i.e. the same unanswerable shape as `"CITY "` but just under the
exclusion threshold. Chasing it further would be tuning to this fixture.

## Two methodology bugs this benchmark produced before the code did

**1. Exact-name rank 1 read 91.8 % and failed a 95 % gate.** The cause was the metric: **137 of
1,067 entries (12.8 %) share a name with another place** — "Quezon" names six municipalities — so
demanding one specific row at rank 1 demands the impossible and would score a *perfect* ranker at
about 87 %. Scored against the set of same-named places, both systems get 100 %.

**2. The typeahead row reported a spurious loss, and it moved the roadmap.** Queries were each
name's first five characters, so **all 40 municipalities named `CITY OF ...` issued the identical
query `"CITY "`, and all 17 regions issued `"REGIO"`**. No ranker can put 40 documents in 10 slots,
so those queries measured which arbitrary ten a tie-break happened to pick. 137 of 1,067 queries
were unanswerable in this way. Excluding them moved the engine 85.2 % → 94.6 % and maphy 90.4 % →
100 %, and changed the diagnosis completely.

**The wrong diagnosis had consequences worth recording.** From the spurious loss I concluded that
BM25F needed a *static prior* to express maphy's `LEVEL_RANK`, wrote that into `ROADMAP.md` as the
next row, and implemented it. The hypothesis was then tested directly by sweeping the prior's
strength:

| prior spread | 0 | 0.001 | 0.01 | 0.1 | 1.0 |
|---|---|---|---|---|---|
| typeahead hit@10 | 85.2 % | 85.2 % | 85.2 % | 85.2 % | 85.2 % |

**Identical at every strength, including off.** A prior that changes nothing cannot be the
explanation for a gap — which is what sent the investigation to the real cause. The lesson is
narrow and worth keeping: *a fix that does not move the number was never addressing the number*.

That is the fifth time in this repo a methodology was wrong before the code was (`p7-scale.md`
document replication, `p9-sfc-2d.md` sampling a cover, `p11-geo-wasm.md` a shared input parser, and
both entries here). **Every one was caught by a number sitting suspiciously close to a structural
constant of the data** — 12.8 % duplicate names, 40 `CITY OF` collisions — which is worth naming as
a habit rather than luck.

## What static priors are still good for

The feature was built on wrong evidence but is not wrong to have: `IndexBuilder::add_with_prior`,
normalized into `(0, 1]`, serialized in `IDXTEXT2`, and asserted not to override `typo_bucket`.
`docs/adoption.md` independently records that every consumer has a query-independent importance
signal — presyo store trust, profstopick rating counts, sisia catalog level. **What is now missing
is a measurement showing it wins on one of them**, and this bench is not that measurement. It is
listed as unproven rather than as a feature with evidence.

## Not measured yet

- **The barangay tier** (~42,000 entries) — the only place the latency claim can be tested.
- **A prior that pays** — the feature exists; no consumer benchmark yet shows it improving ranking.
- **Aliases.** `place-index.ts` scores alias strings; the fixture carries none.

## Reproduce

```sh
cargo run -p index-bench --release --bin maphy-place
INDEX_BENCH_PRIOR=0 cargo run -p index-bench --release --bin maphy-place   # prior off
```
