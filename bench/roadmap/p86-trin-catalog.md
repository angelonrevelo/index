# P86 — trin's opportunity catalog, and the filter that misses its own data

**Tier:** T1 · **Check:** `cargo run -p index-bench --release --bin trin-catalog` · **Files:** `crates/index-bench/src/trin_catalog.rs`
**Status: MEASURED, 2026-09-08. On trin's own seed catalog the engine's top-10 beats the consumer's
own uncapped filter on CLEAN queries (97.5 % vs 87.9 %) — because users type "masters" and the
filter cannot match "Master's". Corrupted terms: 88 % of queries return nothing for the filter.**

## The consumer

trin's `/browse` endpoint filters candidates through (`adapters/http/routes.ts`):

> `items.filter((c) => c.name.toLowerCase().includes(q) || (c.description ?? "").toLowerCase().includes(q))`

after exact-match filters on domain/tier/country, and the client renders the (uncapped) result
list. The on-disk seed catalog is 52 entries — the live storage has grown past 2,340 (the clamp
comment in their own route records it) — and the bench measures the shipped seed, honestly
labelled as the small-corpus class. The engine indexes name (boost 3) + description as fields and
domain as a facet; 7,688 B total.

## Measured, interleaved with the consumer's own predicate

| query family | engine (top-10) | browse filter (uncapped in-results) |
|---|---|---|
| clean name terms (314) | **97.5 %** | 87.9 % |
| one corrupted letter (307) | **78.8 %** | 10.7 % — **270 queries return NOTHING** |
| domain-filtered text (314 terms × 5 domains) | **97.5 %** in one pass | 87.9 % |
| latency | p50 1.9 µs, p99 337 µs | ~µs (52-entry scan) |

**OVERALL: PASS.**

## The finding a small corpus still yields

**The filter misses entries whose name the user typed correctly.** The bench draws query terms
from the names themselves, stripped to alphanumeric characters — exactly what a user types
("masters", "erasmus"). A name like "Master's Programme" lowercases to "master's", which does not
contain "masters": the apostrophe breaks the substring, the entry silently drops out of the
results, and nothing errors. The engine's tokenizer strips the punctuation at index AND query
time, so the same query lands the entry in the top 10. That is a recall defect in the consumer's
matcher that its own authors have not measured, found by pointing the engine at the corpus — the
profstopick pattern in miniature, before a single integration line was written.

Corrupted terms behave as every substring matcher does: 88 % of queries return an empty list.

**The filter half is free.** The domain dropdown composes with the text box in one engine query
(`search_facet`, facet AND text in one pass, tallies exact) at the same recall as the unfiltered
case — the composition their endpoint does in two steps.

## Still open

- The live catalog is 2,340+ rows and growing; the seed is 52. The bench re-runs unchanged on a
  fresh export — the corpus-on-disk path is one `storage.candidates.list()` dump away.
- orsem-website was the other candidate checked this lane: its search page exists but is not
  wired to a matcher yet (`pages/Search.js` is a static form), so there is no consumer baseline to
  reproduce — an engine bench there would be spec-making, not measurement. Recorded and skipped.
