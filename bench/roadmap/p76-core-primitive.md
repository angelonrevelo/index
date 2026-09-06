# P76 — `index-core` primitive: rank layout, entropy shaping, last-mile, crack piece (Tier 2, gate-excluded)

**Why:** `index-core` is a research crate — nothing in `index-text`, `index-geo`, `index-image` or
`index-wasm` links it; only `index-bench` does (`beat-btreemap`, `crack-converge`,
`fuzzy-decision`). That is fine, but it means its numbers are the *only* thing it produces, so
every structural choice inside it has to be a measured choice rather than a plausible one. Four
were not, and three of the four turned out differently than the code assumed.

All numbers below were measured on one machine (x86-64, `rdtsc` at 3.600 cycles/ns) under
concurrent load from other agents, by interleaving the candidates round-robin inside one process
and keeping the minimum of ≥14 alternating rounds — background load can only inflate a sample,
never deflate it, so a min-of-interleaved comparison is the noise-proof form. Cross-version
numbers (before/after) come from `git stash` on the same worktree, run twice.

## 1. Rank directory layout — the shipped `u32`-per-word directory was already near-optimal

`WaveletTree`/`FmIndex` bottom out in `BitRank::rank1`. Four layouts, same bitvector, same query
stream, `ns/rank` at min-of-14 interleaved rounds:

| bits stored / payload bit | layout | 32 KiB | 512 KiB | 4 MiB | 16 MiB |
|---|---|---|---|---|---|
| 1.500 | `u32` cumulative per 64-bit word, side array (**was shipped**) | 5.82 | 8.69 | 23.05 | 54.40 |
| 1.143 | 448-bit block interleaved into one 64-byte cache line, early-exit popcount loop | 15.23 | 22.10 | 72.36 | 103.48 |
| 1.143 | same block, branchless 7-popcount mask | 18.43 | 23.72 | 89.77 | 129.56 |
| 1.333 | 384-bit interleaved block, packed 9-bit sub-counts (rank9 shape) | 9.31 | 12.27 | 35.65 | 54.28 |
| 1.250 | `{u32 abs, u32 packed sub}` per 256 bits, side array (**now shipped**) | 5.51 | 9.11 | 23.14 | 57.36 |

**The interleaving idea is wrong here, and by a lot.** Folding the directory into the payload
removes a cache miss but forces `i / BLOCK_BIT` on a non-power-of-two, and that division sits on
the dependency chain of every single rank. It costs ~2x at *every* size, including 32 KiB where
there was no second miss to save. The premise — that the side directory's second access is a real
miss — is false: the directory is 1/8th the payload and is probed at a correlated index, so it
stays resident.

What survives is the *space*: a packed entry per 256 bits stores 1.25 bits per payload bit instead
of 1.50, at latency indistinguishable from the old layout (within run-to-run noise at all four
sizes). Take the 16.7% and drop the cache-line theory.

## 2. Wavelet shape — the module documented `n·H_0` and delivered `n·log σ`

The tree was balanced over `min..=max` of the byte values, which spends `ceil(log2 σ)` bits per
symbol regardless of distribution. A BWT is skewed *by construction*. Huffman-shaping over the
symbols actually present, FM-index `bits/char` on Zipf text, n = 2M:

| σ | Zipf s | text H₀ | log2 σ | before | after | |
|---|---|---|---|---|---|---|
| 64 | 1.0 | 4.866 | 6.000 | 12.350 | 8.417 | −31.8% |
| 64 | 1.5 | 3.439 | 6.000 | 12.165 | 6.631 | −45.5% |
| 128 | 1.2 | 4.825 | 7.000 | 13.384 | 8.393 | −37.3% |
| 26 | 0.8 | 4.230 | 4.700 | 11.415 | 7.617 | −33.3% |

Note the *before* column barely moves between s=1.0 and s=1.5 (12.350 → 12.165) while the *after*
column tracks H₀ (8.417 → 6.631). That is the claim actually being delivered rather than asserted.
On uniform-random text (`gen_text`, where H₀ = log2 σ by construction and shaping cannot help) the
remaining win is the directory alone: 5.876 → 5.065, 9.713 → 8.276, 12.601 → 10.728,
14.099 → 12.038 bits/char for σ = 4/26/100/200 — a flat ~15%.

## 3. Suffix-array construction — O(n log² n) comparison sort → O(n log n) counting sort

`FmIndex::new` over 2M characters, before → after, two rounds: 4008/2920 → 1108/1040 ms (σ=4),
2554/2179 → 1147/1273 (σ=26), 1592/2265 → 1054/1074 (σ=100), 1877/2589 → 963/1254 (σ=200).
**2.0–3.6x.** The old comparator rebuilt a `(rank[i], rank[i+k])` tuple on every comparison.

Query latency is a wash, honestly: `count` and `locate` move within run-to-run noise in both
directions. Fusing `access`+`rank` into one descent (`access_rank`) and both range ends into one
descent (`rank2`) removes tree-walk overhead, but the *memory* accesses — which are what a
dependent rank chain actually pays for — are unchanged. Best observed: `locate` 1002 → 871 ns/occ
at σ=4. Do not claim more.

