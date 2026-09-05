# P5 — typo-tolerant term dictionary: feasibility

**Tier:** T1 (runnable + deterministic) · **Bin:** `cargo run -p index-bench --release --bin fuzzy-term`
**Status:** **MEASURED 2026-09-05.** Verdict **FEASIBLE**, with two honest reds and one refuted
hypothesis recorded below. Gate-excluded (lives in `bench/roadmap/`) until the feature is built.

## The question

Can a typo-tolerant term dictionary meet the byte and latency budgets that the *real consumers*
impose (`docs/research/demand.md`), built on `tantivy-fst` + `levenshtein_automata` rather than
from scratch?

This exists because [`demand.md`](../../docs/research/demand.md) Finding 3 identified typo
tolerance as the **largest unmet need across the whole app roster**, and
[`build-or-buy.md`](../../docs/research/build-or-buy.md) found the two crates that do it carry
4.2 M and 4.4 M recent downloads respectively — so the work is *integration and policy*, not
invention. Before writing that integration, the numbers had to exist.

## Policy under test

The convergent production design. Typesense, Meilisearch, Algolia and Lucene arrived at it
independently (`docs/research/relevance.md` §7):

> **≤ 2 edits hard-capped · length gates at 0–3 → 0, 4–7 → 1, 8+ → 2 · first character protected ·
> numeric tokens exempt · fired lazily, only when exact match underdelivers.**

## Thresholds, and where each one comes from

| # | Bar | Derived from |
|---|---|---|
| 1 | term dictionary **≤ 8 bytes/key** | `fst`'s published 2.7 B/key at 119 K dictionary words, loosened 3× for longer mixed-script tokens. The binding real constraint is profstopick's browser budget: its 2.5 MB JSON shard already eats **95.6 % of the 5 MB localStorage quota**. |
| 2 | exact lookup **p99 ≤ 1 000 ns** | This is the 90 % path — fuzzy fires only on the deficit — so it must be effectively free. |
| 3 | fuzzy **p99 ≤ 5 ms** | One keystroke frame is 16.7 ms; profstopick's *entire* post-fix match pass measures 0.43 ms. A 5 ms ceiling on the fallback path keeps the worst keystroke inside one frame with 3× headroom. |
| 4 | typo recall **≥ 2× exact-only** | profstopick measured **109 of 267 real searches returning nothing**. A fuzzy layer that does not move that number is not worth its bytes. |

> An earlier draft of this bench used a 0.1 ms fuzzy bar. It was a number picked because it looked
> tidy, it had no consumer behind it, and it is not used. Recorded so nobody reintroduces it.

## Corpus

Deterministic (`splitmix64`, seeded), dependency-free, and **shaped like the two real corpora** —
an FST's size is dominated by shared prefixes, so random strings would measure nothing:

- `name/profstopick` — `SURNAME, GIVEN M. <rare>`, from pools of 56 Philippine surnames and 33
  given names.
- `product/presyo` — `BRAND VARIANT <rare> SIZE UNIT`, from pools of 50 PH grocery brands, 25
  variants, 7 units.

**Every label carries a generated rare token** (3–5 Filipino CV syllables). This is not decoration.
The first version of this bench used closed pools only and produced a **115-token vocabulary**,
which is not a term dictionary — it is a rounding error, and it flattered the FST enormously
(0.03 B/key). Fuzzy cost scales with **vocabulary size, not document count**, so a realistic long
tail is the whole measurement.

## Measured baseline — 2026-09-05

Windows 11, x86-64 GNU toolchain, `rdtsc` clock, release build. 2,000 probe queries per cell, each
a real token corrupted by one deletion / substitution / transposition at a position **≥ 1** (first
character never touched, digits never touched).

### At profstopick's actual scale (11,949 labels)

