# P95 — phrase COUNT(*)

**Tier:** T3 with live tests (the integer is a silent corruption if wrong) · **Files:**
`crates/index-text/src/index.rs`, `crates/index-text/src/searcher.rs`,
`crates/index-wasm/src/lib.rs`, `include/index.h`, `js/index.mjs`, `js/smoke.mjs`,
`crates/index-cli/src/main.rs`, `scripts/cli-smoke.sh`, `host/python/index_ffi.py`
**Status: SHIPPED, 2026-09-20.** Gate-live, not quarantined.

A quoted query can rank (`search_phrase`) and cannot count. Falling back to `count_all` would
count bag-of-words rows as phrase rows.

## Surface

`Index::count_phrase(&self, query: &str) -> usize`, then the same ABI/CLI shape as p94
(`idx_count_phrase`, `index search --count phrase QUERY`). An index built without positions
returns 0, matching `search_phrase`. Additive at ABI 14:
`idx_searcher_count_any` / `idx_searcher_count_all` / `idx_searcher_count_phrase`.

## Pass criteria

- A position-enabled fixture of `["Colgate Total Toothpaste", "Total Colgate Toothpaste"]`
  reports `count_phrase("Colgate Total") == 1` and `count_phrase("Total Colgate") == 1`
  (each title *is* one of the two phrases). `count_all` is 2 either way — that is the
  bag-of-words trap.
- Adjacent vs gapped reverse: `["Colgate Total Toothpaste", "Total Toothpaste Colgate"]`
  reports `count_phrase("Colgate Total") == 1` and `count_phrase("Total Colgate") == 0`.
- The same index without `set_position()` reports `0`, never `2`.
- Deleted documents are not counted.
- Live gate: `#[cfg(test)]` next to `count_phrase`, `js/smoke.mjs`, and `scripts/cli-smoke.sh`.
  This file stays as the spec, not the only proof.
