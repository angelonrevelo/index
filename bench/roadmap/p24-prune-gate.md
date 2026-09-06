# P24 — the pruning defect, closed

**Tier:** T1 · **Bins:** `prune-consistency` (now GREEN), `pool-audit`
**Status: FIXED, 2026-09-05. `p22` passes at every decoy count; real degradation 9.33 % → 2.23 %.**
**Superseded by `p25`, which closes the remaining 2.23 % at a third pruning site this document missed.**

## The defect, and why the first two attempts failed

`p22-prune-consistency.md` reproduced it and `p23-pool-audit.md` measured it on production data:

> The engine **prunes by score** and **ranks by bucket-then-score**. Those two disagree, so a
> document matching more of the query is discarded on a score bound before the ranking sees it.

Two repairs were built and priced:

| attempt | `p22` | cost |
|---|---|---|
| one pool ordered by `eff` | passes | **10–30×** — the pruning threshold sits below every score, so skipping never engages |
| two pools (score + rank) | red from 512 | 2–6 % — but block-max still skips documents *before* scoring |

## The fix: gate the pruning, do not replace it

The two failures share a diagnosis, and it points at the answer. Score-based pruning is not *wrong*;
it is **conditionally sound**:

> **Skipping on a score bound is valid exactly when the ranking pool is full and its worst member
> has bucket 0.** Then no unseen document can have a better bucket, so entering the answer requires
> beating a *score* — which is precisely what block-max skipping and the non-essential bail test.

The moment the ranking pool's worst member has a bucket above 0, a document matching more of the
query enters regardless of score, and a score bound cannot exclude it.

```rust
fn prune_is_sound(rank_pool: &BinaryHeap<RankCandidate>, rank_cap: usize) -> bool {
    rank_pool.len() >= rank_cap && rank_pool.peek().is_some_and(|w| w.0.bucket == 0)
}
```

Both pruning tests are gated on it. **Nothing else changed** — no new structure, no reordering, no
threshold arithmetic. The condition is monotone (a ranking pool never gets worse), so it flips on
once per query and stays on.

## Measured

**`p22` is green at every decoy count** — 4 through 2,048, where it previously failed from 32 (before
the two-pool fix) and from 512 (after).

Real data, `pool-audit`:

| corpus | original | two pools | **+ gate** | lost-better-match |
|---|---|---|---|---|
| presyo / product names | 28.20 % | 9.33 % | **2.23 %** | 1,136 → 221 → **90** |
| presyo / 3-word tails | 18.21 % | 4.35 % | **3.17 %** | 729 → 66 → **50** |
| blead / business names | 1.66 % | 0.18 % | **0.02 %** | 72 → 3 → **0** |
| maphy / place names | 0.09 % | 0.00 % | **0.00 %** | → **0** |
| profstopick / titles | 0.04 % | 0.00 % | **0.00 %** | → **0** |

**12.6× fewer degraded queries than this morning on presyo, and three of five corpora are clean.**

## Cost, measured interleaved

| | ungated | gated | delta |
|---|---|---|---|
| exact p50 @1M | 1,095 µs | 1,183 µs | **+8.0 %** |
| typo p50 @1M | 1,422 µs | 1,530 µs | **+7.6 %** |
| typo p99 @1M | 42,390 µs | 50,634 µs | **+19.4 %** |

**The absolute figures in that table are not comparable to earlier runs** — the machine had been
benchmarking for hours and both arms are ~4× their usual. Only the interleaved delta is meaningful,
and interleaving is now the house rule precisely because a warm-versus-cold comparison already threw
away a working fix once (`p23`).

**~8 % on p50 and ~19 % on p99**, against 10–30× for the only other design that closes `p22`.

## What is left, honestly

**2.23 % of presyo product queries still differ, and 90 still lose a better-matching document.**

**Now explained — see `p25-essential-gate.md`.** It is the same defect at a third pruning site (the
MaxScore essential/non-essential partition), which drops documents from the candidate *enumeration*
rather than from a pool. `p23`'s rank-pool sweep was right that pool size is not the lever, for the
reason it stated: no pool can retain a document it never sees. Gating that site on THIS predicate costs 20.7x; `p25`
ships a different one (a bucket floor) that reaches 0.00 % on every corpus for +14 % p50 on real
presyo queries.

`prune-consistency` becomes a unit test the day that residual is understood; until then it stays a
roadmap bin, now green, which is a materially different thing from the red it was an hour ago.

## Not measured

- **The residual 2.23 %.** Diagnosed in `p25`; the affordable repair is designed there, not built.
- **Clean-machine absolute latency.** The A/B is valid; the absolute numbers in it are not, and a
  quiet-machine re-run is worth doing before the +8 %/+19 % figures are quoted anywhere load-bearing.
- **Whether the gate hurts low-selectivity queries specifically.** The scale bench's typo arm is the
  closest proxy and it is the +19 % column, but that mixes query classes.

## Reproduce

```sh
cargo run -p index-bench --release --bin prune-consistency   # now PASS at every decoy count
cargo run -p index-bench --release --bin pool-audit
```