| | name corpus | product corpus |
|---|---|---|
| distinct token | 12,029 | 20,071 |
| **term FST** | **96,547 B (8.03 B/key)** | **109,562 B (5.46 B/key)** |
| label FST | 192,800 B (16.14 B/key) | 224,998 B (18.83 B/key) |
| exact p50 / p99 | 210 / 760 ns | 200 / 650 ns |
| fuzzy p50 / p99 | 229 µs / **663 µs** | 63 µs / **597 µs** |
| **typo recall** | **135 → 1999 of 2000 (14.8×)** | **628 → 1981 of 2000 (3.2×)** |

> **The headline.** A 94 KB term dictionary — **3.8 % of the 2.5 MB JSON shard profstopick ships
> today** — takes typo recall from 6.8 % to 99.9 % at a p99 of 0.66 ms, which is 1.5 % of one
> keystroke frame. This is the single strongest result in the repo, and it is not about learned
> indexes.

### Scaling

| Vocabulary | term FST B/key | exact p99 | fuzzy anchored p99 | recall gain |
|---|---|---|---|---|
| 12 K | 8.03 | 760 ns | 663 µs | 14.8× |
| 20 K | 5.46 | 650 ns | 597 µs | 3.2× |
| 247 K | 6.99 | 900 ns | 1.12 ms | 14.0× |
| 260 K | 6.61 | 890 ns | 1.09 ms | 11.0× |
| 848 K | 5.94 | **1 380 ns** | 2.86 ms | 12.3× |
| 861 K | 5.85 | 980 ns | 1.65 ms | 11.8× |

Bytes/key **improves** with scale (more prefix sharing). Fuzzy p99 grows roughly with vocabulary,
as predicted, and stays inside the 5 ms bar even at 861 K distinct terms — an order of magnitude
past presyo's ~296 K aliases.

## The two reds — kept red on purpose

1. **`exact p99 = 1 380 ns` at 848 K terms, against a 1 000 ns bar.** Exact FST lookup p99 grows
   with vocabulary (traversal depth plus cache misses on a 5 MB structure). This is a real finding,
   not noise: the same cell's p50 is 640 ns. Either the bar moves *with evidence* or the exact path
   gets a front cache. It must not be silently relaxed.
2. **`bytes/key = 8.03` on the 12 K name corpus, against an 8.0 bar.** A hair over, and specific to
   this corpus: the synthetic rare tokens are 3–5 syllables (6–10 chars), longer than real Filipino
   name tokens. Worth re-measuring against profstopick's real shard before treating it as a
   property of the design.

`OVERALL: FAIL` and exit code 1 are therefore **correct output today**. This benchmark has been
shown to go red on something real, which is the standard `bench/README.md` sets and which most
benchmarks never meet.

## Refuted hypothesis — recorded because it was measured

This bench was written expecting **prefix-anchoring to be a speed lever** (intuition: protecting
the first character prunes the automaton early). It is not. Anchored and unanchored ED-2 measure
within noise of each other — the anchor effect on p50 ranges **−22.7 % to +9.6 % across cells, with
no consistent sign.** When the query *is* a whole token, the prefix constraint removes almost
nothing.

**First-character protection is a precision and typeahead rule, not a performance one.** The
roadmap must not claim it as the latter. The comparison is printed on every run so the claim stays
falsifiable.

## Promotion criteria

This benchmark is promoted out of `bench/roadmap/` into the live gate when the term dictionary
ships, and must then go green — which requires resolving both reds above, and re-running against
**profstopick's real 11,949-entry shard** rather than the synthetic stand-in (blocked today: the
shard is not committed, only `search-index-manifest.json`, and generating it needs `DATABASE_URL`).

## Reproduce

```sh
cargo run -p index-bench --release --bin fuzzy-term                 # 12K / 260K / 1M scales
INDEX_BENCH_N=11949 cargo run -p index-bench --release --bin fuzzy-term   # profstopick scale
```

Dependencies are **bench-crate only** — `index-core` remains dependency-free
(`crates/index-bench/Cargo.toml`, audited in `docs/research/build-or-buy.md`).
