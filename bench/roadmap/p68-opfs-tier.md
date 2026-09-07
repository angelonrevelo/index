# P68 — the index lives in the browser now, and nobody else can follow

**Tier:** T1 · **Check:** `js/opfs-check.mjs` (in CI) · **Files:** `js/opfs.mjs`, `js/opfs-worker.mjs`, `js/opfs.html`
**Status: SHIPPED, 2026-09-06. 0.086 ms per query, zero network calls, 296 bytes to open a file of
any size. 268 tests.**

`docs/research/landscape.md` §7.5 named a position and, unusually for a gap analysis, explained why
it is empty:

> There is **no engine** where the same index file serves an OPFS-backed browser client, an embedded
> server process and an object-store cold tier. Pagefind proves chunked static search works, but its
> index is **build-time-frozen and bespoke**. **LanceDB closed WASM as "not planned"; Tantivy's WASM
> RFC has been open since 2019 and is read-only; Orama's Rust core has no wasm32 target.**

It is not neglect. It is storage layers:

| | why it cannot follow |
|---|---|
| **Tantivy** | mmap. `p7` recorded the failure mode exactly: *an mmap format does not fail to port to WASM — it silently succeeds and reads the whole index into linear memory.* |
| **Meilisearch, Elasticsearch** | servers. There is no "run it in the tab" mode to add |
| **LanceDB** | WASM closed as "not planned" |
| **Orama** | Rust core has no wasm32 target |
| **Pagefind** | works, but its index is frozen at build time and is not the server's index |

## What shipped

**`js/opfs.mjs`** — persist and load an index from the browser's own filesystem. Writes through a
`.partial` name first, because a tab closed mid-write must not leave a half-index that `idx_open`
refuses; the same reason `collection.rs` writes a temp file and renames.

**`js/opfs-worker.mjs`** — range reads. `createSyncAccessHandle().read(buf, { at })` is worker-only
in every engine that ships it and is the **only** OPFS API that reads a slice without materialising
the file.

**`js/opfs.html`** — the demo, and **`js/opfs-check.mjs`** — the same page driven by a real Chromium
in CI.

## Measured, in a real browser

| | |
|---|---|
| first visit, over HTTP | 17.7 ms, 386,043 B |
| **second visit, from OPFS** | **2.8 ms** |
| **network calls during `search()`** | **0** |
| **per query, in-tab** | **0.086 ms** |
| localhost round trip, *doing no work* | 2.300 ms |
| **speed-up over a server that does nothing** | **~27x** |
| survives a full page navigation | yes, 386,043 B still resident |

**The server comparison is deliberately the friendliest one available**: same machine, no TLS, no
queue, no distance, and no work at the other end. A deployed search service is strictly slower than
that floor, so 27x is a *lower bound* on the gap rather than a favourable framing.

**Counting fetches is the claim, not timing it.** A fast answer could be an HTTP cache hit — still a
request, still fails offline. The page patches `window.fetch` and asserts the counter does not move
across `search()`.

## The 296 bytes are the point

`read_section_table` needs `MAGIC.len() + TABLE_BYTE` — **296 bytes** — to learn where all eighteen
sections live. The worker reads exactly that from a sync access handle and decodes the table in
JavaScript:

```
the whole layout from 296 bytes            file is 386,043 B  (0.077 % of it)
section table decoded in-browser           posting span 303,336 B at 66,835
a single section read without the rest     4,096 B of the dictionary
```

`p56` measured a 10 M index at ~910 MB. **A tab cannot hold that; it can open it.** That is what the
fixed-offset section table has been for since `IDXTEXT1`, and `a_single_posting_list_is_range_readable`
has asserted it in Rust for just as long — but nothing had ever exercised it from a browser, which
is where the property actually earns its keep.

## What it does for the consumer that exists

profstopick ships a **2,505,813-byte JSON shard occupying 95.6 % of the 5 MB localStorage quota**,
and its own instrumentation measured **109 of 267 real searches returning nothing — 40.8 %
zero-result**, mostly name-shaped queries whose professor was already in the corpus.

| | JSON shard | this |
|---|---|---|
| bytes | 2,505,813 | **386,043 (15.4 %)** |
| quota | 95.6 % of 5 MB | a rounding error in a GB-scale quota |
| name order reversed | miss | **hit** |
| one transposition | miss | **hit** |

> **CORRECTED, 2026-09-07.** The `name order reversed` row is **no longer true of profstopick's
> matcher**, and the `bytes` row predates `p69`. Their `search-match.ts` now carries a backtracking
> distinct-token assignment that resolves reversed names on its own — read and ported in
> `js/profstopick-match.mjs`, and verified: `raphael abacan` reaches `ABACAN, RAPHAEL` in *their*
> code. Only the transposition row still separates the two. The artifact is now 220,470 bytes
> (8.8 % of the quota). See the `p81` entry in `CHANGELOG.md`.

The page asserts those last two against the real artifact: `RAPHAEL ABACAN,` and `ABACNA, RAPHAEL`
both resolve to `ABACAN, RAPHAEL`.

## A bug this found in itself

`drop()` called `removeEntry` without `await` inside a `try`, so the rejection escaped as an
unhandled promise rejection and printed a console error on every first visit. The check surfaces
console errors, which is how it was caught — a demo that hides its own console is not a check.

## Still open

- **Nothing writes to OPFS from the engine side.** `apply` runs in the CLI; a browser can persist and
  query an index but cannot yet update one in place. The ABI has the keys for it since `p53`.
- **The range path is not wired to `idx_open`.** The worker proves a section can be read in
  isolation; the module still takes the whole buffer. Closing that is what makes 910 MB *actually*
  queryable in a tab rather than merely openable.
- **Playwright is installed by CI, not vendored.** Locally the check exits 2 with instructions, the
  same contract `browser-check.mjs` uses.
- **One browser.** Chromium only; Safari's `opfs-sahpool` behaviour is untested here.
