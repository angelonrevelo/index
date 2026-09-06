# P77 — the geo tier, profiled and then rebuilt

**Tier:** T1 · **Bins:** `sfc-2d` · `geo-join` · `maphy-place` · `node js/geo-bench.mjs`
**Status: MEASURED 2026-09-06. One correctness bug fixed, one published conclusion overturned,
build up to 24.6× faster, the cell table 2.3–2.7× smaller, and two query shapes the library did not
previously have.**

> Everything below was measured on this machine by running the command in the row. Where a figure is
> computed rather than timed, it says so. The machine was noisy — other agents were building
> concurrently — so every A/B is a stash-and-re-run pair, and the **untouched control arms in the
> same pair bound the noise** at roughly ±20 %.

## 1. Where the time actually went — profile BEFORE any change

Method: temporary `eprintln!` phase timers inserted into `geo_join.rs`'s `build()` and its query
loop, `cargo build --release`, run on both fixtures, then the instrumentation reverted
(`git diff` clean) before anything was changed. The three build phases are the ones the code
already had names for.

### Build, milliseconds

| fixture | L | 1. rasterize rings | 2. interior fill | 3. boundary centre augmentation | 4. sort + pack | total |
|---|---|---|---|---|---|---|
| 88 provinces | 8 | 3.2 | **18.5** | 5.1 | 0.9 | 27.7 |
| 88 provinces | 10 | 7.9 | **297.5** | 22.2 | 11.6 | 339 |
| 88 provinces | 12 | 34.1 | **6,015.0** | 107.5 | 268.8 | 6,425 |
| 2,454 municipal | 8 | 7.3 | 1.9 | **135.3** | 0.9 | 145 |
| 2,454 municipal | 10 | 17.2 | 31.0 | **670.3** | 12.7 | 731 |
| 2,454 municipal | 12 | 65.2 | 738.9 | **3,009.5** | 217.1 | 4,031 |

**The two classification passes are 94 % of build on the province set and 75 % on the municipal
one**, and they are 94 % / 75 % for *different reasons*:

- Phase 2 is `O(cell × ring_len)` — for each polygon, test the centre of every cell in its box. Few
  large polygons means enormous boxes, so provinces pay here.
- Phase 3 is `O(boundary_cell × polygon)` — for each boundary cell, test every polygon. Many small
  polygons means a long inner loop, so the municipal set pays here.

Neither fixture on its own would have shown both. That is the same argument `p10-geo-join.md` makes
about measuring two polygon shapes.

### Query, milliseconds over 53,715 real POIs

`total` is the arm as reported. `lookup only` re-runs the same points doing the Hilbert key and the
binary search but skipping the geometry; `key only` does the key alone.

| fixture | L | total | lookup only | key only | absent | interior | boundary |
|---|---|---|---|---|---|---|---|
| provinces | 8 | 29.42 | 3.64 | 1.94 | 136 | 27,209 | 26,370 |
| provinces | 12 | 39.79 | **44.03** | 3.21 | 691 | 47,956 | 5,068 |
| municipal | 8 | 13.04 | 2.79 | 1.58 | 5,888 | 3,793 | 44,034 |
| municipal | 12 | 36.85 | **29.70** | 3.01 | 17,355 | 30,202 | 6,158 |

Two things fall out and they set the whole plan:

1. **At the operating point (L=6–8) the query is 79–87 % point-in-polygon**, not lookup. Making the
   binary search faster there is not worth doing.
2. **At L=12 the binary search is the query.** 44.03 ms of lookup against a 39.79 ms total is a
   measurement artefact of running the loop twice on a cold table, but the direction is
   unambiguous: 2.0 M cells at 13 bytes each stops fitting in cache, and that — not the geometry —
   is what makes `L` have an interior optimum.

## 2. The correctness bug: a NaN vertex answers confidently and wrongly

