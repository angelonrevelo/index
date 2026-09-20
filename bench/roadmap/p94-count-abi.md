# P94 — exact COUNT(*) on the C ABI, JS host, Searcher, and CLI

**Tier:** T3 with live tests (the integer is a silent corruption if wrong) · **Files:**
`crates/index-wasm/src/lib.rs`, `include/index.h`, `js/index.mjs`, `js/smoke.mjs`,
`crates/index-text/src/searcher.rs`, `crates/index-cli/src/main.rs`, `scripts/cli-smoke.sh`
**Status: SHIPPED, 2026-09-20.** Gate-live, not quarantined.

p93 shipped `Index::count_any` / `count_all` as Rust-only. A host that already searches cannot ask
how many documents match without ranking `k` hits and reading `len`, which is wrong the moment
`k` < matches.

## Surface

- `idx_count_any` / `idx_count_all` — additive, ABI 14 unchanged (range-facet precedent).
- `SearchIndex.countAny` / `countAll`.
- `Searcher::count_any` / `count_all` — sum of per-segment exact counts (a document lives in one
  segment).
- `index search --count any|all QUERY` — prints one integer.

## Pass criteria

1. `js/smoke.mjs`: the new symbols are exported; a 4-document toothpaste fixture reports
   `count_any("toothpaste") == 4`, `count_any("colgate") == 2`, `count_all("colgate toothpaste") == 2`,
   `count_any("") == 0`; a null handle returns the `UINT32_MAX` sentinel.
2. A two-segment `Searcher` reports the same any/all counts as a single `Index` built from the same
   rows, including a deleted document.
3. `scripts/cli-smoke.sh`: `index search --count any toothpaste` on the pipe-built fixture equals
   the number of toothpaste rows (3 before apply, 3 after apply because A-3 is deleted and A-6 is
   Sensodyne — wait: after apply A-3 Aquafresh is gone, A-6 Sensodyne Repair Toothpaste is added,
   so toothpaste stays 3). Assert the integer, not `hit.length`.
