# P25 — the third pruning site, and the bound that makes gating it affordable

**Tier:** T1 · **Bins:** `pool-audit` (`INDEX_DUMP_LOSS=1`), `prune-consistency`, `scale`
**Status: FIXED, 2026-09-05. `pool-audit` is 0.00 % on all six real query sets — every cell, first time.**
**Cost figures below are superseded by `p26`, which keeps the correctness and removes most of the price
(exact p50 1,108 us -> 476 us).**

`p24` closed the pruning defect from 9.33 % to 2.23 % and said of what was left:

> The residual is not explained here and should not be assumed to be the same defect.

It **is** the same defect, at a third site `p24` did not gate. Assuming otherwise would have been
wrong, but so was leaving it unexamined.

## Finding it: dump the case instead of theorising

`pool_audit` gained an `INDEX_DUMP_LOSS=1` mode printing the `(bucket, score)` column of `search`
beside `search_exhaustive_unpooled` for the first few losing queries. One case settles it:

```
---- bucket-loss case 1: "Frabelle Foods Chicharon Bulaklak 50g"
got    bucket        score   |   truth   bucket        score
1           0      27.6935   |   1            0      27.6935
...ranks 2-6 identical...
7          22      13.5718   |   7            9       8.3271
8          22      13.5718   |   8            9       8.2355
9          22      13.3408   |   9            9       7.7416
```

Two facts fall straight out:

1. **Pruning was not running.** The ranking pool's worst member has bucket 22, so `prune_is_sound`
   is false for the whole query and both `p24` gates are shut.
2. **The pool's ordering is not at fault.** `bucket_scale = prefix_sum[last] + 1.0` exceeds any
   achievable score, so `eff` is *exactly* lexicographic — bucket 9 beats bucket 22 by 13 whole
   scale units. Had those documents reached `admit`, they would have been kept.

So they never reached `admit`. They were never scored.

## The cause: a third pruning site that does not look like one

The MaxScore essential/non-essential partition:

```rust
while first_essential < term.len() && prefix_sum[first_essential + 1] <= threshold {
    first_essential += 1;
}
```

`p24` gated the block skip and the non-essential bail and left this, because it does not look like
pruning — it looks like bookkeeping. But demoting a term removes its postings from the **candidate
enumeration**. A document appearing only in demoted terms is never a candidate, never scored, never
offered to either pool; its bucket cannot compete because the document does not exist.

This retires the open puzzle from `p23`, whose own comment sat four lines above the cause:

> sweeping `rank_cap` from `k` to `16k` returns byte-identical degradation ... no pool can retain a
> document it never sees.

That was read as evidence pools were not the lever. It was evidence **enumeration** was.

## Why the `p24` predicate cannot be reused — measured, not assumed

Gating this site on `prune_is_sound` fixes it completely (every `pool-audit` cell 0.00 %) and costs
**20.7×** — exact p50 399 µs → 8,268 µs at 1 M, interleaved, two rounds. Worse than the one-pool
design `p24` rejected at 10–30×, for the same reason: `prune_is_sound` is false whenever the ranking
pool's worst member has a bucket above 0, which is the *common* case, so `first_essential` never
advances and MaxScore degenerates into a full OR scan.

The predicate is wrong for the site. `prune_is_sound` asks *"can an unseen document beat the pool on
**score**?"*, correct where a score bound is what does the skipping. This site stops generating
documents, so the question is *"can it beat the pool on **bucket**?"*

## The fix: bound the bucket floor of an unenumerated document

`bucket_of` charges a fixed penalty per missed group. A document matching **none** of `term[f..]`
therefore misses every group lying wholly inside that suffix, and its bucket has a floor:

```rust
// bucket_floor[f] = least bucket any document can have if it matches none of term[f..].
// Group g is wholly inside term[f..] exactly while f <= (min index of g).
```

Non-increasing in `f`, so it drops into the same monotone `while` as `prefix_sum`:

