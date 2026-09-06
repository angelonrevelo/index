# P73 — the build was never bound by the thing everyone threaded

**Tier:** T1 · **Bin:** `scale` · **Check:** `a_threaded_build_is_byte_identical_to_a_serial_one`
**Files:** `crates/index-text/src/index.rs` (only)
**Status: SHIPPED, 2026-09-06. 1.89x at a million documents, bytes identical at every rung.
287 tests. No new dependency.**

`p56` closed with *"No concurrency. Every number here is single-threaded."* The obvious reading is
that `build()` and `rebuild_meta()` want threads. That reading is wrong, and measuring it first is
the only reason this landed as a real win rather than a 12 % one.

## Where the time actually went

Env-gated timers around a 1,000,000-document build, before anything changed:

| phase | ms | share |
|---|---|---|
| `add()` — tokenize + alias | 6,659 | 27 % |
| **`add()` — insert into `term_post`** | **13,198** | **54 %** |
| `build()` posting drain | 917 | 4 % |
| `build()` + `rebuild_meta()` — everything else | ~1,700 | 7 % |
| `TermDict::build` (FST), expansion, facet, numeric, `key_order` | < 40 | ~0 % |

> **`build()` and `rebuild_meta()` together were 2.6 s of 24.3 s. Threading them and nothing else
> caps out at a 12 % win.**

The cost was the inner `BTreeMap<u32, [u16; MAX_FIELD]>` in `term_post`: one node allocated and one
tree walked per (term, document) pair, ~15 M times — **sorting data that already arrived sorted**,
because `add()` assigns document ids in ascending order and a posting list is wanted in exactly
that order.

## What changed

1. **`term_post` and `term_pos` became append-only columns.** `HashMap<String, TermPost>` where
   `TermPost` is `(Vec<Posting>, Vec<[u16; MAX_FIELD]>)` and `hit(doc, field)` bumps the last entry
   when a document mentions the term again. Sorted term order is established **once**, by one
   `sort_unstable_by` in `build()` — 14 ms at 1 M. Keys are distinct, so that is a total order and
   reproduces `BTreeMap<String>` byte for byte. The posting drain became moves only, 917 ms to ~18 ms.
2. **Two threading primitives on `std::thread::scope`** — `par_index_map` / `par_slice_mut`, no new
   dependency. Chunks are claimed from a `Mutex` queue (8 per thread) because per-term work is
   wildly uneven: one term can carry 400,000 postings while its neighbours carry three.
3. **Parallel regions:** the saturation pass; one fused pass computing `block_last` / `block_max` /
   `max_sat` / `champion` per term; `first_term` binary searches; facet id columns; the numeric
   transpose; `numeric_order` per column.

**Position flattening stays serial by construction** — the flat array is *defined* by its append
order, so a threaded version has to re-concatenate in that same order. It measures 0 ms at 1 M.
`key_order` stays serial too: one string sort, 0 ms on this corpus. Flagged, not fixed.

## Measured, `bin/scale`, uncontended pairs on the same tree

| documents | before | after | speedup | bytes |
|---|---|---|---|---|
| 5,000 | 86 ms | 65 ms | 1.32x | identical |
| 20,000 | 335 ms | 247 ms | 1.36x | identical |
| 61,467 (real) | 1,234 ms | 809 ms | 1.53x | identical |
| 250,000 | 4,364 ms | 2,639 ms | 1.65x | identical |
| **1,000,000** | **18,436 ms** | **9,732 ms** | **1.89x** | **identical** |

**A correction to the lane's own report.** The worker measured 2.38x. It branched from a base that
predated `p69`, whose delta-varint had already removed part of the cost it was measuring against, so
its "before" column was slower than this tree's. Re-run as a paired A/B on the real tree the figure
is **1.89x**. The first pair measured here was worse still (1.52x) because another agent was holding
cores — *a build benchmark run next to a running build benchmark is not a measurement.*

## Byte-identity, three ways

A build that uses every core must serialize to the same bytes as one that uses a single core, or an
index can no longer be content-hashed and every consumer's cache key becomes a function of the build
machine.

- **`a_threaded_build_is_byte_identical_to_a_serial_one`** — a 7,000-document corpus that *asserts*
  it crosses the threshold, rebuilt with a `#[cfg(test)]` thread-local forcing `parallel_thread`
  to 1, compared on `to_bytes()`. Parallel against **genuinely serial**, not against a second
  parallel run — repeating the threaded build would only prove chunk order does not matter.
- **The `scale` ladder** reports identical byte counts at all five rungs, and its own
  *"reloads identically at every scale: PASS"*.
- **`pool-audit`** stays **0 (0.00%)** rank-1, top-10 and actually-worse across all six real query
  sets. A build change that moved ranking would show here.

The determinism argument is structural rather than disciplinary: every output slot is written by
exactly one `f(i)`, `f` reads only shared immutable state, and nothing accumulates across slots. No
`unsafe` anywhere.

## The threshold: `PARALLEL_MIN_WORK = 25_000`

Derived from two measurements, both recorded at the constant. `std::thread::scope` costs **~87 us
per thread on this box**; the cheapest work unit — one posting in the saturation pass — costs
**~35 ns**. A region reaches two threads only at `2 x 25_000` units, where it has ~1.75 ms of serial
work to halve against 175 us of startup: a 5x margin at the *trigger*, on the *cheapest* region.

`parallel_thread` also scales threads as `work / 25_000`, so a mid-sized build does not spawn 16
threads to do 60,000 units. **On `wasm32` this is the entire safety story**: `available_parallelism`
reports `Unsupported`, so the plan is 1 thread and a spawn is never reached.
`cargo build -p index-wasm --target wasm32-unknown-unknown --release` is clean.

## What did NOT help

- **`PARALLEL_MIN_WORK = 100_000`** was measurably *worse* than 25,000 on mid-sized corpora — a
  400 K build at 1,615 ms vs 1,175 ms — because the doc-side regions stayed serial. Sweeping
  5 K/10 K/25 K/50 K/100 K put everything from 5 K to 50 K inside noise; 25,000 was chosen on the
  overhead arithmetic, not on a noisy 3 %.
- **Threading below ~5,000 documents does nothing measurable.** The threshold exists to avoid a
  loss, not to chase a win.
- **`TermDict::build` is inherently serial and does not matter** — 30 ms of a 9.7 s build.

## Still open

- **Tokenization is now the largest single item** — 6.9 s of the remaining 9.7 s at 1 M, inside
  `add()` in `analyze.rs`. It is not threadable without buffering documents, which changes `add()`'s
  contract. **This is the next lane, and it is worth more than this one was.**
- **`key_order` is a single unthreaded string sort.** Free on every corpus measured; a keyed corpus
  in the millions has not been built.
- **Query is still single-threaded.** `p56`'s note is only half retired: this is build concurrency.
- **`to_bytes()` still materialises the whole artifact.** Unchanged by this.
