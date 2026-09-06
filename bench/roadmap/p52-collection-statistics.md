# P52 — a term's rarity belongs to the corpus, not to the segment holding it

**Tier:** T1 · **Bins:** `segment-scale`, `cdc-equivalence` · **API:** `CollectionStat`, `Searcher::set_collection_stat`
**Status: SHIPPED, 2026-09-06. No format change, no ABI change. 258 tests.
Broad-query agreement 51-64 % -> 84-87 %, and it no longer degrades with segment count.**

`p38` and `p50` both ended at the same wall from opposite directions:

> `p38`: ranking is fine — 96–98 % — but it does not improve with anything we tried.
> `p50`: **no compaction threshold holds rank-1 above ~93 %**, because the first delta already costs
> that much.

Both named the cause and neither fixed it: **each segment scored against its own collection
statistics.** IDF answers *"how rare is this term"*, and rarity is a property of the corpus, not of
the shard that happens to hold the row.

## The size of the distortion

A three-row delta scores a term at `ln(1 + 2.5/1.5) = 0.98`. The same term in a 4,001-row base
scores `7.89`. Same word, same corpus, **an eight-fold difference decided by which segment the row
landed in** — which is exactly what `p51` caught on production data: a school renamed through
`index apply` scored 1.31 in the incremental collection and 7.01 in a rebuild.

## The first fix made it worse, and that is the finding

The obvious move is to give every segment the collection's document count. It took twenty minutes
and `ranking_skew_is_bounded_for_a_small_delta` rejected it immediately: a selective query that must
find document 3 returned document 199 instead.

The reason is worth keeping. A small segment has a small `df` **as well as** a small `n`, and the two
partially cancel. Raising `n` alone leaves `df = 1` against a corpus of 200, so every term in a small
segment becomes *more* inflated than before:

| | delta (5 docs) | base (195 docs) |
|---|---|---|
| before, own `n` | `idf(product)` 0.29 | 0.03 |
| **`n` corrected only** | **3.60** | **0.03** |
| both corrected | 0.03 | 0.03 |

> **A partial correction of a ratio is not a partial improvement.**

So `df` is summed across segments too, and the two travel together.

## Why it has to be keyed on text

A term id is an index into one segment's dictionary and means something different in the next, so
document frequency can only be summed by the term's **text**. The dictionary stores an FST rather
than the strings — deliberately, at 7.46 bytes per term — so there is no id→text map to consult.

But the FST stream already yields the key bytes during traversal and the expansion was throwing them
away. `TermDict::expand_lazy_text` keeps them, for one allocation per match and **no extra
traversal**. The single-index path never calls it and pays nothing: matches are carried as
`(TermMatch, String)` pairs with an empty string, which does not allocate.

This is the shape Elasticsearch calls `dfs_query_then_fetch`, arrived at from the same constraint.

## Measured: the ceiling is gone

`segment-scale`, presyo's 241,789 real products, against a monolithic index over identical rows:

| segments | broad overlap | selective rank-1 | selective overlap |
|---|---|---|---|
| | `p38` -> **p52** | `p38` -> **p52** | `p38` -> **p52** |
| 2 | 63.7 % -> **84.5 %** | 98.0 % -> **99.8 %** | 90.0 % -> **95.0 %** |
| 5 | 51.6 % -> **84.4 %** | 95.8 % -> **99.2 %** | 86.6 % -> **93.6 %** |
| 10 | 51.1 % -> **85.8 %** | 96.2 % -> **99.4 %** | 86.1 % -> **93.7 %** |
| 25 | 51.9 % -> **87.0 %** | 96.4 % -> **99.0 %** | 85.1 % -> **92.6 %** |
| 50 | 55.6 % -> **84.9 %** | 96.2 % -> **99.4 %** | 85.2 % -> **92.4 %** |

**The numbers are now flat in segment count.** Two segments and fifty give the same answer quality,
which is the thing `p38` could not achieve by tuning: it raises the ceiling instead of moving the
trade-off.

`cdc-equivalence`, 8,000 change-stream operations, is the same story from the other side:

| ops | deleted | rank-1 `p50` -> **p52** | overlap `p50` -> **p52** |
|---|---|---|---|
| 400 | 1.7 % | 93.0 % -> 92.0 % | 87.7 % -> **89.5 %** |
| 4,000 | 14.7 % | 87.7 % -> **89.0 %** | 73.4 % -> **84.3 %** |
| 8,000 | 24.8 % | 84.0 % -> **88.7 %** | 66.4 % -> **82.0 %** |

**The fix flattens the DECAY, not the initial offset.** Rank-1 fell 9 points over 8,000 operations
before and falls 3.3 now; overlap fell 21 points and now falls 7.5. The residual ~8 % at batch 1 is
not segmentation at all — it is that a deleted row still counts toward `df` until a rebuild, which
is documented behaviour and is what compaction removes.

## What it costs, measured interleaved

Comparing against a number from an earlier run is not a measurement on a machine where a parallel
build is running: the one-segment p50 alone swung 15 → 30 us between runs while every quality column
stayed bit-identical. So both arms are timed back to back on the same collection, the method `p27`
and `p46` already use here.

| segments | p50 off | p50 on | | typo p99 off | typo p99 on | |
|---|---|---|---|---|---|---|
| 2 | 41 us | 47 us | **1.13x** | 5,422 us | 10,321 us | 1.90x |
| 5 | 139 us | 173 us | **1.25x** | 6,915 us | 14,227 us | 2.06x |
| 10 | 448 us | 783 us | 1.75x | 8,707 us | 17,846 us | 2.05x |
| 50 | 3,200 us | 6,621 us | 2.07x | 17,406 us | 33,665 us | 1.93x |

**A second dictionary expansion per segment is the price of knowing a corpus-wide `df` at all.**
At the segment counts `p38` tells you to stay within — single digits — that is +13 % to +25 % on p50.
The typo tail roughly doubles, and that is the number to weigh: it is the same tail `p47` failed to
move, now paying for ranking parity.

`Searcher::set_collection_stat(false)` trades the parity back. **On by default**, because with it off
a document's score depends on which segment holds it and nothing in the result says so.

## Still open

- **The second expansion is avoidable.** Both passes traverse the same automaton over the same
  dictionary; the first pass could hand its expansion to the second instead of the second
  re-deriving it. That is where the ~2x goes, and recovering it needs `plan` split into an
  expand phase and a weigh phase. Not done.
- **Learned-expansion terms still score per segment.** The expansion table stores term ids and no
  text, so those fall back to the local `df`. Every other query path is corrected.
- **`avg_len` is still per segment**, and unlike `df` it is baked into `sat` at build time, so
  correcting it means recomputing every posting's saturated contribution whenever the collection
  changes. It is a second-order effect — average field length varies little between segments of one
  corpus — but it is the remaining reason two arms are not bit-identical.
