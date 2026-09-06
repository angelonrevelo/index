# P38 — what incremental updating actually costs

**Tier:** T1 · **Bin:** `segment-scale` (new) · **Corpus:** presyo, 241,789 real products
**Status: PASS, 2026-09-06. Ranking holds; latency does not. Keep segment counts in single digits.**

`p37` made live updates reachable from every host and closed with the limit that mattered:

> **Not measured at scale.** What a 50-segment collection costs at a million rows is unmeasured.

Two costs. The first is obvious and the second is the one nobody measures.

## The measurement

A monolithic `Index` over all 241,789 products is the oracle. The same rows are then split **in
corpus order** — so ordinals match and the two can be compared document for document — into a 60 %
base plus evenly-sized deltas, which is the shape an application that indexes once and appends daily
actually produces. Equal segments would be a friendlier shape than reality.

| segments | build | p50 | typo p99 | broad overlap | **selective rank-1** | selective overlap |
|---|---|---|---|---|---|---|
| **1** (control) | 1.71 s | **10 us** | 2,646 us | 100 % | 100 % | 100 % |
| 2 | 1.68 s | 20 us | 4,089 us | 63.7 % | **98.0 %** | 90.0 % |
| 5 | 1.60 s | 63 us | 5,085 us | 51.6 % | **95.8 %** | 86.6 % |
| 10 | 1.64 s | 322 us | 6,891 us | 51.1 % | **96.2 %** | 86.1 % |
| 25 | 1.60 s | 1,210 us | 10,378 us | 51.9 % | **96.4 %** | 85.1 % |
| 50 | 1.67 s | 2,326 us | 12,050 us | 55.6 % | **96.2 %** | 85.2 % |

**Latency is the real cost: 10 us → 2,326 us, 232× at 50 segments.** A query runs against every
segment, so this is the expected shape and the reason `needs_compaction()` exists. **Ranking is
fine**: what a shopper sees first is right 96–98 % of the time even at 50 segments.

## Getting the metric wrong first, twice

The first version of this bin reported **exact top-10 sequence equality** and nothing else, and it
said segmentation destroys ranking: **19.3 % at two segments.** That number is true and it is
useless. Two corrections were needed before it meant anything.

**First: the query set.** The probe was a single first word — `"Colgate"` — which matches thousands
of near-identical products. The bin now measures this directly: **70.3 % of broad queries have a
top-10 whose scores span under 5 %.** Among near-ties the order is arbitrary, so *any* scoring
change reshuffles it. Measuring only that arm blames segmentation for instability the query set
already had. A selective arm — whole product names — was added.

**Second: the metric.** The selective arm still read 32.4 %, so ties were not the whole story. Rather
than publish that, `INDEX_SEG_DIAG=1` prints the two rankings side by side:

```
---- disagreement 2: "Meiji Snack Chocolate 50g"
seg        doc  bucket      score   |   mono       doc  bucket      score
1          483       0    16.9187   |   1          483       0    17.0021
2       185015       0    15.7361   |   2       142036       0    15.6295
3       142036       0    15.5133   |   3       185015       0    15.5642
```

**The same documents, in a slightly different order.** Ranks 2 and 3 trade places because their
mono scores differ by 0.07 and per-segment IDF moved them past each other. Exact sequence equality
counts that as a total failure; a shopper would not notice it.

So the headline metrics are now **set overlap** — did the same documents come back — and **rank-1**,
which is what a user sees first. Exact-order equality is deliberately not the headline, and the
reason is printed in the bin's own output.

**This is the third time in this roadmap that a scary number turned out to be the measurement**
(`p35` aliasing, `p34` timer resolution, now this). The pattern is consistent enough to be a rule:
*before publishing a number that says something is broken, print the raw rows it came from.*

## What this means for using it

- **Segments are cheap to build and cheap to hold.** Build time is flat across the sweep — total
  work is the same regardless of how it is split.
- **Keep the count in single digits.** Between 1 and 10 segments, p50 goes 10 → 322 us; both are
  interactive. Beyond that it stops being free.
- **Rebuild when `needs_compaction()` turns on.** It watches skew and deletions, which is exactly
  the axis this table shows to be expensive.
- **Broad, near-tied queries will reshuffle** and there is no fixing that with segmentation
  policy — those results were never stable. If stable ordering under ties matters, the tiebreak has
  to become part of the ranking contract, not an accident of score arithmetic.

## Honest limits

- **241 k documents, not a million.** The corpus is real rather than recombined, which is the
  tradeoff `p6` already documented. Segment *count* is the axis swept here, not corpus size.
- **Uniform delta sizes.** Real appends are lumpy — a thousand rows one day, three the next.
- **No deletions in the sweep.** `needs_compaction` also watches deleted ratio, and its contribution
  to latency is unmeasured.
- **One split policy.** 60 % base was chosen, not derived. A 90/10 split would show less drift and
  is probably more realistic for a daily append.
- **No merge policy exists to evaluate.** Compaction is a rebuild; nothing merges two segments.

## Reproduce

```sh
cargo run -p index-bench --release --bin segment-scale
INDEX_SEG_DIAG=1 cargo run -p index-bench --release --bin segment-scale   # the raw rankings
```
