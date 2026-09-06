# P9 — is a viewport a range scan over an ordered key?

**Tier:** T1 · **Bin:** `cargo run -p index-bench --release --bin sfc-2d`
**Status: MEASURED, 2026-09-05. Answer: yes, at 100 % recall.**
**Re-run on REAL maphy coordinates the same day — the synthetic result held.**

> **The over-fetch conclusion in this file is WRONG and is superseded by
> `bench/roadmap/p77-geo-tier.md` §5.** Every "over-fetch" figure below was produced by a cover that
> kept its pending cells on a stack and emitted whatever happened to be on it when the budget ran
> out. Splitting the cell with the largest area outside the rectangle first takes the same 1 %
> viewport from **16.5× to 1.14×** at 100 % recall with **fewer** ranges. The claim below that
> "over-fetch is dominated by point clustering rather than by cover coarseness" is the sentence that
> was wrong, and the recall and range-count columns are the ones that stand.

## Why this reopens a closed question

`docs/roadmap-rejected.md` rejected "vectors as a physical ordered-key range scan", and it was right
to: space-filling-curve locality collapses past ~8–10 dimensions for information-theoretic reasons,
and no clever curve fixes it. `bench/roadmap/p3-sfc-dimensionality.md` was written to prove that with
one experiment — and it also wrote down the harness's own correctness condition:

> *"the harness is correct if at D=2 recall@10 ≈ 1.0 (sanity — SFC works in 2D)"*

**That sanity condition is the product question for geometry.** A map is D=2. The rejection was about
high dimensions and says nothing against the plane, and S2 is the production proof that the plane
works. So this measures D=2 on its own terms, against the workload maphy has: **41,966 barangay
polygons and 95,200 POIs** over the Philippine bounding box, on a 16-bit-per-axis grid (~15 m cells).

The bin now runs on **maphy's actual POI coordinates** — `bench/fixture/maphy-poi.csv`, 53,715
points extracted from `apps/web/public/data/poi/*.geojson` (50,412 schools, 2,070 hospitals, 1,209
ports, 24 volcanoes) — falling back to synthetic clusters when the fixture is absent. Both are
quantized on an **identical grid**, widened to `114.2..126.7 E` so that Kalayaan, the westernmost
real POI at 114.287 E, is not clamped onto the grid edge.

## Measured — real vs synthetic, same grid, 53,715 points each

The synthetic model was 80 % gaussian clusters on real population centres, 20 % scattered. The
question this table answers is whether that stand-in was honest.

| | curve | viewport | recall | ranges | over-fetch |
|---|---|---|---|---|---|
| **real** | morton | 1 % | 100.0 % | 29 | 16.5× |
| **real** | **hilbert** | 1 % | **100.0 %** | **17** | 16.5× |
| synthetic | morton | 1 % | 100.0 % | 29 | 15.4× |
| synthetic | **hilbert** | 1 % | **100.0 %** | **17** | 15.4× |

**Identical range counts, over-fetch within 7 %.** kNN recall is if anything *better* on real data
(hilbert @0.1 % W: 89.4 % real vs 86.6 % synthetic), because actual settlement is more sharply
clustered than a gaussian mixture — which helps a space-filling curve rather than hurting it.

So the synthetic distribution was a sound stand-in and the earlier conclusion stands unchanged.
That is worth recording precisely because the opposite outcome was the thing to worry about: this
is the check that would have caught a result produced by the generator rather than by the curve.

Cover budget on real data: **budget 8 → 1 range at 21.3× over-fetch, 100 % recall**; budget 512 →
136 ranges at 12.8×. The operating point stays at 8–16 cells.

## Measured — 95,200 synthetic points (original run, narrower grid)

### kNN: a ±W window around the query key

| curve | W | recall@10 | candidates per true hit |
|---|---|---|---|
| morton / z-order | 0.1 % | 90.6 % | 20.9 |
| morton / z-order | 1.0 % | 97.7 % | 193.2 |
| **hilbert** | 0.1 % | **91.1 %** | 20.8 |
| **hilbert** | 1.0 % | 96.8 % | 194.8 |

