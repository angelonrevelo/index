# P29 — the bounded-error lever, priced; and a correction to p27

**Tier:** T1 · **Bin:** `scale` with `INDEX_CAP_SWEEP=1` · **API:** `Index::search_capped`
**Status: SHIPPED AS OPT-IN, 2026-09-06. Default behaviour unchanged. It does not reach the bar, and
the reason overturns `p27`'s explanation of the tail.**

`p27` closed the tail as an optimization problem and said the remaining lever was a product
decision. This document builds that lever, measures it, and hands back numbers instead of a
question.

## What was added

`Index::search_capped(query, k, cap)` — identical to `search` but caps how many dictionary terms
each query token may expand to. `search` still passes `MAX_EXPANSION = 16`; nothing else changes.
The cap is a query-time parameter threaded through `plan`/`emit`, not state, so it needs no
serialization change and is safe across threads.

**The error is one-sided and predictable.** Expansions are already ordered by edit distance first,
then document frequency ascending, so a lower cap discards the vaguest and least discriminative
matches first. It can only *lose* a hit that a rarer, more distant correction would have found; it
never invents one and never reorders what it keeps. `cap == 0` is treated as 1, so exact queries stay
exact.

## Measured at 1 M documents

Agreement is against the default answer, which `pool-audit` has already shown exact against brute
force. 2,000 corrupted real school names.

| cap | typo p50 | typo p99 | top-10 identical | rank-1 identical |
|---|---|---|---|---|
| **16 (default)** | 1,141 us | 16,368 us | 100 % | 100 % |
| 8 | 1,123 us | 12,033 us | 99.50 % | 99.50 % |
| **4** | 1,116 us | **11,046 us** | 98.10 % | 98.10 % |
| 2 | 1,028 us | 11,912 us | 93.90 % | 94.05 % |
| 1 | 1,036 us | 12,434 us | 88.10 % | 88.50 % |

**The best available trade is cap 4: a 33 % cut in typo p99 for 1.9 % of queries changing.** Below
4 the accuracy falls off and the latency stops improving — the curve is not monotone, which is the
finding.

## The correction to p27

`p27` concluded, and `ROADMAP.md` repeated:

> the 17 ms typo p99 is ... typo expansion putting up to `MAX_EXPANSION` terms in every group, so
> documents matching all groups are genuinely rare.

**That is wrong, and this sweep is what shows it.** At `cap = 1` there is no expansion at all — each
token contributes its single closest dictionary match — and typo p99 is still **12.4 ms**. Expansion
accounts for roughly **24 %** of the tail. Three quarters of it survives with expansion switched off.

What remains is the cost of **exact bucket-first ranking itself**. With a corrupted token, the
pool's worst member carries a high bucket whether that token expanded to 16 terms or 1, so
`prune_is_sound` stays false, the `p26` bound stays unsatisfiable, and the scan runs long. The
gates are exact and have no slack — `p26` and `p27` were right about that — but the tail they
produce is not attributable to expansion.

## Which means the 5 ms bar is not reachable by any arm measured today

| build | correctness | typo p99 @ 1 M |
|---|---|---|
| `p24` (two gates) | 2.23 % of presyo queries wrong | 9.4 ms |
| `p26`/`p29` default | exact | 16.4 ms |
| `p29` cap 4 | 98.1 % agreement | 11.0 ms |
| **`p7` bar** | | **5 ms** |

**Nothing measured today meets it, including the build that predates every correctness fix.** The
bar was set on a different machine in a different state, and `p7`'s own margin came partly from
pruning that discarded documents the ranking wanted. It stays red and unraised; what has changed is
that the red is now explained rather than merely observed.

## Honest limits

- **A cap is not an adaptive cap.** A per-query budget keyed on posting-list length would spend the
  expansion where it is cheap. Not built; the flat sweep is what prices the idea.
- **The sweep reports `p99` from 2,000 queries**, so the 99th percentile is the 20th slowest query
  and is noisy. The p50 column is solid; treat p99 differences under ~10 % as unresolved.
- **This measures agreement, not quality.** A changed answer is not necessarily a worse one — the
  document the cap drops may be a correction no user wanted. Deciding that needs labels this corpus
  does not have.

## A defect this bin caught in itself

The first run reported `typo p50 1,036,708 us`. `time_each` returns **nanoseconds** and the main
`scale` table divides by 1,000; the sweep did not, and printed nanoseconds labelled `us`. It was
caught by adding a **control row that times plain `search` through the same harness** — it read
1,177,257 us against the table's 936 us directly above it. That control is now permanent: a sweep
that cannot reproduce the row above it is measuring something else.

## Reproduce

```sh
INDEX_CAP_SWEEP=1 INDEX_BENCH_N=1000000 cargo run -p index-bench --release --bin scale
```
