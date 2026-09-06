# P43 — the rest of the filter bar, and paging

**Tier:** T1 · **API:** `FacetClause`, `Index::search_clause`, `Index::search_page`
**Status: SHIPPED, 2026-09-06. ABI 8 → 9, 44 symbols. 124 tests.**

Two gaps named in earlier documents, closed together because both change the same call path.

`p30`: *"Conjunction only. `A AND B` across slots. No OR, no exclusion, and no two values from one
slot (\"Colgate or Oral B\"), which real storefronts do offer."*

`p39`: *"No pagination. Top-`k` only, no offset. Page 5 means asking for `k=100` and slicing."*

## The clause model

```rust
pub struct FacetClause<'a> { pub slot: usize, pub value: &'a [&'a str], pub exclude: bool }
```

**OR within a clause, AND across clauses.** That is not an arbitrary choice — it is how every filter
bar behaves: ticking two brands *widens* the result, ticking a brand and a category *narrows* it.
`FacetClause::any` and `::none` are the two constructors.

## The rule that decides whether a filter is safe

An unknown value is ignored rather than fatal, because a filter bar built from one segment's labels
can legitimately name a value another segment has never seen. The two ends of that rule are
**deliberately opposite**:

| | |
|---|---|
| **include**, every value unknown | matches **nothing** — nobody can satisfy it |
| **exclude**, every value unknown | excludes **nothing** — there is nothing to remove |

Getting these the wrong way round is how a filter silently returns the entire corpus, which looks
exactly like a working search. Both are asserted, in Rust and again through the ABI from JavaScript.

The same asymmetry governs a document with **no value** in the slot: kept by an exclude, dropped by
an include — an unbranded row survives *"not Colgate"*, which is what a shopper means.

## Paging, and its honest cost

```rust
pub fn search_page(&self, query: &str, offset: usize, k: usize) -> Vec<Hit>
```

**Cost grows with `offset`, not with `k`.** The engine over-fetches `offset + k` and drops the
prefix, because rank order is only known once everything above the page has been scored. That is
true of every engine without a stored cursor; it is documented on the method rather than left to be
discovered in production. For deep paging, filter instead of paging.

`pages_partition_the_ranking_without_gaps_or_repeats` asserts the property that actually matters:
three pages of ten equal one request for thirty, **no document appears on two pages**, and past the
end is empty rather than wrapped.

## A refactor the lint asked for and the next feature will need

`search_opt` reached **eight positional parameters**, four of them slices that are usually empty —
`search_opt(q, k, false, MAX_EXPANSION, &[], &[], 0)` says nothing about what those defaults mean.
Replaced with a `Scan` struct, so each entry point states only what it changes:

```rust
self.search_opt(Scan { offset, facet: &filter, range, ..Scan::new(query, k) })
```

Clippy flagged it at 8/7 arguments; the right response was the struct rather than an `allow`,
because phrase support is next and would have made it nine.

## The ABI spelling

`idx_search_clause` takes a NUL-separated spec, one clause per entry, `slot[!]=v1|v2|...`:

```
0=Colgate|Oral B     slot 0 is Colgate OR Oral B
1!=discontinued      slot 1 is NOT discontinued
```

`|` separates alternatives, so **a facet value containing `|` cannot be expressed** — every other
byte can, including `=` after the first one. That limit is stated in the header rather than
discovered by a host with a pipe in its data.

## Verified

Rust: 3 new tests (124 total) covering OR, AND, NOT, NOT-with-OR, both unknown-value rules, unknown
slot, missing values, and page partitioning. JavaScript, through the real ABI:

```
PASS  OR within a clause widens
PASS  NOT excludes
PASS  an all-unknown include matches nothing
PASS  an all-unknown exclude excludes nothing
PASS  a malformed spec errors, never matches all
PASS  three pages of 1 equal one request for 3
PASS  past the end is empty, not wrapped
```

No regression: `pool-audit` 6/6 zero cells, `real-corpus`, `facet-shop`, `booted-schema` and
`alec-surface` all PASS.

## Honest limits

- ~~**`Searcher` has no clause or paging API yet.**~~ **Closed in the same change** —
  `Searcher::search_clause` and `search_page` exist, with `clauses_and_pages_span_segments`
  asserting that OR reaches into a second segment and that pages partition the *merged*
  ranking, where a segment's 3rd-best can be the page's 1st. Not exposed through the
  `idx_searcher_*` ABI yet, which is the seam that remains.
- **Deep paging is linear in `offset`** and nothing warns a caller at runtime — no cap, no error at
  page 10,000, just increasing latency.
- **No cursor/`search_after`.** The scalable alternative to offset paging is a resume token, which
  needs a stable total order over `(bucket, score, doc)` — which `p42` just established, so this is
  now buildable and is not built.
- **`|` is unescapable** in the ABI spec form. The Rust API has no such limit.
- **Not benchmarked.** Clause filtering costs one extra binary search per document over the old
  single-id compare; obviously small, and not measured.

## Reproduce

```sh
cargo test -p index-text facet_clauses
cargo test -p index-text pages_partition
node js/smoke.mjs
```
