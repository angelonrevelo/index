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
