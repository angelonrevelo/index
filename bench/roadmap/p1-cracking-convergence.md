# P1 — Stochastic cracking convergence & never-worse-than-scan (Tier 1, gate-excluded, promote-on-build)

**Why this is the riskiest item on the board:** "gets faster the more you query it" is true *only for near-random workloads*. Sequential/clustered queries — the natural real pattern — are cracking's documented worst case, and naive cracking mutates the array on reads. This benchmark refuses to let us ship the marketing claim without proving the failure modes are contained.

## Workloads (run all three on identical data; seeded)
- **(a) random** — uniform-random range queries.
- **(b) sequential** — consecutive/clustered ranges (the realistic worst case).
- **(c) adversarial** — repeated tiny ranges at the array ends.

Plot cumulative query time vs query count for each.

## Pass-criteria (mechanical)
1. **(a)** convergence: the Nth query (N large) is ≥5× faster than the 1st.
2. **(b) and (c)**: cumulative time **never exceeds a full-scan baseline by more than 1.2×**. Cracking must never make things *worse* than not indexing.
3. Implementation is **stochastic / forced-randomization** cracking (naive cracking is rejected — see `docs/roadmap-rejected.md`).

## Red→green proof
- Green with stochastic cracking.
- Swap in naive cracking → criterion 2 must FAIL on workload (b) or (c). (This is the whole point: the test exists to catch the naive variant.)
- Restore → green.

## Companion (Tier 2, manual)
Concurrency: run workload (a) at 1 and 8 threads. We **expect to lose** to a B-tree on concurrent reads (cracking mutates on read). PASS = the gap is measured and written into the roadmap honestly, not hidden or claimed away.