Found by a differential fuzz that asserts `locate == locate_by_scan` at **every** point of a grid,
not a sample. `crates/index-geo/src/lib.rs`, a ring whose fourth vertex is `NaN`:

| bad coordinate | order | probes | disagreements with the scan |
|---|---|---|---|
| `f64::NAN` | 4 | 40,401 | **300** |
| `f64::NEG_INFINITY` | 4 | 40,401 | **300** |
| `f64::INFINITY` | 4 | 40,401 | 0 |
| `1e300` | 4 / 6 | 40,401 | 0 |

The mechanism is one line. `segment_cell` maps a coordinate to a column with
`(v.floor() as i64).clamp(0, n - 1)`, and **Rust saturates a NaN float-to-int cast to `0`**. So the
two segments meeting a NaN vertex rasterize into column 0 rather than where they belong, the cells
they really cross are never marked boundary, and the interior fill then claims them — after which
they answer with a stored polygon id and **run no geometry at all**. `INFINITY` escapes because it
saturates to `i64::MAX`, which clamps to the far edge instead of the near one and happens not to
overlap the polygon here; that is luck, not a property.

This is not a synthetic worry. `geo_build` in `index-geo-wasm` is documented as parsing data that
may have arrived over a network, and GeoJSON with a `null` or truncated coordinate parses to NaN.

**Fix:** `Polygon::new` drops non-finite vertices. Dropping rather than rejecting is deliberate — it
keeps the index and `locate_by_scan` looking at the *identical* geometry, which is what makes the
differential assertion mean anything. Regression test:
`a_non_finite_vertex_does_not_produce_a_phantom_interior_cell`.

Three smaller contract holes were closed at the same time, each with a test:

- `CellIndex::build` now **asserts** non-degenerate bounds. A zero-width box divided by zero in the
  cell transform and produced an index whose every answer was a confident `None`. `geo_build`
  already refused this at the ABI; the library did not, so the two disagreed about what a valid
  index is.
- `hilbert_2d(0, …)` returns 0 explicitly instead of computing `1 << (order - 1)` on an underflowed
  `u32` — a debug panic and a masked shift in release, two answers from one public function.
- The **antimeridian** is now written down rather than left to be discovered:
  `longitude_past_180_is_indexable_for_antimeridian_bounds` records that a crossing viewport is
  expressed by extending `bounds` past 180 and that the caller must normalize into that frame
  (`locate(-179)` is `None`, `locate(-179 + 360)` is the polygon). Poles and exact-corner queries
  are covered by `a_polygon_touching_the_pole_is_exact`.

What the fuzz **did not** find is worth recording too: 30 random multi-polygon trials with holes at
three grid resolutions, enclaves inside holes, edges lying exactly on cell boundaries, zero-area and
duplicate-vertex rings, empty polygons, and polygons reaching outside `bounds` all agree with the
scan **everywhere**, at every probe. The classification algorithm is sound; the bugs were all at the
edge of the input contract.

## 3. Build: one row sweep instead of two nested loops

Both classification passes fall out of a single sweep. Every ring edge is turned into the horizontal
crossings it makes with the grid's row centre lines, bucketed by row. A row is then swept left to
right carrying a per-ring parity, so the set of polygons containing a cell centre is known in `O(1)`
per cell — for *every* polygon at once, in ascending id, which is also the tie-break rule. Cost
falls from `O(cell × ring_len) + O(boundary_cell × polygon)` to the total edge-row span plus one
visit per cell.

The crossing predicate and the `x` it solves for are written exactly as `point_in_ring` writes them,
so the sweep and a direct point test cannot disagree about a tie.

**A/B, `cargo run -p index-bench --release --bin geo-join`, three runs each side, minimum taken,
before obtained by `git stash`:**

