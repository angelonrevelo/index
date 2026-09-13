# P91 — the secondary sections: −25.1 % more off the file, off the path the query never touches

**Tier:** T1 · **Check:** `cargo test -p index-text` · **Files:** `crates/index-text/src/format.rs`
**Status: SHIPPED, 2026-09-13. 18,305,266 → 13,704,342 bytes on the real presyo schema (−25.1 %),
latency at parity, precision identical. Format IDXTEXT11 → IDXTEXT12, 139 index-text tests.**

After `p88` the posting section stopped being the fat half of the file, and the byte breakdown of
a real 241,793-row presyo build moved the target:

| section | bytes | share |
|---|---|---|
| doc_key | 3,342,803 | 18.3 % |
| **posting_offset** | **2,874,064** | **15.7 %** |
| posting | 5,988,335 | 32.7 % |
| **doc_len** | **1,934,344** | **10.6 %** |
| **facet_id** | **1,934,344** | **10.6 %** |
| first_term | 967,172 | 5.3 % |

Three sections — 37 % of the file — were fixed-width tables whose contents are mostly tiny.

## The three encodings

- **posting_offset → block directory + varint deltas.** Every 64 entries gets a fixed 16-byte
  `(cumulative value, byte where this block's deltas start)` record at a COMPUTABLE position, and
  the entries inside a block are varint deltas from their predecessor. Fixed `u64` spent eight
  bytes on a list length that is usually one or two varints. The directory exists because
  variable-width deltas alone would destroy random access — the byte position of a later block is
  unknowable without decoding everything before it — and it keeps any single entry decodable in
  at most 64 varints, which is what the range tier (`posting_span`) and the OPFS browser tier
  fetch one term's list through. 2.87 MB → 1.15 MB.
- **doc_len → one varint column per field, an all-zero field one flag byte.** A name-only corpus
  leaves fields 1.. empty for EVERY document; the fixed `u16 × 4` stride paid two bytes per doc
  per empty field. 1.93 MB → ~0.28 MB.
- **facet_id → width-coded per slot.** A slot whose label count fits `u8` costs one byte per
  document, not four; the width's maximum value is the "no value" sentinel. 1.93 MB → ~0.73 MB
  on the two presyo slots (u16 brands, u8 categories).

`doc_key` (18.3 %) was left alone deliberately: the keys are short random ids, so prefix
compression has nothing to grip, and dict-coding them is a design decision this row does not
pretend to have made.

## Measured

Real presyo schema, 241,793 rows, keys + two facets:

| | IDXTEXT11 | **IDXTEXT12** |
|---|---|---|
| file | 18,305,266 B | **13,704,342 B (−25.1 %)** |

`presyo-catalog` warm: exact p50 159 µs, typo p50 675 µs, typo p99 4,954 µs, precision@10 61.7 %
— parity on every arm with the pre-p91 runs. The sections changed here are load-time and
range-read structures, not the in-memory query path, which is why the file shrank a quarter and
the queries did not move. Combined with `p88`: **25.13 MB → 13.70 MB, −45.5 % of the session's
starting file, bytes/doc 103.9 → 56.7** on this schema.

## Verification

Round-trips for expansion, numeric ranges, priors, deletions, facets (single and conjunctive),
unscored columns and keys all pass unchanged; the range tier's byte-for-byte match with a full
open passes; and the wasm `idx_range_load` header check now reads the `12` layout. One defect the
first draft carried and the round-trips caught: the sequential offset decode filled nothing —
every reload saw a zero span — which is exactly the class of bug `scale`'s reload check exists
for, caught here at unit scale.

**Old files are refused loudly** (magic `IDXTXT11` → named as an older version by `KNOWN_MAGIC`).