The D=2 sanity condition holds. The two curves are within noise of each other for kNN, which is
worth stating because "Hilbert beats Morton" is repeated as though it were true everywhere.

### Viewport: a rectangle answered by contiguous key ranges

This is the query a map actually issues on every pan and zoom, and it is **not** the same question as
kNN.

| curve | viewport | recall | ranges | over-fetch | µs/query |
|---|---|---|---|---|---|
| morton / z-order | 1 % | **100.0 %** | 27 | 24.5× | 24 |
| morton / z-order | 5 % | **100.0 %** | 26 | 5.6× | 48 |
| **hilbert** | 1 % | **100.0 %** | **17** | 24.5× | 27 |
| **hilbert** | 5 % | **100.0 %** | **17** | 5.6× | 63 |

**Here Hilbert does earn it: 37 % fewer ranges at identical recall and identical over-fetch.** Fewer
ranges means fewer scans, and — for the format this repo already has — fewer HTTP range requests.

### Cover budget: the knob, swept

| budget | recall | ranges | over-fetch |
|---|---|---|---|
| **8** | **100.0 %** | **2** | 29.3× |
| 16 | 100.0 % | 3 | 22.1× |
| 32 | 100.0 % | 8 | 21.8× |
| 64 | 100.0 % | 17 | 24.5× |
| 128 | 100.0 % | 34 | 22.2× |
| 512 | 100.0 % | 133 | 21.0× |

**A budget of 8 answers a 1 % viewport with two key ranges at 100 % recall.** Spending 64× more
ranges buys 8.4 → 21.0 over-fetch, which is nothing. **The operating point is 8–16 cells**, and
over-fetch is dominated by point clustering rather than by cover coarseness — no amount of extra
cells fixes the fact that a viewport over Metro Manila contains a lot of points.

## What this means for the engine

A viewport is a **small set of contiguous ranges over a sorted key**. That is precisely the access
pattern `crates/index-text/src/format.rs` was built for: a section table, `u64` offsets, and a
`posting_span` that turns an id into a byte range fetchable on its own. A spatial layer is therefore
not a new engine — it is a new *key* over the machinery that already exists, plus a cover function.

## The bug this benchmark caught in itself

The first version **sampled** the cell grid with a stride instead of covering it, and measured
**0.7 % recall with 4,356 "ranges"**. A sampled cover is not a cover — it silently omits most of the
rectangle. The failure looked exactly like a property of the curve and was a defect in the harness.
That is the second time in this repo a benchmark's own methodology produced a wrong conclusion
before the code did (see `p7-scale.md` on document replication), which is itself the argument for
writing the methodology down next to the number.

## Not measured yet

- ~~**Real coordinates.**~~ **CLOSED 2026-09-05** — re-run on 53,715 real maphy POIs; the synthetic
  result held to within 7 % on over-fetch and exactly on range count. See the table above.
- ~~**Polygons, not points.**~~ **CLOSED 2026-09-05** — `bench/roadmap/p10-geo-join.md` measures
  point-in-polygon over 88 real provinces and 2,454 real municipal polygons: **28.2× over a naive
  scan, 9.1× over a bbox-prefiltered one**, with every arm asserted equal to the scan.
- **The alternative.** No comparison against an R-tree (SQLite R*Tree, FlatGeobuf's packed Hilbert
  R-tree) has been run. A range scan being *sufficient* is not the same as it being *better*, and
  FlatGeobuf in particular already does the cloud-native version of this well.

## Reproduce

```sh
cargo run -p index-bench --release --bin sfc-2d                       # real maphy POIs
INDEX_BENCH_POI=/nonexistent INDEX_BENCH_N=53715 \
  cargo run -p index-bench --release --bin sfc-2d                     # synthetic, same grid
```
