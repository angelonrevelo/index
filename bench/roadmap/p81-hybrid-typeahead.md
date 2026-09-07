# P81 — reading the incumbent's source, and finding it had already fixed half of what we claimed

**Tier:** T2 · **Files:** `js/compare.html`, `js/profstopick-match.mjs`, `js/hybrid.mjs`
**Status: SHIPPED, 2026-09-07. Demo and comparison surface only; nothing in `crates/` changed.**

Every profstopick claim in this repo traces to one production measurement — *109 of 267 real
searches returned nothing, 60 of them name-shaped* — and to a baseline matcher reproduced in
`real_corpus.rs`. Building a side-by-side page meant reading their **current** source rather than
the measurement, and the two no longer agree.

## What their matcher actually does today

`profstopick/src/lib/search-match.ts` is not the strawman it would be convenient to compare against:

- **Diacritic folding**, so `pena` reaches `PEÑA-REYES`. Their own comment records the bug this
  fixed: a previous version stripped non-ASCII instead of folding it, making 72 entries unreachable.
- **Punctuation stripping**, so `math30.23`, `MATH 30-23` and `math 30.23` are one query.
- **A backtracking distinct-token assignment** (`assignToken`), so `raphael abacan` reaches
  `ABACAN, RAPHAEL`. `DISTINCT` is load-bearing — without it `jacob c` matches `Jacob, Precious` by
  spending one token twice — and it backtracks rather than greedily, because greedy strands `jacob`
  on `j jacob` against `Jacob, Jones`.

> **They shipped that assignment BECAUSE of the 40.8 % measurement.** Their code comment cites it by
> date. So the 60 name-shaped misses that this repo has repeatedly offered as the headline are
> **already fixed, by them, in their own code.**

`bench/roadmap/p68-opfs-tier.md` claimed `name order reversed | miss | hit`. That row is now false
and carries a correction. **What survives is the misspelling half**, which a prefix/substring matcher
cannot reach by construction: every route it has requires the typed characters to appear, in order,
somewhere in the name.

## The reproduced baseline is still fair

Checked before touching the headline table. `PrefixBaseline` in `real_corpus.rs` requires **every
query token to match somewhere**, in any order, so it already handles reversed names — and it has
**no edit distance anywhere**. Since the published comparison is a *typo* set, the numbers stand
unchanged:

| | engine | baseline |
|---|---|---|
| exact hit@1 | 99.8 % | 99.8 % |
| **typo hit@10** | **99.9 %** | **14.9 %** |
| zero-result rate | 0.1 % | 85.1 % |

## Two things the page found about our own side

**1. `prefix: true` is not optional, and omitting it understates the engine badly.** The first
version of `compare.html` called `search()` without it, which matches the last token as a *complete*
term — so every half-typed query, which is every query until the user stops typing, found nothing:

| query | theirs | `index` without `prefix` | `index` with `prefix` |
|---|---|---|---|
| `pen` | 3 | **0** | 6 |
| `gar` | 10 | **0** | 10 |
| `cru` | 10 | **0** | 10 |
| `ab` | 10 | **0** | 10 |

**2. Even with prefix on, `index` ALONE is a downgrade for this typeahead.** On short fragments its
fuzzy expansion outranks the obvious prefix hits:

| query | theirs leads with | `index` alone leads with |
|---|---|---|
| `pen` | `PEÑA-REYES` | `TEH, ABIGAIL` |
| `gar` | `GARCES, IAN JUNE` | `GARDON, HAROLD` |
| `mar` | `MARAMARA, MELISSA` | `BANARIA, MARGARET` |
| `joh` | `JOHNSON, JOSEPH` | `CO, SR. MA. ANICIA` |

## The structural complementarity, and the merge

Neither side fixes the other by tuning:

- **Theirs does infix; `index` cannot.** `gracia` must reach `DIVINAGRACIA, GERALD G.` A term
  dictionary stores `divinagracia` as one token, so `gracia` is neither a prefix of it nor within
  edit distance — **an FST walk cannot arrive there at all**. It offers `GARCIA` at distance 1: a
  reasonable guess and the wrong professor.
- **`index` does typos; theirs cannot.** `ABACNA`, `ABAAN`.

`js/hybrid.mjs` merges them under one rule: **literal matches rank above fuzzy ones.** A character
the user typed is evidence; an edit the engine guessed is inference, and inference must not displace
evidence. That is `index`'s own internal rule — `typo_bucket` outranks BM25 — applied one level up.
Neither source is re-scored, because re-scoring across two systems whose scores mean different things
is how a blended list becomes worse than both inputs.

**Measured over 16 queries, the merged top result is the better of the two on every one.** It is the
only configuration that is strictly better than what profstopick ships today.

## Still open

- **The merge is a demo, not a shipped integration.** It lives in `js/`, is not in CI, and has no
  test — only the 16-query check run by hand.
- **`index` cannot do infix, and that is not a bug to file but a property to state.** Reaching
  `DIVINAGRACIA` from `gracia` would need a suffix automaton or n-gram terms; both cost bytes the
  format currently spends elsewhere.
- **The 40.8 % figure should stop being quoted as a current number.** It is a 2026-08-17
  measurement of a matcher that has since changed. Nobody has re-measured profstopick's zero-result
  rate against their current code, and this repo should not imply otherwise.
