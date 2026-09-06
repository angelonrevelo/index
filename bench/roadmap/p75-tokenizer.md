# P75 — the tokenizer, and two suspects the measurement killed

**Tier:** T1 · **Bin:** `scale` · **Check:** `fast_tokenizer_matches_the_legacy_one`
**Files:** `crates/index-text/src/analyze.rs` (only)
**Status: SHIPPED, 2026-09-06. Analyzer 6,757 -> 3,642 ms (1.86x); 1 M build 9,666 -> 5,478 ms.
Token stream provably unchanged. 292 tests.**

`p73` ended by naming this: *"Tokenization is now the largest single item — 6.9 s of the remaining
9.7 s at 1 M — and it is the next lane, worth more than this one was."*

## The profile, before anything changed

A standalone harness reconstructed `scale.rs`'s exact 1 M-document recombination — 3,000,000 field
strings, 86 MB, **13.69 M tokens** — and timed *cumulative* stages through the public API. Its
full-stack number, 6,757 ms, reproduces `p73`'s 6,659–6,900 ms, so it is measuring what `add()` pays.

| stage (cumulative) | ms | marginal |
|---|---|---|
| S0 char scan floor | 799 | — |
| S1 `fold()` | 1,851 | **+1,052** — a `String` per field, `char::to_lowercase` per char |
| S2 + `folded.chars().collect::<Vec<char>>()` | 2,952 | **+1,101** — a second whole-field alloc, purely to index ±1 |
| S3 + split loop growing a `String` per token | 4,714 | **+1,762** |
| S4 + `parse_quantity` per token | 4,239 | ~0 (inside S3's noise) |
| S5 = `tokenize()` | 6,106 | +1,392 |
| S6 = `tokenize` + `apply_alias` | 6,757 | **+651** — a hash of every non-numeric token |

## The two suspects the brief named, and the numbers that killed them

**"Skip re-lowercasing text that is already lowercase."** The corpus is **97.7 % ASCII, but only
3,270 bytes of 85,989,866 are already lowercase.** The optimization is worth **0.004 %** here, and
would cost a scan to discover it. Dead on arrival — measured, not argued.

**"Avoid a heap allocation per token."** Isolated directly: fold-into-reused-buffer plus a byte scan
with **no** `String` per token measures 733 ms; adding one `to_owned()` for all 13.69 M tokens
measures 932 ms. **The per-token allocation is ~170 ms — 2.5 % of the total.** The leading suspect
was not the cost.

**The cost was the two whole-field allocations and the char-at-a-time scanning around them**, which
is where the work went instead. *A profile is worth more than a plausible mechanism, including one
this document's own brief asserted.*

## What changed

1. **`fold_into(s, &mut String)`, byte-wise with an ASCII fast path.** No ASCII char carries a
   diacritic and none case-maps outside ASCII, so a maximal ASCII run is `push_str` +
   `make_ascii_lowercase` — a memcpy and a vectorized pass. **1,851 -> 655 ms.**
2. **`tokenize` folds into a thread-local reused buffer**, so 3 M per-field allocations become zero.
3. **The `Vec<char>` is deleted.** Token text is a contiguous **byte range** of the folded string.
   That is structural, not incidental: the only rewrite the split performs is a kept separator
   becoming `.`, and `,`/`-`/`.` are all one byte, so it is length-preserving. The two lookarounds
   that needed the `Vec<char>` are both `is_ascii_digit`, which **no multi-byte character can
   satisfy**, so a byte index answers them exactly. Non-ASCII bytes still decode a `char` and use the
   Unicode `is_alphanumeric` tables. Kills S2's +1,101 ms.
4. Each token is **one exact-capacity copy** of a slice, instead of a `String` grown a character at a
   time (which reallocated 3–4 times for a 12-character token).
5. `parse_quantity` early-out: a token whose first byte is not an ASCII digit can never parse — an
   exact restatement of checks already inside the function, hoisted above a `find` and a `replace`.
6. `apply_alias` gained a **length gate** — `AliasTable` tracks its longest folded surface form
   (10 bytes for the starter table), so any longer token skips the hash entirely. **651 -> ~400 ms.**

## Measured

Analyzer, in the harness: **6,757 -> 3,642 ms (1.86x)**. That is near its floor — the same harness
measures **2,588 ms** for fold + byte scan + one `to_owned` + `Vec<Token>` push and nothing else.

`bin/scale`, paired on the same tree, best uncontended pair:

| documents | before | after | speedup | bytes |
|---|---|---|---|---|
| 5,000 | 60 ms | 33 ms | 1.82x | identical |
| 20,000 | 212 ms | 124 ms | 1.71x | identical |
| 61,467 (real) | 496 ms | 434 ms | 1.14x | identical |
| 250,000 | 2,704 ms | 1,607 ms | 1.68x | identical |
| **1,000,000** | **9,666 ms** | **5,478 ms** | **1.76x** | **identical** |

**With `p73`, a million-document build has gone 18,436 -> ~5,478 ms — 3.4x — with the serialized
bytes unchanged throughout.**

A third pair taken while the box was contended gave 9,665 -> 6,872 (1.41x) with *every* rung inflated
on both sides. Reported for honesty, not used.

## Proof the token stream did not move

A change to tokenization silently changes every ranking in the engine, so this is the constraint that
outranks the speed.

- **`fast_tokenizer_matches_the_legacy_one`** — the pre-change `fold`, `parse_quantity`,
  `merge_split_quantity` and `tokenize` are copied **verbatim** into `mod legacy` and asserted equal
  on `text`, `position` and `is_numeric` over 54 cases: empty and whitespace-only fields, every
  separator class, precomposed **and** combining accents, `ß`, `İ`, `ẞ`, `ﬀ`, `ǅ`, Greek, Cyrillic,
  CJK, Hangul, Arabic, Devanagari, emoji with regional indicators, fullwidth digits, a Roman numeral,
  a vulgar fraction, NBSP, tabs/CR/LF, quantities that must and must not parse, a
  **100,000-character mixed-script field**, and **every ASCII byte 0..127**.
- Companions: `fast_fold_matches_the_legacy_fold` (including prefixes cut at 1/2/3/7/13/64, so an
  ASCII-run boundary cannot be observable), `quantity_parsing_matches_the_legacy_one`,
  `alias_application_matches_the_legacy_one`.
- **`pool-audit` 6/6 zero cells**, and **byte-identical serialized indexes at all five `scale`
  rungs**, both re-run here on the real tree.

## Still open

- **`Vec<Token>` is now the largest remaining item** — +1,656 ms: 3 M vec allocations, 13.69 M
  32-byte pushes and `String` drops. Fixing it means changing `tokenize`'s **return type**, which is
  API surface in `index.rs`, so it was out of this lane's ownership. **That is the next lane here.**
- **The tokenizer is still single-threaded**, for `p73`'s stated reason: threading it changes
  `add()`'s contract.
- **The speedup is not independently reproduced.** Correctness was fully verified here; the timing
  table is the lane's, because three other agents held cores during the merge. It joins `p74` and
  `docs/benchmarks.md` in wanting one re-run on an idle box.
