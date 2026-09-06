# P48 — document keys, and the invariant that makes an update expressible

**Tier:** T1 · **API:** `IndexBuilder::with_key`, `Index::doc_of_key`, `Searcher::delete_key`
**Status: SHIPPED, 2026-09-06. Format IDXTEXT6 -> IDXTEXT7. No ABI change. 145 tests.**

Asked for something that works on any database, and the first thing in the way was not the database
at all.

## The engine could not say which row it meant

`Searcher::delete` takes a dense global ordinal, assigned at insertion. **No database row carries
one.** So every incremental operation an application actually performs —

> the row with `sku = 'A-1'` changed

— was inexpressible. `p37` shipped live updates and `p38` measured what they cost, and both were
about *appending*, which needs no identity. Updating and deleting do, and neither was reachable
from outside the crate.

So a document may now carry the application's own key, stored verbatim.

**Not to be confused with the internal->external id map `Index` deliberately does not have.** That
one is a docID *reordering* permutation for ranking performance, implemented and measured worse in
`p7` (typo p99 6.40 -> 7.02 ms). This is an application identifier and touches no ranking. The
comment there now says which is which, because "we deliberately do not have an id map" reads like a
prohibition on exactly this.

## Keys are not facets, and are stored differently

| | facet | key |
|---|---|---|
| cardinality | low — 145 categories over a million products | one per row |
| stored as | interned label id, `u32` per document | the string, per document |
| looked up by | id, for filtering | string, for identity |

Interning a key would store one label per row and buy nothing, so keys are kept in document order
and the sorted lookup is **derived at load** — like `numeric_order` and `posting_base`, because a
stored copy of a sort is a second version of a fact that can disagree with the first.

## The invariant everything else rests on

> **At most one live document per key.**

Segments are immutable, so an update can only mean *append the new version and retire the old one*.
That retirement happens inside `Searcher::push`, not in the caller, and the reason is that a caller
who forgets it gets no error — just a collection that accumulates every historical version of every
row and returns all of them. A duplicate that reads as a ranking bug and is actually a bookkeeping
one.

`push` therefore tombstones any key the incoming segment re-uses, and returns how many. Zero when
either side is unkeyed, so an append-only collection is untouched and pays nothing.

Resolution searches segments **newest first**, which is what makes the answer right even for a
collection assembled without that guarantee.

## Two decisions that look small and are not

**A blank key is NO key, not a key of `""`.** Two rows with an empty key field would otherwise both
answer to the empty key, and a change stream would update an arbitrary one of them. So blank rows
are excluded from the lookup entirely: still searchable, never addressable. `keyed_count()` sits
below `live_count()` when that happens, and the CLI prints a warning at build time, because the
alternative is discovering it months later when an update silently does nothing.

**A key duplicated inside ONE segment resolves to the last document carrying it** — the same
newest-wins rule applied across segments, so there is one rule rather than two.

## The format, and the failure it is validated against

`IDXTEXT6` -> `IDXTEXT7`, section table 17 spans -> 18. `every_offset_in_the_table_is_u64` failed the
moment the span was added, which is the fourth time that assertion has forced a magic bump rather
than letting the format grow silently.

Keys are **positional**, one per document, so the reader counts them against `doc_count` rather than
trusting the section. `a_short_key_array_is_refused` truncates it by one entry and requires an
error, because the alternative is the worst failure available here: every later key shifts onto the
wrong document, `doc_of_key` resolves a real key to a real-but-wrong row, and a change stream
updates the wrong record. **Corruption that looks exactly like working software.**

## Still open

- **Keys are not exposed over the C ABI**, so the JavaScript and Python hosts cannot resolve or
  delete by key. The Rust `Searcher` and the CLI can. That is the obvious next ABI bump and it is
  not done.
- **An index written by an older build will not open**, and there is no migration path other than
  rebuilding from the source of truth. Consistent with how every prior format bump was handled, and
  cheap because compaction was already "rebuild from the application's database".
