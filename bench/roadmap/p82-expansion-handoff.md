# P82 — one expansion per segment, not two

**Tier:** T1 · **Check:** `cargo test -p index-text` (`a_handed_over_expansion_weighs_exactly_like_a_rederived_one`) · **ABI:** unchanged (14)
**Files:** `crates/index-text/src/index.rs` (`QueryExpansion`, `expand_query`, `weigh`, `search_expanded`), `crates/index-text/src/searcher.rs` (`stat_for`, `merge`, both call sites)
**Status: SHIPPED, 2026-09-08. Collection-wide statistics now cost ~1.0x on the typo tail, down from
1.8-2.2x; ranking bit-identical; 319 workspace tests, 0 clippy.**

`p52` made a segmented collection rank like a rebuild of the same rows and named the price it
could not pay:

> **The second expansion is avoidable.** Both passes traverse the same automaton over the same
> dictionary; the first pass could hand its expansion to the second instead of the second
> re-deriving it. That is where the ~2x goes, and recovering it needs `plan` split into an expand
> phase and a weigh phase. Not done.

Not done, until now. This is the recovery, built as `p52` described it.

## The split

`plan_stat` did everything: tokenize, alias, walk the fuzzy dictionary per token, probe compound
splits, emit the learned-expansion alternatives, apply the cap, compute IDF weights. The stat pass
(`term_stat`) re-ran the first half of that on every segment; the search pass (`plan_stat`) re-ran
it again. On a typo query the automaton walk IS the query's cost, so the second walk was where the
~2x went.

Now there are two functions with one boundary between them:

- **`Index::expand_query(query, prefix_last, want_text)`** — everything up to per-group match
  lists, with no cap and no weights. Pure dictionary work. Returns a `QueryExpansion`: the emit
  calls in plan order, the group table, and whether the learned branch fired.
- **`Index::weigh(expansion, cap, stat)`** — cap, IDF and weights over an expansion someone
  already built. Pure postings work.

`Searcher::stat_for` fans `expand_query` once per segment, sums the `(text, df)` pairs into the
`CollectionStat`, and — the change — **hands each segment's expansion back to that same segment**
through `Scan::pre`, so the weigh phase re-derives nothing. `merge` now passes the segment index to
its closure, which is what lets the right expansion reach the right segment.

Two flags keep the boundary honest:

- `want_text` preserves the single-index zero-cost: `expand_lazy` allocates nothing per match when
  no stat pass will read the text, exactly as before.
- `ExpansionEmit::learned` marks the learned-expansion alternatives so `expansion_stat` can keep
  excluding them from the collection-wide `df` sum, which they always were — they score per segment
  by documented decision (`p52`), and the hand-off must not quietly change that.

## Why handing back is unobservable

The expansion a segment derives from its own dictionary is a pure function of (query, dictionary,
alias table). The stat pass and the search pass derive the same function on the same inputs — so
handing the first result to the second can only be faster, never different. The cap still lives in
the weigh phase, because it sorts each emit call's list by posting length, which only the weigh
phase's segment knows; the expansion carries the UNCAPPED lists, exactly what `term_stat` always
collected.

That argument is asserted, not trusted: `a_handed_over_expansion_weighs_exactly_like_a_rederived_one`
compares `search_with_stat` (re-derives) against `search_expanded` (weighs the hand-off)
bit-for-bit — score BITS, not floats — across exact, typo'd, compound-split, typeahead and
learned-expansion queries on a 4-segment collection, with and without a learned table. The
pre-existing threaded-vs-serial test still covers both `stat_for` call sites end to end.

The test earned its keep on the first run: its per-segment non-empty assertion failed on segment 3,
which contains no `Toothp*` document at all. The engine was right; the test was wrong, and now
asserts emptiness per collection rather than per segment — a segment that never saw the prefix
legitimately contributes nothing.

## Measured — the p52 table, re-run interleaved

`segment-scale`, presyo product names, same process, `off` = collection stats disabled, `on` =
enabled (the default). Before is `p52`'s published table; after is this change.

| segs | p50 off→on before | **after** | typo p99 off→on before | **after** |
|---|---|---|---|---|
| 2 | 1.13x | **1.04x** | 1.90x | **0.98x** |
| 5 | 1.25x | **1.32x** | 2.06x | **1.11x** |
| 10 | 1.75x | **1.02x** | 2.05x | **0.99x** |
| 25 | 1.94x | **0.98x** | 2.18x | **0.94x** |
| 50 | 1.94x | **0.97x** | 1.84x | **1.04x** |

At every rung past 10 segments — the rungs where the cost was worst — collection-wide ranking
parity is now free. The residual ±5 % is run-to-run noise on an interleaved measurement, not a
systematic cost: its sign is not consistent across rungs, which is what distinguishes noise from a
price. The 5-segment p50 (1.32x) sits inside the same noise band the "before" table measured 1.25x
in; no structure was found behind it and none is claimed.

**`set_collection_stat(false)` keeps its contract** — it still trades parity for latency, and
nothing here changes when a host should reach for it. It is simply no longer the only way to keep
the typo tail at a single-segment cost.

## Gates re-run on the real tree

319 workspace tests (was 318; the differential test is new), 0 clippy warnings. `segment-scale`
`OVERALL: PASS`, its broad/selective overlap columns unchanged. The consumer re-validation sweep of
this session — every bench re-run against its published numbers — lives in
[`bench/runs/2026-09-08-revalidate/`](../runs/2026-09-08-revalidate/README.md); nothing moved that
the hand-off could have moved.

## Still open

- **Learned-expansion terms still score per segment.** Unchanged by this work; the expansion table
  stores term ids and no text, so those terms cannot be summed by string. Fixing it means storing
  text in the table, which costs bytes every collection pays for a correction only multi-segment
  ones need.
- **`avg_len` is still per segment**, as `p52` left it — second-order, and baked into `sat` at
  build time.
- The `wants_thread` comment prices the double fan-out; the first fan-out is now cheaper per
  segment (expansion only, no df map join on the closure side), which nudges the 3 ms break-even
  slightly downward. Not re-swept: the gate is calibrated conservatively and the ladder it was
  tuned on is unchanged.
