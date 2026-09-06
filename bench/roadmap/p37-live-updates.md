# P37 — incremental updates, reachable at last (and a correction to my own answer)

**Tier:** T1 · **Bins:** `js/smoke.mjs`, `host/python/index_ffi.py`, `searcher` unit tests
**Status: SHIPPED, 2026-09-06. ABI 6 → 7, 41 symbols. Live updates work from Rust, JavaScript and Python.**

## The correction that started this

Asked what was missing, I answered: *"No incremental updates. `Index` has no `add`. Once built it's
immutable."* **That was wrong.** `crates/index-text/src/searcher.rs` opens with:

> *Multi-segment search — adding documents without rebuilding.*

It has existed since before this session. I audited `Index`'s public API and never looked at
`Searcher`'s, then reported the absence as fact. The lesson is the one `p33` already paid for:
**check the repository's own record before describing it.**

But the audit that followed found something worse than what I'd claimed.

## The real gap: two capabilities that could not be used together

`Searcher` exposed exactly two query methods — `search` and `search_prefix`. Everything built in
`p30`–`p32` (facets, conjunctions, ranges, histograms, sort) lived only on `Index`.

**So an application could have live updates OR shopping filters, and not both.** Nobody chose that
tradeoff; it is just where the seam stopped when new features were added to the wrong side of it.

And at the host boundary it was worse still: **`Searcher` was not in the C ABI at all.** Zero
mentions. From JavaScript, Python, Go or a browser, incremental updates were unreachable — every
host had to rebuild the whole corpus to add one row. The engine's most product-critical property
was Rust-only.

## What was added

| | |
|---|---|
| `Searcher::search_facet_all` / `_facet` / `_facet_at` | conjunctive faceted search across segments |
| `Searcher::search_range` / `search_filtered` | the whole filter bar |
| `Searcher::search_sorted` / `_sorted_filtered` | sort by value across segments |
| `Searcher::facet_tally_at` / `range_tally` | counts across segments |
| `Searcher::facet_of_at` / `numeric_of` / `facet_label_at` | per-document lookup by global ordinal |
| `Searcher::facet_config_is_uniform` | a check for mismatched segments |
| **15 new C ABI symbols** | `idx_searcher_new/push/close/search/search_facet_all/search_range/search_sorted/facet_tally/delete/doc_count/segment_count/live_count/needs_compaction/result_ptr/result_len` |

## The subtle bug this design invites, and the test that catches it

**Facet labels are interned per segment.** Segment 0 might see "Colgate" first and give it id 0;
segment 1 sees "Aquafresh" first and gives *that* id 0. The integers are not comparable across
segments.

A tally merged on the id would add unrelated categories together and report a **plausible number** —
the worst failure mode available, because nothing looks wrong. So `facet_tally_at` merges on the
**string**, and `search_facet_all` resolves the value inside each segment independently.

`facet_tally_merges_by_value_not_by_interned_id` builds exactly that arrangement, asserts the two
segments really do disagree about ids, and then checks the merged counts. A segment that has never
seen a value contributes nothing rather than erroring — a value can legitimately exist only in the
newest delta.

Also tested: `k=1` on a cross-segment sort returns the **global** cheapest, not the first segment's
cheapest; global ordinals survive an append (a document id handed out before the append still
resolves); and `facet_config_is_uniform` detects a segment whose slot 0 is a different field, which
would otherwise make `search_facet_at(.., 0, ..)` mean two different questions at once.

## Proven end to end

Rust: 4 new unit tests (115 total). JavaScript and Python each run the same scenario through the
real ABI — build, append a segment, typo-search across both, tally by value, filter, delete:

```
PASS  a new segment is appended without rebuilding
PASS  ordinals continue across the append
PASS  a typo query finds a row added after the build, at its global ordinal
PASS  the tally merges by value across segments (2 and 2), not by interned id
PASS  a conjunctive facet filter spans segments
PASS  live_count drops after a delete
```

The Python host gained `Live`, and with it `Index._take()` — handing the raw pointer to something
that **consumes** it and zeroing the wrapper. Without that the Python object would still close a
handle the searcher now owns: a double free, and the one memory bug a `ctypes` host can still write.

## Honest limits

- **Per-segment statistics.** IDF and length norms are per segment, so a term rare overall but
  common inside a small delta scores differently there. `p38` quantifies it: the disagreements
  are adjacent swaps between documents whose scores differ in the third decimal. `searcher.rs` documented this before today;
  it is the standard cost of segmented search and Lucene has it too. It means scores are *comparable
  enough to rank*, not identical to a full rebuild.
- **Compaction is a rebuild, not a merge.** `needs_compaction()` tells an application when, but the
  application supplies the rows. Nothing here merges two segments into one.
- **No automatic segmentation.** A host decides when to cut a new segment; there is no policy, no
  size trigger, no background thread.
- **`facet_config_is_uniform` is advisory.** `push` accepts a mismatched segment — it is just an
  `Index` — so this is a check to assert once after loading, not an enforced invariant.
- ~~**Not measured at scale.**~~ **Measured in `p38-segment-scale.md`**: on 241,789 real products,
  p50 goes 10 us to 2,326 us from 1 to 50 segments (232x) while rank-1 agreement holds at
  96–98 %. Latency is the cost; ranking is not. Keep the count in single digits.

## Reproduce

```sh
cargo test -p index-text searcher
node js/smoke.mjs
python host/python/index_ffi.py
```
