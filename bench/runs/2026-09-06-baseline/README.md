# 2026-09-06 — image tier baseline

The committed baseline [`bench/README.md`](../../README.md) requires, so that a later change is a
real `diff` rather than a memory.

## `image-corpus.log`

`cargo run --release -p index-bench --bin image-corpus`, run to completion over all **17,311 files**
of `alec/expressway_dump` with no `--limit`. Verdict: **`OVERALL: PASS`, one verdict withheld.**

Reproduce:

```sh
cargo run --release -p index-bench --bin image-corpus
# or point it elsewhere:
INDEX_IMAGE_CORPUS=/path/to/corpus cargo run --release -p index-bench --bin image-corpus
```

## What is stable in this file and what is not

**Stable — diff these.** The census, EXIF rate, every duplicate and near-duplicate count, the
bytes/image figures, the fused-correctness counts, the index-size table, and the check list. Two
independent full runs on this machine diffed to **zero** on every verdict-bearing line.

**Not stable — ignore these in a diff.** The latency table. It is `rdtsc`-timed on a Windows
desktop with other work running; the numbers move run to run and the two full runs differed by a few
percent. Treat a latency change under ~20% as noise, exactly as `ROADMAP.md` already does for the
±0.9 ms p99 variance it records at 1 M text documents.

**The run takes roughly 15 minutes and is I/O-bound**, not CPU-bound: the corpus is 2.84 GB, of
which 2.04 GB is 5,304 non-image files the scrape also captured.

## Two cautions about reading this baseline

**The dedup rate cannot be sampled.** A 1,500-file systematic sample (every 11th file) measured the
exact-duplicate rate at **9.00%**; the full corpus measures **29.82%**. Striding across a corpus
strides *across* duplicate clusters, so a sampled dedup figure is wrong by a factor of three rather
than merely imprecise. If a future run uses `--limit`, its dedup section is not comparable to this
one. Everything else sampled cleanly (cheap tier 2.14 vs 2.049 ms; 112.0 B/image exactly).

**The near-duplicate tables are near-saturated here.** At dHash <=4, **97.07%** of images are within
radius of something, because this scrape is dominated by hero images re-exported at several sizes.
That is a fact about the corpus, not about the hash, and no general near-duplicate rate should be
read out of it. The tables are printed at every threshold anyway, because
[`docs/research/image.md`](../../../docs/research/image.md) §5's point — that the method chooses the
answer — is what they exist to show.

## The withheld verdict is not a pass

`p58`'s recall check prints `[HELD]`. This repo ships no embedding model, so the run's default
vectors are seeded noise and a recall figure over them would measure the arithmetic rather than the
retrieval. Re-run with `--embedding <path>` (header `u32 count`, `u32 dim`, then f32 little-endian)
to get a real verdict. Until then §4's `UNVERIFIED` stands.
