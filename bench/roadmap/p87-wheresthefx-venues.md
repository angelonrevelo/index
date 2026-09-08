# P87 — wheresthefx's OSM venues: the punctuation trap, at five-figure scale

**Tier:** T1 · **Check:** `cargo run -p index-bench --release --bin wheresthefx-venues` · **Files:** `crates/index-bench/src/wheresthefx_venues.rs`
**Status: MEASURED, 2026-09-08. The fourth p84 candidate and the first at five-figure scale:
13,487 real Metro-Manila venues. The engine's token-class recall is 99.3 % clean / 89.7 % on
one-letter corruption at p99 ≤ 0.3 ms; the `ilike` predicate returns NOTHING for 1,178 clean
queries and 10,185 of 12,003 corrupted ones.**

## The consumer

wheresthefx's event search matches (`app/api/src/db/event.ts`):

> `or(ilike(event.title, term), ilike(event.description, term), ilike(event.venueName, term)))`
> with `term = '%' + filter.search + '%'`

The venue surface this bench measures is the same predicate over the venue ingest fixture —
13,487 OSM venues (name, optional aliases on 440 of them, category slug, coordinates). The engine
indexes name (boost 3) + aliases + slug-as-facet: **8,718 terms, 55,449 B — 4 B/venue — built in
38 ms.**

## The metric, stated before the numbers

Per-venue recall is meaningless on this corpus and the bench says why in its header: **OSM names
are branch-heavy.** "Jollibee" or "7-Eleven" name hundreds of distinct branches, so a brand token
maps to hundreds of different venue names; no top-10 can enumerate them and no text can prefer one
branch over another. The honest question is **token-class recall**: does the answer set contain a
venue whose name — punctuation stripped — contains what was typed? Punctuation is the honest trap:
a user types `7eleven`, the stored name is `7-Eleven`, and a raw substring predicate cannot meet
its own venue. Both matchers are scored against the same class definition.

## Measured, interleaved, 12–13 K queries per family

| query family | engine (top-10, class recall) | `ilike` filter (uncapped, class recall) |
|---|---|---|
| clean distinctive tokens (12,313) | **99.3 %** | 90.4 % — **1,178 queries find NOTHING** |
| one corrupted letter (12,003) | **89.7 %** | 12.1 % — zero results on 10,185 |
| 3-letter typeahead (12,805 keystrokes) | **98.9 %**, p50 12 µs, p99 45 µs | a 13,487-row scan; no typeahead story |

**OVERALL: PASS.**

Three things worth carrying:

1. **The 90.4 % is the ilike predicate failing on ITS OWN data.** Every one of the 1,178
   zero-result queries names a venue that exists in the fixture — the user typed the brand without
   the punctuation the OSM name carries (`7eleven` → `7-Eleven`). The engine's tokenizer meets the
   typed form at index time and query time; the substring predicate has no such meeting point.
2. **The corrupted-letter gap (89.7 % vs 12.1 %) is the profstopick 109/267 finding at five-figure
   scale**, with the same shape: the brand is in the corpus, the typo makes it unreachable, and
   the zero-result list is what the visitor sees.
3. **Typeahead at 45 µs p99 is a property the SQL predicate cannot have at any tuning**: a
   prefix scan over 13,487 rows per keystroke is what the drizzle `ilike` endpoint does today.

## Two metric-design records, because the first two drafts were wrong

- Per-venue recall on a branch-heavy corpus measures tie-breaking luck (hundreds of
  text-equivalent venues, arbitrary doc ids) — the first draft read 73.8 % "clean" and the
  instrument was the defect, not the engine.
- The class check must fold case AND cover aliases — a venue found through its alias is a correct
  answer whose name alone fails the check, and an unfolded query token ("Sta") fails every stored
  name. Both drafts were caught by the absurdity of their own numbers before anything was written
  down.

## Still open

- The fixture is the ingest snapshot; live events add `title`/`description` text the venue surface
  does not carry. An event-corpus bench needs an export from the owner.
- `nookr2` (SQL seed), `openbid` (no static seed) remain unmeasured pending exports — the coverage
  table tracks them.