```rust
while first_essential < term.len()
    && prefix_sum[first_essential + 1] <= threshold
    && bucket_floor[first_essential + 1] > worst_bucket
{
    first_essential += 1;
}
```

Demotion is safe once an unenumerated document is guaranteed a bucket **strictly worse** than the
ranking pool's worst member: it cannot enter the ranking pool however it scores, and the `prefix_sum`
test beside it still covers the scoring pool. A group with no surviving term is charged at every `f`
— every document misses it equally, including the pool member it is compared against.

**The bound is true exactly where `prune_is_sound` is false.** A pool full of high-bucket documents
is easy to beat, so the case that made the naive gate cost 20.7× is the case this permits demotion in.

### Shrinking the ranking pool, which the bound made load-bearing

The bound compares against the ranking pool's *worst* member, so a **bigger pool has a worse worst
member** and suppresses demotion for no gain. `rank_cap` was `k.max(16)`; `p23` had already shown
`k` to `16k` byte-identical, so nothing depended on the 16. Interleaved:

| `rank_cap` | exact p50 | typo p50 | typo p99 |
|---|---|---|---|
| `k.max(16)` | 1,378 µs | 1,909 µs | 30,166 µs |
| **`k.max(1)`** | **1,037 µs** | **1,503 µs** | **16,973 µs** |

**Typo p99 nearly halves**, audit unchanged at 0.00 %. A pool sized for one purpose turned out to be
sized wrong for a second purpose it acquired.

## Measured

`pool-audit`, against brute force over the whole corpus:

| corpus / query set | queries | `p24` top-10 diff | **`p25`** |
|---|---|---|---|
| presyo / product names | 4,028 | 2.23 % | **0.00 %** |
| presyo / category names | 145 | 0.00 % | **0.00 %** |
| presyo / 3-word tails | 4,004 | 3.17 % | **0.00 %** |
| blead / business names | 4,330 | 0.02 % | **0.00 %** |
| maphy / place names | 1,067 | 0.00 % | **0.00 %** |
| profstopick / titles | 2,253 | 0.00 % | **0.00 %** |

**Every cell zero — rank-1, top-10 and actually-worse.** The arc across one day is
**28.20 % → 9.33 % → 2.23 % → 0.00 %** on presyo product queries.

### Cost

Real presyo queries, six interleaved pairs, **medians** (one floor@k round was a 5.0 ms p99 outlier,
which is why the median and not the mean is quoted):

| | `p24` | **`p25`** | delta |
|---|---|---|---|
| p50 | 199 µs | 227 µs | **+14 %** |
| p99 | 1,512 µs | 1,610 µs | **+6.5 %** |

Synthetic `scale` at 1 M, three interleaved pairs, means:

| | `p24` | **`p25`** | delta |
|---|---|---|---|
| exact p50 | 410 µs | 923 µs | **2.25×** |
| typo p50 | 813 µs | 1,447 µs | 1.78× |
| typo p99 | 8,490 µs | 15,893 µs | 1.87× |

The two disagree because the `scale` corpus is **built adversarially** — its header records that it
ties scores near the threshold so nothing can be skipped. It is the worst case by construction; real
queries are the ~14 % column. Both are reported because quoting only the flattering one is how the
warm-vs-cold error in `p23` happened.

## What this does not fix

- **The `p24` gates still cost their ~8 % p50 / ~19 % p99.** `p25` is measured *on top of* `p24`,
  not against the pre-`p24` engine.
- **`bucket_floor` is a floor, not the true minimum.** It ignores that a document must also *reach*
  the demoted terms to collect their distances, so it is conservative — a tighter bound would permit
  more demotion. Not attempted; the current one already recovers the affordable case.
- **Clean-machine absolute latency** is still unmeasured; only the interleaved deltas are load-bearing.

## Reproduce

```sh
INDEX_DUMP_LOSS=1 cargo run -p index-bench --release --bin pool-audit
cargo run -p index-bench --release --bin prune-consistency
cargo run -p index-bench --release --bin scale
```
