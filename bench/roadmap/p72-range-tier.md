# P72 — query an index without reading it

**Tier:** T1 · **Check:** `js/opfs-check.mjs` (in CI) · **ABI:** 13 -> 14
**Files:** `crates/index-wasm/src/lib.rs` (`RangeHandle`, `idx_range_open/load/plan/search`)
**Status: SHIPPED, 2026-09-06. 37.5 % of the file read; 4/4 hits identical to a whole-file open;
zero network calls.**

`p68` shipped the OPFS tier and named the gap it left:

> The range path is **not wired to `idx_open`**. The worker proves a section can be read in
> isolation; the module still takes the whole buffer.

`p56` measured a 10 M index at ~910 MB. **A tab cannot hold that.** This closes the gap.

## The design: a prefetch PLAN, not a lazy handle

The module answers *which byte ranges does this query need*, the host reads them, and hands them
back in.

**A lazy handle was rejected on a fatal ground, not a stylistic one:**
`createSyncAccessHandle().read()` is **worker-only and synchronous**, and cannot be awaited from
inside a WASM call without JSPI/Asyncify or `SharedArrayBuffer` + `Atomics.wait`. This ABI should not
demand cross-origin isolation of every host that wants to open a file.

A plan is also **inspectable**: the host can *count the bytes*, and the byte count is the actual
claim being made. A lazy handle would make "37.5 % of the file" an assertion rather than a
measurement.

## The assembled image

`Index::from_bytes` validates every posting list against the offset array, so it **cannot** take a
file with holes — and that refusal is correct and worth keeping. So `index-wasm` assembles a
**smaller, entirely valid index image**: resident sections verbatim, a posting section holding only
the fetched lists, and a rewritten offset array. Everything downstream is unmodified; it is opening a
real index that happens to be small.

## Measured in a real Chromium, against the shipped artifact

| | |
|---|---|
| opened from 296 B + resident sections | **82,707 B** -> 1,322 docs, 4,564 terms |
| after four real queries | **82,785 B of 220,470 B = 37.5 %** |
| hits vs a whole-file open | **4/4 identical**, doc-for-doc and score-for-score |
| network calls | **0** |

## The answers are exact, not approximate

Planning uses **the same dictionary expansion the real search runs**, and every fetched list carries
its true length, so df-ordering, the expansion cap and BM25 are the file's own.

Where `split_compound` could tie-break on a `df` the planner has not read, the call **verifies** — it
re-derives the term set from the assembled image and returns `u32::MAX` so the host falls back to a
full open, **rather than answering from an incomplete term set**. An approximate answer that looks
exact is the failure mode this repo refuses to have.

## Three fixes the lane could not have made

`p69` changed the format underneath it, mid-flight:

- **An unfetched term was given a zero-length span.** Under the old fixed-width postings an empty
  list genuinely was zero bytes. Since `p69` every list opens with a varint count, so *no postings*
  is the byte `0x00` — and a zero-length span is not an empty list but an **unreadable** one, which
  `from_bytes` correctly refuses. Three range tests failed on exactly this. Unfetched terms now get a
  one-byte empty list, and the reason is written at `narrow`.
- **ABI 13 -> 14 was propagated to the four JS hosts but not to `host/python/index_ffi.py`**, which
  pins the version and failed.
- Its own report flagged the CRLF/LF churn it caused and had already normalised it.

## Gates re-run on the real tree

285 workspace tests, 0 clippy, `pool-audit` 6/6 zero cells, and `js/smoke.mjs`,
`js/image-smoke.mjs`, `host/python/index_ffi.py`, `scripts/cli-smoke.sh`, `js/opfs-check.mjs` all
`OVERALL: PASS`.

## Still open

- **37.5 % is one artifact and four queries.** The fraction should fall sharply with file size — a
  910 MB index touched by four queries ought to read far less than a third — but that is unmeasured,
  because the 8 M artifact has never been put in a browser.
- **Read-only.** `p68`'s note stands: nothing writes to OPFS from the engine side.
- **No plan caching across calls.** A second query will not re-fetch what is resident, but it does
  re-derive the plan from scratch.
