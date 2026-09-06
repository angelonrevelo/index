# P51 — the real corpus: 17,311 scraped web images

**Tier:** T1 · **Bin:** `image-corpus` · **API:** the whole image tier, end to end
**Status: BUILT AND RUN, 2026-09-06.** `crates/index-bench/src/image_corpus.rs`, run to completion
over all **17,311 files** (2,838 MB): `OVERALL: PASS` with one verdict explicitly withheld (`p49`).
Baseline committed under [`bench/runs/2026-09-06-baseline/`](../runs/2026-09-06-baseline/). It surfaced two things nobody planned: the
field-budget collision that became [`p56`](p56-field-budget.md), and a **contradiction** of the
expectation that scraped assets carry lying extensions — `meta::sniff` disagreed with the
extension on **0 of 1,500** files.

This repo's rule is that a benchmark runs on real data or it does not count. For the image tier that
rule bites harder than usual, because the failure modes are all properties of *messy* corpora:
stripped metadata, near-duplicates, mixed formats, and files that are not really photographs.

## The corpus

`alec/expressway_dump` on this machine — **17,311 files**, a genuine scrape of a public site's
asset tree (Supabase-hosted hero images and page assets), mixed WebP/JPEG/PNG. It was not curated
for this benchmark, which is exactly its value.

**It is the right shape for the OSINT question and the wrong shape for the personal-photo
question**, and the benchmark must say so rather than generalise from it:

- scraped CDN assets, so EXIF is largely stripped — §7's prediction, testable here;
- near-duplicates are expected to be common (hero images re-exported at several sizes);
- it contains no faces to speak of and no personal photography.

**The corpus ceiling is recorded, not hidden.** 17,311 images is below the 20–30 k the product
question asks about and far below the 100 k+ where every incumbent's measured failures appear
(§2). Anything claimed above this count is recombination and must be labelled as such — the same
discipline `ROADMAP.md` already applies to the 61,467-row text ceiling.

## What the benchmark must produce

1. **A corpus census, first.** Format mix, dimension distribution, byte-size distribution, EXIF
   presence rate, exact-duplicate rate by content hash. **Print it before any result**, because
   every number below is meaningless without it — and the EXIF presence rate is itself the test of
   §7's claim.
2. **Cheap-tier cost** (`p48`): p50/p99 ms per image and bytes per image, measured.
3. **Dedup**: exact-duplicate rate by digest, and near-duplicate rate at each documented hash
   threshold. §5 predicts scraped corpora run 3–37% depending on method — this produces the number
   for *this* corpus, with the method stated.
4. **Vector recall** (`p49`): measured recall of the binary-prefilter pipeline vs the exact oracle,
   the missing image-embedding number §4 records as `UNVERIFIED`.
5. **Fused query correctness** (`p50`): short pages, leaks, agreement, and p50/p99 vs the text-only
   baseline.
6. **Index size**, total and per image, against the corpus's own byte size.

## Where the embeddings come from

This crate ships no model (§8, and `crates/index-image/src/lib.rs` states why). The benchmark
therefore has two modes, and **must never silently use the second**:

- `--embedding <path>` — real vectors produced by a host-side encoder, the honest path.
- `--embedding synthetic` — a seeded, deterministic stand-in so the *plumbing* can be gated in CI
  without a model. Every number derived from synthetic vectors must be printed with a
  **`SYNTHETIC`** marker, and the recall check of `p49` must **refuse to report a verdict** in this
  mode. A recall figure over made-up vectors measures nothing.

## Acceptance

- Runs to completion over all 17,311 files without panicking, including on whatever malformed or
  truncated files the scrape contains. **A crash on hostile input is a failure of `p48` check 5**,
  not a corpus problem.
- Deterministic: same corpus, same verdict, every run. Baseline committed under `runs/` per
  [`bench/README.md`](../README.md) so before/after is a real `diff`.
- Prints `OVERALL: PASS` / `FAIL` with a bounded check list, not a token dump.
- **Skips cleanly with a stated reason when the corpus is absent**, since the path is machine-local
  and CI will not have it. A missing corpus must not turn the gate red, and must not silently pass
  either.
