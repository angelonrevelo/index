# P92 — SIMD block decode: measured first, tried, reverted — the bit-loop was never the cost

**Tier:** T1 · **Check:** `cargo test -p index-text` · **Files:** removed (the measurement tool
stays: `crates/index-text/examples/loadtime.rs`)
**Status: BUILT, MEASURED, REVERTED, 2026-09-13. Width-specialized unpack arms were
differentially tested against the reference and measured NO win on the real presyo index
(170–184 ms vs a 164–181 ms baseline, all within run noise), so the general loop stays. The
negative result closes the last row of the honest-limits speed list.**

## The measurement that opened the row

`Index::from_bytes` on the real 13,704,342-byte presyo index: **median 164–181 ms across runs**
(`examples/loadtime.rs`, median of 5, page cache warm). That is a real cost — every native open
and every `apply` pays it — and `p88`'s block-FOR columns are decoded in it, so "SIMD block
decode" looked like the lever the honest-limits list named.

## What was tried

Width-specialized unpack arms for the widths the corpora actually produce — byte-aligned
`from_le_bytes` loops for 8/16/32 bits, eight-values-per-u64 extraction for 1/2/4 — with the
general bit-cursor loop as fallback and a differential test pinning every specialized arm to the
reference across 14 widths × 6 block lengths, including ragged tails and cursor drift.

Result: **no measurable win.** 170.3 / 184.2 ms vs the 163.8 / 180.9 ms baseline range — the
spread between two runs of the SAME binary is larger than the difference between the binaries.
The bit-unpack arithmetic is not where the 164 ms goes: per-term `Vec` allocation, dictionary
parse and bounds-checking dominate, which is why the fast arms moved nothing.

## The decision

Reverted. A specialization that adds 60 lines and a second code path to a correctness-critical
decoder, for a change inside run noise, is exactly what this repo's own methodology says to
refuse — `p83` (bucket-tiered enumeration) and `p33`'s corrections are the precedent. The
differential test went with it; the general loop was never wrong.

What this row leaves behind that is real:

1. **`examples/loadtime.rs`** — the load-cost measurement this row opened with, for whoever
   profiles the next lever (allocation-free decode or a dict-parse fast path are the named
   candidates the measurement points at).
2. **The negative result, recorded**: SIMD block decode is not the lever at these corpus sizes.
   The browser range path decodes ONE list per query (sub-µs); native load spends its time
   elsewhere. The honest-limits line is updated accordingly.

`p90` (docID reordering, opt-in) and `p91` (secondary sections, −25.1 %) are the levers that
paid this session. Not every named lever does; that is what measuring is for.
