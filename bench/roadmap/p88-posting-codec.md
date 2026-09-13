# P88 — mode-coded posting lists: −37.5 % of the presyo index, −59 % at 1 M, latency at parity

**Tier:** T1 · **Check:** `cargo test -p index-text` + `cargo run -p index-bench --release --bin scale`
· **Files:** `crates/index-text/src/format.rs`
**Status: SHIPPED, 2026-09-13. 57.3 → 35.8 B/doc on presyo's 241,677 real products, 79.3 → 32.4
B/doc at 1 M. Interleaved A/B: latency at parity on every arm. Format IDXTEXT10 → IDXTEXT11,
134 index-text tests.**

## The measurement that chose the design

`p69` delta-varint encoded the postings and roughly halved the file. Before choosing the next
codec, the posting section of a real presyo build (241,793 products, 359,257 terms, 12.8 MB of
postings = 51 % of the file) was decoded list by list:

| component | bytes | share |
|---|---|---|
| list count prefixes | 361,376 | 3 % |
| document-id deltas | 3,593,791 | 30 % |
| term frequencies | 8,860,692 | **67 %** |

**Two thirds of posting bytes are term frequencies, and only 1.19 of 4 frequency slots per posting
are nonzero.** Every zero still cost a whole varint byte — 27 % of the posting section was literal
zeros. The list-length distribution decides the rest of the design: the **median list holds ONE
posting** (distinctive terms dominate the vocabulary), while **lists longer than 64 postings carry
72 % of posting bytes** (common terms: brands, units, category words). So:

- **Short lists (< 64 postings) — sparse varint.** Per posting: document-id delta, one **mask
  byte** naming the nonzero frequency slots, then a varint per set bit. Block framing on a
  one-posting list costs more than it compresses.
- **Long lists (≥ 64 postings) — block-FOR columnar.** Five columns (document-id deltas, then one
  frequency column per field), each cut into 128-value blocks: one width byte per block, values
  packed LSB-first; a whole-column of zeros is a single `0xFF` kind byte. A frame of reference
  pays one byte where a varint pays two or three, and an outlier widens only its own block.

The mode is chosen by the list's own count, which both sides read first — there is no mode byte to
disagree about. Term-id → byte-range addressing (`posting_span`) is unchanged, so the range tier
and the OPFS browser tier still fetch one list and decode it standalone.

## Measured, interleaved on one machine

`presyo-catalog`, 241,677 real products, two alternating passes per arm:

| | baseline (IDXTEXT10) | **p88 (IDXTEXT11)** |
|---|---|---|
| index bytes/doc | 57.3 | **35.8 (−37.5 %)** |
| exact p50 / p99 | 158–169 / 1,073–1,132 µs | 162–178 / 1,130–1,381 µs |
| typo p50 / p99 | 660–686 / 5,055–5,133 µs | 676–688 / 5,084–5,239 µs |
| precision@10, all rows | identical | **identical** |

`scale`, 61,467 real schools extended to 1 M:

| documents | before B/doc | **after B/doc** | |
|---|---|---|---|
| 5,000 | 105.2 | **66.7** | real |
| 61,467 | 88.8 | **45.5** | real |
| 250,000 | 81.1 | **35.9** | recombined |
| 1,000,000 | 79.3 | **32.4** | recombined |

**Space is unambiguous and deterministic; latency is at parity** — inside the harness's own
run-to-run noise on every arm, exactly as `bench/README.md` warns for p99 figures. The recombined
ladder compresses more than the real corpora because recombination repeats rows, which lengthens
posting lists and widens the columnar arm's share. Build time is unchanged (2,985 ms vs 3,016 ms
at 1 M). The 5 ms typo bar stays failed at 1 M by design — `p83`'s decision is untouched; nothing
here claims the tail.

## The bug the in-memory benches could not see

The first draft had **two** encoder/reader disagreements, and both survived every search-side bench
because **the benches search the in-memory index and never reload the bytes**:

1. The writer's columnar document-id column wrote absolute ids where the reader accumulated gaps.
2. The writer emitted the five columns **block-major** (block 0: all columns, block 1: all
   columns) while the reader walked them **column-major** (all of column 0, then column 1). The
   two layouts agree on every single-block list (≤ 128 postings) and diverge on the first
   multi-block one.

What caught them was `scale`'s standing check — *"serialized index reloads identically at every
scale"* — at the 20,000-row step, the first build whose longest list exceeded one block. The
presyo-catalog A/B, with 97.0 % precision reproduced exactly, was blind to it. The lesson is the
`p22` lesson one layer down: **a bench that never crosses the serialization boundary cannot vouch
for the serialization**; the reload check is load-bearing and stays in the gate. The regression
test `posting_round_trip_at_both_mode_boundaries_and_across_blocks` now round-trips 63/64/129/300
posting lists — the mode boundary and two multi-block shapes — and
`columnar_posting_corruption_is_rejected_loudly` pins the new failure modes (unknown column kind,
33-bit block, truncated payload, count below the columnar floor).

One incidental find: the old count sanity bound (`count ≤ bytes remaining`) rejected legitimate
columnar lists, because 64 ascending deltas pack into 8 bytes — an order of magnitude under the
old one-byte-per-posting floor. It is now mode-aware.

## What it costs and what it does not claim

- The 5 ms typo bar still fails at 1 M (14.07 ms, recombined) and still passes to ~250 K on real
  corpora. No tail claim is made.
- `docID reordering` and SIMD block decode remain unbuilt; this is the postings-compression row of
  the README's honest-limits list only.
- Old files are refused loudly: an `IDXTXT10` magic now names itself as an older version to
  rebuild, via `KNOWN_MAGIC`.
