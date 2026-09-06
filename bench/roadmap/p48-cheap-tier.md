# P48 — the cheap tier: what every image gets, for under 10 ms and ~150 bytes

**Tier:** T1 · **Bin:** `image-corpus` · **API:** `hash`, `color`, `meta`
**Status: SHIPPED, 2026-09-06.** Built in `crates/index-image/{hash,color,meta}.rs`.
Measured on the FULL 17,311-file corpus: **p50 2.049 ms** (budget <10 ms) and **112.0 B/image**
(budget <200 B) — both **HOLD**. **0 panics, 0 unreadable, 0 decode failures** over all 17,311
hostile scraped files, including the 5,304 that are not images.
The rotation/crop limits are asserted by test, so they cannot quietly become an overclaim.

Before any model runs, there is a tier of signal that costs almost nothing and answers a
surprising fraction of real questions. [`docs/research/image.md`](../../docs/research/image.md) §6
prices it:

| Signal | Cost | Bytes |
|---|---|---|
| dHash / aHash 64-bit | <1 ms | 8 |
| pHash 64-bit (DCT) | ~1–5 ms | 8 |
| OKLab palette, 5–8 swatch | ~2–10 ms on a downsampled copy | 24–64 |
| EXIF / XMP | <1 ms, **no pixel decode** | tens |
| JPEG quantisation table | <1 ms, already parsed | ~128 |

**No model. No GPU. No image-decoding dependency.** That is the whole point: it works on the machine
that has the files, today, and it is what makes a first result appear before an embedding pass has
finished.

## The three decisions

**Thresholds are measured, not chosen.** Meta's recommended PDQ threshold is **<=31 of 256 bits**;
random unrelated pairs average **128/256**. For 64-bit pHash the widely-repeated "threshold 10" is
**too loose** — a measured corpus had **31 of 63** flagged matches be false positives at <=10,
falling to 6 at threshold 4. Every default constant must carry the number that justifies it, and
**thresholds must not be ported across code lengths**.

**Colour buckets live in OKLab, not RGB or Lab.** RGB Euclidean distance does not match perception.
The FOSS reference implementation (`sergeyk/rayleigh`) uses CIELab because it predates OKLab (2020),
which exists specifically to fix Lab's blue-hue non-uniformity. The achromatic case is the one that
actually breaks systems: **near-zero chroma has no meaningful hue**, so greys must land in dedicated
neutral buckets rather than being scattered across hue cells by numerical noise.

**Missing metadata is the normal case.** §7: X/Twitter, Instagram, Facebook, TikTok, Snapchat,
LinkedIn and Reddit all strip EXIF from the downloadable copy. The index must rank what it has and
never reject a document for absent metadata. Note also that stripping protects the uploader from
other *users*, not from the *platform* — absence at download is not absence at the source.

## Acceptance

1. **Cost.** Measured p50 per image for the whole cheap tier is **under 10 ms**, and total stored
   bytes per image **under 200**, on the real corpus of `p51`. Both printed, not asserted from the
   table above.
2. **The hash actually works.** On the real corpus: a re-encode and a resize of the same image stay
   under the recommended threshold; an unrelated pair sits near 50% of the code length.
3. **The hash's limits are proven by test, not by comment.** A >5-degree rotation and a >5% crop
   must be shown to FAIL to match — the documented limit is asserted, so it cannot silently become
   an overclaim later.
4. **Greys do not get a hue.** A neutral image lands in a neutral bucket across the full lightness
   range. This is the check that catches the classic colour-search bug.
5. **The EXIF parser survives hostile bytes.** The corpus is scraped, so the parser eats arbitrary
   input. It must be structurally incapable of panicking or hanging: bounds-checked reads, a
   cyclic-IFD guard, capped allocation, no `unwrap`. Proven by a seeded fuzz-shaped test over
   thousands of random buffers plus truncation at every length from 0 to 64 bytes.
6. **Determinism.** Same bytes, same hash, same palette, every run. No clock, no `rand`.

## Explicitly not built

- **PDQ bit-compatibility.** The 256-bit hash here is PDQ-*shaped* — same construction, not
  guaranteed bit-identical to Meta's. It therefore does **not** interoperate with ThreatExchange
  blocklists, and must not claim to.
- **PRNU sensor noise.** §6 prices it at seconds per image plus a reference set per camera. It is
  lab forensics, not an ingest-tier signal.
- **Error Level Analysis.** Valid only as one weak signal in an ensemble; standalone ELA is not a
  reliable "edited: yes/no" test and will not be shipped as one.
