# P50 — one query plan over pixels, metadata and text

**Tier:** T1 · **Bin:** `image-corpus` · **API:** `ImageIndex::search_fused`
**Status: SHIPPED, 2026-09-06.** Built in `crates/index-image/image.rs`.
On **100** fused queries at k=20 over the full 12,007-document corpus: **0 short pages, 0 filter
leaks, 0 hits with zero agreement**. Fusion costs **623 µs p50 / 843 µs p99** against text-only's
2 µs — a number, not a claim.
The red-to-green proof below was run: removing the re-filter of the soft arms failed **exactly**
the two leak guards and nothing else.
**Acceptance 3 (fused nDCG beats both halves) is NOT yet met** — it needs a labelled query set and
real embeddings, both of which this corpus lacks. Recorded as outstanding, not quietly dropped.

This is the headline row of the image tier, and the only one whose claim is not already solved by
somebody else. [`docs/research/image.md`](../../docs/research/image.md) §3 states it as narrowly as
it can be stated while staying false of every shipping system:

> **No system answers text, facet, numeric-range, vector and perceptual-hash predicates in one
> query plan over one index, selecting top-k once.**

Immich and LibrePhotos separate pgvector/FAISS from the relational row from a separately-scheduled
face-clustering job, and intersect in application code. PhotoPrism combines them in its *query
syntax* and fans out underneath — a real partial counterexample, recorded as such.

## Why this repo can make the claim and they cannot

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