| fixture | arm | build before | build after | speedup | query before | query after |
|---|---|---|---|---|---|---|
| provinces | cell L=6 | 3 ms | 5 ms | 0.6× | 32.44 ms | 39.12 ms |
| provinces | cell L=8 | 23 ms | 8 ms | **2.9×** | 24.17 ms | 24.83 ms |
| provinces | cell L=10 | 329 ms | 33 ms | **10.0×** | 20.26 ms | 21.57 ms |
| provinces | cell L=12 | 7,002 ms | 285 ms | **24.6×** | 50.20 ms | 37.24 ms |
| municipal | cell L=6 | 27 ms | 8 ms | **3.4×** | 16.44 ms | 15.77 ms |
| municipal | cell L=8 | 152 ms | 17 ms | **8.9×** | 15.22 ms | 14.86 ms |
| municipal | cell L=10 | 949 ms | 51 ms | **18.6×** | 23.36 ms | 13.98 ms |
| municipal | cell L=12 | 5,051 ms | 330 ms | **15.3×** | 33.22 ms | 27.74 ms |
| provinces | *scan (control)* | — | — | — | 42.77 ms | 48.45 ms |
| provinces | *bbox (control)* | — | — | — | 35.90 ms | 46.47 ms |
| municipal | *scan (control)* | — | — | — | 431.67 ms | 443.67 ms |
| municipal | *vertex L=8 (control)* | 109 ms | 134 ms | — | 19.04 ms | 19.66 ms |

**The strongest evidence that the index is unchanged is not a timing.** Every cell count, every
interior/boundary split and every `pip tests` figure is byte-identical across the pair — provinces
L=12 is 2,041,156 cells and 5,320 tests on both sides — and the bin `exit(1)`s unless all 53,715
answers equal the scan arm's. A rewritten build that produced the same table 24.6× faster is the
claim, and those are the numbers that establish it.

**The control arms are why the query column is reported as a wash.** The province scan and bbox arms
— which this lane did not touch — came out 13 % and 29 % *slower* in the AFTER pair, so the query
column is measuring the machine, not the change. Two rows survive that: province L=12 got 35 %
faster and municipal L=10 got 67 % faster while their controls moved ≤ 4 %, which is the narrower
`u32` key doing what §1 predicted it would.

**Browser half, `node js/geo-bench.mjs`, two runs each side:**

| arm | build before | build after | speedup |
|---|---|---|---|
| WASM cell index, L=7 | 83 ms | 31 ms | **2.7×** |
| WASM cell index, L=8 | 167 ms | 24 ms | **7.0×** |
| WASM cell index, L=9 | 446 ms | 51 ms | **8.7×** |

Query throughput there is not claimable in either direction: the JS scan control moved 606 → 744 ms
between the pairs.

## 4. Memory: 13 bytes a cell to 8, and the heap allocation per boundary cell to zero

The cell table was `Vec<(u64, Cell)>` where `Cell` is `enum { Interior(u16), Boundary(Vec<u16>) }`.
`std::mem::size_of::<(u64, Cell)>()` is **32** on this target (measured, `rustc -O`), plus one heap
allocation and `2 × len` bytes for every boundary cell's list.

It is now a `u32` Hilbert key (a key at `order ≤ 16` needs at most 32 bits), a `u32` candidate
offset, and a shared candidate array — with the interior flag folded into the top bit of the single
candidate id rather than carried in a parallel `Vec<u8>`.

| index | cells | candidates | before, computed | after, measured | ratio |
|---|---|---|---|---|---|
| municipal L=8 | 9,290 | 18,027 | 323 KB + 8,077 allocations | **143 KB, 0 allocations** | **2.3×** |
| provinces L=12 | 2,041,156 | ~2.15 M | ~65.3 MB + 107,299 allocations | **24.0 MB, 0 allocations** | **2.7×** |

The "before" column is computed from the measured `size_of` and the cell/candidate counts the bin
prints, not timed — it is arithmetic, and it is labelled as such. The "after" column is what the bin
now prints. Note that the bin's old byte figure (`cells × 10 + members × 2`) was an *idealized
packed* size that the code never actually had; the new one is the real resident size, which is why
the printed KB moves the wrong way at L=8 (121 → 124) while the real footprint fell by 2.3×.

