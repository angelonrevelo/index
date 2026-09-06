# P32 — sort by value, and what it costs to give up pruning

**Tier:** T1 · **Bin:** `facet-shop` · **API:** `search_sorted` / `search_sorted_filtered`
**Status: SHIPPED, 2026-09-06. `OVERALL: PASS`. The tail is 8x a ranked search, and that is the point.**

`p31` shipped numeric ranges and named the gap:

> **No sort-by-value.** A numeric column that can be filtered cannot yet be ordered on, which is the
> next thing a shopper reaches for after a price filter.

## What was added

| | |
|---|---|
| `Index::search_sorted(q, k, slot, ascending)` | Order by a numeric column instead of relevance. |
| `Index::search_sorted_filtered(...)` | The whole filter bar first, then the sort. |
| `idx_search_sorted` | ABI 5 -> **6**, 26 symbols. Exercised from both hosts. |

No format change: sorting reads the column `p31` already stores.

## Giving up pruning is the entire story

Every latency number this project has published rests on the same mechanism: **relevance ordering
lets the engine stop early.** Once the pool holds `k` documents no unseen document can beat, the
rest of the postings can be skipped, and that is why 1 M documents answer in microseconds.

**A numeric order gives it nothing to stop on.** The cheapest item in the corpus may match the query
worst, so it cannot be skipped on any relevance bound — every matching document must be visited.
That is not an implementation shortcut to be optimised away later; it is what sorting by an
unrelated column *means*.

So `search_sorted` accumulates the full match set the way `search_exhaustive` does, applies the
filter before scoring rather than after, and orders by value. Cost tracks the query's **match
count**, not `k`.

**Measured, and it is the predicted shape rather than a surprise:**

| 336 real queries, 241,677 products | p50 | p99 |
|---|---|---|
| `search` (relevance, prunes) | 10 us | 315 us |
| `facet_tally` | 9 us | 64 us |
| `range_tally` | 10 us | 179 us |
| **`search_sorted`** | **18 us** | **2,521 us** |

**8x the tail of a ranked search.** Still interactive at a quarter of a million products, and the
number a host needs in order to decide whether to offer the control at all. Publishing the p50 alone
would have hidden it.

## Three decisions

**Absent values are excluded.** A product with no price has no position in a price order. It is
dropped, exactly as it is from a range filter and a histogram bucket — one rule, three places.

**`k` truncates after ordering.** `search_sorted(q, 2, ..)` returns the two cheapest matches, not two
arbitrary ones that happened to be visited first. Obvious, and asserted, because the natural
early-exit implementation gets it wrong.

**Ties break by relevance, then by document id.** A page of equally priced items is still ordered by
how well it matches, and is stable across runs. Tested with three documents at the same value: the
shortest field wins on BM25 length normalisation, and the order is identical on repeat.

`score` and `typo_bucket` are still populated honestly, so a host can show relevance beside a price
sort. They simply do not determine the order.

## Verified two ways

A sorted result that is merely *in order* proves very little — an implementation that dropped half
the matches would also be in order. So `facet-shop` checks:

1. **Order**, ascending and descending, over the returned values.
2. **Membership against a different code path**: every row of the sorted top-25 must also appear in
   a `search_range` covering the same span. A bug that drops or invents documents fails here even
   when the ordering is perfect.

**60 queries, 0 wrong.**

## Honest limits

- **No sorted index.** This is a linear pass over the match set. A sorted column with a skip list
  would let `k` cheapest be found without visiting everything, and would change the p99 above. Not
  built; the 2,521 us is the cost of not building it.
- **One column at a time.** No secondary sort key, no "price then rating".
- **No sort on relevance-plus-value blends**, which is what most storefronts actually ship
  ("relevance" as a default sort that quietly weights popularity).
- **The `p31` limits still stand**: no numeric index for ranges either, and edges are unvalidated.

## Reproduce

```sh
cargo run -p index-bench --release --bin facet-shop
node js/smoke.mjs
python host/python/index_ffi.py
```
