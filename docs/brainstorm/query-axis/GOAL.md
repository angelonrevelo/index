# Goal prompt — query-axis layers (paste into `/goal`)

Copy everything below the line into a fresh session. Do not invent a second engine.
Do not chase TIN 10,260 QPS on 8.0 GB Wikipedia. Do not open a consumer-app PR.

---

Goal: recursively add one **query axis** at a time until a typed query is answered by
**composing existing arms** (exact key, exact phrase, bag-of-words BM25, typo, alias,
facet/category, numeric filter, Filipino↔English, emoji token) instead of one inverted-index
walk pretending to be all of them.

Repo: `C:\Users\maran\code\index` (HEAD at start of this write: `c0114ab`). MIT OR Apache-2.0.
One C ABI, no wasm-bindgen, no sidecar, no AGPL, no `CREATE EXTENSION`. Singular naming
everywhere (`arm`, `axis`, `alias`, `hit` — never `arms`/`axes` as identifiers). `/convention`.

## Why this exists

An inverted index is lossy. It is good at "documents that contain these tokens, maybe misspelled."
It is bad at SKU `BEA-ESS-ESS-001`, quoted order, a category label that is not in the title, an
emoji that the tokenizer drops, `bigas` vs `rice`, a filter the user already applied. Those are
**different questions**. Ranking them all as BM25 (with typo expansion) is how exact matches get
buried and how filters leak.

Algolia ranks **typo, then words, then exact** as separate criteria, not as one score.
Typesense turns typo off for part numbers. This repo already has most of the arms; it does not
**plan** them. `crates/index-text/src/query.rs` parses `red "ice cream" -discontinued` and
**nothing in the engine consumes `Query`** (module docs say so). `fuse::rrf` exists and search
does not call it for query axes.

## Inventory — do not rebuild these

| axis | already in tree | hole |
|---|---|---|
| exact key / SKU | `doc_of_key`, `with_key` | search does not try the key before BM25 |
| exact phrase | `search_phrase`, `count_phrase` | mixed `red "ice cream"` is parsed, not executed |
| bag-of-words | `Index::search` BM25F + MaxScore | this is the default for everything |
| typo | FST + Levenshtein, `typo_bucket` primary sort | shipped; do not replace |
| alias | `AliasTable` before index and query | table is empty of PH/Filipino rows |
| category / facet | `search_facet_all`, `search_filtered` | not chosen by a planner from the query string |
| numeric filter | `search_filtered` ranges | same |
| fold (accent) | `analyze::fold` NFKC/case/marks | Peña-Reyes works; not a new crate |
| size/unit | `merge_split_quantity` | P4 still lists `1.5L ≡ 1500ml` as spec |
| derived expansion | `p17` / expansion table | not a synonym LLM |
| rank fusion | `fuse::rrf` / `convex` | text+image only today |
| emoji | tokenizer test corpus includes 😀 and 🇵🇭 | **dropped**: non-alphanumeric → separator (`analyze.rs` tokenize_folded) |
| Filipino↔English | P4 row `bigas`/`gatas`/`gamot sa ubo`/`sabon panlaba` | **spec, not loaded** |
| query planner | `query::parse` | **no executor** |

## Recursive loop (one axis per iteration — this is the whole method)

Until the axis table below is all SHIPPED or REJECTED:

1. **Pick the next red axis** in this order (do not skip ahead):
   1. planner consumes `query::parse` (the compose point; without it every other axis is a new `search_*` nobody calls)
   2. exact-key arm (SKU / application key beats bag-of-words)
   3. mixed phrase execution (`term` + `phrase` + `exclude` from `Query`)
   4. emoji kept as tokens (index and query, same fold)
   5. Filipino↔English alias rows on the existing `AliasTable` (no stemmer)
   6. size/unit canonicalization negatives (`300g` never matches `800g`)
   7. optional: brand-only Double Metaphone (`kolgate`/`nesle`/`kokakola`) — P5 already specified it
2. **Write the miss fixture first** under `bench/roadmap/p98-query-axis.md` (gate-excluded until the axis lands). One row: query, document, expected rank-1 key, which axis must win, which axis must not steal it.
3. **Implement only that axis** as a function the planner can call. Do not add a new crate. Do not add a new `.idx` magic. Prefer `Index` methods + planner dispatch.
4. **Compose**: the planner runs applicable arms, then `fuse::rrf` (or the existing typo_bucket rule when the arm is the same scorer). Exact-key hit is rank-1, not fused away.
5. **Gate**: `cargo test -p index-text --lib` and the new axis test. Promote the fixture into the live test when the axis is built. `bash scripts/gate.sh` before claiming a checkpoint.
6. **Sync docs**: one ROADMAP cell, one CHANGELOG sentence. Do not rewrite Honest limits to claim TIN 10,260 QPS.
7. **Repeat.** Stop an axis if it fails the deletion test (adds more than it earns) and log it in `docs/roadmap-rejected.md` with a verbatim reason.

## Invariants every axis must keep

- Exact match still beats typo (`typo_bucket` is the primary sort today; do not invert it).
- Numeric tokens are never fuzzy (`300g` ↛ `800g`).
- Empty query → empty result, never everything.
- Deleted documents are not counted or returned.
- One fold, used at index and query (`analyze.rs` rule 1). A second tokenizer is how Peña-Reyes went unreachable.
- Hosts already searching (C ABI, JS, CLI, Python) get the planner through `search`, not a new symbol per axis unless the axis cannot be expressed as a query string.

## Done

Not "all edge cases in the universe." Done when:

- `Index::search` (and `Searcher::search`) execute a `query::parse` plan for mixed phrase + exclude.
- A keyed document is rank-1 when the query equals its key, even if BM25 would prefer a longer field.
- Emoji in a field is retrievable by the same emoji in the query.
- The four P4 Filipino fixture strings resolve via `AliasTable` without an LLM.
- `300g` still never matches `800g`.
- `bash scripts/gate.sh` OVERALL PASS.
- ROADMAP p98 rows that shipped are marked SHIPPED; the rest stay spec or rejected.

## Forbidden

TIN Wikipedia bake-off. GitHub Actions (local `scripts/gate.sh` is the gate). Consumer PRs.
Sidecar. wasm-bindgen. Building a new FST. SPLADE / LLM synonyms in the hot path. charabia
until the P4 build-vs-buy audit is actually re-run and recorded. Raising the 5 ms typo bar.
Plural collection names.

## Verifier (for `/goal --until`)

```
cargo test -p index-text --lib query
cargo test -p index-text --lib
```

Plus, once an axis claims SHIPPED, a test that **fails if that axis is reverted** (red-green).
A passing suite that never asserted the new axis is theater.
