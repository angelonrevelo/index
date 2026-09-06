# P53 — a host can finally say WHICH ROW changed

**Tier:** T1 · **API:** `idx_build_key`, `idx_doc_of_key`, `idx_searcher_delete_key` (+5)
**Status: SHIPPED, 2026-09-06. ABI 12 -> 13, 69 symbols. No format change. 268 tests.**

`p48` added document keys and named what it left behind:

> **Keys are not exposed over the C ABI**, so the JavaScript and Python hosts cannot resolve or
> delete by key. The Rust `Searcher` and the CLI can.

That is a bigger gap than it reads. Every incremental operation an application performs is keyed on
**its own id** — *"row `sku-1` changed"* — and the only deletion the ABI offered,
`idx_searcher_delete`, takes a **dense global ordinal assigned at insertion**. No database row
carries one. So a browser or a Python service could search a live collection and could never update
it, which meant the change-stream story `p50` shipped stopped at the edge of Rust.

## Eight symbols

| | |
|---|---|
| `idx_build_key(b, field)` | designate the key column, before any row |
| `idx_doc_of_key(h, key, len)` | ordinal for a key, or `UINT32_MAX` |
| `idx_key_of(h, doc)` | the key back out, into the result buffer |
| `idx_keyed_count(h)` | rows that can be addressed at all |
| `idx_searcher_doc_of_key(s, ...)` | the **live** ordinal across every segment |
| `idx_searcher_delete_key(s, ...)` | **what a change stream's delete becomes** |
| `idx_searcher_key_of(s, global)` | the key of a global ordinal |
| `idx_searcher_has_key(s)` | whether every segment is addressable |

An update needs no new symbol: it is `idx_build_key` on a delta plus `idx_searcher_push`, which
retires the shadowed row itself. That was `p48`'s design decision and it pays off here — a host
expressing an update writes no bookkeeping of its own.

## Three behaviours the tests pin, because each is a trap

**A deleted document is still found by `doc_of_key`.** It answers *"which ordinal is this row"*,
which is exactly what a caller needs in order to delete it. Filtering would make deleting an
already-deleted row indistinguishable from deleting a row that never existed.

**Deleting an absent key returns 0 and is not an error.** A change stream replayed from an earlier
offset re-delivers deletes; refusing would make replay impossible, and replay is the only recovery a
pipe-based consumer has.

**Every failure returns a sentinel, never a trap.** Null handles, null keys, non-UTF-8 keys and an
out-of-range key field are all exercised: a trap kills the WASM instance and takes the host page
with it.

## Both hosts, gated

`the_abi_updates_and_deletes_by_application_key` drives the whole cycle in Rust; `js/smoke.mjs`
repeats it through the raw ABI (113 checks now); `host/python/index_ffi.py` repeats it again with a
`key=` build option and `Live.delete_key`. **A host that is not gated is not shipped, only
published** — `p44` learned that when `js/index.mjs` sat at ABI 2 for four versions.

`SearchIndex.docOfKey` / `keyOf` / `keyedCount` and the Python `Index.doc_of_key` /
`Live.delete_key` / `Live.has_key` are the ergonomic wrappers over those symbols.

## Still open

- **`index apply` is still the only complete change-stream consumer.** The hosts can now express
  every operation it performs, but nobody has written the JavaScript or Python equivalent, so the
  ABI is proven by tests rather than by a second implementation.
- **A blank key field is still silent at the host level.** `keyed_count` reports it and the CLI
  warns; the hosts expose the number but do not warn.
