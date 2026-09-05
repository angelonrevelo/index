# Indexing geometry, vertices and 3D models — what exists, and what does not

*Researched 2026-09-05. The session's WebSearch quota was exhausted before this ran, so findings
come from direct fetches of primary sources — specs, source code, GitHub/crates.io/S3 APIs — rather
than from third-party benchmark round-ups. That is stronger for dates and spec text, weaker for
independent performance sweeps. Anything unconfirmed is marked **UNVERIFIED**, including several
numbers that are widely repeated and have no primary source.*

The question this was asked to answer: **is there a real, unfilled opportunity in indexing geometry
for web and mobile apps, the way this repo did for text?** The short answer is yes, but not where
it looks — most of the cloud-native geometry stack is solved, and the gap is somewhere else.

---

## 1. Spatial indexes

**Lucene BKD** (read from `BKDConfig.java` / `BKDWriter.java`, apache/lucene main): a recursive k-d
partition with `DEFAULT_MAX_POINTS_IN_LEAF_NODE = 512`, `MAX_DIMS = 16` stored but
`MAX_INDEX_DIMS = 8` indexed. Leaf compression is three-way, chosen per leaf by cost estimate:
all-equal sentinel, low-cardinality RLE, or per-dimension common-prefix stripping. A source TODO
records **60 M OSM points → 1.1 MB in-heap index**. It replaced numeric trie fields, deprecated in
Lucene 6.0.

