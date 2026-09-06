# P66 — exact facet counts beside an active vector arm

**Tier:** T2 · **Bin:** `image-corpus` · **API:** `ImageIndex::facet_tally_fused`
**Status: REJECTED, 2026-09-06 — the same day it was written. Acceptance 4 killed it.**

This row required its own comparison class be verified before it claimed anything. That check was
run immediately, and the claim did not survive: **plain SQL satisfies the property, and so does
Solr.** The file stays on disk because it settles the question cheaply if the topic reopens — the
same reason `p3-sfc-dimensionality.md` was kept. Full reasons in
[`docs/roadmap-rejected.md`](../../docs/roadmap-rejected.md).

> **Why it is dead.** The SQL standard computes aggregates at step 4 and `LIMIT` at step 9, so a
> `GROUP BY` is *already* exact over the full `WHERE`-filtered set. Worse for the claim: pgvector's
> HNSW index is only ever consulted for an `ORDER BY <distance> LIMIT k` branch, so an aggregate
> branch is never routed through it and **`hnsw.ef_search` truncation cannot reach the counts.**
> Postgres satisfies the property in *both* exact and HNSW mode. DuckDB VSS, sqlite-vec and
> ClickHouse follow for the same structural reason. **Solr satisfies it too**, in the rerank
> configuration (`rq={!rerank reRankQuery=$rqq}` with a `{!knn}` reranker) — and that one is a
> search engine, which is more damaging than the databases.
>
> The systems that *do* degrade — Vespa, Elasticsearch, Weaviate, Qdrant, Milvus, LanceDB — degrade
> because they are purpose-built vector engines whose faceting is defined over the **ANN result
> set** rather than a predicate match set. That is a design choice of that product category, **not a
> law**, and mistaking one for the other is exactly how `p59` overclaimed.

**What is left is a performance question, not a capability one**, and it is not worth a row until
somebody measures it: whether one fused pass beats a two-branch SQL plan. Even that is shaky — with
`WITH m AS MATERIALIZED (...)` Postgres reads the base table once and the tuplestore twice, which a
reviewer can fairly call one pass too.

---

## The row as originally written, preserved

Everything below is the row as written before acceptance 4 was run. It is kept verbatim because the
reasoning that led to a wrong conclusion is the useful part — it shows exactly which assumption was
load-bearing (that "purpose-built vector engines degrade facet counts" generalises to *all* systems)
and it did not.

This row exists because retracting `p59`'s novelty claim turned up one property that might actually
be a differentiator — and the honest thing to do with it is write it down as an open question rather
than promote it into the boast that just failed.

## Where it came from

[`docs/research/image.md`](../../docs/research/image.md) §3 records the retraction: the claim that no
system fuses text, facets, ranges, vectors and hashes in one plan with one top-k is **false** —
Vespa does it in one `rank-profile`, and Lucene 9+ does it under one `IndexSearcher.search(query, k)`.

But the same sweep found something every system checked gets *wrong* in the same way:

| System | Facet counts under an active vector arm |
|---|---|
| **Vespa** | Documents it outright: *"Grouping counts are not accurate when using nearestNeighbor"* |
| Elasticsearch / OpenSearch | Aggregations collapse to the top-`k` (or `k x shards`) under approximate kNN |
| Weaviate | `Aggregate` is a separate query needing its own `objectLimit` |
| Milvus | No facet counts; `group_by` is barred outright on binary indexes |
| LanceDB | Scanner refuses: `"Cannot use limit/offset with aggregate."` |
| Qdrant | No facet counts alongside a top-k |

The reason is structural. An ANN arm returns a *sample* — the top `k` or an over-fetched `k x n` —
so any count computed from it is a count over the sample, not over the match set. Getting it right
means visiting every matching document, which is exactly what an ANN index exists to avoid.

## Why this repo is positioned for it, and why that is not yet a claim

`index-text`'s `facet_tally` already counts over **every** matching document rather than the top `k`
— that is `p30`'s stated contract, and `facet-shop` asserts it exact against brute force on
presyo's 241,677 products. And `p58` chose a **flat scan** over an ANN graph, so the vector arm
visits every live vector anyway. A count over the full fused match set is therefore cheap here for
the same reason it is expensive everywhere else.

**None of which is implemented.** Stated plainly so this row cannot be mistaken for an achievement:

- `ImageIndex::search_fused` produces **no tallies at all**.
- `facet_tally` takes a **text query only** — it cannot see a vector or hash predicate.
- So today `index` has the *ingredients*, and none of the dish.

## What must be built

| | |
|---|---|
| `ImageIndex::facet_tally_fused(&FusedQuery, slot)` | Exact counts over every document satisfying the **whole** fused predicate — text, facet, range, vector radius and hash radius together. |
| `range_tally_fused` | The same for numeric histograms (`p31`'s shape). |
| Cost accounting | Counting over the full match set is a second pass over the candidate set. **Report what it costs**; a filter bar that doubles query latency is not obviously worth having. |

## Acceptance

1. **Exact against brute force.** Counts match an exhaustive scan on the real corpus, for at least
   40 fused queries spanning every arm combination. Zero wrong.
2. **The counts sum correctly.** Per `p31`'s rule, a histogram's buckets sum to *at most* the match
   count — a document with no value in the slot is in no bucket — and that is asserted, not assumed.
3. **Cost is a number, not a claim.** p50/p99 of a fused query with and without tallies, at full
   corpus size.
4. **The comparison is verified, not inherited.** The table above is from a single research sweep.
   Before this row claims a differentiator it must be checked against at least **pgvector** and
   **DuckDB VSS**, neither of which was tested. A `COUNT(*) ... GROUP BY` alongside
   `ORDER BY ... LIMIT k` may well satisfy the property in plain SQL, unaccelerated — and if it
   does, the differentiator is about *speed*, not capability, and this row must say so.

## The rule this row is written under

`p59` overclaimed because it was measured against the competitors that flattered it. This row starts
from the assumption that it will turn out the same way. **It may not survive acceptance 4**, and
that is an acceptable outcome — `docs/roadmap-rejected.md` is a normal destination. What is not
acceptable is repeating the mistake: no differentiator is claimed here until the comparison class is
the right one and the code exists.
