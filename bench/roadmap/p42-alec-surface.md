# P42 — long documents, and the total-order bug they exposed in ten seconds

**Tier:** T1 · **Bin:** `alec-surface` (new) · **Corpus:** `alec/expressway_dump`, 139 MB, read not committed
**Status: PASS, 2026-09-06. Found and fixed a comparator that was not a total order.**

Every corpus this engine had been measured on is **short-field**: product names (241 k × ~6 words),
place names, business names, course titles, table names. `alec`'s web-surface scrape is the opposite
and the largest single data file in the house — **119,179 rows with a fixed 1,000 characters of raw
HTML context each, 123.6 MB of text.**

That shape matters for things nothing else here tested: BM25 length normalisation two orders of
magnitude out, and the pruning gates of `p24`–`p26`, whose bounds were designed and audited on short
fields.

## It broke on the first run

```
thread 'main' panicked at library/core/src/slice/sort/shared/smallsort.rs:860:5:
user-provided comparison function does not correctly implement a total order
```

`rank_cmp` — **the single ranking comparator, used by both the pruned and the exhaustive path** —
asked:

```rust
if (a.score - b.score).abs() <= SCORE_TIE_REL * scale { Ordering::Equal } else { ... }
```

A tolerance is **not an equivalence relation**. `a ~ b` and `b ~ c` does not give `a ~ c`, so the
derived ordering is not transitive, and `sort_by` is entitled to panic on it — which Rust's sort now
detects and does.

The intent was right and is documented in the code: `f32` addition is not associative, so the pruned
and exhaustive paths can compute microscopically different scores for the same document and the
order must not depend on which path ran. **The mechanism was wrong.**

**Short fields never exposed it.** A tolerance comparator only misbehaves when one result set holds
enough scores spaced *finer than the tolerance* for the intransitive chain to matter. Six-word
product names do not produce that. A thousand characters of shared HTML boilerplate produce it
immediately — 119 k documents share a vocabulary of just **7,459 terms**.

## The fix: quantize, do not compare differences

```rust
fn canon_score(x: f32) -> f32 {
    let half = 1 << (SCORE_TIE_BITS - 1);
    let mask = !((1 << SCORE_TIE_BITS) - 1);
    f32::from_bits(x.to_bits().wrapping_add(half) & mask)
}
```

Rounding each score onto a grid makes the comparison a **pure function of one value**, so
transitivity holds by construction. `rank_cmp` becomes plain lexicographic ordering on
`(bucket, quantized score desc, doc)` — three total orders chained, which is one.

The grid is three mantissa bits, a relative step of `2^-20 ≈ 9.5e-7`, matching the `1e-6` tolerance
it replaced. Rounding is to nearest rather than truncating, which halves how often two scores meant
to tie land either side of a boundary. `SCORE_TIE_REL` is kept — nothing compares against it now,
but it is the only statement of how coarse the grid should be, and a test asserts they still agree.

**Two tests were added, and the first is the one that should have existed all along:**
`rank_cmp_is_a_total_order` checks antisymmetry and transitivity exhaustively over scores spaced one
ULP apart — precisely where a tolerance comparator fails — and then sorts them, which is what
panicked. `the_score_grid_matches_the_documented_tolerance` pins the grid to `SCORE_TIE_REL` and
checks the quantization is monotone, since a non-monotone grid could reorder two scores.

## Measured, after the fix

| | |
|---|---|
| corpus | 119,179 rows, **123.6 MB of text**, 1,000 chars each |
| vocabulary | **7,459 terms** — two orders below presyo's 117 k, on 40 % as many documents |
| build | 9.1 s |
| index | 141.4 MB — **114 % of the source text** |
| **exactness vs brute force** | **0 differences, 0 lost documents** |
| find a row by its value | **100 %** rank-1 |
| one typo | 86.3 % rank-1 |
| short query | p50 **365 us**, p99 4.0 ms |
| 8-word query from a body | p50 **12.0 ms**, p99 **18.5 ms** |

**The gates hold on this shape**, which was the real question — `p24`–`p26` were tuned on short
fields and this is the first evidence they generalise.

Two numbers deserve attention rather than celebration:

- **The index is larger than the text it indexes.** 141 MB for 123 MB. With a 7,459-term vocabulary
  over 119 k documents, posting lists are enormous and each posting carries a document id plus four
  `u16` field frequencies. This corpus is close to worst case for that layout.
- **A body query costs 12 ms at p50**, against 365 us for a short one — 33×. Eight common HTML terms
  each drag a posting list covering most of the corpus, and pruning cannot help when every candidate
  scores about the same. This is the regime `p29` described from the other direction.

## Honest limits

- **One dump of nine.** `alec` holds `aceit`, `anycase`, `biyahe`, `calculai`, `mrt`, `ooh`, `orki`
  and `suzzy` alongside `expressway`; only the largest is indexed.
- **The text is HTML, not prose.** Boilerplate-heavy and tag-dense, which is why the vocabulary is
  so small. A corpus of real long-form prose would stress length normalisation differently and is
  still untested.
- **`surface_type` has two values**, so faceting here is nearly trivial and proves little.
- **No labels.** "Right answer" means "the row the query text came from", which is a self-retrieval
  task, not a relevance judgement.
- **Index size not investigated.** 114 % is reported, not explained or optimised.

## Reproduce

```sh
cargo run -p index-bench --release --bin alec-surface
cargo test -p index-text rank_cmp_is_a_total_order
```