Halving the searched array is also the query win in §3's last two rows: the binary search is the
cache-missing part of a lookup.

## 5. The overturned conclusion: the cover was spending its budget at random

`p9-sfc-2d.md` measured a 1 % viewport at **16.5× over-fetch** and concluded:

> *"over-fetch is dominated by point clustering rather than by cover coarseness — no amount of extra
> cells fixes the fact that a viewport over Metro Manila contains a lot of points."*

**That is wrong, and the mistake is in the harness, not in the curve.** The cover kept its pending
cells on a **stack** and emitted whatever was on it when the budget ran out — so a level-2 cell
covering a quarter of the country got emitted whole because of traversal order. Keeping them in a
max-heap keyed by the area *outside* the rectangle, and always splitting the worst offender, spends
the same budget deliberately.

`cargo run -p index-bench --release --bin sfc-2d`, 53,715 real maphy POIs, 1 % viewport, hilbert:

| split order | budget | recall | ranges | over-fetch |
|---|---|---|---|---|
| descent (LIFO) | 8 | 100.0 % | 1 | 21.32× |
| descent (LIFO) | 64 | 100.0 % | 17 | 16.46× |
| descent (LIFO) | 512 | 100.0 % | 136 | 12.82× |
| **best-first** | 8 | 100.0 % | 3 | **1.80×** |
| **best-first** | 64 | 100.0 % | **14** | **1.14×** |
| **best-first** | 512 | 100.0 % | 100 | **1.01×** |

**14.4× less over-fetch at the same budget, with fewer ranges, at identical 100 % recall.** At
budget 512 the cover is within 1 % of a perfect index. Spending extra cells *does* fix it; the
earlier sweep could not show that because every budget was being spent on arbitrary cells.

This is the third time in this repo a benchmark's own methodology produced a wrong conclusion before
the code did — after `p7-scale.md` on document replication and `p9`'s own sampled cover. It is also
the second time `p9`'s cover specifically has been the culprit, which is worth saying plainly.

The headline table changes with it, and it changes what Hilbert is *for*:

| curve | split order | viewport | recall | ranges | over-fetch |
|---|---|---|---|---|---|
| morton | descent (LIFO) | 1 % | 100.0 % | 29 | 16.46× |
| hilbert | descent (LIFO) | 1 % | 100.0 % | 17 | 16.46× |
| morton | best-first | 1 % | 100.0 % | 25 | 1.14× |
| **hilbert** | **best-first** | 1 % | 100.0 % | **14** | **1.14×** |

Over-fetch is now identical between the two curves, so **Hilbert's entire advantage is range count —
1.8× fewer scans, which for the portable format is 1.8× fewer HTTP range requests.** The previous
table let that be confused with a locality claim.

## 6. Capability the library did not have

`crates/index-geo` shipped `MAGIC` as a `pub const` and **never used it**: there was no serializer,
no viewport query, and the module doc's "one index, two query shapes" was true of the design and
false of the code — the cover lived only inside a bench bin.

- **`CellIndex::cover` / `cover_slot` / `polygon_in_view`.** The best-first cover from §5, as key
  ranges, as half-open slot ranges into the sorted cell table (the form a range reader wants — one
  contiguous run is one byte range), and as the map's own question, *what is on screen?*
  `polygon_in_view` returns a superset, which is the right contract for a draw list.
- **`to_bytes` / `from_bytes`.** The same shape as `index-text`'s `format.rs` and for its reasons: an
  eight-byte magic then a fixed-size table of `u64` `(offset, length)` spans, so a reader fetches
  `MAGIC.len() + TABLE_BYTE` = **120 bytes** blind and then seeks. Four sections — key, offset,
  candidate, geometry — all little-endian and stride-regular. `from_bytes` bounds-checks every span
  and cross-checks the offsets against the candidate section's real length, and is tested against a
  foreign magic and five truncation points.
