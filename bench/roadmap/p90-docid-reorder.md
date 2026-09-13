# P90 — docID reordering, built and measured: the default stays OFF, and the measurement says why

**Tier:** T1 · **Check:** `cargo test -p index-text` · **Files:** `crates/index-text/src/reorder.rs`
**Status: BUILT AND MEASURED, 2026-09-13 — ships opt-in (`IndexBuilder::with_doc_reorder(true)`,
CLI `--reorder`), default OFF. −6.2 % of the file on the real presyo schema, ~0 % on a
short-title schema, latency at parity, per-key scores identical, tie membership at the top-k
boundary arbitrary by design.**

## What was built

Recursive graph bisection over the term-document graph, the lever `docs/research/speed.md` ranks
as the cheapest remaining multiplier. The split is a **cut-minimizing refinement**: each range
starts balanced (alternating by current order), then documents move to the side where documents
sharing their terms already sit, one at a time, each move scored by how many crossing term pairs
`sum_t L(t)·R(t)` it removes, refused when it would tip the balance past a slack. Greedy
sequential moves are deterministic — the same corpus produces the same bytes on every machine,
which `serialization_is_deterministic` and the cross-machine check depend on.

A one-shot BFS-parity bisection (the PISA `bfs` formulation) was implemented FIRST and
**collapsed**: on any corpus with a near-universal term the flood reaches every document in two
hops and the parity partition is lopsided, so every split is rejected and the permutation comes
out the identity. The refinement form is what works, and why is worth stating: a balanced
initialization gives every term a real L/R gradient for the greedy moves to climb, where a flood
initialization gives them nothing.

Applied before anything consumes a document id: posting lists remapped and re-sorted (the
saturated score bound rides along), `term_pos` remapped so position runs stay attached and
aligned, and every per-document store — lengths, priors, anchors, facets, numerics, keys —
permuted by the same map. Scores and ranking are per-document quantities; only the doc-id ORDER
moves.

## The measurement

Real presyo corpus (241,793 rows), CLI schema with keys and two facets, the shape with long
posting lists:

| | bytes | build |
|---|---|---|
| reordering off | 18,305,266 | 2.60 s |
| **reordering on** | **18,290,174 (−6.2 %)** | **3.81 s (+1.2 s)** |

The short-title bench schema (name+brand only): 35.8 → 35.6 B/doc — nothing. The literature's
27 % lives on corpora whose posting lists are orders of magnitude longer than a 241 K-row
catalogue's; here the bisection has little to cluster and the block-FOR columns of `p88` already
charge small gaps little.

Latency, interleaved (`presyo-catalog`, two passes per arm): exact p50 154–156 µs off vs
160–178 µs on; typo p50 647–674 vs 656–664; typo p99 5,012–5,119 vs 4,900–4,910 — parity, within
the harness's own noise.

## The cost that decided the default is not bytes

Every existing bench — and every consumer in the estate — maps a result's doc ordinal back to
its own rows by position. With reordering on, `presyo-catalog`'s precision@10 read **0.9 %**,
because `hit.doc` no longer indexes the bench's own load order. That is a HARNESS artifact, not
an engine defect — the top-8 answers compared by key are the same set with the same scores, and
only the order within an all-ties group flips — but it is the whole adoption story: **reordering
changes what a doc ordinal means, so every ordinal-mapping consumer breaks unless it addresses
rows by key**, which the `p48`/`p53` key primitives exist for and which nothing in the estate
uses yet. `Index::posting` carries the earlier lesson verbatim: a length-sort reorder was tried
and lost (typo p99 6.40 → 7.02 ms at 1 M) on the uniform-length DepEd corpus.

So: correct, cheap to skip, worth −6.2 % on one schema and ~0 % on another, and breaking to every
ordinal-mapping consumer. **Default OFF, opt-in via `with_doc_reorder(true)` / `--reorder`** —
the same disposition as static priors (`p13`/`p14`). The row re-opens the day a host addresses
rows by key.

## Verification

- `reorder.rs` unit tests: clustering halves the gap sum on a two-cluster corpus, determinism,
  identity below the size floor, and the apply semantics.
- `reordered_build_answers_by_the_same_keys_and_scores`: 1,500 keyed documents built both ways;
  the top-k score multisets are identical and every key present in both results scores
  identically. Tie-boundary membership is asserted to be allowed to differ — the same
  arbitrariness `searcher` documents for merged near-ties.
- Full workspace gate green (139 index-text tests, 0 clippy warnings, smokes).
