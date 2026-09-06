# P23 — the defect is on real data, the cost was mismeasured, and the fix is back in

**Tier:** T1 · **Bin:** `cargo run -p index-bench --release --bin pool-audit`
**Status: MEASURED and FIXED, 2026-09-05. Reverses the decision recorded in p22.**

## Why this row existed

`p22-prune-consistency.md` reproduced a real correctness defect, built two repairs, and **reverted
both** on one specific ground:

> The bug has not been observed on any real corpus, while the latency cost is unconditional.

Both halves of that sentence were wrong, and this bin is what found out.

The reasoning was only as good as the looking behind it, and "all the consumer benches pass" is weak
evidence: **none of them was designed to detect this.** They score precision@10 and hit@1 against
labels, which a pool eviction can survive — swap one *correct* document for another *correct* one
and every existing metric is unmoved.

## Finding 1 — it happens constantly on real data

`search` against `search_exhaustive_unpooled` (brute force over the whole corpus, the same ground
truth `p22` uses), on real fixtures with real queries:

| corpus / query set | queries | rank-1 differs | top-10 worse | lost a better-matching doc |
|---|---|---|---|---|
| presyo / product names | 4,028 | 0.00 % | **28.20 %** | **1,136** |
| presyo / 3-word tails | 4,004 | 0.00 % | **18.21 %** | **729** |
| blead / business names | 4,330 | 0.00 % | 1.66 % | 72 |
| maphy / place names | 1,067 | 0.00 % | 0.09 % | 1 |
| profstopick / titles | 2,253 | 0.00 % | 0.04 % | 1 |
| presyo / category names | 145 | 0.00 % | 0.00 % | 0 |

**Rank 1 is never wrong** — the single best answer always survives. But on **28.2 % of real presyo
product queries**, the top-10 contained a document that matched *less of the query* than one the
pool had discarded. Every degraded query was of that kind, not a reordering among equals: the
severity column was added precisely because "different" and "worse" are not the same claim.

## Finding 2 — the latency cost was a measurement error

`p22` recorded the two-pool repair as costing **+50–75 % on p99**. That figure compared a run on a
**thermally loaded machine, hours into a benchmarking session**, against a **cold-machine baseline
recorded that morning**. It was measuring the machine, not the change.

Interleaved A/B in one session, three pairs, toggling only the ranking pool:

| | without | with | delta |
|---|---|---|---|
| exact p50 @1M | 305 µs | 310 µs | **+1.7 %** |
| typo p50 @1M | 631 µs | 650 µs | **+3.0 %** |
| typo p99 @1M | 6,406 µs | 6,812 µs | **+6.3 %** |

One pair returned a *lower* p99 with the fix enabled, so p99 is within noise. **The real cost is
2–6 %.**

## Finding 3 — on the real workload the fix is free within noise

The 2–6 % above is from the `scale` bench: DepEd school names over a 41,069-term dictionary. presyo
has **117,472 terms** and much longer queries, so the number had to be re-earned rather than assumed
to transfer. Timing `search` over **4,028 real presyo product names**:

| | without | with | delta |
|---|---|---|---|
| p50 | 144.3 µs | 144.7 µs | **+0.3 %** |
| p99 | 1,006 µs | 1,019 µs | **+1.2 %** |

**Indistinguishable from noise.** Real product queries carry many terms, so the scoring loop
dominates and one extra small-heap operation per candidate disappears into it. The DepEd 2–6 % is
the conservative upper bound; the consumer workload pays about 1 %.

### And the first attempt at this measurement was biased, again

Running OFF-then-ON three times reported **OFF at 171 and 169 µs against ON at 144 µs** — the fix
apparently making things *faster* by 15 %. That was warm-up: the first process of each pair pays
cold-cache costs, and OFF always went first. Alternating the order collapsed the spread to the table
above.

Third ordering artifact of the day, after the warm-versus-cold baseline that cost a working fix.
**A/B without alternating the order is not an A/B.**

