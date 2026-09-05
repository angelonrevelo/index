# P2 — Fuzzy-substring viability decision (Tier 1, gate-excluded, promote-on-build)

**Why:** "fuzzy search over compressed text" is the most-wanted feature and the most fragile. k-error backward search over an FM-index is real (every DNA aligner does it) but has **exponential dependence on edit distance k**, and on a byte/Unicode alphabet (σ=256) the top-level branching explodes. This experiment decides — within a day of implementation — whether the headline feature is viable or must be reframed as "fuzzy *term* + exact substring."

## Prompt / scenario
Implement k-error backward search (Levenshtein-NFA × FM-index backtracking) on real text and measure latency as k grows.

## Inputs (frozen, committed)
- Pizza&Chili `english` corpus, alphabet σ=256.
- Query patterns of length 8 and 16, k ∈ {1, 2, 3}, 1000 queries each (seeded).

## Pass-criteria (mechanical) — this is a DECISION gate, not just pass/fail
- k=1: p99 < 1 ms.
- k=2: p99 < 20 ms.
- **Decision trigger:** if k=3 p99 > 1 s OR k=2 p99 > ~100 ms → declare fuzzy-over-FM-index viable **only for k≤2**, document it as a hard product limit, and route higher-k through the q-gram filter+verify fallback (P2 fallback row).

## Red→green proof
- Green when k=1/k=2 meet the latency bars on correct code.
- Inject defect: remove the early-termination cutoff (BWA-style "stop at l+2 differences") → k=2/k=3 latency must blow past the bars (FAIL), proving the cutoff is what makes it tractable.
- Restore → green.

## Note on conflation (Triage)
This benchmark tests fuzzy *substring* search. Fuzzy *term* search (dictionary, ed≤2) is already solved by the `fst` crate and is NOT this. The product decision of which one `index` actually needs is in ROADMAP Triage; this experiment informs it.
