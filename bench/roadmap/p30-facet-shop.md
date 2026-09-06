# P30 — faceting: the interaction shopping search actually is

**Tier:** T1 · **Bin:** `facet-shop` · **API:** `with_facet` / `search_facet` / `facet_tally`
**Status: SHIPPED, 2026-09-06. `OVERALL: PASS` on presyo's 241,677 real products, two facet slots.**

The goal names shopping as the most important case. Every bench before this one asks *"is the
top-10 right"* — but shopping search is not only ranking, it is **filter and count**. A shopper
types "milk", sees `Dairy (412)  Snacks (37)  Beverage (18)`, clicks one, and expects a full page
from inside that category.

**The engine had no facet capability at all.** `facet` appeared 25 times in `index.rs` and meant
only "a field used to *learn query expansions*". Nothing stored a value, filtered on one, or counted
one. That was the largest missing piece of the stated goal, and it was missing silently.

## What was added

| | |
|---|---|
| `IndexBuilder::with_facet(field)` | Store a field verbatim as a facet. Also `set_facet_field`, non-consuming, for the ABI. |
| `Index::search_facet_all(q, k, &[(slot, value)])` | **Conjunctive** filter-then-rank: brand AND category, one pass. |
| `search_facet` / `search_facet_at` | Sugar for slot 0, and for one slot. |
| `Index::facet_tally_at(q, slot)` | Counts over **every** matching document. |
| `facet_label_at` / `facet_of_at` / `facet_slot_count` / `facet_field` | The values, and the slot -> field mapping. |
| C ABI | `idx_build_facet`, `idx_search_facet`, `idx_search_facet_all`, `idx_facet_tally`, `idx_facet_count`, `idx_facet_slot_count`. ABI 2 -> **4**. |
| Format | `IDXTEXT2` -> **`IDXTEXT4`**, 11 -> 13 spans. |

**Multiple facet fields, because one is not shopping.** Call `with_facet` once per field; call
order is *slot* order, and the query API takes the slot rather than the schema field index. A
conjunction is one pass over the candidate set, not an intersection of separate result sets.

The ABI spells a conjunction as a NUL-separated spec, `0=Colgate\01=Accessory` -- slot
decimal, value everything after the **first** `=`, so a value may contain `=`. A malformed spec
returns `u32::MAX`; an unsatisfiable one returns 0. **Never everything**, which is the dangerous
reading of a filter and the one a host would not notice.

**Facet values are deliberately not tokenized.** `"Lucky Me"` is one value, not two terms; a shopper
filtering by brand wants the brand, and tokenizing would make `"Lucky Me"` and `"Me Lucky"` the same
facet. This is the one place the engine stores field text rather than an analysis of it. Values are
interned, so the cost is the field's cardinality plus one `u32` per document — 241,677 products over
146 categories store 146 strings.

**Filtering happens after scoring and before admission.** That is what keeps pruning sound: the
thresholds are then derived from allowed documents only, and a block-max bound over all documents is
still an upper bound over the allowed subset. Filtering earlier — skipping the posting outright —
would be faster and would break `first_essential`'s accounting, which assumes every enumerated
document is scored.

## Measured on presyo's real catalogue

241,677 products, 146 categories, 336 probe queries.

| | |
|---|---|
| slots | **19,793 brands** and **146 categories** |
| index size | 28.2 MB -> 30.5 MB, **+9.55 B/doc** for both slots (+8.2 %) |
| build | 3.1 s -> 3.6 s |
| **tally correctness** | **40 / 40 queries exact** against exhaustive scoring |
| **filter-then-rank** | 104 (query, smallest category with >=10 matches) pairs: **0 short pages, 0 leaks** |
| **conjunction** | 80 (brand, category) pairs vs independently intersected single-slot results: **0 wrong** |

| latency, 336 queries | p50 | p99 |
|---|---|---|
| `search` (unfiltered) | 10 us | 331 us |
| `search_facet` | 7 us | 440 us |
| **`facet_tally`** | **12 us** | **88 us** |

**A full tally over 241,677 products costs 88 us at p99** — it can run on every keystroke.
`search_facet` is *faster* at p50 (fewer documents reach `admit`) and slower at p99 (a selective
filter fills the pool slowly, so the threshold stays low and less is pruned). Both are expected.

The filter-then-rank row is the one that matters most. It is built to catch **rank-then-filter**,
where asking for 10 results in a small category returns 1 because only 1 of the global top-10 was in
it — the standard failure of bolting a filter onto a search engine from outside. Zero short pages
across 104 deliberately-hardest cases.

## Two defects found by the tests written for this

**1. `Reader::need` could panic on corrupt input.** It computed `self.p + n`; a corrupt `u64` length
prefix arrives as a near-`usize::MAX` `n` and the addition **overflows**, panicking — in a format
whose contract is that it never does. `expansion` had the same latent path; the facet sections just
gave `corrupt_input_errors_rather_than_panics` a second length-prefixed section to reach. Now
`checked_add`, and both sections bound the claimed count by what the span can physically hold before
allocating for it.

**2. `idx_build_facet` trapped on its first draft.** `with_facet` consumes the builder, so the ABI
version swapped through a placeholder `IndexBuilder::new(Schema::new(vec![]))` — and `Schema::new`
asserts a non-empty field list. A panic in a WASM export kills the whole instance. Fixed by adding
the non-consuming `set_facet_field`, which returns `false` instead of asserting. Caught by
`the_abi_facets`, written in the same commit.

**The `TABLE_BYTE` assertion did its job.** `every_offset_in_the_table_is_u64` asserts the section
table is exactly 176 bytes; it failed on this change, and that failure is what forced the magic bump
to `IDXTEXT3`. Without it the format would have grown silently and an old reader would have
mis-parsed two facet offsets as posting data — garbage results rather than an error.

## Honest limits

- **The tally oracle shares `plan()` with the tally.** It compares `facet_tally` against
  `search_exhaustive`, which is a different counting path but the *same* query planner. It validates
  the counting, not the planning. `p11` recorded the general form of this: two implementations that
  share an input parser agree about the parser.
- ~~**No range facets.**~~ **Closed by `p31-numeric-range.md`** — numeric columns, half-open range
  filters and histograms. The limit as written ("price buckets need numeric values, which the engine
  does not store") was correct when written and is no longer true.
- ~~**Conjunction only.**~~ **Closed by `p43-clauses-paging.md`** — `FacetClause` adds OR within
  a clause and NOT, with the unknown-value rules asserted in both directions.
- **The conjunction check is one-directional.** It verifies the one-pass result is a *superset*
  of a single-slot top-50 filtered by the other slot, which is the direction that can be wrong.
  The reverse cannot be checked that way: the one-pass result legitimately contains documents
  ranked below 50 on either slot alone.

## Reproduce

```sh
cargo run -p index-bench --release --bin facet-shop
node js/smoke.mjs                  # facet checks included
python host/python/index_ffi.py    # same checks, native tier
```
