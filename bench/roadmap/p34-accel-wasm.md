# P34 — the analytics kernels measured where they ship

**Tier:** T1 · **Bin:** `js/accel-bench.mjs` (new) · **Artifact:** `index_accel.wasm`, 6,342 bytes
**Status: PASS, 2026-09-06. WebAssembly keeps 72–93 % of native on five of six kernels, and loses 25× on the sixth.**

`p33` measured these kernels natively and stated the limit plainly:

> **These are host measurements, not wasm.** The kernels are `no_std` and identical, but a browser
> adds bounds-checked memory and no SIMD. Nothing here claims a browser number.

This claims one. It is also the direct answer to the goal's central question — *how do we translate
that speed to webapps* — for the analytics half.

## How it runs

The real 6,342-byte artifact, instantiated in Node, driven **exactly as onegrid's host drives it**:
the module never allocates, and the host bump-allocates every buffer above `og_heap_base()`. Memory
is grown once, up front, before any typed-array view exists — `memory.grow()` detaches every
existing view, and a view cached across a growth reads zeroes in silence.

It checks correctness in this tier as well as speed, because **a native pass cannot catch a defect
that only exists at 32-bit pointer width**, which is exactly the gap `p33`'s correction left open:

| check | result |
|---|---|
| `filter_mask` over 1 M rows vs a JavaScript count | **394,118, agrees** |
| `aggregate` COUNT vs the live-row count | **847,045, agrees** |
| `top_k(32)` indices really are in descending value order | **agrees** |

## Measured, 1,000,000 rows

| kernel | native | **wasm** | wasm / native |
|---|---|---|---|
| `aggregate` (sum) | 387.0 M rows/s | **359.2** | **93 %** |
| `top_k(100)` | 272.3 | **208.3** | 76 % |
| `filter_mask` | 191.5 | **177.2** | **93 %** |
| `group_code` | 295.3 | **216.5** | 73 % |
| `sort_pass` | 7.6 | **6.9** | 91 % |
| `bitmap_op` | 354,030 | **14,073** | **4 %** |

**Five of six kernels keep 73–93 % of native inside WebAssembly.** A million rows aggregated in
2.8 ms, filtered in 5.6 ms, grouped in 4.6 ms — in a browser tab, single-threaded, from a 6 KB
artifact with no upload and no server.

## The one that collapses, and why it is the expected one

`bitmap_op` runs at **4 % of native** — 14.1 G rows/s against 354 G. It is the only kernel whose
work is *byte-parallel*: `a & b` over `N/8` bytes, which a native compiler auto-vectorises into
32-byte AVX operations, and which the wasm32 build cannot, because SIMD is not enabled.

So the loss lands exactly where theory says it should, and nowhere else: the value kernels are
gated by dependent loads and comparisons that vectorise poorly in any target, so wasm gives up
little. **This is what "no SIMD" costs for the shipped artifact.** `p35` runs the experiment: with
`simd128` *and* non-overlapping buffers the same kernel reaches roughly 45 % of native, a 9–16x
gain. The 4 % figure is correct for what ships today and misleading as a statement about wasm.

Note also that `bitmap_op` is *still* 14 G rows/s in wasm — 0.071 ms for a million-row boolean
combination. The 25× ratio is real and worth knowing; the absolute number is not a problem.

## A measurement defect this bin found in itself, twice

The first native run printed `bitmap_op` at **0.00 ms / 215,519 M rows/s**. That is not a result: it
is a single shot below the timer's resolution, reported to six figures. The second run, after
averaging over 200 repetitions, printed `0.00 ms / 347,558 M rows/s` — still unreadable, because the
**display** carried two decimals while the value was 0.003.

Both arms now average over 200 repetitions and print three decimals. The wasm figure moved from
4,658 to 14,073 M rows/s once averaged — a **3× correction**, which is how unreliable the single
shot was. A bench that prints six significant figures from a measurement with none is worse than
one that prints nothing.

## Honest limits

- **Node, not a browser.** Same V8 and same wasm compiler tier for a hot loop, but no browser
  process model, no other tabs, and no Safari or Firefox.
- ~~**`simd128` not tested.**~~ **Done in `p35-simd128.md`** — and it corrected this document's
  reading. `simd128` gives **9–16x on `bitmap_op`**, but only with non-overlapping buffers; the
  measurement below aliased its output with an input, which blocks vectorisation entirely.
- **Single-threaded.** No workers, no shared memory, no `atomics`.
- **One shape of data.** Small integers with NaN, `-0.0` and infinities salted in — the same
  generator as `accel-kernel`, so the two tiers are comparable to each other, and neither resembles
  a wide production column.
- **Correctness here is three properties, not a differential harness.** The full 2,400-trial
  comparison is `accel-kernel`, natively; onegrid's own suite covers the wasm path at 294/294.

## Reproduce

```sh
cargo build -p index-accel --release --target wasm32-unknown-unknown
node js/accel-bench.mjs
cargo run -p index-bench --release --bin accel-kernel   # the native arm
```
