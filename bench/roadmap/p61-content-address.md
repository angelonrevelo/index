# P52 — the 1:1 proof: content address, dedup, and the compression that is not ours to write

**Tier:** T2 · **Bin:** `image-corpus` · **API:** `ImageDoc::digest`, `ImageIndex::duplicate_of`
**Status: PARTIAL, 2026-09-06.** `digest.rs` (SHA-256, FIPS 180-4 vectors, cross-checked against
`sha256sum`) and `ImageIndex::duplicate_of` / `duplicate_group` / `duplicate_byte` are built and
tested. Measured **29.82% redundant exact copies** over the full 17,311-file corpus (42.69% among
images alone) — landing on the ~30% the literature reports for LAION-2B, from an independent
scrape. Near-duplicate counts are printed at every threshold rather than as one headline rate.
**The dedup rate cannot be sampled**: a 1,500-file systematic sample said 9.00%, because striding
across a corpus strides across duplicate clusters.
**Acceptance 4 — the round-trip proof — is now EXERCISED and PASSES.** `scripts/jxl-roundtrip.py`
drives real `cjxl`/`djxl` 0.12.0 over the corpus: **2,000 / 2,000 JPEG transcoded and restored
BYTE-EXACT**, verified by SHA-256 against the original. `OVERALL: PASS`.

**Both of §5's published claims FAILED to reproduce on this corpus, and that is the finding:**

| §5 claim | Measured here |
|---|---|
| JPEG XL lossless saves **13–22%** (~20% typical) | **6.65%** — does NOT reproduce |
| **~1%** of real JPEG cannot be losslessly reconstructed | **0.00%** over 2,000 files — does NOT reproduce |
| A generic compressor wins **1–3%** on entropy-coded JPEG | **1.06%** (zlib control) — **reproduces** |

The explanation is the corpus, not the codec. These are small, already-optimised CDN assets from a
single pipeline (p50 file size 27 KiB); Cloudinary's ~20% is measured on typical photographic
JPEGs with far more redundancy left in their entropy coding, and the ~1% refusal rate comes from
files carrying unusual trailing bytes that a clean CDN pipeline does not emit.

**The consequence sharpens this row's own conclusion.** At 6.65%, transcode is worth *less* here
than it looked, while dedup measured **29.82%** — so on this corpus the content address is not
merely the cheaper win, it is roughly **4.5x** the bigger one.

The product ask was "compress to the fewest bytes while the download is still 1:1 with the
original, all attributes preserved". [`docs/research/image.md`](../../docs/research/image.md) §5
answers it, and the answer reorders the work:

> Byte-exact recovery **is** lossless compression, bounded by information theory. An
> already-JPEG-encoded corpus is already entropy-coded — zstd and brotli win **1–3%** on it. The
> best available lossless transcode, JPEG XL, wins **~20% MEASURED**, and **fails outright on ~1%**
> of real JPEGs. Meanwhile **~30% of a scraped corpus is duplicated.**

So dedup beats the codec, and the codec is somebody else's finished work (libjxl, BSD-3). **This row
does not write a codec.** Lepton, PackJPG, brunsli and FLIF all tried and are all dead or absorbed;
neural lossless compression is GPU-only and has no deployable decoder.

What belongs in the engine is the part nobody else provides: **the proof that the round-trip is
actually 1:1**, and the dedup that does most of the saving.

## What must be built

| | |
|---|---|
| `ImageDoc::digest` | A 32-byte content address over the **original bytes**, computed in-crate (no dependency). Dedup key and round-trip proof in one. |
| `ImageIndex::duplicate_of(digest)` | Exact-duplicate lookup, reusing the existing key-field machinery (`p40`) rather than a new structure. |
| Near-duplicate report | Clusters at the `p57` hash thresholds, with the threshold printed beside every count. |
| Round-trip verification | Given an original and a restored file, assert digest equality. **This is the whole "1:1" claim, reduced to one comparison.** |

## Acceptance

1. **Exact-duplicate rate** on the real corpus of `p60`, by digest. Printed with the corpus census.
2. **Near-duplicate rate** at each documented threshold, with the count at each — not one headline
   number. §5 records a **12x spread** (3% to 37%) across published methods, which means any single
   figure is a methodology choice, not a fact, and the benchmark must present it that way.
3. **Byte accounting**: total corpus bytes, bytes after exact dedup, and the projected saving from a
   ~20% lossless transcode applied to the deduplicated remainder — with the transcode figure clearly
   marked as **cited, not measured here**, since this repo does not run libjxl.
4. **Round-trip proof**: for any file the host claims to have restored, digest equality holds or the
   check fails. **A transcode that cannot be proven byte-exact must be reported as a failure**, not
   rounded up to success — §5 measures that at ~1% of real JPEGs, so the failure path is the common
   case for a hundred-file batch and must be exercised.

## Explicitly not built

- **A codec.** See above. The honest ceiling is ~20% and libjxl has it.
- **A JXL binding.** There is no pure-Rust JXL *encoder*; taking libjxl would put a C++ toolchain
  into a crate whose whole thesis (§8) is that it has no dependencies. Transcoding is a host-side
  decision, and this row gives the host the digest to prove it went right.
- **A thumbnail cache.** The preview tier reduces *bandwidth*, not archival storage. Worth doing,
  but it is a different claim and belongs in its own row — the free path (`p57`'s embedded-EXIF
  thumbnail, and JPEG DC-only 1/8-scale decode) is already noted there.
