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

## `image-corpus-real-embedding.log` — the run with a real encoder

The same benchmark, driven by **12,007 real CLIP ViT-B/32 embeddings** rather than seeded noise.
This is the authoritative run: **all eight checks PASS, nothing withheld.**

    cargo run --release -p index-bench --bin image-corpus -- --emit-manifest manifest.tsv
    python scripts/embed-corpus.py manifest.tsv embedding.bin   # 133 s, 90 img/s, RTX 2060 SUPER
    cargo run --release -p index-bench --bin image-corpus -- --embedding embedding.bin

What it adds over the synthetic run:

- **`p58` recall@10 = 0.9870** — the binary-prefilter pipeline against the exact oracle, on IMAGE
  embeddings. `docs/research/image.md` §4 had this as `UNVERIFIED`; every published
  binary-quantisation recall figure is for *text*.
- **`p59` acceptance 3 HOLDS**: fused nDCG@10 **0.2932** > vector-only **0.2910** > text-only
  **0.0203**, on the non-degenerate near-duplicate labelled set.
- Latency roughly **2.4x higher** than the synthetic run (fused p50 1,467 µs vs 623 µs). Real
  embeddings over a 42.69 %-duplicate corpus cluster densely, so the Hamming shortlist carries far
  more ties into the rerank. A benchmark that had quietly used synthetic vectors would have
  understated its own cost by more than double.

**Read the acceptance-3 margin honestly.** Fusion beats the vector arm by **+0.0021 nDCG — about
0.7 % relative, over 100 queries.** It satisfies the letter of the acceptance criterion and it is
the right sign, but it is far too thin to call a decisive win. The reason is visible in the same
table: the text arm scores 0.0203, because a CDN URL path is a poor caption. On a corpus with real
captions or alt text the fusion gain should be much larger — and that, not a bigger number here, is
what would actually settle the question.

## Why there are two labelled sets

The first one built was **exact-duplicate groups**, and it turned out to be degenerate: byte-identical
files give byte-identical pixels, hence identical embeddings, hence a vector arm at nDCG 1.0000 *by
construction*. Nothing can exceed a perfect oracle, so that set cannot adjudicate "fused > vector"
at all. It is still printed, as a `[note]`, because the shape is informative — but it does not vote.

The set that votes is **near-duplicates with exact duplicates removed** (dHash <= 4, sha256 differs):
the pixels genuinely differ so the vector arm is strong but imperfect, the paths differ so the text
arm is independent, and the label comes from a perceptual hash — neither arm's own function — so it
hands neither a free win.

## The withheld verdict is not a pass

Without `--embedding`, `p58`'s recall check prints `[HELD]`. This repo ships no embedding model, so the run's default
vectors are seeded noise and a recall figure over them would measure the arithmetic rather than the
retrieval. Re-run with `--embedding <path>` (header `u32 count`, `u32 dim`, then f32 little-endian)
to get a real verdict. Until then §4's `UNVERIFIED` stands.
