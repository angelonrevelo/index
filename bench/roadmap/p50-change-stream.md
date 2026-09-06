# P50 — continuous sync, and the bug the bench found on its first run

**Tier:** T1 · **Bin:** `cdc-equivalence` · **CLI:** `index apply`
**Status: SHIPPED, 2026-09-06. Membership exact over 8,000 operations. Ranking drift measured and
NOT gated, for reasons `p38` established.**

Continuous sync was asked for alongside "any database", and those two answers together rule out the
obvious design. A CDC daemon needs a driver per source — logical replication, binlog, change
streams — which is exactly what `p49` refused. So **the change stream is a pipe too**:

```sh
pg_recvlogical -S idx -f - --start -P wal2json | jq -c '...' | index apply -d data/ --jsonl
```

Whatever already emits changes feeds the same tool. The engine stays database-ignorant.

## The contract: a stream of states, not of edits

One record per line, an operation and a key:

```json
{"op":"u","after":{"sku":"A-1","name":"Colgate Total Charcoal 150g"}}
{"op":"d","before":{"sku":"A-3"}}
```

`--key after.sku,before.sku` takes **fallbacks, first present wins**, which is what covers a stream
that puts the row in `after` on an upsert and in `before` on a delete without special-casing either.
`c/create/i/insert/u/update/r/read/upsert` and `d/delete` cover Debezium and wal2json without
either being named in the code. A record with no `op` at all is an upsert, so a plain row stream
works unchanged.

## The bug, found in the first run of the bench that was written to look for it

`cdc-equivalence` compares an incrementally-updated index against a model of the table. Its first
run reported **212 keys resolving wrongly, and 35 more live keys than the model had.**

The cause was ordering. Upserts are batched into one new segment while deletes tombstone existing
ones, so **every delete in a batch was applied before any upsert in it, whatever order they
arrived**. A stream saying `upsert k` then `delete k` ended with `k` PRESENT: the delete ran against
the old segment, and the new one was appended afterwards.

Nothing threw. Membership silently diverged from the source of truth — precisely the failure the
whole feature exists to prevent, shipped in the feature itself.

**The fix is to collapse the stream per key before applying anything.** A change stream is a
sequence of *states*, so a key's last record decides it and every earlier one is redundant.
Collapsing makes the order between upserts and deletes irrelevant **by construction** rather than by
careful sequencing — and a thousand updates to one row become one document instead of a thousand.

`a_delete_after_an_upsert_wins_and_the_reverse_also_wins` is the regression test, and
`scripts/cli-smoke.sh` drives the same hazard end to end through the real binary: a row inserted and
deleted in one stream must end up absent.

## Two claims, held to different standards on purpose

**Membership is EXACT, and gated.** Every key the model holds resolves to exactly one live document,
and the live count equals the model count. No scoring is involved, so there is no room for "close
enough". Over **8,000 operations across 20 batches** on 13,000 real business names — inserts of
unseen rows, updates, deletes — **0 keys wrong, 14,288 = 14,288.**

**Ranking DRIFTS, and is measured rather than gated.** Two documented causes, which compaction
removes together:

- a deleted row still counts toward document frequency and average field length until a rebuild
  (`Index::deleted`, and what Lucene does between merges);
- each segment scores against its own collection statistics (`p38`).

| ops | deleted | skew | `needs_compaction` | rank-1 | top-10 overlap |
|---|---|---|---|---|---|
| 400 | 1.7 % | 2.3 % | no | 93.0 % | 87.7 % |
| 2,000 | 8.2 % | 10.3 % | no | 89.0 % | 78.9 % |
| 4,400 | 15.8 % | 20.3 % | **YES** | 86.7 % | 72.6 % |
| 8,000 | 24.8 % | 31.6 % | YES | 84.0 % | 66.4 % |

**Most of the drift arrives with the very first delta** — 93 % at 400 operations — and then decays
slowly. That matches `p38`'s 98.0 % rank-1 at two segments, minus the extra cost of deletions, which
`p38` did not have: it measured appends only. **This is the first measurement of what deleting and
updating cost, as distinct from appending.**

## The metric was wrong first, and `p38` had already paid for that lesson

The first version reported exact top-10 sequence equality and read **15 %**, which looks like a
catastrophe. `p38` had already found and named this:

> That number is true and it is useless. ... 70.3 % of broad queries have a top-10 whose scores span
> under 5 %, so among near-ties the order is arbitrary.

The probe here was a two-word name prefix — a broad query. Switching to whole names (selective) and
to `p38`'s metrics, rank-1 and overlap, gives the table above. The retired metric is still printed
in an `order(p38)` column so a reader can see *why* it was retired rather than take it on trust; it
falls to 4 %, and means nothing.

**The gate is set at rank-1 >= 80 %, below the observed 86.7 %, and deliberately not at it.** A bar
fitted to today's measurement only ever says "nothing changed". This one is a regression detector;
what the number *means* is argued here, not encoded there.

## The operational finding

`needs_compaction` fires at a default ratio of 0.2, by which point rank-1 has already settled near
its floor. For a delete-heavy stream that is late: the drift is front-loaded, so the choice is
between rebuilding often (and paying build cost) or accepting ~87 % rank-1 parity with a rebuild.
`Searcher::set_compaction_ratio` exists for tuning it, and the bin prints the advice.

**No threshold holds rank-1 above ~93 % under this workload**, because the first delta already costs
that much. That is a property of per-segment statistics, not of the compaction signal.

## Still open

- **Nothing checkpoints the stream position.** `apply` is idempotent for deletes (re-delivering one
  is a no-op and says so) and for upserts (the last write wins), so replay is safe — but the tool
  does not record where it got to, and cannot. That belongs to whoever runs the pipe.
- **A batch is one segment**, so a stream applied one record at a time produces one segment per
  record. Batching is the caller's job and the tool should probably say so more loudly than a note
  at the end of a run.
- **Ranking parity is bounded by per-segment statistics**, which is `p38`'s open problem, not a new
  one. Global collection statistics across segments would fix both.