## 4. PLA/PGM last mile — this, not prediction, is where a lookup's time goes

Phase split at n=1M, ε=16, min-of-5, `PlaIndex`:

| dataset | predict only | full search | last mile (derived) | share |
|---|---|---|---|---|
| sequential | 7.92 ns | 96.30 ns | 88.39 ns | 92% |
| uniform | 18.46 ns | 88.13 ns | 69.67 ns | 79% |
| lognormal | 12.38 ns | 89.25 ns | 76.87 ns | 86% |
| hard | 20.38 ns | 110.05 ns | 89.67 ns | 81% |

So the model is not the cost; confirming the key is. Replacing the window's `binary_search` with a
branchless count of the keys below the probe (interleaved A/B, min-of-20, ns/op):

| dataset | ε | binary | linear scan | branchless count |
|---|---|---|---|---|
| uniform | 16 | 173.56 | 150.09 | **144.63** |
| uniform | 32 | 203.35 | 177.66 | **140.41** |
| uniform | 64 | 353.44 | 218.75 | **163.79** |
| hard | 16 | 181.58 | 151.27 | **143.28** |
| hard | 64 | 405.74 | 258.84 | **217.72** |
| sequential | 32 | 135.03 | 117.26 | **87.74** |
| lognormal | 32 | 117.55 | 134.01 | **97.67** |

At ε ≤ 8 it is a wash and binary search sometimes wins (uniform ε=4: 178.26 vs 188.70). At the
shipped ε=16 the count wins on every distribution, and at ε=64 it wins ~2x — which is what makes
the larger, cheaper models usable at all. Early-exit linear scan is consistently worse than the
branchless count: the unpredictable exit branch costs more than the loads it skips.

## 5. Crack piece limit — smaller pieces are *worse*, and sorting is a workload trade

`CRACK_LIMIT` was 1024 with no recorded justification. Sweep at n=1M, 3000 queries, tail =
mean ns over the last 200 queries (`sort=0`, i.e. no sorted-piece promotion):

| piece_limit | random tail | sequential tail | ends tail | pieces (random) |
|---|---|---|---|---|
| 64 | 3730 | 666 | 106 | 21508 |
| 128 | 3012 | 784 | 85 | 16302 |
| 256 | 2414 | 790 | 82 | 12331 |
| **1024** | **1870** | 2610 | 83 | 7949 |
| 4096 | 3010 | 1815 | 80 | 6484 |

Shrinking the piece does **not** buy converged latency — 1024 is already the minimum of the curve,
and 64 is 2x worse, because the cracker index grows 2.7x and the `BTreeMap` probe overtakes the
partition it saved. Keep 1024, now for a reason.

Separately: once a piece is small, sorting it once turns every later crack inside it into a binary
search that moves no data at all. Triggered on the **second** crack landing in the piece (an
unconditional trigger taxes the random workload ~10% on total time for sorts that never amortize),
before → after, two rounds, `crack-converge` `last=`:

| workload / mode | before | after |
|---|---|---|
| sequential, stochastic | 2090 / 1830 ns | **620 / 410 ns** |
| ends, stochastic | 110 / 110 ns | **60 / 80 ns** |
| random, stochastic | 2830 / 2110 ns | 3290 / 3640 ns |

Elements moved per query on the sequential workload drops 5.86M → 3.54M cumulative. **The random
workload gets ~1.5x worse at the tail** and that is the price: a uniform-random stream keeps
reaching pieces it has never touched, so it pays the sort and never collects. Taking the trade
because `p1-cracking-convergence.md` names sequential/clustered "the realistic worst case" and "the
riskiest item on the board", while random passes its criterion by ~400x either way.

## Pass-criteria (mechanical)
1. `cargo test --workspace --release` — 0 failures.
2. `cargo clippy --workspace --all-targets --release` — 0 warnings.
3. `cargo build -p index-wasm --target wasm32-unknown-unknown --release` — succeeds.
4. `crack-converge`: naive still FAILs the sequential `cum/scan ≤ 1.2x` bound (the red→green proof
   in `p1` depends on it) and stochastic still passes all three.
5. `beat-btreemap`: PASS on space, p50 and p99 for all four distributions at the single default ε.

## Not done, and why
- **Reducing `SAMPLE` below 32.** `locate` costs up to 32 LF-steps and halving the sampling would
  roughly halve it, but each halving adds ~1 bit/char, which would eat the entropy-shaping win
  just measured. Unmeasured trade — left alone deliberately.
- **Enumerating only the symbols present in `bwt[sp..ep]` during fuzzy backtracking**, instead of
  looping the byte range and asking. `contain()` now skips absent symbols in O(1), which is most
  of the benefit for sparse alphabets; the tree-walk enumeration is the remaining O(σ) → O(distinct)
  step and is not implemented.
- **Wiring `index-core` into `index-text`.** Out of scope for this pass and probably wrong: the
  text engine's ordered-key needs are served by `tantivy-fst`, and a PGM index over `u64` document
  keys would be a new surface, not a drop-in.
