# P84 — yclap's campus forest gallery, on its own data

**Tier:** T1 · **Check:** `cargo run -p index-bench --release --bin yclap-species` · **Files:** `crates/index-bench/src/yclap_species.rs`
**Status: MEASURED, 2026-09-08. First p84 candidate run for real. The gallery's substring matcher
returns NOTHING for 823 of 885 (93 %) one-letter-typo'd species queries; the engine answers 99.4 %
of them — and the clean-typeahead finding is the honest surprise, recorded below.**

## The consumer

yclap's campus forest gallery searches 1,098 modeled Philippine species through one line of
JavaScript (`web-forest/public/model-gallery.js`, `renderList`), fired on every keystroke:

> `${e.common_name} ${e.scientific_name} ${e.species_code}`.toLowerCase().includes(q)

— a case-folded substring over the concatenated fields, results rendered in manifest order,
uncapped. The engine indexes the same manifest (common name boost 3, scientific binomial, slug) —
**2,855 terms, 24,000 B, 21 B/doc, built in 8 ms** — plus the iNat pipeline corpus that feeds the
model: 3,928 taxa (2,307 with preferred common names), 42,615 B. The baseline in the bench
reproduces the JavaScript semantics exactly.

## Measured, one process, interleaved matchers

| query family | engine | gallery matcher |
|---|---|---|
| clean common names (906) | hit@10 **100.0 %** | in-list 100.0 % |
| one transposed letter (885) | hit@10 **99.4 %** | in-list 6.9 % — **823 queries return NOTHING** |
| reversed scientific binomial (906) | hit@10 **100.0 %** | in-list 99.1 % |
| typeahead, first-word prefixes (2,559 keystrokes) | top-10 hit 87.1 %, p50 36 µs p99 14 µs | in-list 100.0 %, p50 11 µs |
| pipeline corpus, clean binomials (364) | hit@10 100.0 %, p99 5 µs | — |

Gates: clean hit ≥ 95 % PASS · typo hit@10 ≥ 90 % PASS · typeahead p99 ≤ 5 ms PASS · pipeline
recall PASS. **OVERALL: PASS.**

## The transposition result, and why the 6.9 % is not zero

A substring test has no edit distance: `narra` typed as `nraa` is a substring of nothing, so the
gallery's list goes empty and stays empty. The 6.9 % of typo'd queries their matcher *does* answer
are accidents — the corrupted string happens to appear elsewhere in the concatenation (typically
inside the scientific binomial or the slug). That accident is also why **reversed word order
scores 99.1 % for the baseline**: the `species_code` slug (`felis-catus`) repeats the binomial
after it (`… felis catus felis-catus`), and the reversed pair `catus felis` is a substring across
that boundary. The matcher "handles" word order by accident of the slug, for exactly the pairs
where the epithet precedes the genus in the concatenation. Recorded because it is a perfect
specimen of the class: substring matching working for a reason nobody designed, which a refactor
of the slug format would silently remove.

**Zero-result rate is the number a visitor feels:** 823 of 885 typo'd species queries render an
empty list today. The engine's answer costs 24 KB — the manifest the gallery already ships is
~10× that.

## The honest surprise: clean typeahead belongs to the filter

On clean prefixes the gallery matcher keeps the species in its list 100 % of the time — its list
is uncapped filtering, so nothing the user typed away can be missing. The engine's ranked top-10
holds the species 87.1 % of the time: when many species share a prefix, ten slots cannot hold them
all and BM25F ranks by relevance, not by name. **For yclap's UX — a scrollable filtered list where
the list itself is the answer — the right shape is the engine as a FILTER (large k or the facet
path), not a ranked top-10**, and the typo win comes with it. A ranked integration would be a UX
change, not a drop-in; recorded so the adoption decision starts from the real shape.

## Still open

- yclap's pipeline data also holds `ancestor-taxa` ranks (species → kingdom) — a rank facet and
  taxonomy-aware query expansion are unmeasured here; the corpus is on disk whenever wanted.
- The gallery has no group-filter + text combined measurement yet (`iconic_taxon_name` is a facet
  column the engine already supports); the modeled set is near-single-group today, so the
  measurement would answer nothing yet.
