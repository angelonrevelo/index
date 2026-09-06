# P26 — one exact bound replaces three approximations

**Tier:** T1 · **Bins:** `scale`, `pool-audit`, `prune-consistency`
**Status: FIXED, 2026-09-05. Exact retrieval now costs nothing on exact-match queries.**

`p25` reached 0.00 % on every corpus and cost 2.25x on the `scale` corpus. This document removes
most of that cost by replacing the *approximate* soundness predicates with the exact one, and
records a bar that fails.

## The three predicates were all approximations of the same thing

By the end of `p25` there were three different tests guarding three pruning sites:

| site | predicate | shape |
|---|---|---|
| block skip | `prune_is_sound` — pool full **and** worst bucket is 0 | special case |
| non-essential bail | `prune_is_sound` | special case |
| term demotion | `bucket_floor[f] > worst_bucket` | strict inequality |

Each is *sufficient*, none is *necessary*, and the ranking pool already stores the exact quantity
they were all approximating. `eff = score - bucket * bucket_scale` with `bucket_scale` larger than
any achievable score is **exactly lexicographic** `(bucket, score)` — one number that decides
admission. So for any bound on an unseen document:

> **A document with `score <= S` and `bucket >= B` has `eff <= S - B * bucket_scale`.
> Skipping it is sound exactly when that cannot beat the ranking pool's worst `eff`.**

Both surviving sites take that form directly:

```rust
// block skip: range_bound over [candidate, range_end), range_floor over the same range
can_skip = range_bound - range_floor as f32 * bucket_scale <= worst_eff;

// demotion: prefix_sum[f] bounds the score, bucket_floor[f] floors the bucket
prefix_sum[f] - bucket_floor[f] as f32 * bucket_scale <= worst_eff
```

## Why the strict-bucket form was nearly useless, measured

The block-range floor was first written as `range_floor > worst_bucket`, by analogy with `p25`. It
fires on **0.02 %** of evaluations. Instrumenting 200,000 of them says why:

| | mean | median | max |
|---|---|---|---|
| `range_floor` | 3.3 | 3 | 24 |
| `worst_bucket` | 3.3 | 3 | 24 |

**They are the same distribution — floor and worst bucket are almost always equal.** A strict `>`
therefore almost never holds. And equality is not undecidable: on a bucket tie the comparison falls
through to score, which is precisely what `eff` already encodes. The approximation threw away the
common case.

This was worth measuring rather than reasoning about. The hypothesis before instrumenting was that
`range_floor` would be **0** on typo queries — a group is reachable if any of its up-to-16 expanded
terms has a cursor in range, so intuitively no group is ever unreachable. `range_floor` was 0 on
**none** of the 200,000 evaluations. The intuition was backwards and the fix it implied would not
have worked.

## Measured

All interleaved, medians of three pairs, 1 M documents.

**Against `p25`** — the same correctness, cheaper:

| | `p25` | **`p26`** | delta |
|---|---|---|---|
| exact p50 | 1,108 us | **476 us** | **-57 %** |
| typo p50 | 1,769 us | 1,116 us | -37 % |
| typo p99 | 20,199 us | 16,915 us | -16 % |

**Against `p24`** — what full correctness actually costs, now that it is bought efficiently:

| | `p24` (2.23 % wrong) | **`p26`** (0.00 % wrong) | delta |
|---|---|---|---|
| exact p50 | 504 us | **498 us** | **parity** |
| typo p50 | 985 us | 1,149 us | +17 % |
| typo p99 | 9,440 us | 17,272 us | **+83 %** |

**Exact-match retrieval is now correct at no measurable cost.** The entire price of exactness is
paid in the typo tail, which is the honest shape of the tradeoff: typo expansion is what puts many
terms in a group and many groups in a query, and that is what makes bucket-first ranking expensive
to prune.

`pool-audit` remains **0.00 % on all six real query sets**; `prune-consistency` green at every decoy
count; 103 tests; `real-corpus` and `sisia-catalog` both `OVERALL: PASS`.

## A bar that fails, stated plainly

`scale` reports **`OVERALL: FAIL`**: worst typo p99 is 17 ms against `p7`'s **5 ms** bar at 1 M
documents.

**This is not a regression introduced here.** The same interleaved run puts `p24` at 9.4 ms — also
over the bar — and `p24` is the build that predates every correctness fix in `p25` and `p26`. The
bar has been failing on this machine all day, in every arm measured, and no document said so. Cold
absolute numbers on a settled machine, for the record:

| docs | exact p50 | typo p50 | typo p99 |
|---|---|---|---|
| 61,467 (real DepEd) | 267 us | 561 us | 2.82 ms |
| 250,000 | 636 us | 1,071 us | 8.84 ms |
| 1,000,000 | 1,122 us | 1,751 us | 19.55 ms |

Two things are true and neither should be dropped:

- **"A million rows, milliseconds" holds for exact search** — 476 us p50 at 1 M — and for typo
  search at every scale up to 250 k.
- **It does not hold for the typo p99 at 1 M.** `p7` set that bar when ranking was unsound, and part
  of the old margin was pruning that discarded documents the ranking wanted.

Raising the bar to match the new behaviour would be marking one's own homework. It stays at 5 ms and
`scale` stays red until either the tail is fixed or the bar is changed for a stated reason.

## The next lever, named — and since measured and REVERTED

**`p27-group-seeding.md` built this and it does not work.** Per-group seeding improves the pool's
worst bucket from median 6 to 4 but leaves the bucket-0 fraction at 1.7 %, and costs +31 % exact p50
(+96 % at equal budget). The original text is kept below because the reasoning was sound and only
the measurement settled it.

## The next lever, named but not built (superseded)

`prune_is_sound` and the `eff` bound both key off the **ranking pool's worst member**, so the tail is
governed by how fast that pool fills with good documents. Champion seeding currently seeds from one
term, which raises the *score* threshold but not the *bucket* quality of the pool. Seeding from an
intersection across query groups would put low-bucket documents in the pool early, which is exactly
what both bounds need on the queries that currently scan everything. Not attempted here.

## Reproduce

```sh
cargo run -p index-bench --release --bin scale
cargo run -p index-bench --release --bin pool-audit
cargo run -p index-bench --release --bin prune-consistency
```
