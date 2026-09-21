# P97 — TIN parity and in-process enterprise grade (not built)

**Tier:** T3 · **candidate-tier: expected-red until built** · **gate-excluded** until an engine
change lands. **Status: partial.** Efficiency (stored df / sparse 64-doc page presence for
overlapping OR/AND and deletions) and concurrent-snapshot reliability and CRC integrity shipped
2026-09-20/21. Speed (10,260 QPS on 8.0 GB) and observed GitHub check-run are **not** shipped.

This is **not** a claim the bars are met. p93 measured the gap. p94/p95 exported COUNT. This file
is the acceptance for closing the gap and for in-process reliability/CI — without becoming a
Postgres extension.

## Speed bar (TIN, verbatim)

PlanetScale TIN, Wikipedia disjunction `COUNT(*)`, 8.0 GB, in-memory:

**10,260 QPS / 2 ms p99 / 1.7 MB/query.**

Source: `planetscale.com/blog/introducing-tin` (2026-09-16), restated in
[`p93-tin-shape.md`](p93-tin-shape.md).

## Current gap (this repo, verbatim)

Cost is **linear in postings**. At 1.84 GB: 3,774 QPS / 6.0 ms. Extrapolated to 8.0 GB:
**~870 QPS / ~26 ms — ~12x behind.** Full 8.0 GB build needs ~28 GB peak RSS; this Mac has 24 GB.
The proxy is the largest holdable Wikipedia slice plus that linear extrapolation. Replacing the
named 8.0 GB / 10,260 / 2 ms / 1.7 MB/query bar with a smaller corpus and calling it reached is
forbidden.

## Efficiency bar

COUNT does **not** walk every posting of every term. The lever TIN names: **stored per-term counts**
and **page-level work elision** (AND/OR of page bitmaps). Pass when a COUNT's bytes-read and wall
time stay flat as posting-list length grows on a holdable corpus, and the 8.0 GB projection moves
with it. Fail while `count_any` / `count_all` still iterate postings for the answer.

Holdable proof (2026-09-21): overlapping OR/AND and deleted COUNT visit 0 postings at 64 and 4096
docs (`count_any_of_overlapping_or_deleted_terms_does_not_walk_postings`). Release proxy
(`count_any_overlapping_qps_is_not_linear_in_postings --ignored`): 16x docs (4096 → 65536) is
6.6x wall, visits stay 0 -- not linear in postings. That is **not** 10,260 QPS on 8.0 GB.

## Reliability bar (in-process, not MVCC)

TIN's concurrent-write bar, translated: readers stay correct and do not stall while writers run.
Here that is **`index apply` / Searcher mutation concurrent with search and COUNT**. After a
bounded mixed load, membership and COUNT must match a rebuilt snapshot of the same final rows.
Torn views, stalled readers, or COUNT disagreeing with rebuild = fail. This is **not** Postgres
visibility maps, WAL, VACUUM, or replicas.

## Enterprise-grade bar (this product form)

1. **On-disk integrity:** a truncated or byte-flipped `.idx` is refused by `idx_open` and by
   `index search` / `index stat` (loud error, not plausible garbage).
2. **Observed CI:** `gh run list --repo <this origin> --limit 5` is non-empty. "File written,
   first GitHub run pending" is not this row DONE. Local `cargo test` is not a check-run.

Not in this row: SLA, SSO, SOC2, multi-tenant, 99.9% uptime, `CREATE EXTENSION`.

## Pass / fail

| bar | pass | status |
|---|---|---|
| efficiency | COUNT does not walk every posting of a live undeleted term, including overlapping OR/AND and deletions | **SHIPPED** (`count_any_cost` / `count_all_cost` visits 0) |
| speed | holdable `tin-shape` COUNT + linear projection no longer ~12x behind 10,260 QPS / 2 ms / 1.7 MB/query | **not built** (`INDEX_TIN_TSV` absent; do not substitute the 65 536-doc proxy) |
| reliability | concurrent apply + query matches rebuilt snapshot | **SHIPPED** |
| enterprise grade | corrupt `.idx` refused; GitHub check-run observed | **integrity SHIPPED; CI not observed** |

Marking any of these SHIPPED while the corresponding row above is red is a ROADMAP lie.

## What this file is not

An implementation. A Wikipedia 8.0 GB bake-off (needs ~28 GB peak). A sidecar. AGPL. A consumer-app
PR. Promote into `cargo test` only when the engine change exists; until then it stays under
`bench/roadmap/` as the spec.
