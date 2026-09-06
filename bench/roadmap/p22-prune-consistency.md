# P22 — the pool defect is general, and here is the minimal reproduction

**Tier:** T1 · **Bin:** `cargo run -p index-bench --release --bin prune-consistency`
**Status: GREEN 2026-09-05 — closed by the pruning gate in `bench/roadmap/p24-prune-gate.md`.**

> **CLOSED by `bench/roadmap/p24-prune-gate.md`.** This bin now passes at every decoy count. The
> repair was not to replace score-based pruning but to **gate** it: skipping on a score bound is
> sound exactly while the ranking pool is full and its worst member has bucket 0. Cost ~8 % p50 /
> ~19 % p99, against 10–30 % ... 10–30× for the only other design that closes it.

## What this settles

`p21-pool-eviction.md` found that learned expansion could lose correct results, worked around it by
widening the candidate pool, and asserted that the underlying defect was **general**:

> The engine prunes by score and ranks by `typo_bucket`-then-score. Those two disagree.

That was an assertion. `p21` also said the differential test should be written before any fix is
attempted. **This is that test, and it reproduces the defect with no expansion in play.**

## The reproduction

```
   decoys   truth rank 1   search() rank 1   agree
        4           3000              3000     yes
       16           3000              3000     yes
       32           3000              3200      NO
       64           3000              3200      NO
     2048           3000              3200      NO
```

**The break is at 32 decoys, which is exactly the pool size** — `(k * 3).max(32)` with `k = 10`.
Once enough high-scoring worse-bucket documents exist to fill the pool, the correct answer is
evicted before the bucket sort ever runs.

Ground truth is `Index::search_exhaustive_unpooled`, added for this: score every matching document,
sort by the engine's own comparator, no pool, no pruning, no skipping. Neither `search` nor
`search_exhaustive` could serve — **both apply the same pool**, and `search_exhaustive`'s own comment
says so.

## Getting the corpus right took three attempts, and the failures are the finding

The first two constructions **passed for the wrong reason**, and understanding why explains when this
bug can and cannot bite:

1. `beta` in one document → maximal IDF → the supposedly low-scoring full match scored *high*.
2. `beta` in 400 documents → still far rarer than `alpha` → same outcome.

> **To be bucket 0 a document must match every group, and the group the decoys MISS is precisely the
> one carrying the IDF.** Matching it is what makes the full match score well. The bucket advantage
> and the score advantage come from the same place, so they usually move together.

The third construction breaks that coupling by making the discriminating term **genuinely common**:

- 3,000 documents containing `gamma` and nothing else relevant — so `gamma` has almost no IDF;
- 200 **long** documents containing `alpha beta gamma` — bucket 0, score dragged down by length;
- N **short** documents containing `alpha beta` — missing only the worthless `gamma`, so worse
  bucket but much higher score.

That is not a contrived shape. It is **"iphone 15 pro case"** — a query with one common word, where
the exhaustive product descriptions are long and the near-miss titles are short and numerous. It is
the shopping-search case this project exists for.

## What it means

- **The defect predates `learn_expansion`.** Every query with the shape above has been affected for
  as long as the pool has existed.
- **`p21`'s multiplier is not a fix, only a mitigation for expanded queries**, and the sweep in `p21`
  already showed no constant is right (presyo saturates at 6, blead at 24, profstopick still
  climbing at 48).
- **The bucket-aware pruning bound is no longer optional work.** It is the repair for a demonstrated
  correctness bug, not a refinement.

## Why this is a bench and not a unit test

It is **expected to fail** while the defect stands. `bench/roadmap/` is where this repo keeps specs
for unfinished work — a roadmap's red tests are a feature, a gate's red tests are a bug — so it is
excluded from CI like the other roadmap bins. It becomes a unit test the day the fix lands.

## Two fixes were built and measured. Both were reverted.

`p21` named the repair as "a bucket-aware pruning bound". Both plausible forms were implemented
against this acceptance test and then **backed out**, because the measurement did not justify them.
The numbers are recorded so the next attempt starts from evidence rather than from the same two
ideas.

### Attempt 1 — fold bucket into a single monotone score

`eff = score − bucket × scale`, with `scale` larger than any achievable score, so the pool's
ordering becomes *identical* to the ranking's. **It works: `p22` passes at every decoy count.**

| | baseline | attempt 1 |
|---|---|---|
| exact p50 @1M | 274 µs | **8,051 µs** |
| typo p50 @1M | 598 µs | 12,955 µs |
| **typo p99 @1M** | **6.0 ms** | **57.5 ms** |

**10–30× slower.** The reason is structural, not an implementation flaw: the pruning threshold is
the pool's worst member, and an `eff` threshold sits far below any score, so block skipping never
engages. **Pruning by score is only sound when ranking is by score** — making the ordering correct
destroys the bound that makes it fast.

### Attempt 2 — two pools over the same candidates

Keep the score-ordered pool (it owns the pruning threshold, so skipping is unchanged) and add a
second, `k`-sized pool ordered by final rank; merge and deduplicate at the end.

| | baseline | attempt 2 |
|---|---|---|
| exact p50 @1M | 274 µs | 400–530 µs |
| **typo p99 @1M** | **6.0 ms** | **9–10.5 ms** |
| `p22` first failure | 32 decoys | **512 decoys** |

**A 16× improvement in the failure threshold for +50–75 % on p99.** Correct as far as it goes, all
103 tests green — but it does **not** close the hole, because block-max skipping discards documents
*before* they are scored and neither pool ever sees them. That residual is exactly the part attempt
1 fixed and attempt 2 cannot.

> **REVERSED by `bench/roadmap/p23-pool-audit.md`.** Attempt 2 is back in the engine. Both premises
> below turned out to be false: the defect **does** occur on real data (28.2 % of real presyo product
> queries), and the latency figure was a **measurement error** — a warm-machine run compared against
> a cold baseline. Interleaved A/B puts the true cost at **2–6 %**, not 50–75 %.

### Why both were reverted (superseded — see p23)

**The bug has not been observed on any real corpus.** `real-corpus`, `sisia-catalog`,
`presyo-catalog`, `maphy-place` and `blead-industry` all pass; it reproduces only on a corpus built
to trigger it. The latency cost, by contrast, is **unconditional — every query pays it**.

Trading 50–75 % of tail latency, on a project whose claim is "a million rows in milliseconds", for a
partial fix to a defect no real workload has exhibited is a bad trade *at this evidence level*. It
becomes a good trade the moment someone sees it in production, and the acceptance test is now
sitting here waiting for that day.

**What would change the decision:** a real corpus that reproduces it. That is a cheaper thing to
look for than a redesign, and it is the honest next step.

## The fix, and the trap in it

Recorded in `p21` and repeated here because it is the whole difficulty: the pool's worst score **is**
the pruning threshold, so a bucket-ordered heap has no valid threshold at its root, and tracking the
true minimum separately drives the threshold toward zero and disables block skipping entirely.

**Pruning by score is only sound when ranking is by score.** A correct fix needs a per-bucket
threshold, or bucket folded into a monotone score so that one bound serves both. This bin is the
acceptance test for it.

## Not measured

- **How often the shape occurs in the real corpora.** `real-corpus`, `sisia-catalog` and
  `presyo-catalog` all pass their gates, so it is not common there — but "not common" is not "does
  not happen", and none of those benches was designed to detect it.
- **Whether champion-list seeding masks it in practice.** Champions are seeded by score, so they
  should not help, but that is reasoning rather than measurement.

## Reproduce

```sh
cargo run -p index-bench --release --bin prune-consistency
```
