# P21 — the pool eviction bug, and every number it changed

**Tier:** T1 · **Bins:** `presyo-catalog`, `presyo-expand`, `blead-industry`, `profstopick-dept`
**Status: FIXED and re-measured, 2026-09-05. Consolidates corrections to p15, p17, p18, p20.**

## The bug

`p20-profstopick-dept.md` recorded, as undiagnosed, that expansion *cost* 10.8 points on a corpus
where its terms are priced below exact matches and should therefore have been harmless. That was an
engine question about a just-shipped feature, so it should not have stayed open.

> **The candidate pool is ordered by SCORE. The final ranking is ordered by `typo_bucket` FIRST.**
> A document with a perfect bucket but a modest score can be evicted from the pool before the
> bucket sort ever sees it.

Expansion makes this far worse: it adds many terms whose matches are *scoring competitors that were
not there before*, so they crowd out documents that would have won the final ordering.

Diagnosed by asking for a larger pool and watching the loss disappear:

| pool (`ask k`) | plain | expansion |
|---|---|---|
| 10 | 99.2 % | 88.3 % |
| 25 | 99.2 % | 92.2 % |
| 50 | 99.2 % | 95.3 % |
| 100 | 99.2 % | 97.2 % |
| **400** | 99.2 % | **100.0 %** |

**The documents were always ranked correctly. They were being thrown away before the sort.**

## The fix

```rust
let pool = match expanded {
    true  => (k * 24).max(256),
    false => (k * 3).max(32),
};
```

Tied to expansion firing rather than raised globally, because `real-corpus` and `sisia-catalog`
never expand and a wider pool would cost them tail latency for nothing. Both are unchanged
(`OVERALL: PASS`), as are `maphy-place` and the WASM smoke test.

**It is not free where it does fire**: presyo's category-query p50 went **313 µs → ~700 µs**, against
163 µs for an ordinary product query. A browse query now costs roughly 4× a lookup.

## Every number it changed

| bench | measurement | before | after |
|---|---|---|---|
| `p15` presyo | in-sample gain | +25.5 pt | **+21.8 pt** |
| `p17` presyo | **held-out gain** | +23.9 pt | **+21.5 pt** |
| `p18` blead | in-sample gain | +20.4 pt | **+18.1 pt** |
| `p18` blead | **held-out gain** | +6.7 pt | **−1.5 pt** |
| `p20` profstopick | in-sample | −10.8 pt | **−0.8 pt** |
| `p20` profstopick | held-out | −38.6 pt | −34.7 pt |

The held-out arms were affected for a second reason worth stating: they build the expanded query as
a **string**, so the engine's expansion path never fires and the pool is never widened for them.
They now ask for a large pool and truncate.

## What the corrected numbers say — a three-way rule

`p18`'s "vocabulary reuse" condition is **not refuted; it is sharpened**, and the divergence is
starker than the contaminated numbers showed:

| corpus | baseline | in-sample gain | held-out gain | retention |
|---|---|---|---|---|
| **presyo** — grocery products | 75.3 % | +21.8 pt | **+21.5 pt** | **~99 %** |
| **blead** — business names | 77.4 % | +18.1 pt | **−1.5 pt** | **~0 %** |
| **profstopick** — course titles | 99.2 % | −0.8 pt | −34.7 pt | n/a |

**The adoption rule that falls out of it:**

1. **The facet already retrieves well** (profstopick, 99.2 %) → **do not expand.** There is nothing
   to gain and only noise to add.
2. **There is a gap and members reuse vocabulary** (presyo) → **expand.** It works and it
   generalizes: 83 % of the gain survives on documents the table never saw.
3. **There is a gap but members do not reuse vocabulary** (blead) → **expand only if you rebuild the
   table often.** +30.7 in-sample collapses to −1.5 held out, so the table is worth almost nothing
   the moment the corpus moves.

Case 3 is the one that would have shipped a disappointment. blead's in-sample +30.7 is the largest
gain of the three corpora and its held-out value is *negative* — **an adopter measuring only
in-sample would have concluded it was the best fit of the three.**

## The multiplier is a workaround, and the sweep proves it

`k * 24` was chosen to clear the observed curve. Sweeping it across all three corpora shows **no
constant is correct**:

| mult | presyo | profstopick | blead |
|---|---|---|---|
| 3 | 96.5 % | 87.5 % | 84.8 % |
| 6 | **97.0 %** | 92.2 % | 89.6 % |
| 12 | 97.0 % | 93.6 % | 95.2 % |
| 24 | 97.0 % | 96.7 % | **95.6 %** |
| 48 | 97.0 % | **99.4 %** | 95.6 % |

**presyo saturates at 6, blead at 24, and profstopick is still climbing at 48.** On presyo, 24 buys
nothing over 6 and costs ~200 µs per query. A corpus-dependent constant is the signature of a
workaround, not a fix, and `EXPANDED_POOL_MULT` now carries this table in its doc comment so the
next person does not have to rediscover it.

24 is kept because it is where two of three corpora have saturated and the third has most of its
gain.

## The actual defect, named

> **The engine prunes by score and ranks by bucket-then-score. Those two disagree.**

Block-max pruning skips work whose *score* cannot beat the pool's worst *score*. But the final
ordering puts `typo_bucket` first, so a document that would rank top can be pruned for having a
score below a threshold that has nothing to do with how it will be ranked.

The obvious repair — evict by `(bucket, score)` instead of by score — **does not work as stated**,
and the reason is worth recording so it is not attempted twice:

```rust
threshold = heap.peek().score   // the pool's worst score IS the pruning threshold
```

Making the heap bucket-ordered means its root is no longer the worst-scoring member, so its score is
not a valid threshold. Tracking the true minimum separately keeps correctness but drives the
threshold toward zero — because bucket-0 documents with tiny scores are exactly what the change is
meant to retain — and that disables block skipping entirely.

**Pruning by score is only sound when ranking is by score.** A correct fix has to make the pruning
bound bucket-aware (a per-bucket threshold, or folding bucket into a monotone score), and that is a
retrieval-loop redesign affecting every query rather than a constant. It is **not attempted here**,
and the workaround is labelled as one — see `p22` for the reproduction that makes it mandatory.

## What this cost, methodologically

This is the **eighth** methodology error caught in this repo and it changed six published numbers.
Two things made it findable, and both were deliberate practice rather than luck:

- **`p20` recorded the anomaly as undiagnosed rather than explaining it away.** A file that had said
  "expansion adds noise, as expected" would have closed the question and kept the bug.
- **The in-sample/held-out split existed at all.** The pool artifact hurt held-out arms far more
  than in-sample ones, so a project measuring only in-sample would have seen nothing wrong.

## Not measured

- **A bucket-aware pruning bound.** The real fix. **`bench/roadmap/p22-prune-consistency.md` has
  since reproduced the defect with no expansion involved**, at exactly the pool size, so this is no
  longer optional work — it is the repair for a demonstrated correctness bug, and `p22` is its
  acceptance test.
- **profstopick's residual −2.2 pt.** Smaller than the −10.8 that prompted the investigation, and
  consistent with "nothing to gain, slight noise added", but not separately explained.

## Reproduce

```sh
cargo run -p index-bench --release --bin presyo-catalog
cargo run -p index-bench --release --bin presyo-expand
cargo run -p index-bench --release --bin blead-industry
cargo run -p index-bench --release --bin profstopick-dept
```
