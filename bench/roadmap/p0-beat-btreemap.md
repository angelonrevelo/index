# P0 — Beat BTreeMap on bytes/key AND p99 (Tier 1, gate-excluded, promote-on-build)

**Why:** the learned core exists only if it wins a *dual* — smaller AND faster-tail — over the std baseline. Winning one axis is the normal space/time tradeoff and not interesting. Winning both simultaneously is the whole pitch.

## Inputs (frozen, committed)
- SOSD `amzn`, `face`, `wiki` (10M-key subsets for CI speed; full 200M for the headline run).
- Seeded random + sequential query sets (commit seeds).
- Baselines: `std::collections::BTreeMap`, and a sorted `Vec` + binary search.

## Metrics
- `bytes/key`, `ns/lookup` **p50 and p99**, build throughput (keys/sec, single thread).
- Capture baseline under `runs/<date>-baseline/`.

## Pass-criteria (mechanical)
1. bytes/key < `BTreeMap` on all three datasets.
2. p99 ns/lookup ≤ `BTreeMap` p99 on all three datasets. **(p99, not mean — this is the honest bar.)**
3. No single-config tuning sweep allowed: one default ε must clear (1)+(2). If you need per-dataset grid search to win, you've inherited RMI's worst property → FAIL.

## Red→green proof
- Green on the tuned-but-single-config index.
- Inject defect: bloat the model (store full keys) → criterion 1 FAILs; or remove last-mile bound → criterion 2 FAILs.
- Restore → green.

## Honest expectation
Per the independent Tsinghua evaluation, expect this to hold for **read-mostly, integer, point-lookup, single-thread, in-memory**. Do NOT extend the claim past that box without a separate benchmark.
