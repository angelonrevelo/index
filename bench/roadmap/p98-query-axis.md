# P98 — query-axis planner

**Tier:** T3 · **Status: SHIPPED 2026-09-21.** Goal prompt:
[`docs/brainstorm/query-axis/GOAL.md`](../../docs/brainstorm/query-axis/GOAL.md).

Indexing is one arm. Exact key, quoted phrase, alias, facet, emoji, Filipino gloss, and
numeric identity are other arms. This file was the miss fixture; the rows below are now
`#[cfg(test)]` in `index-text` (`planner_unquoted_bag_of_words_and_empty_query`,
`exact_key_is_rank_one_even_when_bm25_prefers_a_longer_field`,
`mixed_phrase_and_exclude_run_through_search`,
`emoji_in_the_query_retrieves_the_emoji_document`,
`filipino_alias_rows_resolve_the_p4_fixture_strings`,
`three_hundred_g_does_not_match_eight_hundred_g_and_exact_beats_typo`,
`searcher_search_runs_the_query_axis_plan`). It is **not** a TIN QPS claim and **not** a
new `.idx` format.

## Grounding

- `crates/index-text/src/query.rs`: `parse` exists; `Index::search` / `Searcher::search` consume
  `Query`.
- `crates/index-text/src/fuse.rs`: `rrf` exists; same-scorer arms keep `typo_bucket`; exact-key
  is prepended, not fused.
- `crates/index-text/src/analyze.rs` `tokenize_folded`: emoji is a token (same fold at both ends).
- ROADMAP P4: `bigas` / `gatas` / `gamot sa ubo` / `sabon panlaba` resolve via
  `AliasTable::philippine_grocery`.
- `typo_bucket` still ranks exact above typo.

## Miss fixture (at spec time) → now

| query | document | axis that should win | today |
|---|---|---|---|
| `K99` when that is the application key | row whose key is K99 | exact-key | **SHIPPED** — rank-1 even vs a longer BM25 field; deleted key absent |
| `red "ice cream"` | "red ice cream" vs "red cream ice" | mixed phrase | **SHIPPED** — in-order wins; `-discontinued` drops |
| `😀` | field containing 😀 | emoji token | **SHIPPED** — tokenizer keeps it |
| `bigas` | rice / "bigas" alias | Filipino alias | **SHIPPED** — four P4 strings via loaded `AliasTable` |
| `300g` | 800g bag | numeric identity | **SHIPPED** — search negative; fuzzy skip kept |

## Pass / fail per axis

| axis | pass | status |
|---|---|---|
| planner | `search` of a `query::parse` result runs phrase + term + exclude; unquoted `search` still matches today's bag-of-words on a fixture | **SHIPPED** |
| exact-key | query equal to a live key → that doc is rank-1 | **SHIPPED** |
| mixed phrase | `red "ice cream"` hits the in-order row, not the reversed bag | **SHIPPED** |
| emoji | index and query of 😀 retrieve the row | **SHIPPED** |
| Filipino alias | `bigas` retrieves the rice row via `AliasTable` | **SHIPPED** |
| size negative | `300g` does not retrieve an 800g-only row | **SHIPPED** |
