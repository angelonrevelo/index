# Portability — getting a Rust engine into every language and runtime

> Web sweep 2026-09-05. Sources dated. `UNVERIFIED` = searched, not found. The sweep's WebSearch
> quota was exhausted early, so much of this rests on direct fetches of canonical sources (MDN,
> V8/SpiderMonkey blogs, vendor docs, GitHub/crates APIs) rather than search.

**The load-bearing insight: one file format, four transports.** Design the index as term-range
ordered chunks with `u64` offsets, a small fixed block size (~1 KiB), and an aligned zero-copy body.
Then mmap it natively, Range-fetch it in the browser, page it from OPFS, and stream it from R2 —
**the same bytes, four readers.** Everything else here is a transport, not a rewrite.

> **Do not commit to a format only mmap can read. That decision is irreversible and closes WASM
> permanently.**

---

## 1. WASM — performance and hard limits

**Slowdown vs native: plan for 1.3–1.5×, tail 2×.** The canonical measurement is still Jangda et
al., USENIX ATC'19 — **45 % (Firefox) / 55 % (Chrome) mean slowdown, peak 2.08× / 2.5×** on SPEC CPU
([arXiv 1901.09056](https://arxiv.org/abs/1901.09056)). **No 2024–2026 replication of that
methodology exists (UNVERIFIED)** — treat 1.45–1.55× as unsuperseded. The best modern proxy: V8's
2025 speculative-optimization work moved **SQLite3-wasm by 1 %**, real workloads "between 1 % and
8 %" ([v8.dev, 2025-06-24](https://v8.dev/blog/wasm-speculative-optimizations)). **The gap is
memory-bound, not compiler-bound; it is not closing.**

**SIMD128 is safe to require** — Safari 16.4+, **95.69 % global**
([caniuse](https://caniuse.com/wasm-simd)). **relaxed-simd is not** — Safari flag-only, Firefox 145.
Both are Phase 5 in `features.json`, so **phase ≠ shipped**. Measured SIMD speedup: MediaPipe
hand-tracking **14–15 → 38–40 FPS ≈ 2.6×**. But **no published SIMD128 numbers for string search /
bitmap / UTF-8 work (UNVERIFIED)** — simdutf does not even list wasm as a target. Wider SIMD
(`flexibleVectors`) is **Phase 1**, so **128-bit lanes are a permanent ceiling**. Emscripten
documents `i8x16` shifts lowering to **5–11 x86 instructions** and saturating truncation to
**8–14** — keep variable shifts out of bitmap loops.

**Memory.** wasm32 caps at **4 GB**. memory64 is Phase 5 (Chrome 133 / Firefox 134 / Node 24 /
Wasmtime 30) but **absent from Safari entirely**, and costs **10 % to over 100 %**, because wasm32 on
a 64-bit host elides bounds checks via a 4 GiB guard reservation
([spidermonkey.dev, 2025-01-15](https://spidermonkey.dev/blog/2025/01/15/is-memory64-actually-worth-using.html)).

> **Stay on wasm32 and shard. But make on-disk offsets `u64` now** — 32-bit `usize` was a named
> blocker in Tantivy's browser PR, and it is not fixable after the format ships.

**Instantiation.** Liftoff code is **~50 % slower than TurboFan** on desktop, and tier-up is eager.
Chrome **does not cache modules under 128 kB**, caches only TurboFan output, and compiled code is
**5–7× the `.wasm` size** ([v8.dev](https://v8.dev/blog/wasm-code-caching)). Compile ms/MB:
**UNVERIFIED** — measure the real artifact.

**Boundary.** A raw exported call is **~2–5 ns** (2.5 ns monomorphic; no re-measurement since 2018,
UNVERIFIED). Irrelevant. What costs is **wasm-bindgen marshalling** — every `String`/`Vec<u8>` is an
O(n) copy plus allocation. Reading *out* is free via `new Uint8Array(memory.buffer, ptr, len)`.

> **The trap: `memory.grow()` detaches every view — even `grow(0)`**
> ([MDN](https://developer.mozilla.org/en-US/docs/WebAssembly/JavaScript_interface/Memory/grow)).
> Resizable-ArrayBuffer integration is still open (spec issue #1292, since 2021).
>
> **onegrid's existing hand-written raw-pointer C ABI with no wasm-bindgen is exactly right**
> (`demand.md` Finding 4). One call per query, return `(ptr, len)`, re-derive the view every time.
> Its `WasmHeap` bump allocator with JS owning the heap is the correct shape, not a workaround.

### How the real projects do it — they converged

- **DuckDB-Wasm** joins two Parquet files (11.8 MB + 2.6 MB = 14.4 MB) in **40 ms cold / 6 ms warm,
  reading only 475 KB — 3.3 %** — via HTTP Range over synchronous XHR with **exponentially-growing
  readahead**, skipping row groups on predicates
  ([PVLDB 15(12):3574](https://www.vldb.org/pvldb/vol15/p3574-kohn.pdf)). Beats sql.js **4–11×**
  (TPC-H geomean 0.073 s vs 0.809 s at SF=0.5). Publishes **no wasm-vs-native factor (UNVERIFIED)**
  and dropped SF=1.0 because of the 4 GB cap. Current docs warn the *built-in* httpfs extension may
  still download whole files.
- **Pagefind** searches **10,000 pages in under 300 kB total payload** (typically ~100 kB) using
  term-range-ordered chunks — its own docs build is **2,496 pages → 27 index chunks** — with result
  *fragments* fetched separately from index chunks, and per-language gzipped wasm.
- **tantivy-wasm** searched a **14 GB index downloading ~1.5 MB in ~2 s** using a `FileSlice`/XHR
  Directory with **32 KB chunks and speculative prefetch** ([PR #1067](https://github.com/tantivy-search/tantivy/issues/1067)).
- **Orama is pure JS/TS, not wasm at all** — its "21 µs" claim is marketing-grade, corpus unstated
  (UNVERIFIED).
- **sql.js** holds the whole DB in linear memory and inherits the 4 GB cap hard; **wa-sqlite** ships
  7+ VFS variants but **publishes no numbers (UNVERIFIED)**.

## 2. Browser storage

**localStorage is disqualified.** 5 MiB, **UTF-16 (2×)**, synchronous, `Window`-only — under 2 MB of
usable binary after base64.

> profstopick's 2.5 MB JSON at 95.6 % of quota is **the expected outcome, not a bug.** The fix is
> not a better store; it is not shipping the index (§ item 3).

**Quota is a non-issue at 5–50 MB.** Chrome/Edge **60 % of disk**; Firefox **min(10 % of disk,
10 GiB group limit)**; Safari 17+/macOS 14+ **~60 %, with no prompts since Safari 17.0**
([MDN, 2026-01-05](https://developer.mozilla.org/en-US/docs/Web/API/Storage_API/Storage_quotas_and_eviction_criteria);
[WebKit, 2023-08-10](https://webkit.org/blog/14403/updates-to-storage-policy/)) — **the widely-cited
"Safari = 1 GB with prompts" is dead.** Embedded WebViews get only 15 %.

**Eviction is the real constraint**, and two facts dominate:

1. Eviction is **whole-origin and atomic** — IndexedDB, Cache API and OPFS are deleted together,
   never partially.
2. **Safari deletes all script-written storage after 7 days without user interaction.** Installing to
   the home screen exempts you from the 7-day rule but does **not** raise quota.

> **Consequence: any persisted index is a one-week cache on Safari. Always ship a rebuild path.**

**OPFS is the right store.** `FileSystemSyncAccessHandle` gives `read(buffer, {at: offset})` — **the
only random-access primitive in the browser, and the mmap analogue.** Chrome 102 / Firefox 111 /
Safari 15.2, sync `truncate/getSize/flush/close` from Safari 16.4, **95.48 % global**.
**Dedicated-worker only, every browser, no exceptions** — the "Safari allows it on main thread"
rumour is false per WebKit's own launch post. SQLite's **`opfs-sahpool` VFS needs no COOP/COEP**,
works everywhere since March 2023, and is "easily the highest OPFS performance"; the plain `opfs` VFS
needs cross-origin isolation and is broken below Safari 17
([sqlite.org](https://sqlite.org/wasm/doc/trunk/persistence.md)).

**OPFS vs IndexedDB MB/s: UNVERIFIED** — no credible current benchmark exists. Treat any "OPFS is
N× faster" figure as unsourced.

**Cache API + Range is a trap.** MDN documents nothing about 206 in `Cache`, and **Cloudflare's Cache
API explicitly rejects 206.** Store whole shards as normal 200s; do Range only against
network/CDN, send `If-Range` with an ETag, and handle the silent **200-instead-of-206** fallback that
edge compression causes.

> **Practical ceiling: ship ≤ 5 MB eagerly, shard above that, treat ≥ 20 MB as strictly lazy.**

## 3. Native bindings

**napi-rs 3.12.2** (2026-08-21) is the Node answer — **~25 target triples** (linux gnu/musl × 5
arches, macOS × 2 + universal, Windows msvc × 3, android × 2, freebsd, wasi, ohos, loongarch,
riscv64, s390x, ppc64le), shipped as `@scope/pkg-<triple>` optionalDependencies. Used by SWC, Prisma,
Rspack. Requires Rust 1.88, Node ^20.17/^22.13/≥23.5. Neon is the smaller ecosystem; **no published
head-to-head call-overhead numbers (UNVERIFIED)**.

**PyO3 0.29.2 / maturin 1.15.0 — and the 2026 landmine.** **abi3 cannot be used with free-threaded
Python**: *"the free-threaded build uses a completely new ABI and there is not yet an equivalent to
the limited API"*; PyO3 warns and ignores the setting
([pyo3.rs](https://pyo3.rs/v0.29.2/free-threading.html)). Free-threading is official in CPython 3.14.
**The one-wheel-per-platform abi3 promise dies the moment you support `3.13t`/`3.14t`**, and the
matrix reverts to per-version × per-platform.

**uniffi** (MPL-2.0, active 2026-09-04): first-party **Kotlin, Swift, Python full; Ruby partial and
frozen**. Correct tool for mobile — XCFramework plus per-ABI `.so`.

**C ABI + cbindgen 0.29.4 is the actual lowest common denominator.** Go (cgo), PHP FFI, Ruby FFI,
LuaJIT all consume it. cgo per-call overhead ~50–100 ns is **UNVERIFIED**; wazero is the pure-Go
escape hatch from cgo cross-compilation pain. **onegrid's hand-written raw-pointer ABI generalizes
directly to native FFI — it is the same artifact.**

**Component Model: not ready, and the phase number says so.** **Phase 1** in the official proposals
list — a Bytecode Alliance convention, not a browser feature; no browser loads components natively
(jco transpiles to core wasm + JS). WASI 0.3 with native async launched 2026-06-11, and "The Road to
Component Model 1.0" (2026-06-08) confirms 1.0 is still ahead. **`cargo-component`'s last release was
2025-04-07 — 17 months stale.** Do not bet distribution on this in 2026.

**Maintenance cost, concretely:** sqlite-vec is the honest benchmark — **12 loadable + 7 static +
amalgamation + cosmopolitan CLI ≈ 21 artifacts per release**, covering android × 4, ios × 3,
linux × 2, macos × 2, windows. That is the per-release tax, and it compounds per language ecosystem.

## 4. Edge runtimes

**Cloudflare Workers — and the limit changed the day before this research.** Worker size is now
**64 MiB uncompressed on both free and paid** (changelog **2026-09-04**); the old 1 MB free / 10 MB
paid gzipped ceiling is gone. Memory **128 MB per isolate**, JS heap + wasm combined,
**per-isolate not per-invocation** — so **a warm isolate caches hot shards across requests.** CPU
30 s default / 5 min max (paid; free 10 ms), **1 s global-scope startup budget**, 6 simultaneous
connections, 50/10,000 subrequests.

> **Hard constraint: `WebAssembly.instantiate` accepts pre-compiled modules only** — the engine must
> be in the bundle; only *data* can stream.

Storage: **R2 egress free**, Class B $0.36/M, 5 TiB objects; **KV values 25 MiB**; **D1 10 GB/DB
(500 MB free)**; **Durable Objects 128 MB memory + 10 GB SQLite each**; Hyperdrive for Postgres
pooling; Vectorize 20 M vectors, 1536 dims, topK 50. **Cache API rejects 206.**

**Vercel Edge is dead for this** — 1 / 2 / 4 MB gzipped by plan, and `runtime = 'edge'` is
**unsupported from Next.js 16.3** ([vercel.com, updated 2026-08-03](https://vercel.com/docs/functions/runtimes/edge)).
The correct Vercel target in 2026 is a **Fluid compute Node function: 4 GB memory, 250 MB bundle** —
but that is regional, not edge.

**Fastly Compute** looks ideal (Wasmtime, 100 MB packages) and is not: **128 MB heap, 50 ms CPU per
request, and a fresh sandbox per individual request** — no amortization of index load. Its "35 µs
cold start" figure is **UNVERIFIED**. **Netlify Edge**: 512 MB but the same 50 ms cap.
**CloudFront Functions**: 10 KB, 2 MB memory, no network, no WASM — categorically out.
**Deno Deploy**: Classic sunsets 2026-07-20; the new platform bills at a 768 MB memory unit, but
**no hard per-isolate limit is documented (UNVERIFIED)**.

> **Cloudflare Workers is the only mainstream edge runtime that can host a real index** — ~100 MB
> practical in-memory, 64 MiB bundled, Pagefind-style chunks in R2 for anything larger.

## 5. In-database extension — the door closed in 2026

**pgrx 0.19.2** (2026-07-30). **ParadeDB 0.25.6** (2026-08-27), **AGPL-3.0** — Tantivy + DataFusion
in Postgres via pgrx, giving a BM25 index and the `@@@` operator.

**The deployment blocker is severe and specific:**

| Platform | pg_search / ParadeDB |
|---|---|
| **AWS RDS / Aurora** | **NO.** Absent from the PG17/18 extension list; only AWS-vetted extensions install. You get pg_trgm, pg_bigm, pgvector 0.8.2. |
| **Supabase** | **NO.** Not in the docs; `pg_search` returns **zero code hits across `supabase/postgres`**. You get tsvector, pg_trgm, pgvector. |
| **Neon** | **WAS, NOW NO.** Docs state pg_search is **deprecated: "Not available for new Neon projects. Existing installs must migrate before September 21, 2026."** That is **16 days from this sweep.** |
| **Works where you control the image** | Railway, Render, Fly.io, DigitalOcean, Dokku — per ParadeDB's own README |

> **The in-database path is closed on three of the four largest managed Postgres platforms.** It
> remains viable on a VPS you own — which is exactly presyo's situation (self-hosted PG16 on a SG
> VPS). It blocks the *general product*, not the *first customer*.

**SQLite:** loadable extensions **cannot be dynamically loaded in WASM** — they must be statically
compiled in. Same on iOS/Android system SQLite and Cloudflare D1. `better-sqlite3` supports loading;
Python's `sqlite3` often ships with `enable_load_extension` disabled.

## 6. Sidecar — rejected, for a better reason than "the socket is slow"

Measured IPC round-trips ([goldsborough/ipc-bench](https://github.com/goldsborough/ipc-bench), 100 B
messages): **shared memory 0.21 µs · unix domain socket 7.7 µs · TCP loopback 14.2 µs · ZeroMQ/TCP
40.2 µs.** Against a sub-millisecond query, UDS is **1–3 %**. **The transport is not the argument.**

**The codec is.** `serde_json` deserialize of a 6 MB payload is **103.59 ms** versus **5.6 ns** for an
rkyv archive read — **moving a sidecar off JSON is ~7 orders of magnitude more valuable than moving
it off TCP.**

The real argument is tenancy and embeddability:

| Engine | Version | Artifact | Resident cost |
|---|---|---|---|
| **Meilisearch** | 1.53.1 | 133 MB binary | **305 MB RSS for a 9.1 MB corpus** (their table: 224 MB disk, **205 GB virtual** from the LMDB map reservation); ~10–24× disk multiplier |
| **Typesense** | 30.2 | **380 MB image** | 20 MB idle, then **2–3× searchable-field bytes, hard-resident**; ≥ 2 vCPU |
| **Qdrant** | 1.19.1 | 74 MB image | always mmaps vectors; **default `cached` tier pre-warms into RAM at startup** |
| **sonic** | 1.8.1 | **8 MB tarball** | ~30 MB — but it is an *identifier* index, not a document index |

**And the two engines you would most want as a sidecar cannot be embedded at all.** Meilisearch
closed that door deliberately: crates.io `milli` is a **0.1.0 placeholder published 2020-07-15**,
while the real crate carries **`publish = false`** in the monorepo — and it is LMDB/`heed`-backed, so
mmap by construction, so it would not port to WASM either. Typesense and Qdrant are server-only by
architecture.

**Only two genuinely embeddable engines exist:** Tantivy 0.26.1 (100+ commits since 2026-06; Datadog
employment has not slowed Masurel/Seitz) and **SeekStorm 3.3.8** (2026-09-03) — explicitly
"in-process library & multi-tenancy server". SeekStorm is the closest existing thing to `index`; at
~320× less adoption than Tantivy and with vendor-authored benchmarks, it is a **reference design,
not a dependency.**

> **Verdict: no sidecar.** presyo's box already runs Node, Nginx and a 15 GB Postgres on one SG VPS.
> A 380 MB image needing 2–3× resident RAM is the worst possible tenant for it — and a sidecar buys
> centralization that is not needed while costing a serialization boundary that is.

## 7. Zero-copy and mmap

Tantivy's `MmapDirectory` uses **memmap2 0.9.11**, caches mappings weakly, and hands up `OwnedBytes`,
so postings reads are **zero-copy slices straight off the page cache**. **But Tantivy issues no
`madvise` by default** (`madvice_opt: None`) — you get `MADV_NORMAL` fault-around, which reads
**128 KB per 4 KB fault**, brutal for scattered postings seeks.
`MmapDirectory::open_with_madvice(path, Advice::Random)` is a free, apparently unexercised lever.

**The CIDR'22 mmap paper is on our side, explicitly.** Crotty/Leis/Pavlo
([PDF](https://db.cs.cmu.edu/papers/2022/p13-crotty.pdf)) measured mmap at **2–20× worse than fio** —
random reads collapsing to near-zero for ~5 s once the page cache filled, TLB shootdowns at
**1.5–2.0 M/s**, a 10-SSD RAID showing ~20× gap. Its own §6: *"maybe use mmap… your working set (or
the entire database) fits in memory and the workload is read-only. Otherwise, never."* **That is the
exact definition of immutable search segments on one VPS** — no dirty pages, so the
transactional-safety problem is structurally absent, and the collapse conditions are unreachable on
one disk. Residual risks: **`SIGBUS` on I/O error** (unhandleable in-process — SQLite's own mmap doc
concedes the same) and read amplification.

**Lance is the clean counter-example**: **exactly one `mmap` reference in the whole repo, in
test-data generation.** The read path is `lance-io/src/scheduler.rs` — a userspace prefetcher with a
`BinaryHeap` priority queue, IOPS/bytes atomics and explicit backpressure. Range reads plus your own
scheduler is what portable looks like. (Its "100× faster random access" is repo marketing;
methodology page 404s — **UNVERIFIED**.)

**mmap does not exist anywhere in WASM, and wasi-libc lies about it.** WASI preview2's
`wasi:filesystem` exposes only positional `read`/`write`. **wasi-libc's `mmap` is a fake** — its own
comment says it *"just allocates memory with malloc and reads and writes data with pread and
pwrite"*; Emscripten does the same. The browser has none by design (no MMU exposure).

> **So an LMDB/mmap-based index does not fail to port. It silently `malloc`s and `pread`s your entire
> index into linear memory, with no way to opt out.** That is the worst failure mode available: it
> looks like it works.

Three replacements: **(a)** whole index in linear memory (fine ≤ 50 MB); **(b)** OPFS
`read(buf, {at})` as a manual pager, worker-only; **(c)** HTTP Range as the pager — phiresky's
sql.js-httpvfs answers a query against a **670 MiB** database in **10–20 GETs / 130–270 KiB**, and a
full-text query against an ~8 MiB FTS index by fetching **~70 KiB**, with **1 KiB pages** and
exponential read-ahead. **Tantivy's `Directory` trait is precisely the seam where (b) or (c) plugs
in.**

**Alignment.** rkyv 0.8.18 requires it: `AlignedVec<16>` default, **root stored at the *end* of the
buffer** (a trailing byte breaks `access`), **32-bit offsets by default**. WASM tolerates misaligned
loads as a *hint*, but **Rust UB does not care** — constructing `&T` from a misaligned pointer is UB
on every target. [rkyv #575](https://github.com/rkyv/rkyv/issues/575) is exactly this bug, hit
passing bytes *"over the wire via wasm and `Uint8Array`"*. **Assume OPFS reads are unaligned** (no
spec guarantee).

rkyv performance ([rust_serialization_benchmark](https://github.com/djkoloski/rust_serialization_benchmark),
2026-08-21, EPYC 7763), 6 MB `mesh`: `serde_json` deserialize **103.59 ms** · rkyv `read` **5.6 ns**
· rkyv `access` **1.2459 ns** · flatbuffers 44.8 ns · capnp 13.8 ms. **But rkyv's *deserialize*
(1.56 ms on `log`) is no better than bitcode** — the win exists *only* if the query path reads
directly out of the archive and never materializes owned types.

> **The correct pattern:** allocate the destination *inside* WASM linear memory with an aligned
> allocator (`AlignedVec<16>` or `std::alloc::alloc` with an explicit `Layout`), export that offset
> to JS, and have `read()` fill *that* region. **Never read into a JS-allocated buffer and copy to an
> arbitrary offset.** Use `bytecheck` across trust boundaries; `access_unchecked` + a checksum is
> fine for indexes we produced.

---

## Recommended distribution strategy

| # | Artifact | Ships to | Specific limitation it hits | Effort |
|---|---|---|---|---|
| **1** | **Core crate + chunked format** — `u64` offsets, block-addressed, aligned zero-copy body, `Directory`-style trait over mmap ∥ Range ∥ OPFS | foundation for all of it | None yet — but 32-bit `usize` on wasm32 kills you later if offsets aren't `u64` now, and an mmap-only format closes WASM forever | **3–4 wk** |
| **2** | **wasm32 + hand-written C ABI** (no wasm-bindgen — reuse onegrid's pattern), SIMD128 required, relaxed-simd not | the profstopick SPA | 1.3–1.5× native; 128-bit lanes forever; `grow(0)` detaches views; keep `.wasm` > 128 kB for Chrome caching (cached code is 5–7× its size); ship gzipped | **2–3 wk** |
| **3** | **Sharded static index, fetch-only-what-you-touch** — index chunks separate from result fragments (Pagefind model) | same SPA; **zero new infra** | Nothing persisted means nothing to evict. Pagefind does 10,000 pages in < 300 kB — **this alone solves the 95.6 %-of-quota problem** | **1–2 wk** |
| **4** | **napi-rs native module** | the presyo/sisia Node backends, at native speed | ~25 platform packages via optionalDependencies; the CI matrix is the cost, not the code | **1–2 wk** |
| **5** | **OPFS worker pager** (`opfs-sahpool` style, no COOP/COEP) for indexes > 5 MB | offline / repeat-visit SPA | Worker-only everywhere; whole-origin atomic eviction; **Safari's 7-day rule makes this a one-week cache** unless installed. Always ship a rebuild path | **2 wk** |
| **6** | **Cloudflare Worker** — engine in the 64 MiB bundle, shards in R2 via Range | edge / multi-region | 128 MB isolate; **no runtime wasm compilation**; Cache API rejects 206; 6 concurrent connections | **2 wk** |
| **7** | **PyO3 / maturin wheels** | Python consumers | **abi3 is dead for free-threaded 3.13t/3.14t** — matrix reverts to per-version | **1–2 wk** |
| **8** | **cbindgen C ABI + header** | Go, PHP, Ruby, Lua, anything with an FFI | Manual memory contract at every call site; cgo per-call cost UNVERIFIED | **1 wk** |
| **9** | **uniffi** (Swift / Kotlin) | mobile | Per-ABI `.so` + XCFramework; binary size; Ruby support frozen | **2–3 wk** |
| **10** | **pgrx extension** | **a VPS Postgres you own, only** | **RDS no, Supabase no, Neon deprecated (migrate by 2026-09-21).** Only Railway / Render / Fly / DigitalOcean | **4+ wk** |

**Explicitly do not build:** a Vercel Edge artifact (1–4 MB, deprecated in Next 16.3), a Fastly
artifact (50 ms CPU, fresh sandbox per request), a WASM Component (Phase 1; `cargo-component` stale
17 months), or memory64 support (10–100 % slower, no Safari). And **do not start with a sidecar** —
on one VPS it is a second failure domain for a boundary cost you cannot measure.

**Order matters.** Items **1 → 2 → 3** deliver the highest-pain win — the SPA eating 95.6 % of
localStorage — in roughly **6–9 weeks with no infrastructure at all**, and they do it **by not
shipping the index rather than by storing it better.** Items 4–6 then reuse the identical format at
zero marginal format cost. Everything from 7 onward is distribution surface rather than engineering:
the cost is CI matrix maintenance (sqlite-vec's ~21 artifacts per release is the realistic tax), it
compounds per platform, and it should be added only when a real consumer asks.

## Five things worth an afternoon of measurement before committing

All **UNVERIFIED** in this sweep, all cheap to settle, all gate a decision above:

1. **OPFS vs IndexedDB throughput** on the target shard size — no credible current benchmark exists.
2. **Cache-API behaviour with 206** across all three engines.
3. **CDN Range support with compression enabled** — the silent 200-instead-of-206 fallback.
4. **iOS practical WASM memory ceiling.**
5. **rkyv checked-vs-unchecked validation cost** on our own shard size.
