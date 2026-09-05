# P11 — point-in-polygon in a browser: the number nobody had published

**Tier:** T1 · **Bins:** `node js/geo-bench.mjs` · `node js/geo-browser-check.mjs`
**Status: MEASURED, 2026-09-05. 4.43 M points/s in Node, 3.61 M in real Chrome, ~30× a JS scan.**

## Why this row existed

`docs/research/geometry-sota.md` surveyed the geometry stack and found most of it genuinely solved
— FlatGeobuf's bbox-over-HTTP, PMTiles tile addressing, COPC octree range reads, meshopt as the
vertex codec. It found exactly one hole:

> **(c) Point-in-polygon in the browser at scale — no published numbers at all.** No JS-vs-WASM PIP
> throughput benchmark, no GPU-PIP benchmark, no rasterization-PIP benchmark. *"This is a
> measurement vacuum, not a solved problem."*

and ranked filling it third of five opportunities with the note that *"publishing this number is
itself a moat."* `p10-geo-join.md` filled the native Rust half. This is the browser half.

The only calibration in any language is DuckDB's spatial join: **58,033,724 points × 310 polygons
in 28.7 s ≈ 2.02 M points/s**, native and multicore on an M3 Pro
([duckdb.org, 2025-08-08](https://duckdb.org/2025/08/08/spatial-joins.html)).

## What was built

- **`crates/index-geo`** — the cell index from `p10`, extracted into a real library. `forbid(unsafe_code)`,
  no dependencies, 7 unit tests including an **exhaustive** 100×100 grid check that the index agrees
  with a brute-force scan at three grid resolutions.
- **`crates/index-geo-wasm`** — hand-written raw-pointer C ABI, **no wasm-bindgen**, same convention
  as `index-wasm`. **56,796 bytes raw, 22,334 gzipped.**
- **`js/geo.mjs`** — host, plus `scanLocate`, the pure-JS baseline.
- **`js/geo-bench.mjs`**, **`js/geo-browser.html`**, **`js/geo-browser-check.mjs`** — Node and real
  browser harnesses.

## Measured — 2,454 municipal polygons × 53,715 real POIs

**Node v24.12.0, win32/x64:**

| arm | build | query | points/s | vs JS scan |
|---|---|---|---|---|
| JS scan (bbox + crossing) | — | 394.45 ms | 136 k | 1.0× |
| WASM cell index, L=6 | 69 ms | 13.52 ms | 3.97 M | 29.2× |
| WASM cell index, L=7 | 49 ms | 13.23 ms | 4.06 M | 29.8× |
| **WASM cell index, L=8** | 108 ms | **12.13 ms** | **4.43 M** | **32.5×** |
| WASM cell index, L=9 | 305 ms | 13.03 ms | 4.12 M | 30.3× |

**Headless Chrome 147.0.7727.15**, module fetched over HTTP with the real `application/wasm` MIME
requirement, query on the main thread:

| arm | query | points/s | vs JS scan |
|---|---|---|---|
| JS scan (bbox + crossing) | 410.40 ms | 131 k | 1.0× |
| WASM cell index, L=7 | 15.60 ms | 3.44 M | 26.3× |
| **WASM cell index, L=8** | **14.90 ms** | **3.61 M** | **27.5×** |

## Measured — 88 provinces (few, large) × 53,715 POIs

| arm | query | points/s | vs JS scan |
|---|---|---|---|
| JS scan (bbox + crossing) | 122.43 ms | 439 k | 1.0× |
| WASM cell index, L=8 | 30.46 ms | 1.76 M | 4.0× |
| **WASM cell index, L=9** | **25.55 ms** | **2.10 M** | **4.8×** |

The regime split from `p10` reappears unchanged: **many small polygons is the favourable case**, and
88 large ones is not, because a bbox-prefiltered scan over 88 candidates is already cheap. The
optimum L also shifts right (8 → 9) when polygons are larger, which is the same trade seen from the
other side.

## The result that matters: WASM is at parity with native

| tier | points/s | conditions |
|---|---|---|
| DuckDB `SPATIAL_JOIN` | 2.02 M | native, **multicore**, 310 polygons, M3 Pro |
| native Rust (`p10`) | 4.50 M | native, single thread, 2,454 polygons |
| **Node (V8)** | **4.43 M** | single thread, 2,454 polygons |
| **Chrome 147** | **3.61 M** | single thread, main thread, over HTTP |

**The browser costs about 20 % against native Rust, not a factor.** That is the finding — a
point-location index does not have to be a server-side thing, and the reason is visible in the
design rather than in the compiler: most queries touch no geometry at all. An interior cell answers
from a stored id and an ocean cell from a failed binary search, so what crosses into WASM is a
binary search over a `u64` array, which is exactly the kind of work WASM does at native speed.

Stated with the caveats that make it comparable rather than flattering: different hardware,
different data, single-threaded against DuckDB's multicore, and DuckDB solves the harder general
case. **The honest claim is not "faster than DuckDB"** — it is that a browser can do this at all, at
a rate nobody had written down.

Payload for the comparison: **22,334 bytes gzipped**, against DuckDB-WASM's 149.4 MB npm package.

## The bug this benchmark caught, and why the assert saved it

The province arm first reported **34,726** of 53,715 POIs inside a province where the Rust bench
said **51,732**. The JS loader treated the first ring of each polygon as the outer boundary and
every subsequent ring as a hole — which is right for a simple polygon and **wrong for a
MultiPolygon, where an island is a second outer ring**. In an archipelago of 88 provinces that
silently turned most of the country into holes.

The per-point assert did **not** catch it, because both arms shared the loader and were wrong
together. What caught it was the *cross-tier* comparison against a number produced by an
independent implementation in another language. **Two agreeing implementations that share an input
parser agree about the parser, not about the answer** — which is the argument for keeping the Rust
and JS paths independent rather than sharing a loader for convenience.

`js/geo.mjs` now takes `{ pt, outer }` rings explicitly and its doc comment names the trap.

## Wired into the gate

`.github/workflows/gate.yml` builds `index-geo-wasm` and runs `geo-bench` on both fixtures. It is a
**correctness** gate, not a performance one: the bench throws unless the WASM index and the JS scan
return the identical polygon for every one of 53,715 points. CI runners are far too noisy to gate on
timings, and the agreement is what has caught every bug in this line of work.

## Not measured yet

- **Safari and Firefox.** Chrome only. The ABI is deliberately fixed-width — `i32` offsets, no
  memory64, no relaxed SIMD — because the survey found **relaxed SIMD is Safari-flag-gated and
  memory64 is Safari-unsupported**, but "designed for it" is not "measured on it".
- **SIMD.** No explicit vectorization. The crossing test is a natural fit for it and is unexercised.
- **Workers.** Everything here is one thread on the main thread. A map would likely want neither.
- **41,966 barangays.** Still the real target, still an extrapolation.
- **Against a real R-tree.** Still no head-to-head with `geo-index`, SQLite R\*Tree or DuckDB-WASM
  on the same machine and data. `p9`'s caveat continues to stand.

## Reproduce

```sh
cargo build -p index-geo-wasm --release --target wasm32-unknown-unknown
node js/geo-bench.mjs                                                        # Node, municipal
INDEX_BENCH_PROVINCE=bench/fixture/maphy-province.txt node js/geo-bench.mjs  # Node, provinces
node js/geo-browser-check.mjs                                                # real Chrome
```

The browser check borrows Playwright from a sibling checkout rather than adding a dependency:
`cmd /c mklink /J node_modules ..\onegrid\node_modules`.
