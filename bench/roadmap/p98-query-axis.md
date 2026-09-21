# P98 — query-axis planner (not built)

**Tier:** T3 · **candidate-tier: expected-red until built** · **gate-excluded** until an axis
lands in `cargo test`. **Status: spec.** Goal prompt:
[`docs/brainstorm/query-axis/GOAL.md`](../../docs/brainstorm/query-axis/GOAL.md).

Indexing is one arm. Exact key, quoted phrase, alias, facet, emoji, Filipino gloss, and
numeric identity are other arms. This file is the miss fixture + pass condition for composing
them. It is **not** a TIN QPS claim and **not** a new `.idx` format.

## Grounding

- `crates/index-text/src/query.rs`: `parse` exists; **nothing consumes `Query`**.
- `crates/index-text/src/fuse.rs`: `rrf` exists; text search does not call it for query axes.
- `crates/index-text/src/analyze.rs` `tokenize_folded`: non-alphanumeric Unicode (emoji) is a
  separator, so 😀 never becomes a token.
- ROADMAP P4: `bigas` / `gatas` / `gamot sa ubo` / `sabon panlaba` still **spec**.
- `typo_bucket` already ranks exact above typo; do not invert.

## Miss fixture (today)

| query | document | axis that should win | today |
|---|---|---|---|
| `K99` when that is the application key | row whose key is K99 | exact-key | BM25 over name tokens; key is lookup-only |
| `red "ice cream"` | "red ice cream" vs "red cream ice" | mixed phrase | whole-string `search_phrase` or bag-of-words `search`, not both |
| `😀` | field containing 😀 | emoji token | tokenizer drops it |
| `bigas` | rice / "bigas" alias | Filipino alias | no row in `AliasTable` |
| `300g` | 800g bag | numeric identity | numeric tokens already skip fuzzy; keep that |

## Pass / fail per axis

Promote into `#[cfg(test)]` when the axis is built. Until then this file stays under
`bench/roadmap/`.

| axis | pass | status |
|---|---|---|
| planner | `search` of a `query::parse` result runs phrase + term + exclude; unquoted `search` still matches today's bag-of-words on a fixture | **not built** |
| exact-key | query equal to a live key → that doc is rank-1 | **not built** |
| mixed phrase | `red "ice cream"` hits the in-order row, not the reversed bag | **not built** |
| emoji | index and query of 😀 retrieve the row | **not built** |
| Filipino alias | `bigas` retrieves the rice row via `AliasTable` | **not built** |
| size negative | `300g` does not retrieve an 800g-only row | **already true** for fuzzy; keep |