## The decision, reversed

The fix is restored. It is worth it by an enormous margin:

| corpus | worse before | worse after | lost-better-match before → after |
|---|---|---|---|
| presyo / product names | 28.20 % | **9.33 %** | 1,136 → **221** |
| presyo / 3-word tails | 18.21 % | **4.35 %** | 729 → **66** |
| blead / business names | 1.66 % | **0.18 %** | 72 → **3** |
| maphy / place names | 0.09 % | **0.00 %** | 1 → **0** |
| profstopick / titles | 0.04 % | **0.00 %** | 1 → **0** |

**3× fewer degraded queries and 5–11× fewer lost better-matching documents, for 2–6 % latency.**
Two corpora go clean. `real-corpus` and `sisia-catalog` still `OVERALL: PASS`; 103 tests green.

## What is still red, and why that is correct

`p22` still fails from **512 decoys** rather than 32. The residual is documents that block-max
skipping discards *before scoring*, so neither pool ever sees them — the part only the
single-`eff`-pool design fixes, and that one really does cost 10–30×.

So the ledger is now: **a 16× wider safety margin on the synthetic worst case, most of the real
degradation gone, at a few percent.** The remaining hole needs a bucket-aware *pruning bound*, which
is still a retrieval-loop redesign and still not attempted.

## Two mistakes worth keeping

**1. "No consumer bench detected it" is not evidence of absence** when no consumer bench was built to
detect it. Every existing metric compares against labels; this defect swaps correct documents for
less-correct ones *inside* the labelled set, which those metrics cannot see. The audit had to
compare against the engine's own comparator instead.

**2. Never compare a warm measurement to a cold baseline.** The +50–75 % that justified reverting a
working fix was thermal drift across a long session. The A/B that corrected it took three minutes
and should have been the first thing run, not the last. **Interleave, or do not claim a delta.**

That is the ninth methodology error recorded here, and the only one that caused a *correct fix to be
thrown away*.

## Not measured

- ~~**The 9.33 % that remains on presyo product names.**~~ **CONFIRMED as pre-score skipping** by
  the rank-pool sweep above: if it were eviction, a larger ranking pool would have moved it, and it
  does not move at all.
- ~~**Whether `k`-sized is the right ranking pool.**~~ **SWEPT.** From `k` to `16k` the result is
  **byte-identical** — 376 queries worse, 221 losing a better-matching document, same score gaps at
  every size. The ranking pool **saturates at `k`**, and that is itself the proof that everything
  still missing is discarded *before scoring*: no pool can retain a document it never sees. This
  also settles the residual's attribution, which was listed below as unconfirmed.
- ~~**Real-query latency**, as opposed to the synthetic scale bench.~~ **MEASURED** — below.

## Finding 3 — on the real workload the fix is free

The 2–6 % in Finding 2 came from the `scale` bench, which queries DepEd school names over a
41,069-term dictionary. presyo is a different workload: long product names over **117,472 terms**.
A number measured on one has to be re-earned on the other rather than assumed to transfer.

4,028 real presyo product-name queries, A/B'd with the order **alternated** to cancel warm-up bias:

| | without | with | delta |
|---|---|---|---|
| p50 | 144.3 µs | 144.7 µs | **+0.3 %** |
| p99 | 1,006 µs | 1,019 µs | **+1.2 %** |

**Free within noise.** The DepEd figure stands as a conservative upper bound; it does not describe
this workload, where scoring long queries dominates and one small-heap operation per candidate
disappears into it.

**The alternation is the point.** Running *without* first in every pair produced 171 / 169 µs for
that arm against 144 µs for the other — the first process of each pair pays cold costs, and reading
that as a real difference would have been the **third** wrong delta in this file's lineage. The
first one cost a working fix an entire round.

## Reproduce

```sh
cargo run -p index-bench --release --bin pool-audit
cargo run -p index-bench --release --bin prune-consistency
```
