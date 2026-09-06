# P69 — delta-varint the posting lists: indexes roughly halve, and the typo tail improves with them

**Tier:** T1 · **Bin:** `real-million` · **Files:** `crates/index-text/src/format.rs`
**Status: SHIPPED, 2026-09-06. 106.4 -> 60.0 B/doc. The shipped profstopick artifact went
386,043 -> 220,471 bytes. 112 index-text tests.**

`p54` proved the idea on the position sections and named the next target:

> The **posting list is the largest section in the file** and is still fixed width — a `u32` doc id
> plus `MAX_FIELD` `u16` term frequencies per posting.

Doc ids are strictly ascending within a term's list, so their deltas are small and varint-encode
well. That is the whole change.

## Measured, `bin/real-million`, bytes per document

| documents | before | after | |
|---|---|---|---|
| 100,000 | 106.4 | **60.0** | -44 % |
| 250,000 | 105.4 | **54.2** | -49 % |
| 500,000 | 101.3 | **51.5** | -49 % |

| artifact | before | after |
|---|---|---|
| profstopick, shipped | 386,043 B | **220,471 B** |

## The tail improved with the size rather than paying for it

The expectation with a variable-width encoding is that you buy space with decode time. The opposite
happened:

| documents | typo p99 before | after |
|---|---|---|
| 250,000 | 5,183 us | **3,286 us** |
| 500,000 | 5,601 us | **5,392 us** |

Smaller postings mean more of a list per cache line — the same reason `p7` kept `Posting` at
8 bytes. **The encoding is not a space/time trade here; it is both.**

## Every list is self-contained, on purpose

The count is written **per list** rather than once for the section:

```rust
for list in s.posting.iter() {
    let at = post.here();
    // The count is per LIST rather than once for the section, so one list still decodes
    // on its own: that is the range-read property this format exists to provide.
    post.varint(list.len() as u64);
    let mut prev = 0u32;
    for p in list.iter() {
        post.varint((p.0 - prev) as u64);
        prev = p.0;
        for v in p.1.iter() { post.varint(*v as u64); }
    }
    list_byte.push(post.here() - at);
}
```

That costs a few bytes per term and preserves the property `a_single_posting_list_is_range_readable`
asserts and `p68`/`p72` actually consume: **one posting span can be fetched and decoded without the
rest of the file.** A section-wide count would have made every range read depend on bytes it did not
fetch.

The consequence bit the range tier immediately: **"this term has no postings" is now the byte
`0x00`, not a zero-length span.** See `p72`.

## Delegated, and the two things the worker could not see

Built by a free `hy4-preview` worker in an isolated worktree. The gate was re-run here on the real
tree, and two changes were made before merge:

- **It grew `MAGIC` from 8 to 9 bytes**, for `"IDXTEXT10"`. That silently breaks the fixed head a
  range reader depends on: `js/opfs-worker.mjs` fetches `MAGIC.len() + TABLE_BYTE` blind, and `p68`
  publishes *"296 bytes to open a file of any size"* — **but you cannot know the head's size until
  you have read the version out of it.** The magic is now held at eight bytes forever as
  **`IDXTXT10`**, dropping the `E` rather than the invariant, and the reason is written where the
  constant is.
- **`Reader::u16` became dead** once postings stopped reading fixed-width frequencies. This repo
  runs at zero clippy warnings.

Its version-history handling was **kept as written**: `KNOWN_MAGIC` lets a stale file report
*"that is an IDXTEXT9 file, this build reads IDXTXT10"* rather than *"bad magic"*. Those are
different instructions to an operator and only one of them is true.

## Gates re-run on the real tree

112 index-text tests, 0 clippy, `pool-audit` 6/6 zero cells, `real-corpus` PASS 2/2, `js/smoke`,
`host/python/index_ffi.py`, `scripts/cli-smoke.sh` and `js/opfs-check.mjs` all PASS.

## Still open

- **No format version negotiation beyond a refusal.** A stale file gets a good message and stops;
  nothing migrates it.
- **The frequency columns are still one varint per field per posting**, including the zeros. A
  bitmap of present fields would drop most of them; unmeasured.
