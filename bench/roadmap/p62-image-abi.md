# P62 — the image tier through the C ABI

**Tier:** T2 · **Bin:** `js/image-smoke.mjs` · **API:** `index-wasm` v12
**Status: SHIPPED, 2026-09-06.** ABI 11 -> **12**, 49 -> **61 symbols**, no `wasm-bindgen`, zero
module imports. The same fused query returns **identical documents in identical order** in Rust,
Node, **headless Chromium 147** on the main thread, and **Python `ctypes` in the standard library
alone**. Costs reported rather than buried: artifact **+13.9% raw / +14.3% gzipped**
(181,798 -> 207,823 B); linear-memory high-water **1.79 MiB** in Node, **3.20 MiB** in a browser
tab; extrapolated wasm32 ceiling **1.63 M images at 512-d**.
Closing the browser arm caught a latent false green: `artifact/web/` held an **ABI-2 module from
2026-09-05**, so that check had been passing against a build nobody had made that day.

`index-wasm` is the reason this project's claim is "one index format serves a browser, a server, an
edge worker and a database" rather than "one Rust crate". A hand-written C ABI, **no
`wasm-bindgen`**, so one artifact serves the browser, Node, edge workers and native FFI from Go,
Python, PHP and Ruby. `include/index.h` is that ABI as a real C header and `host/python/` proves it
in the standard library alone.

An image tier that only Rust can call would forfeit the whole thesis — and forfeit it precisely
where it matters most, because [`docs/research/image.md`](../../docs/research/image.md) §1 shows
**every FOSS incumbent requires a server**, and §2 shows the browser is where they fail.

## What must be added

| | |
|---|---|
| `idx_image_push_vector(ix, doc, ptr, len)` | Vectors cross the boundary as a flat `f32` buffer the host already owns. |
| `idx_image_search_vector(ix, ptr, len, k, oversample)` | The `p58` pipeline. |
| `idx_image_search_fused(...)` | The `p59` claim, callable from JavaScript. |
| `idx_image_hash_near(ix, hi, lo, max)` | Near-duplicate lookup over the hash column. |
| `idx_image_why(hit)` | Which signal found it — the explicability `p59` requires, preserved across the boundary. |
| `include/index.h` | Extended, since the header is the contract, not documentation of it. |

ABI 11 -> **12**.

## The decision this row turns on

**The vector never crosses as JSON.** A 512-d `f32` embedding is 2 KB raw and roughly 6 KB as JSON
text, and it must be parsed on arrival. At the ingest rates §8 records — `rclip` managed 1.28 M
images in 3 hours — serialisation would dominate the boundary cost for no benefit. The host writes
`f32` into linear memory at an offset the module hands out, exactly as the existing analytics
kernels do.

**The corollary is a real constraint, and `portability.md` §1 already names it:** an mmap-based
format does not fail to port to WASM, it silently reads the whole index into linear memory. A vector
column is the largest thing this engine has ever put in that memory. The benchmark must therefore
report **linear-memory high-water mark**, not just latency, and state the corpus size at which a
32-bit address space becomes the binding limit.

## Acceptance

1. `js/image-smoke.mjs` builds an index from real corpus data, runs a fused query in **Node** and in
   **headless Chromium**, and matches the Rust result exactly — same documents, same order.
2. Artifact size reported raw and gzipped, before and after, since gzipped is what a browser
   downloads. A regression is a finding, not a rounding error.
3. **Linear-memory high-water mark** printed at each corpus size, with the extrapolated ceiling.
4. `host/python/` exercises the new symbols through `ctypes` in the standard library alone, proving
   the ABI is genuinely language-neutral rather than JavaScript-shaped.
5. No `wasm-bindgen` dependency appears. If it does, the row has failed regardless of its numbers.
