# P46 — the sort tail, and an algorithm that is usually worse

**Tier:** T1 · **Bin:** `facet-shop` · **API:** `Index::search_sorted` (unchanged), `search_sorted_arm`
**Status: SHIPPED, 2026-09-06. No format change, no ABI change. 133 tests.**

`p32` shipped sort-by-value and published its own bad news rather than hiding it:

> **A numeric order gives it nothing to stop on.** The cheapest item in the corpus may match the
> query worst, so it cannot be skipped on any relevance bound — every matching document must be
> visited. ... **8x the tail of a ranked search.**

That reasoning is correct, and it is about **the posting scan**. It is not about the problem.

## Invert the loop and the bound comes back

Walking the numeric column in **value order** and testing each document for a match visits
documents best-first, so after `k` of them match, no unvisited document can displace one — every
remaining document has a worse value by construction. The stopping rule the scan cannot have.

The order is `numeric_order`: per numeric slot, every document that *has* a finite value, sorted.
**Derived at load, never serialized**, exactly like `block_max` — it is a sort of a column the index
already stores, and a stored copy is a second version of the same fact that can disagree with the
first. Four bytes per valued document per numeric slot, and **no format or ABI change at all**.

Membership is tested by binary search in each query term's posting list, which is the arm's cost
and the reason the choice below weighs `terms` against postings.

**The tie is the trap.** `p32` fixed that ties break by relevance and then by document id, so
stopping at exactly `k` returns an arbitrary subset of the documents sharing the `k`-th value — and
it looks completely plausible. The walk therefore continues while the value is unchanged, sorts the
whole boundary group, and only then truncates.

## The finding: the new arm is usually worse

Measured on presyo's 241,677 real products, 336 real queries, arms forced individually:

| `search_sorted`, k=10 | p50 | p99 |
|---|---|---|
| **arm: scan** (`p32`) | 19 us | 2,615 us |
| **arm: walk** (`p46`) | **1,579 us** | **9,779 us** |

**The walk is 83x worse at the median and 3.7x worse at the tail.** That is not a defect in it. Most
presyo queries are *selective* — a handful of matches over a quarter of a million rows — so "walk
until `k` match" walks nearly the whole column. The scan's cost tracks match count; the walk's
tracks `k / selectivity`. They are mirror images, and each is catastrophic where the other is fine.

So neither arm ships alone. **Both ship, and the cheaper is chosen** from numbers the index already
has, before either runs:

```
scan  ~  sum of the query terms' document frequencies
walk  ~  k * live / matches * terms
```

`sum(df)` over-estimates the match count whenever terms overlap, which makes the walk look worse
than it is and biases the choice toward the scan. That is the conservative direction on purpose:
the scan is the arm that has always been correct here.

## What the chooser is worth

Same corpus, same probe set, four consecutive runs:

| | p50 | p99 |
|---|---|---|
| `search_sorted`, scan only (`p32`) | 19 us | 2,615 / 2,560 / 2,405 / 2,639 us |
| **`search_sorted`, chooser (`p46`)** | **19 us** | **1,387 / 1,366 / 1,332 / 1,361 us** |

**The tail nearly halves — ~1.9x — and the median does not move.** Which is the whole shape of the
claim: the chooser leaves the queries that were already fast exactly alone, and only fires on the
broad queries that were the tail in the first place. `p32`'s own figure was 2,521 us, so the "before"
here reproduces its measurement rather than inventing a flattering one.

## Two implementations of one answer is a bug factory

The only reason this is safe to ship is that the two arms are held to being the **same answer**, not
merely to being individually plausible.

- `sorted_arms_agree_document_for_document` forces both over 6 queries x 7 values of `k` x 2
  directions on a fixture built for the hard case: **three price levels over sixty rows**, so every
  `k` lands *inside* a tie group. Agreement is document for document, score for score, bucket for
  bucket. The public entry point is then required to return whichever it chose, unchanged.
- `facet-shop` repeats it on **336 real queries x 2 directions**, every run. **0 disagreements**
  across four runs, 2,688 comparisons.
- The filtered path is checked against the unfiltered answer **narrowed by hand**, not against the
  other arm — so it cannot pass by both arms being wrong the same way.

`Index::search_sorted_arm` is public for exactly this reason and says so: the chooser deliberately
never runs both, so without it the claim could not be checked at scale by anything outside the crate.

## Still open

- **The chooser is an estimate, not a measurement.** It cannot see term overlap, so a two-term query
  whose terms cover the same documents is judged twice as broad as it is, and takes the scan when
  the walk would have won. A cheap intersection estimate would sharpen it.
- **The walk re-scores from scratch per document.** It looks each document up in every term's
  posting list; a query with many expansions pays `terms` binary searches per candidate, which is
  what makes its median so poor. Nothing shares work between the two arms.
- **Only `k` and selectivity drive the choice**, not `offset`. `search_sorted` has no paged form yet,
  so this has not had to matter.
