# P59 — one query plan over pixels, metadata and text

**Tier:** T1 · **Bin:** `image-corpus` · **API:** `ImageIndex::search_fused`
**Status: SHIPPED, 2026-09-06.** Built in `crates/index-image/image.rs`.
On **100** fused queries at k=20 over the full 12,007-document corpus: **0 short pages, 0 filter
leaks, 0 hits with zero agreement**. Fusion costs **623 µs p50 / 843 µs p99** against text-only's
2 µs — a number, not a claim.
The red-to-green proof below was run: removing the re-filter of the soft arms failed **exactly**
the two leak guards and nothing else.
**Acceptance 3 (fused nDCG beats both halves) is NOT yet met** — it needs a labelled query set and
real embeddings, both of which this corpus lacks. Recorded as outstanding, not quietly dropped.

This row was written as the headline of the image tier, on a claim that has since been refuted:

> ~~No system answers text, facet, numeric-range, vector and perceptual-hash predicates in one
> query plan over one index, selecting top-k once.~~

**RETRACTED 2026-09-06.** That was tested only against self-hosted photo apps — the wrong comparison
class. **Vespa** does exactly this in one `rank-profile` with a single top-k; **Lucene 9+** composes
`KnnFloatVectorQuery` into a `BooleanQuery` under one `IndexSearcher.search(query, k)`; and Hamming
distance over binary codes is first-class in Vespa, Elasticsearch and LanceDB. Full evidence and the
per-system verdicts: [`docs/research/image.md`](../../docs/research/image.md) §3.

**What survives is not about query modelling.** Vespa and Elasticsearch are servers, Lucene is a JVM
library; this runs the same index in a browser through a hand-written C ABI, dependency-free, under
MIT OR Apache-2.0. That is a packaging property, and it is the honest one.

**The engineering is unaffected.** Fan-out is still a real defect where it happens — Weaviate's own
docs call hybrid *"two searches under-the-hood"* and over-fetch to 100 before trimming; LanceDB's
scanner carries `"Cannot have both nearest and full text search"`. Selecting once is still right.
It is simply also what Vespa does, so it is not a differentiator.

## Why the design is still right, even though the claim was not

`index-text` already carries the hard half: BM25F with exact `u16` field lengths, block-max MaxScore
top-k, categorical facets (`p30`), numeric ranges (`p31`), sort-by-value (`p32`), phrases (`p45`)
and multi-segment live update. A fused image query is *that* engine with two more column types
bolted into the same candidate loop — not a second system.

The decisive design point is **where top-k is selected**. Fan-out architectures select top-k per
subsystem and then intersect, which is why they return short pages: a document ranked 51st by text
and 51st by vector may be the best fused result, and both subsystems have already discarded it.
Selecting once, over a fused score, cannot make that mistake.

## What must be built

| | |
|---|---|
| `ImageIndex::add(&ImageDoc, &Doc, Option<&[f32]>)` | One image enters as one document: text field, facet, numeric column, palette term, hash column, vector. |
| `ImageIndex::search_fused(&FusedQuery, k)` | Text + facet + range + vector radius + hash radius, **one candidate pass, one top-k**. |
| `Hit::why -> Why` | Which signal(s) found this document. A fused answer is only worth having if it is explicable. |
| Fused scoring | Convex combination over normalised per-signal scores, reusing `fuse::convex`; agreement count breaks ties. |

## Acceptance — all must hold on the real corpus of `p60`

1. **No short page.** For 100 hardest fused queries at k=20, the result count equals
   `min(k, matching_count)`. Zero short pages. This is the defect the fan-out architectures have
   structurally, so it is the first thing asserted.
2. **No leak.** Every returned document satisfies every hard predicate (facet, range, hash radius).
   Zero violations across 100 queries.
3. **Fusion beats either half alone.** On a labelled query set, fused nDCG@10 must exceed both
   text-only and vector-only. If it does not, the row is **wrong and must be rejected**, not tuned
   until it passes — `docs/roadmap-rejected.md` is the destination.
4. **One pass.** Assert by instrumentation that the posting lists and the vector column are each
   traversed **once** per query, and top-k heap-selected once.
5. **Explicability.** For every hit, `why.agreement() >= 1`, and a hit found by two signals ranks
   above a same-score hit found by one.
6. **p50/p99 recorded** at the corpus size of `p60`, alongside the text-only baseline, so the cost
   of fusion is a number rather than a claim.

## Red-to-green proof required

Per [`bench/README.md`](../README.md), a benchmark that has never gone red has not been shown to
catch anything. Inject each of these and confirm the named check fails:

- select top-k per signal then intersect -> check 1 fails (short pages appear);
- drop the facet predicate after candidate generation -> check 2 fails;
- score fused as `max` instead of the convex combination -> check 3 regresses.

## Known limit, stated now rather than discovered later

BM25 statistics are per-segment (as in Lucene, whose IDF is per-shard), and `searcher.rs` already
asserts the bound this puts on a broad query's near-ties. **A fused query inherits that limit**, and
the vector half does not fix it: a broad text query with a weak vector signal can still reorder
near-ties across a segment boundary. Assert the bound; do not claim it is absent.
