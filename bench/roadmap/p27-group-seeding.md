# P27 — bucket-aware champion seeding: a negative result

**Tier:** T2 · **Bins:** `scale` (interleaved), instrumented seed audit
**Status: TRIED, MEASURED, REVERTED, 2026-09-06. The named lever for the typo tail does not work.**

`p26` closed with a named next step:

> Both bounds key off the ranking pool's worst member, so the tail is governed by how fast that pool
> fills with low-bucket documents. Champion seeding currently seeds from one term and raises the
> *score* threshold only. Seeding from an intersection across query groups is the named lever.

It was built. It loses on every axis. This document exists so the idea is closed with a measurement
rather than left as a plausible-sounding suggestion at the foot of another document.

## The reasoning, which is still correct

Since `p26`, both pruning bounds compare against the ranking pool's worst `eff`, which is
lexicographic `(bucket, score)`. So the pool's **bucket quality** — not just its score threshold —
governs how early pruning engages. Champion seeding takes the single term with the highest
`max_score`; its champions are all strong in *one* group and therefore mostly high-bucket. Seeding
one term per **group** is bounded by `groups x CHAMPION_SIZE` rather than `terms x CHAMPION_SIZE`
(which `p7` had already measured as a loss at 7.44 ms vs 6.02 ms), and a document appearing in
several groups' champion lists is exactly a low-bucket document found without scanning for it.

## It works, and it is not nearly enough

Instrumented over 19,993 real queries, reporting the ranking pool's worst member immediately after
seeding:

| | seeds scored | pool worst bucket, median | mean | **fraction at bucket 0** |
|---|---|---|---|---|
| single-term (`p26`) | 32 | 6.0 | 6.50 | **1.7 %** |
| per-group (`p27`) | 62 | 4.0 | 5.90 | **1.8 %** |

The seeds do get better — median worst bucket 6 to 4 — for roughly double the scoring work. But the
quantity that actually gates `prune_is_sound` is the **fraction saturated at bucket 0**, and it does
not move: 1.7 % to 1.8 %. Improving the pool from bucket 6 to bucket 4 does not help a predicate
that asks for bucket 0, and it barely helps the `eff` bound either, because one unit of bucket is
worth `bucket_scale` — more than any achievable score difference.

## Measured cost, interleaved, medians of three pairs at 1 M

| | `p26` | per-group, full budget | per-group, equal budget |
|---|---|---|---|
| exact p50 | **487 us** | 639 us (+31 %) | 957 us (+96 %) |
| typo p50 | **1,105 us** | 1,226 us | 1,582 us |
| typo p99 | **16,748 us** | 18,613 us | 18,597 us |

Both budgets were tried, because the first result confounded seed *quality* with seed *count*:

- **Full budget** (`pool` champions per group) scores ~4x the documents.
- **Equal budget** (`pool / groups` per group) holds total seeding cost fixed and spreads it.

Equal budget is **worse than full budget**, which settles it. Concentrating the budget on the single
most discriminative term raises the score threshold hardest, and that matters more than spreading it
for bucket diversity. The existing design is not an accident.

## What this says about the tail

The 17 ms typo p99 at 1 M is not caused by a cold pool that better seeding could warm. Both `p26`
bounds are already exact; there is no slack in them to recover.

> **CORRECTED by `p29-expansion-cap.md`.** This section originally continued: *"It is caused by typo
> expansion putting up to `MAX_EXPANSION` terms in every group."* That is wrong. Capping expansion
> to **1** — no expansion at all — still leaves typo p99 at **12.4 ms** against 16.4 ms uncapped, so
> expansion accounts for only about **24 %** of the tail. The rest is the cost of exact bucket-first
> ranking itself: a corrupted token gives the pool a high-bucket worst member whether it expanded to
> 16 terms or 1, so the gates stay shut either way.

Anything that fixes the tail therefore has to change **what is ranked**, not how it is pruned —
capping expansion adaptively, or admitting a bounded-error mode with a stated guarantee. Both are
product decisions rather than optimizations, which is why neither was attempted here.

## Reproduce

The reverted implementation is recoverable from this document's description; the audit is:

```sh
cargo run -p index-bench --release --bin scale     # interleave against a rebuilt arm
```
