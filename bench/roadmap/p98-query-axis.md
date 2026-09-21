# P98 — query-axis planner

**Tier:** T3 · **Status: SHIPPED 2026-09-21** (typeahead + default-builder positions: same day, below). Goal prompt:
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
| typeahead | `search_prefix` / `idx_search` prefix=1 runs the same plan; plain prefix queries unchanged | **SHIPPED** |
| default builder phrase | `"dela cruz"` on `idx_build_new("label:3:0.4")` finds its row | **SHIPPED** |

## Typeahead and the default builder (follow-up, same day)

Measured through the shipped wasm on profstopick's label fixture (`label:3:0.4`,
`Cruz, Juan` / `Cruz, Maria Santos` / `Santos, Pedro` / `Dela Cruz, Ana` / `Reyes, Jose`):

| query | prefix | before | after |
|---|---|---|---|
| `cruz -santos` | 1 | Cruz Maria Santos, Cruz Juan, **Santos Pedro**, Dela Cruz Ana | Cruz Juan, Dela Cruz Ana |
| `cruz -santos` | 0 | Cruz Juan, Dela Cruz Ana | same |
| `"dela cruz"` | 1 | Dela Cruz Ana, Cruz Juan, Cruz Maria Santos (quotes ignored) | Dela Cruz Ana |
| `"dela cruz"` | 0 | **nothing** | Dela Cruz Ana |

Two causes. (1) `Index::search_prefix` called `search_opt(Scan { prefix_last: true, .. })`
directly, never `search_planned`. (2) `idx_build_new` never set positions, and
`resolve_query_phrase` refuses a phrase on an index without them (`p45`: empty, never a bag).
Not punctuation, not fold: `"cruz, juan"` matched once positions existed.

Fix: `search_planned` takes `prefix_last`; `Index::scoring_plan` keeps the raw string for a plain
query (byte-identity) and otherwise uses `query::typeahead_scoring_query` — positive clauses in
typed order, prefix only when the last one is an unquoted term. `idx_build_new` records
positions. Tests: `search_prefix_runs_the_query_axis_plan`,
`search_prefix_carries_key_emoji_alias_and_size`,
`search_prefix_of_a_plain_query_is_byte_identical_to_the_pre_planner_path`,
`searcher_search_prefix_runs_the_query_axis_plan`,
`typeahead_scoring_keeps_typed_order_and_prefixes_only_an_open_term`,
`idx_search_runs_the_plan_at_both_prefix_settings_on_the_default_builder`.

Cost of positions by default: 2,253 course titles, one field, serialized **49,260 → 67,540 B
(+37.1 %)** — higher than `p54`'s +15.9 % on 25,979 multi-field rows, because short labels are
mostly single-occurrence postings. Plain queries: **0 of 15,966** wasm result buffers differ
(7,983 typed prefixes of those titles, prefix 0 and 1, before vs after artifact).
