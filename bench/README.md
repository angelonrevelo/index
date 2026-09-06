# bench — benchmark home & methodology contract

This directory is the credibility gate for `index`. The rule (from `/roadmap`): **no roadmap item is "done" until its benchmark runs red→green.**

## Layout

- `bench/<name>/` — live benchmarks for *shipped* features. These run in the normal gate and must stay green.
- `bench/roadmap/` — **quarantined** benchmarks for *not-yet-built* roadmap items. These are **expected-red until built** and are EXCLUDED from the live gate (the runner skips this dir / they are `#[ignore]`d). When the item ships, the benchmark is **promoted** out of `roadmap/` into the live gate and must go green — that promotion is the `/verify` moment.

> A roadmap's red *tests* are a feature. A roadmap's red *gate* is a bug. Keep future-item benchmarks here so `cargo test` / the gate stays green.

## Methodology — mirror the literature, don't invent

We adopt the **SOSD** benchmark contract so our numbers are comparable to published learned-index results, then extend it where SOSD is blind.

**Datasets (SOSD):** 8 sets of 200M × 64-bit unsigned ints. Real — `amzn`, `face`, `osmc`, `wiki`. Synthetic — `logn`, `norm`, `uden`, `uspr`. (Zenodo: https://zenodo.org/records/15240501 · repo: https://github.com/learnedsystems/SOSD)

**Metrics (the standard three + our additions):**
- `bytes/key` — index space.
- `ns/lookup` — but report **p50 AND p99**, not just mean. The literature's blind spot is tail latency, and it's exactly where learned indexes are weakest. p99 is mandatory here.
- `build throughput` — keys/sec, single-thread.
- **Extensions SOSD lacks** (because it's read-only/point-lookup/single-thread/integer-only): insert/write workloads, multi-thread lookup throughput, and the cracking-convergence workloads (see `roadmap/p1-cracking-convergence.md`).

**For compressed text** we additionally mirror the **Pizza&Chili** corpus + sdsl-lite's `indexing_locate` (`bits/char` for space, `locate` time per occurrence for speed). sdsl-lite (C++) is the perf/correctness **oracle**, not a dependency.

## Idempotency requirements (non-negotiable)

- Same input → same verdict every run. No clock/random/network in scoring; seed and pin everything.
- Scorer is deterministic and bounded — emit pass/fail + a short check list, not a token dump.
- Commit the **baseline** under `runs/<date>-baseline/` so before/after is a real `diff`, not a memory.

## Proving a benchmark is real

A benchmark that has never gone red hasn't been shown to catch anything. For each T1 benchmark, run the full **red→green**: passes on correct code → fails when the exact defect it guards is injected → passes on restore. Until that's done, label it **"spec-only, unproven."** Today, all benchmarks here are spec-only (greenfield — no Cargo project yet).

## Environment variables

The **library** reads exactly one (`INDEX_PARALLEL`, documented in the root `README.md`). Everything
below belongs to this harness: corpus locations and sweep parameters. None of them is consulted by
`index-text`, `index-geo`, `index-image` or `index-wasm`, so a deployment never sets them.

**Every one is optional.** A bin with a missing corpus says so and skips that arm rather than
failing silently — a gate that quietly skips is not a gate.

| variable | read by | what it does |
|---|---|---|
| `INDEX_CORPUS_DIR` | most bins | Root the sibling corpora are found under. Defaults to the repo's grandparent, which is why a bin run from a temporary worktree can silently lose corpora — set it explicitly there. |
| `INDEX_MILLION_TSV` | `real-million` | Path to a `name<TAB>vendor<TAB>code` export. Defaults to `bench/fixture/presyo-1m.tsv`, which is **not committed**. |
| `INDEX_IMAGE_CORPUS` | `image-corpus` | Directory of the image scrape. |
| `INDEX_VIDEO_CORPUS` | `video-shot` | Directory of the video corpus. |
| `INDEX_BENCH_PLACE`, `INDEX_BENCH_PRIOR` | `maphy-place` | Place list and per-place prior. |
| `INDEX_BENCH_POI`, `INDEX_BENCH_PROVINCE` | `geo-join`, `js/geo.mjs` | POI and polygon fixtures. |
| `INDEX_BENCH_COURSE` | `profstopick-dept` | Course export. |
| `INDEX_BENCH_LEAD` | `blead-industry` | Lead export. |
| `BOOTED_SCHEMA` | `booted-schema` | Schema file; falls back to `HOME`/`USERPROFILE`. |
| `INDEX_BENCH_N` | `beat-btreemap` | Key count. |
| `INDEX_BENCH_EPS` | `beat-btreemap` | PLA/PGM epsilon. |
| `INDEX_BENCH_Q` | `beat-btreemap` | Query count. |
| `INDEX_BENCH_SIGMA` | `fuzzy-decision` | Alphabet size for the fuzzy-over-FM viability sweep. |
| `INDEX_BENCH_CAP` | `fuzzy-decision` | Expansion cap. |
| `INDEX_CAP_SWEEP` | `scale` | Runs the expansion-cap sweep instead of the plain ladder. |
| `INDEX_SEG_DIAG` | `segment-scale` | Prints the individual near-tie disagreements behind the overlap columns. |
| `INDEX_DIAG` | `real-corpus` | Per-query diagnostic output. |
| `INDEX_DUMP_LOSS` | `pool-audit` | Dumps the queries where the pooled and brute-force answers differ. |
| `INDEX_BROWSER_PORT`, `INDEX_OPFS_PORT` | `js/browser-check.mjs`, `js/opfs-check.mjs` | Ports for the local static server the headless browser loads from. |
