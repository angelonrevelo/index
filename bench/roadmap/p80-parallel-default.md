# P80 — the query fan-out is opt-in now, because the default was 2.4x slower here

**Tier:** T1 · **Bin:** `segment-scale` · **Files:** `crates/index-text/src/searcher.rs`
**Status: SHIPPED, 2026-09-06. Corrects `p74`'s default. 318 tests.**

`p74` shipped the threaded per-segment fan-out **enabled by default**, gated on
`segment_count * doc_count >= 4_500_000`, with `INDEX_PARALLEL=0` as a kill switch. Its own
"Still open" said the speedup had not been independently reproduced. This reproduces it, and the
result inverts the default.

## The measurement

`segment-scale` on presyo's 241,789 real products, alternating `INDEX_PARALLEL=0` and `=1` so both
arms meet the same machine. At the two rungs the gate actually selects:

| segments | serial p50 | threaded p50 | |
|---|---|---|---|
| 25 | 5,141 us / 5,792 us | **12,528 us** | **2.2–2.4x SLOWER** |
| 50 | 14,235 us / 11,296 us | **26,966 us** | **1.9–2.4x SLOWER** |

Threaded was slower at **every** rung, not just these. And below the gate the penalty is savage —
forcing 2 segments onto threads took p50 from 68 us to 5,572 us — which is a useful control: it
confirms the gate itself is doing its job, and that what fails is the decision to thread at all on
this machine.

25 segments x 241,789 documents is 6.0e6, over the 4.5e6 gate, so **the threaded column above is
literally what `p74` shipped as the default.**

## Why both this and `p74` are correct

`p74`'s numbers were taken on a quiet box and are not disputed. This machine, during this
measurement, was running its owner's ordinary applications — a browser, two music/chat clients, and
a game holding 3.3 GB. Every figure above is inflated roughly 4x against `p74`'s on both arms, which
is the signature of a loaded machine rather than of a bad benchmark.

`p74` predicted this exactly, and then defaulted the other way:

> **The threaded path degrades badly under machine contention.** Two of seven runs landed while the
> box was busy: one showed a 50-segment p50 of **36,271 us — 4x worse than serial**. Spawning a
> thread per core per query on an already-saturated machine is pathological.

The error was not the measurement. It was treating "contended" as an exceptional condition worth a
kill switch, when for an **embedded library it is the ordinary one.** `index` runs inside somebody
else's process. It does not own the cores, it cannot see the load, and
`std::thread::scope` spawns fresh OS threads on every call — a bet that the cores are free, placed
by a library with no way to check.

## What changed

`wants_thread` no longer threads unless asked:

- **`INDEX_PARALLEL=1`** enables it for collections that also clear the size gate. Anything else,
  including unset, stays serial. The variable was a kill switch defaulting on; it is now an opt-in
  defaulting off.
- **`Searcher::set_parallel(bool)`** is the programmatic form, so an embedder that *does* own its
  machine does not need an environment variable. **`Searcher::parallel()`** reports what would
  happen.
- `PARALLEL_WORK_MIN`, the work estimate, the `AtomicUsize` claim queue and the segment-index sort
  are all untouched. **Nothing about `p74`'s machinery was wrong** — only its default.

Confirmed after the change: the default run gives 25-segment p50 **7,088 us** and 50-segment
**14,958 us**, in the serial range rather than the threaded one, with every quality column identical
(broad overlap 100.00/84.45/84.41/85.80/87.01/84.94).

## What this costs

**A real win on a dedicated machine now needs one line to claim.** `p74` measured 2.9x on the p99 at
25 and 50 segments on a quiet box, and that remains available and gated — a search server that owns
its hardware should set `set_parallel(true)` and will get it.

That is the right way round. **A default that is 2.4x slower on a shared machine is a worse failure
than an opt-in that is 2.9x faster on a dedicated one is a win**, because the first is silent and
happens to people who never read this file.

## Still open

- **Nothing measures the load and decides.** An engine that could see contention could re-enable
  itself automatically. Every mechanism for that is either unportable or unreliable, so this ships
  as a switch a human sets.
- **The idle-box number is still `p74`'s.** This machine has not been quiet at any point in this
  work, so the 2.9x tail win has been reproduced by nobody but the lane that found it.
- **`p73`'s build threading keeps its default**, and is not covered by this. It is a batch operation
  where the caller is usually waiting on exactly that work — a different bet from spending a
  process's cores on one query.
