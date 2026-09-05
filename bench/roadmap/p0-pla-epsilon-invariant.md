# P0 — PLA ±ε invariant (Tier 1, gate-excluded, promote-on-build)

**Why this first:** the entire project bets on one property — a piecewise-linear model predicts a position, and the true position is *guaranteed* within ±ε so the last-mile search is bounded. If that invariant is ever silently violated, lookups are wrong or slow and nothing downstream is trustworthy. This is the cheapest possible test that the core is correct.

## Prompt / scenario
Build the recursive PLA index over a sorted key array with error parameter ε. For each lookup, record the actual search window the last-mile step had to scan.

## Inputs (frozen, committed)
- SOSD `osmc` and `wiki` (the hardest, most irregular CDFs — they break naive PLAs).
- 10M random query keys drawn from the dataset (seeded; commit the seed).
- ε ∈ {8, 16, 32, 64}.

## Pass-criteria (mechanical, boolean)
1. For 100% of lookups, `|actual_pos − predicted_pos| ≤ ε`. **Any single violation = FAIL.**
2. Measured last-mile scan window ≤ 2ε for 100% of lookups.
3. Reported bytes/key < the same dataset's `BTreeMap` at equal-or-better p50 lookup.

## Red→green proof (required before trusting it)
- Green on correct PLA.
- Inject defect: off-by-one in segment boundary / drop the last segment → criterion 1 must FAIL.
- Restore → green.

## Notes
This is the correctness foundation. It does NOT prove speed (that's `p0-beat-btreemap.md`) — it proves the bound the speed claim depends on.
