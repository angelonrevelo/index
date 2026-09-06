# P56 — an image document is column-hungrier than the engine allows

**Tier:** T2 · **Bin:** `image-corpus` · **API:** `Schema`, `MAX_FIELD`
**Status: SHIPPED, 2026-09-06. Format `IDXTEXT7` -> `IDXTEXT8`. No ABI change.
`index-text` 106 -> 112 tests; workspace 261 -> 267, 0 failing.**

`Schema::with_column(name)` declares an **unscored column**. Facet, numeric and key declarations now
take a *column* index rather than a scored-field index, so a column that is never scored no longer
occupies a scoring slot. `MAX_FIELD` stayed at **4** and the per-posting `[u16; MAX_FIELD]` did not
move — asserted directly by `format::tests::unscored_columns_cost_no_posting_byte`, which serializes
the same corpus with and without five unscored columns and compares the posting section **byte for
byte**, not merely by size. `with_facet(field)` / `with_numeric(field)` are untouched and still take
a scored-field index; `p65` adds a road, it does not close one.

Verified against the shipping consumers rather than asserted: `real-corpus` and `facet-shop` report
identical numbers before and after — same dictionary bytes (29896 / 6789), same accuracy to the
decimal, same 19793 brands / 146 categories, same index growth (`+4240433 B`, 17.55 B/doc), and 0
wrong across every correctness check. Only wall-clock latency moved, and the machine was running
other benchmarks concurrently.

**Acceptance item 3 is now DONE.** `p60`'s schema has been restored to its natural **seven columns**
— `path` as the single scored text field, plus `format`, `shape` and `colour` faceted and `width`,
`height` and `byte` ranged, all as unscored columns. The full corpus run reports
`range(width >= 200)` and `range(height >= 200)` as hard predicates in its fused query set, which is
the concrete capability the four-field packing had cost, and the run is **8 / 8 PASS**.

The original text of this item is kept below because the reasoning is the point: The engine
support the row asked for is in place and exercised by
`index::tests::a_seven_column_schema_fits_in_four_scored_fields`, which builds exactly that
one-scored-field / six-unscored-column image schema and facets and ranges over it.

This row was not planned. It was discovered by running `p60` against the real corpus, which is the
only reason it exists and the best argument for the benchmark that found it.

## What happened

`index-text` caps a schema at **four fields**:

```rust
pub const MAX_FIELD: usize = 4;
assert!(!field.is_empty() && field.len() <= MAX_FIELD, "1..={MAX_FIELD} fields");
```

The natural schema for a scraped image wants **seven**: `path` (the only real text a CDN asset
carries), `format`, `shape` and `colour` to facet on, and `width`, `height` and `byte` as numeric
columns. The benchmark asserted its way straight into the cap.

## Why the cap exists, and why raising it is not free

`MAX_FIELD` is not an arbitrary limit. It is the width of the per-document field-length array
carried on **every posting**:

```rust
term_post: BTreeMap<String, BTreeMap<u32, [u16; MAX_FIELD]>>,
```

That array is what `p14` bought — exact `u16` field lengths, the thing that makes this scorer beat
Tantivy's one-byte fieldnorm on short product titles. Doubling `MAX_FIELD` to 8 doubles it, on every
posting, for **every existing consumer**: presyo's 241,677 products, sisia's catalogue,
profstopick's names. **Taxing four shipping consumers to serve one new corpus is the wrong trade,
and doing it silently would be worse.**

So `p60` was fitted into four fields instead, and the limit was recorded rather than removed. That
is the honest short-term answer and it is not the right long-term one.

## The actual finding

> **A text document and an image document want different numbers of columns, and the engine's
> field budget was sized for the first.**

A product has a brand, a title, maybe a category. An image has a path, a format, a shape, a palette,
two dimensions, a byte length, a capture date, a camera, and a GPS pair — and most of those are
*facet and numeric* columns that only need a field because facets and numerics are currently
declared **by field index** (`with_facet(field)`, `with_numeric(field)`).

That is the real design question this row opens, and it is more interesting than the constant:

> **Should a facet or a numeric column require a scored text field at all?**

Today `Field::new("width", 0.0, 0.75)` declares a field with **boost 0.0** — a field that exists
only to be faceted on and is deliberately never scored. Every one of those consumes a `MAX_FIELD`
slot and a `u16` on every posting, to carry a length nobody reads. That is the waste, not the
constant.

## The options, with their costs

| Option | Cost | Verdict |
|---|---|---|
| Raise `MAX_FIELD` to 8 | +2 bytes/posting/document for every existing consumer | Rejected as a first move — taxes four shipping apps for one corpus |
| Decouple facet/numeric columns from text fields entirely | An API change and a format change (`IDXTEXT`n bump) | **The right answer.** A column that is never scored should not occupy a scoring slot |
| Pack several values into one field's text | Free, but reintroduces the string-parsing `p31` exists to remove | Rejected — it is the bug that row fixed |
| Leave the cap and fit inside it | Free | What `p60` does today, recorded as a limitation |

## Acceptance

1. A schema can declare a facet or numeric column **without** spending a scored-field slot.
2. `[u16; MAX_FIELD]` per posting does **not** grow for a consumer that adds only unscored columns.
   Measured in bytes/document against presyo's 241,677-product baseline: the regression must be
   **zero**, not merely small.
3. `p60`'s schema is restored to its natural seven columns, and the census reports the same numbers
   it reports today under the four-field workaround.
4. Existing consumers' results are bit-identical before and after. This is a refactor of where a
   column lives, not a change to what anything scores — if a single presyo result moves, the change
   is wrong.
