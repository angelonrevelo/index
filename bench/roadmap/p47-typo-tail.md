# P47 — the typo tail: one lever that worked, two that did not

**Tier:** T1 · **Bins:** `scale`, `pool-audit`, `presyo-catalog` · **API:** unchanged
**Status: SHIPPED, 2026-09-06. No format change, no ABI change. 134 tests.
The 5 ms bar is MET on every real corpus and still RED at 1 M recombined.**

`p29` left this item explained rather than closed:

> **Nothing measured today meets it**, including the build that predates every correctness fix. ...
> It stays red and unraised; what has changed is that the red is now explained rather than merely
> observed.

and located the cause: expansion is only ~24 % of the tail, the rest is exact bucket-first ranking
holding `prune_is_sound` false. This document attacks that, and reports what each attempt bought.

## The enabling correctness fix: `eff` was not the comparator

The engine ranks by `rank_cmp` = (bucket asc, **quantized** score desc, doc asc). The ranking pool
orders by `eff = score - bucket * bucket_scale`, built from the **raw** score.

Those are not the same order. When two scores differ but quantize to the same grid point, `eff`
prefers the higher raw score and `rank_cmp` prefers the lower document id:

```
Hit { doc: 12, score: 3.25,      bucket: 0 }   rank_cmp: first
Hit { doc: 20, score: 3.2500002, bucket: 0 }   raw eff : first
```

So the ranking pool could evict a document the final ordering would keep, and the union with the
scoring pool was quietly covering for it. Quantizing `eff` closes the gap —
`eff_induces_exactly_the_final_ranking` pins it, and **fails on the old formula with exactly the
pair above**, which is how it earns its place.

The gap itself was never observed in production (`pool-audit` reads 0.00 % before and after). What
it unlocks is much larger than the gap:

> **The ranking pool now provably holds the top `k` by the final comparator, so the scoring pool
> contributes no document that survives truncation.**

Two things follow, and neither was safe to do before.

## Lever 1: size the scoring pool for pruning, not for recall — the one that worked

`pool` was `max(3k, 32)`, and the comment pinning it says *"3x/32 retains identical recall on both
production corpora"*. **Recall is no longer its job.** Its only remaining job is to supply
`threshold`, and for that a *smaller* pool is strictly better: `threshold` is the pool's worst
score, so a tighter pool raises it and more block-max skipping engages.

Sized to `k` (the learned-expansion branch keeps its own wider pool untouched, so `p21`'s recall
fix is not disturbed):

| `scale`, medians of 3 | before | **after** | |
|---|---|---|---|
| 61,467 real: exact p50 | 214 us | **144 us** | −33 % |
| 1 M: exact p50 | 820 us | **529 us** | **−35 %** |
| 1 M: typo p50 | 1,251 us | **976 us** | −22 % |
| 1 M: typo p99 | 13,092 us | 13,579 us | +4 % |
| presyo 4,028 real queries: p50 | 173 us | **146 us** | −16 % |
| presyo 4,028 real queries: p99 | 1,213 us | **1,062 us** | −12 % |
| presyo 241,677 rows: typo p99 | 4.59–4.96 ms | **4.54 ms** | |
| `facet-shop`: search p50 | 11 us | **8 us** | |

**A third off the median for nothing.** And "for nothing" is measured, not asserted: `pool-audit`
reads **0.00 % in all six cells** on presyo, blead, maphy and profstopick, `real-corpus` holds
100.0 % recall@10 and 99.8 % hit@1, `presyo-catalog` holds precision@10 at 97.0 %, and
`presyo-expand` returns byte-identical held-out figures (55.8 % to 77.2 %).

## Lever 2: ungate the bucket-floor block skip — sound, and worth nothing

`p26`'s second skip reason (a range whose bucket floor is worse than the pool's worst member) sat
behind `range_bound <= threshold`, which made it useless in the exact case it was built for: a typo
query whose pool never saturates at bucket 0 *also* has a low score threshold, so the two conditions
failed together.

The exact `eff` makes ungating sound — skipping a range that cannot enter the ranking pool can only
cost the scoring pool candidates, and those do not reach the answer. Ungated, it is
**latency-neutral at every scale.** It ships because it is free and strictly more general, not
because it helped.

**And the first version of it was a 25 % regression.** Ungated, the O(terms) reachability loop ran
on every candidate instead of on the few that passed the threshold test: 16.4 ms against a 13.3 ms
baseline. The fix is to compute how large the floor would have to be *before* looking for it —

```
need = (canon(range_bound) - worst_eff) / bucket_scale
need <= 0            ->  the range loses on score alone; skip, no loop
need > group_count   ->  no floor can reach it; do not look
```

— which leaves only the band between them paying for the loop. The bound is unchanged; only the
order of evaluation is. **A sound bound evaluated in the wrong order is a pessimization**, and it
cost more than the bound was worth.

## Lever 3: nothing moved the 1 M tail

| | typo p99 @ 1 M |
|---|---|
| baseline | 13.09 ms |
| quantized `eff` alone | 14.00 ms |
| quantized `eff` + ungated skip | 14.01 ms |
| **+ pool sized to `k` (shipped)** | **13.58 ms** |
| bar | **5 ms** |

The ~7 % is the cost of `canon_score` on the hot path, and it is paid for by the median win and by
the bound being sound by construction rather than by luck. Isolating it also settles that the
ungated skip is exactly neutral: the two variants differ by 0.01 ms.

**The median moved 35 % and the tail did not move at all**, which is the finding. The tail is not
threshold-limited — those queries were never waiting on a pool that filled too slowly, which is the
same conclusion `p27` reached from the seeding side and `p29` from the expansion side. Three
independent attacks now agree the 1 M tail is the cost of enumerating documents the ranking rule
genuinely requires visiting, and no tightening of an existing bound reaches it.

## Where the bar actually stands

| corpus | documents | typo p99 | 5 ms bar |
|---|---|---|---|
| presyo, real products | 241,677 | **4.54 ms** | **PASS** |
| DepEd schools, real | 61,467 | **1.81 ms** | **PASS** |
| DepEd, recombined | 250,000 | 6.16 ms | FAIL |
| DepEd, recombined | 1,000,000 | 13.58 ms | FAIL |

**On every corpus of real documents this project has, the bar is met.** It fails only above the real
data, where `scale` recombines 41,069 real terms across up to sixteen times as many documents — and
that row holds vocabulary *fixed* while multiplying documents, so every posting list is longer than
a genuine 1 M corpus of the same shape would produce. `scale` already says so in its own header
("a measurement of posting-list and top-k scaling ... not a claim about dictionary growth"). It is
the right stress test and the wrong thing to call a product bar without that caveat attached.

The bar stays red and unraised, exactly as `p29` left it. What is new is that the failing rows are
now separated from the passing ones by whether the documents are real.

## Still open

- **The 1 M recombined tail, at 13.6 ms.** Three attacks (`p27` seeding, `p29` expansion, `p47`
  bounds) have each moved it by less than 10 %. The next honest step is not another bound: it is
  either a genuinely new enumeration strategy (bucket-tiered candidate generation, where documents
  matching every group are produced by intersection before anything else is scanned) or accepting
  the shape of the ranking rule.
- **No real 1 M corpus exists to test against**, so the failing rows cannot be separated from the
  recombination artefact by measurement. That is the single most valuable missing input here.
- **`pool` is now `k` for the unexpanded path and `EXPANDED_POOL_*` for the expanded one.** Nobody
  has swept the expanded branch since recall stopped being the scoring pool's job; it may be
  similarly oversized.
