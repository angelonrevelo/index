# P31 — numeric ranges: the other half of a filter bar

**Tier:** T1 · **Bin:** `facet-shop` · **API:** `with_numeric` / `search_range` / `range_tally` / `search_filtered`
**Status: SHIPPED, 2026-09-06. `OVERALL: PASS` on presyo's 241,677 real products.**

`p30` shipped categorical facets and named range facets as its top remaining gap:

> **No range facets.** Price buckets are the obvious next want and need numeric values, which the
> engine does not store.

A storefront's filter bar is categorical facets *and* a slider. The slider is two things — a range
filter and a histogram — and **neither is expressible with interned string labels**: `"9.99"` and
`"10.00"` sort and bucket as text, not as numbers.

## What was added

| | |
|---|---|
| `IndexBuilder::with_numeric(field)` | Parse a field once at build time into an `f64` column. Multiple columns, slot-ordered, independent of facet slots. |
| `Index::search_range(q, k, slot, lo, hi)` | Half-open `lo <= v < hi`. |
| `Index::range_tally(q, slot, &edge)` | Histogram over **every** matching document. |
| `Index::search_filtered(q, k, &facet, &range)` | **The whole filter bar in one pass**: brand AND category AND size. |
| C ABI | `idx_build_numeric`, `idx_search_range`, `idx_range_tally`, `idx_numeric_slot_count`. ABI 4 -> **5**, 25 symbols. |
| Format | `IDXTEXT4` -> **`IDXTEXT5`**, 13 -> 15 spans. |

## Three decisions that are the whole feature

**Half-open ranges, `lo <= v < hi`.** Adjacent buckets in a slider must not both contain the
boundary. If they did, the counts printed beside them would add up to more than the result set, and
the shopper would click a bucket and get fewer items than it promised. The tests assert this
directly: a value sitting exactly on an upper bound is excluded.

**Absent means absent, not zero.** Text that does not parse as a finite number becomes `NaN`, which
fails every comparison, so such a document is in **no** range — not even `[f64::MIN, f64::MAX)` —
and is counted in **no** histogram bucket. The alternative, defaulting to `0.0`, would silently pile
every unparseable row into the cheapest bucket, which is exactly where a shopper looks first. The
consequence is stated rather than hidden: **histogram counts sum to at most the match count**, not
exactly it.

**`f64`, not `f32`.** A float carries 24 bits of mantissa, so a price in minor units stops being
exact above about 16.7 million. Eight bytes per document per column is the price of never having to
work out where that boundary falls for a given currency.

## Measured on presyo's real catalogue

presyo's export carries no price column, so this ranges over `size_value`, the numeric attribute it
does carry. The shape is identical to a price slider.

241,677 products, two facet slots (19,793 brands, 146 categories) plus one numeric column:

| | |
|---|---|
| index size | 28.2 MB -> 34.5 MB, **+17.55 B/doc** for all three columns |
| build | 2.3 s -> 3.3 s |
| tally correctness | **40 / 40 exact** vs exhaustive scoring |
| filter-then-rank | 104 hardest cases: **0 short pages, 0 leaks** |
| conjunction | 80 (brand, category) pairs: **0 wrong** |
| **numeric range** | **60 histograms: 0 wrong** |

| latency, 336 queries | p50 | p99 |
|---|---|---|
| `search` (unfiltered) | 13 us | 431 us |
| `search_facet` | 5 us | 417 us |
| `facet_tally` | 11 us | 83 us |
| **`range_tally`** | **9 us** | **179 us** |

**A six-bucket histogram over 241,677 products costs 179 us at p99.** A filter bar can recompute
every count on every keystroke.

### How the histogram is checked

Two independent properties, because a histogram that is merely self-consistent is worthless:

1. **Each bucket count equals what the range *filter* returns for that bucket** — a different code
   path, run with `k = doc_count` so it cannot be truncated.
2. **The buckets partition**: their total must not exceed the number of documents the query matches
   at all. This is what catches a boundary counted twice, which property 1 alone would not.

## Honest limits

- **`size_value` is not a price.** The shape is the same and the code does not know the difference,
  but no claim is made here about presyo's actual price distribution.
- **The range scan is linear in the candidate set.** There is no numeric index — no B-tree, no
  sorted column with binary search. At 241,677 documents that costs 179 us and does not matter; at
  a scale where it does, this is the obvious thing to build and it is not built.
- **`range_tally` walks the full posting union**, like `facet_tally`, so its cost tracks the query's
  match count rather than `k`.
- ~~**No sort-by-value.**~~ **Closed by `p32-sort-by-value.md`**, which also measures what giving
  up pruning costs: p99 2,521 us against 315 us for a ranked search.
- **Edges must be ascending** and are not validated. A caller passing them out of order gets
  meaningless counts rather than an error.

## Reproduce

```sh
cargo run -p index-bench --release --bin facet-shop
node js/smoke.mjs                  # range + histogram checks included
python host/python/index_ffi.py    # same checks, native tier
```