- **ABI 2** in `index-geo-wasm`: `geo_polygon_in_view`, `geo_serialize`, `geo_bytes_ptr`,
  `geo_open`. Nothing existing moved. `geo_open` skips the build entirely — which, with §3's build
  numbers, is now the smaller of the two wins rather than the larger.

### What that costs, measured

`cargo build -p index-geo-wasm --target wasm32-unknown-unknown --release`, raw `.wasm` bytes:

| tree | bytes | delta |
|---|---|---|
| before this lane | 57,037 | — |
| + scanline build, per-ring box, `u32` key, NaN filter | 64,698 | +7,661 |
| + viewport cover and `polygon_in_view` | 78,921 | +14,223 |
| + serialize and open | 86,903 | +7,982 |

**86,903 raw / 34,035 gzipped**, against 57,037 / 22,494. An intermediate version reached 120,927
because `from_bytes` returned `Result<_, String>` and every branch called `format!`, which drags
`core::fmt` and the allocating formatter into a browser payload; switching to `&'static str` errors
gave back **34,024 bytes for no loss of which failure is named**. That is the one place a house
convention (`index-text` returns `String` errors) is deliberately not followed, and the 34 KB is the
reason.

`README.md` still records the old 56,796 / 22,334 figures for `index-geo-wasm`. It is a shared file
another lane is editing, so it is **not** updated here — that number needs correcting at merge.

## 7. What did not work

- **Making the query faster at the operating point.** Everything tried at L=6–8 was inside the
  noise, and §1 says why: 79–87 % of the query there is point-in-polygon on boundary cells, so the
  lookup path is not the thing to optimize. The narrower `u32` key only pays at L=10–12 (1.20–1.67×,
  measured against controls that moved ≤ 4 %) where the binary search dominates — and L=10–12 is not
  the operating point. **Reported as a win at high L and a non-result at low L**, rather than as a
  win.
- **Per-ring bounding boxes in `index-geo`.** Added, because the library was missing a rejection the
  bench copy already had — but its effect is *unmeasured here*, because `geo-join` carries its own
  copy of the index and already had per-ring boxes, so no bin isolates it. It is a correctness-neutral
  change justified by symmetry with the bench, not by a number.
- **The printed KB figure got bigger while the memory got smaller.** The old formula measured a
  packed layout the code did not have. Recorded in §4 because a reader comparing the two bin outputs
  will otherwise conclude the opposite of the truth.

## 8. Still not measured

- **`index-geo` and `geo_join.rs` are two implementations of one algorithm.** The bench crate does
  not depend on `index-geo`, so the row sweep is now written twice and only the library half has
  unit tests. They agree today because both were changed together and both assert against a scan;
  nothing enforces that they keep agreeing.
- **The `cover_slot` / `polygon_in_view` path has no benchmark.** It is unit-tested for coverage and
  supersetness, and §5 measures the cover algorithm inside `sfc-2d`, but no bin measures the
  library's viewport query end to end on real polygons.
- **`geo_open` versus `geo_build` in the browser.** The serializer exists and round-trips in a unit
  test; nobody has timed loading a prebuilt file against building one in WASM.
- **Barangays.** Still 41,966 polygons never run — the same gap `p10` left open.
- **Against a real R-tree.** Still nothing.

## Reproduce

```sh
cargo run -p index-bench --release --bin sfc-2d
cargo run -p index-bench --release --bin geo-join
INDEX_BENCH_PROVINCE=bench/fixture/maphy-municipal.txt cargo run -p index-bench --release --bin geo-join
cargo test -p index-geo --release
cargo build -p index-geo-wasm --target wasm32-unknown-unknown --release && node js/geo-bench.mjs
```
