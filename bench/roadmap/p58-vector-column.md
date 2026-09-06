# P49 — the vector column: brute force, and why that is the right answer

**Tier:** T1 · **Bin:** `image-corpus` · **API:** `VectorColumn`
**Status: SHIPPED, 2026-09-06. Acceptance 1 is MET — recall@10 = 0.9870 on REAL embeddings.**
`crates/index-image/vector.rs` implements the three tiers and the pipeline, with 14 tests green.
On a seeded clustered set the measured top-10 recall vs the exact oracle is **1.0000** at the
default oversample, and **0.333 / 0.603 / 0.876 at oversample 4 / 16 / 64** for a query sitting in
no neighbourhood at all — that second number is the honest boundary of the binary prefilter and is
recorded rather than hidden.
**A real encoder has now run.** `scripts/embed-corpus.py` produced **12,007 real CLIP ViT-B/32
embeddings** over the `p60` corpus (90 img/s on an RTX 2060 SUPER, 133 s, zero placeholders), and
the benchmark measured the binary-prefilter pipeline against `search_exact` at the default
oversample of 4:

> **recall@10 = 0.9870.**

That is the number `docs/research/image.md` §4 recorded as `UNVERIFIED` — every published
binary-quantisation recall figure is for **text** embeddings. It lands inside Qdrant's 0.98–0.9966
text band, so the published result does transfer to image embeddings; it is now a measurement here
rather than an assumption borrowed from a different modality.

**A second result came free, and it justifies the SYNTHETIC discipline in hard numbers.** The same
benchmark reports fused p50 **623 µs on seeded vectors** and **1,467 µs on real ones — 2.4x
higher**. Real image embeddings over a corpus that is 42.69 % duplicates are densely clustered, so
the Hamming shortlist survives with far more ties for the rerank to resolve. A benchmark that had
quietly accepted synthetic vectors would have understated its own latency by more than double.

The reflex when adding vector search is to reach for HNSW. At this project's scale that is the
wrong tool, and [`docs/research/image.md`](../../docs/research/image.md) §4 has the numbers:

| MEASURED | |
|---|---|
| 1 M x 384-d exact brute force, M4 MacBook Pro | **79.7 QPS** single-thread (12 ms), **170.5 QPS** at 10 threads |
| Qdrant binary quantisation, 100 K vectors | **900 MB to 128 MB** — 32x |
| Qdrant binary + **3-4x oversample rerank** | recall **0.98–0.9966** |

30,000 images at 512-d is **1.9 MB of binary code**. A popcount scan over 1.9 MB is
sub-millisecond. An HNSW graph over it costs build time, memory, recall and mutability, and buys
nothing. **The crossover is around 1 M vectors**, and this repo's honest corpus ceiling is far below
it — the same limit `ROADMAP.md` already records for text.

Building the graph anyway would also break the thing that makes `index-text` usable: `searcher.rs`
adds documents without a rebuild. A graph index does not.

## What must be built

Three tiers of the same vector, from one `push`:

| Tier | Bytes at 512-d | Role |
|---|---|---|
| binary, 1 bit/dim | **64** | popcount prefilter |
| int8 + per-vector scale | **516** | rerank the shortlist |
| f32 (optional, flag) | 2,048 | exact final pass |

Pipeline: **binary Hamming shortlist -> int8 rerank -> exact rerank -> true top-k.** Default
oversample **4**, justified by the Qdrant 3–4x figure above.

## Acceptance

1. **Recall against the oracle.** `search()` must return the same top-10 as `search_exact()` on the
   real embedding set of `p60`. Report the measured recall. **The number is the deliverable** — do
   not assert Qdrant's 0.98 and move on.
2. **The published figure does not transfer, and the benchmark must say so.** Every
   binary-quantisation recall number in the literature is for **text** embeddings; §4 records
   `UNVERIFIED` for an image equivalent. This row exists to produce that missing number, so the
   output must print recall for image embeddings explicitly labelled as this repo's own measurement.
3. **Bytes/vector table** printed per tier, matching the arithmetic above within 1%.
4. **Determinism.** Same corpus, same query, same result, including tie-break order.
5. **Never panics** on dimension mismatch, `k == 0`, empty column, `k > len`, or `NaN` in the query.
6. **p50/p99 at 1 k / 10 k / full corpus**, single-threaded, `rdtsc`-timed as elsewhere in this repo.

## Explicitly not built

- **No HNSW/IVF graph.** If a future corpus crosses ~1 M vectors, that is a new row with its own
  evidence, not a pre-emptive dependency.
- **RaBitQ** ([arXiv:2405.12497](https://arxiv.org/abs/2405.12497)) is the theory for D-bit codes
  with a proven error bound. This row implements the plain signed-bit version. RaBitQ is named as
  the thing **not** implemented rather than implied.
- **Matryoshka truncation** is exposed but must be documented as a property of the *model*, not this
  code. §8 records `UNVERIFIED` for an image-retrieval degradation curve; truncating a non-MRL CLIP
  embedding is not safe and the API must not suggest it is.

## Red-to-green proof required

- Skip the rerank stage and return the binary shortlist directly -> check 1 must fail, and the
  measured recall drop is itself the evidence the rerank earns its cost.
- Drop the per-vector int8 scale -> check 1 must fail.
