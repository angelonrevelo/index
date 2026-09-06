# P44 — the filter bar reaches the searcher, and a host that was never gated

**Tier:** T1 · **API:** `idx_searcher_search_clause`, `idx_searcher_search_page`
**Status: SHIPPED, 2026-09-06. ABI 9 -> 10, 46 symbols. 126 tests.**

`p43` shipped `FacetClause` and paging, and named what it left behind:

> Still open: ... `idx_searcher_*` clause/paging exposure.

The Rust `Searcher` had `search_clause` and `search_page` from the day `p43` landed. Nothing across
the ABI did, so **the only surface with more than one segment was the only surface without a filter
bar** — precisely backwards, since a multi-segment collection is the case where a filter bar is
built from labels that not every segment has seen.

## The unknown-value rule is per segment, and that is what makes it work

`p43`'s rule has two deliberately opposite ends: an include whose values are all unknown matches
nothing, an exclude whose values are all unknown excludes nothing. Across segments that rule applies
**per segment**, which is the whole reason it is shaped this way:

| a value known only to segment B | segment A | segment B |
|---|---|---|
| **include** it | contributes nothing | filters normally |
| **exclude** it | removes nothing | filters normally |

Both directions are asserted from Rust, from JavaScript, and from Python, against a pair of segments
that intern the *same* label at *different* ids — because resolution by integer would silently mean
a different brand in each segment, which is a wrong answer that looks like a count.

## Paging across a merge

`idx_searcher_search_page` restates the cost `p43` put on the single-index method and adds the term
the merge introduces: **cost grows with `offset` and with segment count together**, never with `k`.
Every segment must yield `offset + k` before the merge can decide the page, because the page
boundary is global — **a segment's 3rd-best hit can be the page's 1st**. A per-segment slice would
be cheaper and would drop or duplicate rows; the partition assertion is what forbids it.

## Two bugs the work surfaced

**An out-of-range facet slot ignored the rule it was built to obey.** `resolve_clause` began with
`self.facet_label.get(c.slot)?`, so a clause naming a slot the index does not have made the whole
filter unsatisfiable — *including an exclude*. That is `p43`'s asymmetry one level up, and it is
wrong for exactly the case segments create: a newer delta may carry a slot an older segment was
built without, and `1!=discontinued` would then silently delete every older row from the results.
A slot with no values has no *known* value, so it now takes the same two answers as an unknown one.

**`js/index.mjs` — the host an application actually imports — sat at `ABI_VERSION = 2` while the
module was at 9.** Every `build()` and `open()` through it threw `ABI mismatch`. The check worked;
the constant did not. It survived because `js/smoke.mjs` instantiates the WASM module *directly*, so
34 green host checks never touched the file the README tells people to import.

> **A host that is not gated is not shipped, only published.** The smoke test now imports
> `js/index.mjs` the way a consumer would, so the constant cannot drift again.

`searchClause`/`searchPage` were added there and to `host/python/index_ffi.py`, which had no clause
or paging binding at all — "both hosts PASS" had meant "both hosts pass the checks they have".

## The wire limit, enforced at the host now

`|` separates alternatives in the spec, so a facet value containing `|` cannot be expressed. `p43`
stated that in the header. Both hosts now **refuse** such a value instead of encoding it, because
the failure it otherwise produces is a filter that silently means two values where the caller meant
one — the same shape of bug as reversing the include/exclude rule.

## Still open

- `js/index.mjs` exposes no facet hook at build time, so its clause coverage can only assert the
  unsatisfiable/vacuous pair, not a real filter. A `facet` option on `SearchIndex.build` would close it.
- ~~`scripts/build-wasm.sh` writes to `target/wasm-default/`, while `js/smoke.mjs` reads
  `target/wasm32-unknown-unknown/release/`.~~ **Fixed**: only the `simd128` build needs its own
  target dir (that is what the per-flag-set split was for), so the three default builds now use the
  default one and the script and the smoke test can no longer disagree about which artifact is
  under test.