**Be sceptical of the famous percentages.** [Elastic's 2016-02-15 post](https://www.elastic.co/blog/lucene-points-6.0)
gives its results only as chart images; the text makes directional claims on a 60.8 M-point
PlanetOSM subset. Any specific "32 % smaller / 71 % faster" figure is **UNVERIFIED**.

The 2024–26 movement is a partial *retreat* from BKD. **DocValuesSkippers** (Lucene 10.0,
[Elastic 2026-06-12](https://www.elastic.co/search-labs/blog/docvaluesskippers-lucene-range-queries))
— 4,096-doc base blocks, 8:1 merge, 4 levels, *"typically less than 0.1 % of the size of the base
DocValues field"* — remove the need to duplicate numerics into a points structure when values
correlate with doc id. **That is the same insight this repo already tested and rejected for text**
in the docID-reordering experiment: locality between the sort order and the query predicate is
worth more than a dedicated structure, when you have it.

**PostGIS** is the cleanest published comparison
([Ramsey, Crunchy Data, 2021-05-05](https://www.crunchydata.com/blog/the-many-spatial-indexes-of-postgis)),
1 M random 3D points:

| index | build | size | 1,000 box queries |
|---|---|---|---|
| GiST | 15.0 s | 53 MB | 230 ms |
| SP-GiST | 5.6 s | 44 MB | 150 ms |
| BRIN | 0.4 s | **24 KB** | 21,810 ms |

BRIN is 2,200× smaller and 95× slower **on randomly ordered data** — which is the whole point:
BRIN is a bet on physical ordering, and `p10-geo-join.md`'s L-sweep is the same trade seen from the
other side.

**DuckDB**'s R-tree (1.1.0, 2024-09-09, `max_node_capacity` 128) only fires when one argument is a
planning-time constant and **does not accelerate joins**. Joins get a separate operator, and its
benchmark is the single most useful calibration number in this report:
**58,033,724 Citi Bike rides × 310 polygons — nested loop 1,799.6 s, piecewise merge 107.6 s,
`SPATIAL_JOIN` 28.7 s ≈ 2.02 M points/s**, native and multicore on an M3 Pro
([2025-08-08](https://duckdb.org/2025/08/08/spatial-joins.html)).

**SQLite R\*Tree**: Beckmann 1990, 1–5 dims, three shadow tables, explicitly a filter and not an
exact test; no published benchmarks. **S2**: 31 levels, 64-bit cell id, one Hilbert loop over 6 cube
faces, ~1 cm leaves; the "few hundred nanoseconds over a million polygons" line is a vendor claim,
**UNVERIFIED**. **H3**: the correctness fact that matters is that **H3 parent containment is only
approximate** — hexagons do not tile hierarchically — while S2's is exact. No primary H3-vs-S2
latency benchmark exists.

## 2. The cloud-native line — mostly solved. Say so plainly.

**FlatGeobuf is the direct analogue of what this repo built for text, and it works.** Magic /
header / packed Hilbert R-tree / features; `NODE_ITEM_BYTE_LEN = 40`, `DEFAULT_NODE_SIZE = 16`.
Against Shapefile = 1 on OSM Denmark (906,602 LineStrings): full read 0.46, filtered read 0.71,
write 0.39, size 0.77; on 2,511,772 Danish cadastral polygons, full read 0.12, filtered 0.26.
Round-trip counts for a bbox query are not published (≈7–9 for millions of features, **UNVERIFIED**).
Its README carries the lesson that actually matters: **latency, not throughput, is the limit, and
CORS forces an uncacheable OPTIONS preflight per Range request.**

> **Verdict: bbox-over-HTTP is solved and boring. Do not rebuild it.**

**GeoParquet is the shakiest of the four.** 1.0.0 (2023-09-18), 1.1.0 added a bbox covering column
(2024-06-19), and **2.0.0-rc.1 (2026-07-19) is still RC and deletes both the GeoArrow encodings and
the bbox covering column** in favour of native Parquet GEOMETRY/GEOGRAPHY types. **Parquet has no
tree at all** — pruning is min/max row-group statistics, so spatial selectivity is a property of
your sort order, not of the format. Hilbert ordering is simultaneously a compression lever: same
data 2.21 GB badly ordered → 1.37 GB time-ordered → **1.24 GB Hilbert**
([CNG, 2025-01-27](https://cloudnativegeo.org/blog/2025/01/using-duckdbs-hilbert-function-with-geoparquet/)).
Overture release 2026-08-19.0 is 2.53 B buildings and 73.6 M places, **569.6 GB** summed from S3;
**no published end-to-end query-latency benchmark exists** — every "sub-second over 500 GB" claim
is a demo.

**PMTiles v3**: 127-byte header, root directory inside the first 16,384 bytes, Hilbert TileIDs, RLE
dedup, *"at most two cacheable intermediate requests"*. Planet basemap ~120 GB, z0–15. Brandon Liu
is candid in [You Might Not Want PMTiles (2024-05-22)](https://protomaps.com/blog/you-might-not-want-pmtiles):
re-uploading the archive means anything above daily updates *"uses an uneconomical amount of data
transfer."* **COPC** does the same trick for point clouds — LAZ 1.4 plus a 160-byte info VLR at
offset 375 and an octree hierarchy VLR, range-read and depth-limited for LOD. COPC is *not* an OGC
standard, but **LAZ 1.4 became one on 2026-04-23**. **3D Tiles 1.1** bets the opposite way: many
small files with availability bitstreams, not ranges into one file.

## 3. Vector tiles

MVT 2.1 has been frozen since 2016-01-19: extent 4096 by convention (not mandate), zigzag-delta
cursor commands, and **the spec is silent on precision** — at z14/4096 that is ~0.6 m/unit. The
successor is real: **MapLibre Tile (MLT)**, column-oriented and SIMD-friendly, **declared stable
October 2025**, claiming up to 6× better compression than MVT
([SIGSPATIAL '25](https://doi.org/10.1145/3748636.3763208)). The per-dataset benchmark table 404s;
treat 6× as best case, **UNVERIFIED**.

Generation cost is no longer the problem. **Planetiler 0.10.1 builds the OSM planet in 19 minutes**
on 192 cores (81 GB pmtiles), 42 min on 64 cores, 18 h 18 m on 2 cores — against OpenMapTiles'
*">100 days"*. Overture cut its places tileset from >2 hours to **~5 minutes** by switching
tippecanoe → planetiler (2026-06-30). **Tippecanoe publishes no throughput numbers anywhere.**

The costs that remain are structural. A z0–z14 pyramid is **357,913,941 tiles** (z0–15: 1.43
billion); Protomaps notes 300 M tiles ≈ **$1,500 in request fees** alone, and that dedup removes
**70 %+** of a global vector basemap.

**One trap is worth naming because it bit this repo's own benchmark.** PostGIS
`ST_SimplifyPreserveTopology` *"does not preserve boundaries shared between polygons"* — it is
per-geometry only, and you need `ST_CoverageSimplify`. That is exactly why maphy's neighbouring
provinces have metre-wide slivers between them, which is what caused the 28 wrong answers recorded
in `bench/roadmap/p10-geo-join.md`. TopoJSON claims **80 %+** via arc sharing plus quantized delta
encoding (project claim, not a measured benchmark) — and see `p10` for why arc sharing bought only
**1.09×** on maphy's actual data.

## 4. Geometry and vertex compression

Baseline first: **KHR_mesh_quantization** (ratified) takes *"48 bytes per vertex … to 20 bytes per
vertex with often negligible quality impact."* Tangent frames go further —
[zeux, 2026-04-30](https://zeux.io/2026/04/30/quantizing-tangent-frames/): 60-bit baseline → **31
bits**, normal average error 0.0272°.

**Draco is in maintenance mode — last release 1.5.7, 2025-01-17**, recent commits dependency churn
only. Google publishes **no absolute decode throughput**; the 2017 launch blog is 404 and
google.github.io/draco has zero performance claims. The best real-mesh numbers are Cesium's
(2018-04-09): 1.1 M NYC buildings, 738 MB gzipped glTF → **149 MB** Draco+gzip, load 18.9 s →
10.5 s. Decoder payload measured from npm: **`draco_decoder.wasm` = 285,948 bytes**.

**meshoptimizer wins on every axis that matters for the web.** Vertex codec 2–4× on already
quantized data; native decode 3–6 GB/s, but the honest browser figure is the JS decoder's **1–3
GB/s with SIMD**. Index codec targets 1 byte/triangle. Decoder is **`meshopt_decoder.mjs` = 29,059
bytes — ~10× smaller than Draco's wasm**, the strongest verified head-to-head fact in this area.
v1.0 (2025-12-08) made codec v1 default; v1.2 (2026-06-30) is 20–45 % faster on x86.
**KHR_meshopt_compression** is at Release Candidate. No first-party Draco-vs-meshopt head-to-head
exists; "20× faster" claims are **UNVERIFIED**.

Virtualized geometry is where 2025–26 energy went: [zeux, 2025-09-30](https://zeux.io/2025/09/30/billions-of-triangles-in-minutes/)
clusters **1.64 B triangles in ~2 m 35 s** on 16 threads. **No shipping browser equivalent exists.**
Neural mesh compression is research-only (NeCGS's "900×" is a *set-level* ratio, not per-mesh).

## 5. Point clouds

LAZ's primary measured figure is Isenburg 2011 Table V: Minnesota DNR, **29.74 B points, 776 GB →
64 GB = 12.1×** (the repeated "5–15×" has no primary source). Scale is genuinely enormous and
genuinely streamable: **USGS 3DEP on AWS = 2,279 EPT resources, 81,293,466,027,851 points (81.3
trillion)**, read live from `boundaries/resources.geojson` on 2026-09-04 — the widely cited "10
trillion" is a stale 2019 README. **AHN2 = 640 B points in 1.6 TB LAZ**.

Client budgets are hard-coded and small: Potree's **`maxNodesLoading = 4`** in-flight fetches, 2 GPU
uploads/frame, default **`pointBudget = 1,000,000`**. Conversion runs 6–9 M pts/s on SSD.

**The published rendering headlines are native, not browser**: 796 M points at 62–64 fps = 50 B
pts/s on an RTX 3090 ([arXiv 2104.07526](https://arxiv.org/abs/2104.07526)). **No published
WebGL-vs-WebGPU points/sec browser benchmark exists**, and Potree's last substantive commit was
2024-05-14.

Delivery consolidated elsewhere: **`KHR_gaussian_splatting` was ratified 2026-01-27** with **no
compression** — four competing compression companions still open. The only proper browser benchmark
in the space: SuperSplat on M4 Max, 35 M splats **13.3 fps WebGL2 → 75.8 WebGPU (5.7×)**.

## 6. 3D shape search — not solved, and mostly not attempted

Classical ceiling, Princeton Shape Benchmark (2004, 1,814 models): **LFD 64.3 % DCG**. Retrieval was
never good. Supervised classification saturated — ModelNet40 at 94–96 % is dead as a signal.

The number that answers the question is zero-shot Objaverse-LVIS top-1 over 1,156 categories:
OpenShape 46.8, ULIP-2 50.6, **Uni3D-g 53.5 → 55.3**. The 2024–26 successors (TAMM 50.7, MM-Mixing
51.4, 3D-MRL 50.9) beat none of it. **Zero-shot 3D retrieval has been stuck near 55 % for two
years.** Corpora grew ~200× (ShapeNetCore 51,300 → Objaverse-XL **10.2 M**) but are consumed as
*generative training data*, not indexed for query.

Production is keyword search. Sketchfab's v3 API (called live 2026-09-05) exposes **no geometry or
image similarity parameter**. Google Poly died 2021-06-30. Siemens has de-marketed its own engine:
grepping plm.sw.siemens.com's sitemap returns **zero URLs containing "geolus"**. **No shipping ANN
index over 3D embeddings was found anywhere.**

## 7. Where the gaps actually are

**(a) One portable file serving attribute search AND spatial query, serverless — genuinely
unfilled.** **Tantivy has no geo field type at all** (0.26.1, 2026-07-10). Pagefind proves the
chunked static index pattern is cheap (all of MDN under 300 KB) but has filters, not geometry.
sql.js-httpvfs proves range querying works (complex query 10–20 GETs / 130–270 KiB) but its author
calls it *"a demonstration"* and it is five years stale. FlatGeobuf indexes geometry with no
attribute index; GeoParquet indexes attributes with no tree. **The counterargument to confront:
DuckDB-WASM already ships `spatial` and `fts` in one binary** — measure its real gzipped payload
before betting on size.

**(b) LOD as an index rather than a pyramid — unfilled, and the strongest idea here.** Everything
shipping precomputes: tippecanoe and planetiler bake a pyramid, Potree bakes an octree, 3D Tiles
bakes geometric error. The evidence it is wrong-shaped: 357.9 M tiles for z0–14 of which 70 %+
dedups away, and Protomaps stating outright that above daily updates is uneconomical. A
vertex-ranked structure — Visvalingam importance as a sort key, prefix-readable to any tolerance —
would make simplification a **range read** instead of a build. The correctness hook is real:
`ST_SimplifyPreserveTopology` does not preserve shared boundaries, so shared-edge ranking is a
feature nobody ships in a portable format.

**(c) Point-in-polygon in the browser at scale — no published numbers at all.** No JS-vs-WASM PIP
throughput benchmark, no GPU-PIP benchmark, no rasterization-PIP benchmark. DuckDB's 2.02 M pts/s
native multicore figure is the only calibration in any language. **This is a measurement vacuum,
not a solved problem.** → `bench/roadmap/p10-geo-join.md` is the first half of filling it, natively;
the browser half is unrun.

**(d) Indexing vertices themselves as a database problem — real but commercially thin.**
Dedup and shared-vertex topology are treated as a *compression* concern (meshopt, Draco), never as a
queryable index. Mesh similarity is stuck at 55 % zero-shot with no deployed index anywhere. The
honest read: **the gap is real, the demand is unproven.** `p10-geo-join.md` measured the dedup half
on real data and got **1.09×** — consistent with this ranking.

> **Solved — do not rebuild:** FlatGeobuf bbox-over-HTTP, PMTiles tile addressing, COPC octree range
> reads, meshopt as the vertex codec. Compete with none of these.

## Ranked opportunities

1. **Hybrid attribute + spatial static index, one range-readable file.** *Exists:* FlatGeobuf
   (geometry only), Pagefind (text only), sql.js-httpvfs (abandoned), DuckDB-WASM (both, but a
   149.4 MB npm package). *Insufficient:* nothing lets you ask *"barangays named X within this bbox
   with population > Y"* from a CDN. The BM25F + FST + MaxScore stack here is the hard half.
   *Win condition:* fewer HTTP requests and a smaller gzipped wasm than DuckDB-WASM.
2. **Simplification-as-index: a tolerance-addressable vertex stream.** Store vertices sorted by
   Visvalingam importance with shared-edge identity preserved; a prefix read is a valid
   simplification and zoom becomes a range query. *Measure:* bytes-on-wire and Hausdorff error at
   z6/z10/z14 versus a tippecanoe pyramid, plus sliver count at shared boundaries (where GEOS
   provably fails), plus regeneration cost when only attributes change — which should be zero.
3. **WASM point-in-polygon join kernel, with the benchmark nobody has published.** Design for fixed
   -width SIMD: **relaxed SIMD is Safari-flag-gated and memory64 is Safari-unsupported**, so stay on
   i32 offsets and ≤4 GB. *Publishing the number is itself the moat.*
4. **Rust packed-index primitive with a hardened large-data path.** `geo-index` (Kyle Barron) builds
   Hilbert in 80.5–81.4 ms vs rstar's 158.5–160.3, searches 116 µs vs 152 µs, and is **ABI-stable
   with JS flatbush/kdbush** — but states no dataset size or hardware, makes **no WASM or no_std
   claim**, and 0.3.1/0.3.3/0.3.4 were consecutive fixes for build hangs, panics and a stack
   overflow on large data. Build *on* it rather than against it.
5. **Mesh/vertex dedup and similarity index.** Real gap, weakest business case, furthest from the
   atlas, and the benchmark ecosystem shrank to two SHREC tracks a year. **Ranked last honestly.**
