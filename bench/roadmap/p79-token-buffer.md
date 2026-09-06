# P79 — the allocation was jointly owned, which is why neither half alone moved it

**Tier:** T1 · **Bin:** `scale` · **Files:** `crates/index-text/src/analyze.rs`, `index.rs`
**Status: SHIPPED, 2026-09-06. Analyzer 4,240 -> 2,704 ms (1.57x). Build ~1.25x. Bytes identical.
318 tests.**

`p75` closed by naming this: *"`Vec<Token>` is now the largest remaining item — 3 M vec allocations,
13.69 M 32-byte pushes and `String` drops."* It listed three plausible shapes. **The measurement
picked none of them, because the item was not what its name said.**

## The profile, and the row that redirected the lane

Cumulative stages over `scale.rs`'s 1 M recombination — 3,000,000 field strings, 85,989,866 bytes,
13.69 M tokens — best of five, all stages interleaved in one process so contention hits them equally.

| stage | ms | marginal |
|---|---|---|
| S0 field scan floor | 4 | — |
| S1 `tokenize()` -> `Vec<Token>`, consumed | 3,359 | +3,355 |
| S2 = S1 + `apply_alias` (the analyzer as `add()` uses it) | **4,240** | +881 |
| S3 = reused buffer + `apply_alias` | **2,704** | **-1,536** |
| S4 = same, but `truncate(0)` per field (vec reused, token strings freed) | 4,060 | -180 |

Checksums for S2, S3 and S4 are identical (`de32e39fc3b7ee0c`) — the token stream did not move.

**S4 is the whole finding.** Reusing the `Vec` and freeing the strings is worth **180 ms — 12 % of
the item.** The other **1,356 ms is the 13.69 M malloc/free pairs**, which no amount of vector reuse
touches.

## The thing that nearly killed the lane

Three binaries built from one tree, run back to back, 250,000 documents, best of four:

| variant | build ms |
|---|---|
| before | 1,754 |
| **buffer reuse only** (the old cloning map insert kept) | **1,786** |
| buffer reuse + `get_mut`-first | **1,340** |

> **The buffer alone measured 32 ms SLOWER in the real build.**

The old `entry(t.text)` **moved** the token's string into the map, so the free belonged to the map,
not to a leak: one malloc in the analyzer, one free in the map. Reusing the buffer without changing
the map merely relocates the malloc into the clone.

**Either half alone removes one end of a pair and therefore removes nothing.** Only both together
delete the allocation — and that is why this lane had to own `index.rs` as well as `analyze.rs`.
*A profile attributes cost to a line; it does not tell you the cost is jointly owned by two.*

## What changed

- **`tokenize_into(text, &mut Vec<Token>) -> usize`** fills a caller-owned buffer and returns the
  count. It deliberately **does not truncate**: slots past the count are retained scratch, so each
  token's string keeps its heap allocation and is refilled with `clear` + `push_str`.
- **`Quantity::write_token(&mut String)`** so a quantity rewrite lands in the buffer;
  `merge_split_quantity` pushes the removed half to the far end of the buffer rather than dropping
  it, keeping that allocation available too.
- **`IndexBuilder` gained `tok_buf`**, `mem::take`n for the duration of `add()`; `term_post` and
  `term_pos` are looked up by `&str` via `get_mut` first, falling back to the cloning insert only
  for a genuinely new term.
- **`tokenize` is unchanged as API** — a three-line wrapper over `tokenize_into` + `truncate`.
  Nothing outside `add()` was touched: `tokenize_span`, `fold_with_origin`, `query.rs`,
  `real_corpus.rs`, `index-wasm` and the highlighter all still call the same signature. That is also
  what let `p75`'s four `mod legacy` equivalence tests keep passing **unmodified, with no adapter**.

## Measured

**Analyzer: 4,240 -> 2,704 ms (1.57x).** That is the reliable figure, because its halves run
interleaved inside one process.

Build, paired same-binary alternation, bytes identical in every run:

| documents | pairs | median speedup | bytes |
|---|---|---|---|
| 61,467 (real) | 4 | **1.27x** | identical |
| 250,000 | 4 | **1.26x** | identical |
| 1,000,000 (`bin/scale`) | 4 | **1.22x** | identical |
| 1,000,000 (build-only harness) | 6 | **1.26x** | identical |

**The box was contended by three concurrent lanes throughout**, so absolute figures are inflated
~35–100 % on both sides — the `before` binary is `p75`'s shipped code and measured 7,434–11,256 ms at
1 M against `p75`'s reported 5,478. **A median ratio over 20 pairs is reported rather than a headline
pair**, and outliers are named rather than dropped: one 61,467 pair came out at 0.99x, one 1 M pair
at 0.97x, and one build-only pair at 2.06x — all taken while the box was visibly loaded, none usable.

## What did not work

- **Reusing the `Vec<Token>` and only that** — `p75`'s second listed shape, and exactly what a
  callback form would also have delivered. **1,786 ms against a 1,754 ms baseline: no win, inside
  noise, arguably negative.**
- **A borrowing token** — `p75`'s third shape. Dropped once S4 showed the vector was 12 % of the item
  and the three-way showed the cost was jointly owned with the map: borrowing removes the same 180 ms
  the buffer does, and still leaves `term_post` allocating per token unless the map lookup changes
  too. The buffer gets the whole win with **no lifetime at the `add()` call site and no change to
  `tokenize`'s signature**.
- **Trusting a `bin/scale` pair taken across a rebuild.** The first three pairs stashed and
  recompiled between halves, putting them 5+ minutes apart, and gave **1.46x, 1.05x and 1.14x for
  the same code**. Building both binaries once and alternating them halved the spread. Recorded
  because it is a methodology trap this repo will hit again.

## Still open

- **The token text is still copied twice on a miss** — once into the buffer slot, once into the map
  key. Only ~41 K misses on this corpus, so it is not worth chasing here.
- **The tokenizer is still single-threaded**, for `p73`'s and `p75`'s unchanged reason.
- **Not independently reproduced on an idle box.** Joins `p73`, `p74` and `p75`.
