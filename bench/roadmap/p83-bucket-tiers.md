# P83 — bucket-tiered enumeration: tried, measured, reverted

**Tier:** T1 · **Check:** `cargo test -p index-text` (the exhaustive oracle), `pool-audit`, `scale`, `real-million`
**Status: TRIED, MEASURED, REVERTED, 2026-09-08. Exact everywhere it ran; slower everywhere it
mattered; the last named lever for the typo tail is closed by measurement, not by argument.**

`p47` named the one remaining lever for the 5 ms typo bar:

> a genuinely new enumeration strategy (bucket-tiered candidate generation, where documents matching
> every group are produced by intersection before anything else is scanned) or accepting the shape
> of the ranking rule.

`p55` repeated it as "still unbuilt". It is now built, measured, and reverted. This document exists
so the lever is closed the way `p27` closed champion seeding: with numbers, so nobody spends a week
rediscovering it.

## What was built

Planning already splits into per-`(group, distance)` term lists, so the candidate set decomposes by
**bucket shape** — `bucket` is the sum over groups of the best edit distance each group matched at,
and a missed group costs `MISSING_TERM_PENALTY` = 3. An arm of shape "one group at distance ≤ 2,
the rest exact" therefore contains every document with bucket ≤ 2 whose distance comes from that
group. Five shape families cover buckets 0..=2 completely:

| total | shape | arms |
|---|---|---|
| 0 | every group exact | 1 |
| 1 | one group at distance 1 | n |
| 2 | one group at distance 2 | n |
| 2 | two groups at distance 1 | n·(n−1)/2 |

Each arm drove the smallest side of its shape (a group union walked via the block-skipping `seek`)
and binary-searched the rest, so an arm cost roughly its smallest union. Scored candidates fed the
same two pools as the scan. Because `eff` is exactly lexicographic `(bucket, score)` and
`bucket_scale` exceeds every achievable score, a COMPLETED sweep of totals 0..=2 makes every
unscored document (bucket ≥ 3) unable to enter the answer — so a full ranking pool whose worst
member sat at bucket ≤ 2 skipped the union scan outright. That was the win condition.

Correctness held: the exhaustive oracle (`block_max_pruning_agrees_with_exhaustive_or_at_scale`),
`pool-audit` (0.00 % in all six real cells) and `prune-consistency` all passed with the tiers live,
after the exhaustive test caught a real duplicate-admission defect during development — tier
candidates re-admitting champion-seeded documents, which duplicated entries in a not-yet-full
ranking pool (admission only refuses duplicates by strict-`eff` comparison) and under-counted
distinct results. The fix (champion skip via the sorted `seeded` vec, tier docs merged back into it
in one sort) is worth recording: **any pre-scoring phase that feeds the pools must participate in
`seeded`, or the pool's own capacity accounting lies.**

## Why it loses

**1. The abort that bounds the cost is the abort that forfeits the win.** An arm with no score
bound over its remaining candidates cannot stop early without giving up exhaustiveness — so the
only safe stop is "the pool filled", and a pool filled mid-arm means the tiers can no longer claim
the buckets they were walking. The early exit requires COMPLETED arms; the cheap arms are the
aborted ones. Both cannot hold at once.

**2. The scan prunes; the tiers cannot.** The union scan's block-max MaxScore skips whole ranges of
the fat lists; a tier arm walking an intersection scores every candidate it visits. On the
recombined 1 M corpus — vocabulary fixed, every list fat — that is exactly the wrong trade, and the
measurement shows it uniformly:

| `scale` @ 1 M | scan (baseline) | tiers | Δ |
|---|---|---|---|
| exact p50 | 606 µs | 657 µs | **+8.4 %** |
| typo p50 | 1,124 µs | 1,278 µs | **+13.7 %** |
| typo p99 | 14.03 ms | 16.02 ms | **+14.2 %** |

**3. On the shape the tail actually has, the tiers are neutral at best.** `real-million`'s corrupt()
transposes one adjacent character — Levenshtein distance **2** — so the matching documents sit in
bucket 2. Tier 0 and tier 1 find nothing; the tier-2 arm either aborts early (pool already full of
champions — `rank_cap` is seeded before the tiers run) and the scan runs anyway, or walks the
bucket-2 intersection that the scan would have pruned:

| `real-million` @ 1.2 M real rows | scan (baseline) | tiers |
|---|---|---|
| typo p50 | 686 µs | 764 µs |
| typo p99 | **5.32 ms** | 5.34 ms |
| `pool-audit` presyo p50 | 156 µs | 172 µs |

An earlier, less complete variant (tiers 0–1 only, no abort, no champion dedup) measured **6.20 ms**
on the same bench — the regression that exposed both the duplicate-admission defect and the
missing distance-2 shape.

## The verdict this closes

`p27` (seeding, two budgets), `p29` (the cap), `p47` (every bound tightened to exactness) and now
`p83` (the new enumeration) are four independent attacks on the same tail, and all four read the
same verdict: **the 1 M typo tail is the cost of ranking every expansion of every token, and the
union scan with block-max pruning is already the right algorithm for it.** There is no fifth bound
and no sixth arrangement of the same work that reaches 5 ms at a million; what remains is pricing,
not search.

The lever `p47` named is therefore closed by measurement, and the typo-bar decision it was holding
open resolves to the option `p55` already priced: publish the envelope, and hand hosts beyond it
the measured cap. See the ROADMAP's next-list row 5, now decided.

## Reproducing

The tier implementation lived only in the working tree (250 lines over `Index::search_opt`) and is
deliberately not committed; this document is its record. The numbers above were interleaved on one
quiet machine, 2026-09-08, against `real-million` with the corpus regenerated by the recipe in its
header and `scale`/`pool-audit` on their committed fixtures.
