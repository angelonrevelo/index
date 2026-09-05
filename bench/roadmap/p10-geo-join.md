# P10 — point-in-polygon as an index, not a scan

**Tier:** T1 · **Bin:** `cargo run -p index-bench --release --bin geo-join`
**Status: MEASURED, 2026-09-05. Answer: yes — 28.2× over the naive scan, 9.1× over a competent one.**

## Why this one

`p9-sfc-2d.md` closed "is a viewport a range scan over an ordered key?" for **points** and recorded
two gaps. This closes the second:

> *"**Polygons, not points.** A barangay is a polygon; covering a region by its bounding box and
> then testing point-in-polygon is a second step this does not do."*

It also lands on the single largest hole the geometry research turned up. The survey in
`docs/research/geometry-sota.md` found FlatGeobuf's bbox-over-HTTP **solved and boring**, PMTiles
tile addressing solved, COPC solved, meshopt the settled vertex codec — and then this:

> **Point-in-polygon in the browser at scale — no published numbers at all.** No JS-vs-WASM PIP
> throughput benchmark, no GPU-PIP benchmark, no rasterization-PIP benchmark. *"This is a
> measurement vacuum, not a solved problem."*

The only calibration point in any language is DuckDB's spatial join: **58,033,724 Citi Bike rides ×
310 polygons in 28.7 s ≈ 2.02 M points/s**, native and multicore on an M3 Pro
([duckdb.org, 2025-08-08](https://duckdb.org/2025/08/08/spatial-joins.html)).

## The workload — maphy's, not a model of it

maphy depends on `@turf/boolean-point-in-polygon` and answers "which province is this POI in?" by
looping features. Both sides of the join are real files on disk (see `bench/fixture/README.md`):

- **53,715 POIs** — 50,412 schools, 2,070 hospitals, 1,209 ports, 24 volcanoes.
- **88 provinces** — 991 rings, 37,507 vertices, unclipped, PSGC-coded. Ring sizes mean 38, **max 2,214**.
- **2,454 municipal polygons** — 73,519 vertices, decoded from maphy's own PMTiles. Mean 30, max 494.

Two polygon sets, deliberately: **few-and-large** versus **many-and-small**. They give opposite
answers, and a single dataset would have hidden that.

The join is a real answer, not just a timing: Cebu (PSGC 0702200000) holds 1,895 POIs, Pangasinan
1,629, Leyte 1,601.

## The arms

Comparing an index against a *bad* baseline proves nothing, so the middle arm — what any competent
hand-rolled version does — is measured too.

1. **scan** — point-in-polygon against every polygon. The naive loop.
2. **bbox** — reject by bounding box first. The competent loop.
3. **cell index** — rasterize to a 2^L grid, sort cells by Hilbert key. A cell is *absent* (ocean,
   answered by a failed binary search), *interior* (answered by its stored id, **zero** geometry),
   or *boundary* (test only the polygons listed there).
4. **vertex index** — the same grid, but a cell stores **the polygons containing its centre** and
   **the boundary segments crossing it**. The answer is a parity walk from the centre:
   `inside(p,P) = inside(m,P) XOR (crossings of m→p with P's local segments is odd)`.

Arm 4 is the "can vertices themselves be the index?" question. Its appeal is that work is
proportional to the vertices **near the query**, not to the size of the polygon — a 2,214-vertex
coastline should cost the same as a square when the query sits in a quiet stretch of it.

## Measured — 2,454 municipal polygons (many, small)

| arm | build | query | tests | vs scan |
|---|---|---|---|---|
| scan | — | 335.80 ms | 94,888,235 | 1.0× |
| bbox reject, then pip | — | 108.52 ms | 53,257 | 3.1× |
| cell index, L=6 | 18 ms | 12.79 ms | 239,484 | 26.2× · **19 KB** |
| **cell index, L=8** | 116 ms | **11.93 ms** | 66,773 | **28.2×** · 126 KB |
| cell index, L=10 | 618 ms | 14.75 ms | 22,772 | 22.8× · 1,399 KB |
| cell index, L=12 | 4,081 ms | 43.03 ms | 6,845 | 7.8× · 19,743 KB |
| vertex index, L=8 | 114 ms | 17.93 ms | 592,694 | 18.7× · 3,254 KB |

## Measured — 88 provinces (few, large)

| arm | build | query | tests | vs scan |
|---|---|---|---|---|
| scan | — | 34.21 ms | 2,103,199 | 1.0× |
| bbox reject, then pip | — | 30.06 ms | 71,756 | 1.1× |
| cell index, L=8 | 17 ms | 17.72 ms | 30,712 | 1.9× · 121 KB |
| **vertex index, L=8** | 33 ms | **11.65 ms** | 270,641 | **2.9×** · 1,742 KB |

## What the two tables say together

**The vertex index wins exactly when rings are large; the cell index wins when polygons are many and
small.** On provinces the vertex index is 1.5× faster than the cell index (11.65 vs 17.72 ms); on
municipal polygons it is 1.5× *slower* (17.93 vs 11.93 ms) and **26× larger** (3,254 vs 126 KB).

The reason is visible in the fixtures and not in the timings: municipal rings average 30 vertices,
so a whole-ring test is *already* local and the parity walk's advantage evaporates while its storage
cost does not. Province rings reach 2,214 vertices, and there the walk pays.

Had this been measured on one dataset it would have produced a confident and wrong general claim in
whichever direction the dataset leaned. **The honest statement is a rule with a condition attached,
not a winner.**

**L is a real knob with an interior optimum**, and past it the index gets slower *and* bigger:
L=12 holds 1.67 M cells / 19.7 MB and drops to 7.8×, because the binary search stops fitting in
cache. The best operating point is **L=6–8**, and L=6 answers at 26.2× in **19 KB**.

## Throughput, against the only public reference

| | points/s | polygons | conditions |
|---|---|---|---|
| DuckDB `SPATIAL_JOIN` | 2.02 M | 310 | native, **multicore**, M3 Pro |
| **this, cell index L=8** | **4.50 M** | **2,454** | native, **single thread** |
| **this, vertex index L=8** | **4.61 M** | 88 | native, **single thread** |

Roughly **2.2× DuckDB's throughput on one thread with 8× more polygons** — but stated with the
caveats that make it comparable rather than flattering: different hardware, different data, and
DuckDB is solving the harder general case. Most importantly **this is native Rust, not WASM**, and
the vacuum the research identified is specifically a *browser* number. That measurement is the next
row, not a claim this table gets to make.

## Three bugs this benchmark caught in itself

All three were caught by asserting equality with the scan arm over all 53,715 points. **Sampled
agreement would have missed the third entirely**, and the first two looked like performance
properties rather than defects.

1. **Boundary and interior are not mutually exclusive — 28 wrong answers at L=8.** A cell crossed by
   province A's coastline can also lie inside province B, whose own ring never enters the cell,
   because neighbouring provinces were simplified independently and their shared borders do not
   coincide — there are slivers metres wide between them. The complete rule is
   `{P : P's ring crosses the cell} ∪ {P : the cell centre is inside P}`, and it is provably a
   superset: if any point of a cell is inside P while the centre is not, P's boundary crosses it.
2. **A cell centre can be inside several polygons at once — 1,496 wrong answers.** Tile-clipped
   fragments overlap. The centre's answer had to become a *set*.
3. **The parity walk is undecidable when the ray passes exactly through a vertex — 1 wrong answer in
   53,715.** Two segments meet there, each reports a touch, and the count flips twice or not at all;
   the result is decided by floating-point luck. `seg_cross` is now three-valued and returns
   `Degenerate`, and the query falls back to a full test. Picking a tie-break and hoping would have
   passed this bench and failed somewhere else.

This is the third time in this repo a *methodology* produced a wrong conclusion before the code did
(see `p7-scale.md` on document replication, `p9-sfc-2d.md` on sampling a cover instead of computing
one) — which is the argument for writing the methodology down next to the number.

## The negative result: vertex dedup does not pay here

The other half of "indexing vertices" is storing them deduplicated — shared-vertex topology, the
premise of TopoJSON's arc sharing. On the real province geometry:

| quantization | unique / total | dedup |
|---|---|---|
| exact coordinates | 34,442 / 37,507 | **1.09×** |
| ~340 m grid | 34,442 / 37,507 | 1.09× |
| ~85 m grid | 34,442 / 37,507 | 1.09× |
| ~21 m grid | 34,442 / 37,507 | 1.09× |
| ~1,355 m grid | 17,459 / 37,507 | 2.15× *(lossy — collapses distinct vertices)* |

**1.09× is not worth a topology layer**, and it is flat from 340 m down because the source is
already quantized: adjacent provinces do not share vertex coordinates, they were simplified
independently. Most of the 14.5 % duplicate *instances* are ring-closing vertices. TopoJSON's
premise needs a topologically clean source, and maphy's is not one.

This agrees with the survey's own ranking, which put mesh/vertex dedup last: *"the gap is real, the
demand is unproven."*

## Not measured yet

- **WASM.** The number the research says nobody has published is a browser number. This is native.
- **Barangays.** 41,966 polygons is 17× the municipal set. The trend across 88 → 2,454 polygons is
  strongly favourable (1.9× → 28.2×) but it is an extrapolation until run.
- **Against a real R-tree.** Still no head-to-head with `geo-index`, SQLite R\*Tree, or DuckDB on
  the same machine and data. `p9`'s caveat stands: sufficient is not the same as better.
- **Multi-threading.** Every arm here is single-threaded; DuckDB's reference number is not.

## Reproduce

```sh
cargo run -p index-bench --release --bin geo-join                                    # 88 provinces
INDEX_BENCH_PROVINCE=bench/fixture/maphy-municipal.txt \
  cargo run -p index-bench --release --bin geo-join                                  # 2,454 polygons
```
