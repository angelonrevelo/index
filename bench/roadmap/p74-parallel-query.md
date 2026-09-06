# P74 — the query fans across segments, and admits when it should not

**Tier:** T1 · **Bin:** `segment-scale` · **Files:** `crates/index-text/src/searcher.rs` (only)
**Status: SHIPPED, 2026-09-06. Tail-led: ~2.9x p99 at 25 and 50 segments. Ranking bit-identical.
288 tests. No new dependency.**

> **SUPERSEDED IN PART BY `p80`.** The machinery here is unchanged and correct, but the
> **default is inverted**: threading is now opt-in via `Searcher::set_parallel(true)` or
> `INDEX_PARALLEL=1`. Re-measured on the same workstation running its owner's ordinary
> applications, the default shipped below was **2.2-2.4x SLOWER** than serial at 25 and 50
> segments. The win recorded here is real and still available — on a machine this process
> owns. See `bench/roadmap/p80-parallel-default.md`.

`p56` closed with *"No concurrency. Every number here is single-threaded."* `p73` took the build
half. This takes the query half — `Searcher` fans its per-segment pass across OS threads.

## Measured, `bin/segment-scale`, presyo's 241,789 real products

Cross-run comparison was **useless on this box** — the 1-segment control swung 16–29 us run to run,
exactly as `p38`'s own header warns. So the lane added an `INDEX_PARALLEL` kill switch and ran
**seven alternating serial/threaded pairs from the same binary**. Medians of seven:

| segments | p50 serial | p50 threaded | p99 serial | p99 threaded |
|---|---|---|---|---|
| 1 | 23 us | 26 us | 3,712 us | 4,336 us |
| 2 | 56 us | 52 us | 12,158 us | 12,107 us |
| 5 | 187 us | 174 us | 13,784 us | 14,371 us |
| 10 | 807 us | 876 us | 18,293 us | 17,776 us |
| **25** | **3,396 us** | **2,792 us (1.22x)** | **28,377 us** | **9,833 us (2.9x)** |
| **50** | **7,301 us** | **2,919 us (2.5x)** | **47,523 us** | **16,115 us (2.9x)** |

Rungs 1–10 are **below the gate — the same code runs in both arms**, so the deltas there are noise,
which makes them a useful control rather than a result.

**The win is p99-led, and that is the honest headline.** At 25 segments the p50 is a wash (1.22x);
what comes down ~3x is the tail.

## The threshold is a work estimate, not a segment count

```
segment_count * doc_count >= 4_500_000
```

Segment count alone would thread 50 segments of 200 documents — fifty times nothing, fifty spawns.
Document count alone would thread one huge unsplittable segment. **Their product is what predicts
the serial cost**, because every segment is scanned for every query.

The number comes from a measurement, not a guess: on this Windows workstation a `thread::scope` that
spawns and joins and does nothing else costs **~550 us for one thread and ~1.4 ms for fifteen**.
There is no pool, and building one would need either a dependency this repo does not take or
`unsafe` to launder a non-`'static` closure. And a `search` pays that **twice** — `p52`'s statistics
pass and the scoring pass are separate fan-outs that must not interleave. So the fan-out must
displace ~3 ms of serial work. `p38`'s ladder prices serial at ~6.6e-4 us per segment-document,
putting break-even at 4.5e6.

Against that ladder the gate picks exactly the rungs that win: **10 segments scores 2.4e6 and stays
serial — forcing threads there measured 883 → 1,473 us, a LOSS** — while 25 scores 6.0e6 and 50
scores 1.2e7.

Being wrong low costs nothing (a collection under the gate runs precisely the code it ran before);
being wrong high costs every query a millisecond. It is calibrated to one machine and says so.

### An unresolved tension with `p73`, recorded rather than smoothed over

`p73` measured `std::thread::scope` at **~87 us per thread** on this same box; this lane measured
**~550 us for a one-thread scope**. Both numbers are in the tree. The likely reconciliation is
best-of-twenty versus median, and the two thresholds are conservative enough that either reading
leaves them safe — `p73`'s smallest rung measured 86 → 65 ms, so no regression appears where the
tighter number would predict one. **But the repo now asserts two spawn costs that differ ~6x, and
one measurement should settle it.** Open.

## Ranking identity — verified three ways

Order is the whole contract: `base[i]` turns a segment-local ordinal into a global one and the merge
comparators break ties on that global ordinal, so a result assembled in **completion order** would
rank non-deterministically — and on a corpus this repo already measures as **69 % near-ties**, that
reshuffle is invisible until it reaches a user. Each work unit therefore carries its segment index
and the pairs are sorted on it before anything is returned.

1. **`the_threaded_path_returns_a_bit_identical_answer_to_the_serial_one`** — two identical
   8-segment collections differing only in a forced flag, compared across `search`, `search_prefix`,
   `search_page`, `search_facet`, `search_range`, `search_clause`, `search_sorted` (both
   directions), `facet_tally`, `range_tally`, `search_phrase` and `search_phrase_page`, on **score
   bits rather than ordinals**, repeated 25x so a scheduling-dependent answer cannot pass once.
2. **`segment-scale` gated here, forcing BOTH directions** — `INDEX_PARALLEL=0` against
   `INDEX_PARALLEL=1`, so every multi-segment rung genuinely took the threaded path rather than
   falling under the gate. **Every quality percentage is identical**: broad overlap
   100.00/84.45/84.41/85.80/87.01/84.94, selective rank-1 99.80/99.20/99.40/99.00/99.40, selective
   overlap unchanged — 431 broad + 500 selective queries against a monolithic oracle.
3. **`pool-audit` 6/6 zero cells.**

## The caveat that earns the kill switch

**The threaded path degrades badly under machine contention.** Two of the lane's seven runs landed
while the box was busy: one showed a 50-segment p50 of **36,271 us — 4x worse than serial** — and
another a 25-segment p99 of **1,253,112 us**. Spawning a thread per core per query on an
already-saturated machine is pathological.

`INDEX_PARALLEL=0` forces every collection onto the serial path. That is also what an embedder
wants when the host owns its own thread budget: a WASM build, or a server that has already given
every core to a request pool, should not have a library spawning underneath it. On `wasm32` it is
automatic — `available_parallelism` reports `Unsupported`, `cpu_count()` is 1, and `wants_thread`
returns false before anything else is consulted.

**This matters more now than when it was measured**, because `p73` made the *build* threaded too. A
process that builds and queries concurrently can oversubscribe in a way neither lane measured alone.

## Still open

- **The speedup is not independently reproduced.** The correctness above was verified here on the
  real tree; the timing table is the lane's, taken interleaved in one process, and could not be
  re-measured during the merge because three other agents held cores. **It needs one re-run on an
  idle box**, alongside the `docs/benchmarks.md` grid that is waiting for the same thing.
- **`search` opens two `thread::scope`s per query.** Fusing them with a `Barrier` between the stats
  and scoring passes would halve the ~1.4 ms floor and move the crossover from ~20 segments to ~10.
  Not taken: it is an intricate two-phase change and the current result is clean and measured.
- **25 segments is a p50 wash.** If the tail is not what a deployment cares about,
  `PARALLEL_WORK_MIN` should be 8e6 rather than 4.5e6.
- **The constant is calibrated to one Windows workstation** and no other machine has been measured.
- **`cargo fmt` is not this repo's formatter** — it reformats to default `use_small_heuristics`
  where this repo uses `Max`. Noted because the lane lost time to it.
