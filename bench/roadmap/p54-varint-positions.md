# P54 — the offsets cost more than the data, so stop paying fixed width for them

**Tier:** T1 · **Bin:** `phrase-cost` · **Format:** `IDXTEXT8` -> `IDXTEXT9`
**Status: SHIPPED, 2026-09-06. Phrase support now costs +15.9 % instead of +74.7 %. 268 tests.**

`p45` shipped phrase queries and published the number that was wrong with them:

> **the delta is not where it was expected.** 140,640 token occurrences at one `u32` each is
> ~563 KB; the other **~1.2 MB is the offset array**. The addressing costs 2.2x the data it
> addresses, because most (term, document) pairs carry exactly one position, so an eight-byte offset
> points at four bytes of payload.

It named the fix and declined to guess at it. This is the fix.

## Two encodings, one idea

**`position_at` is monotone and almost always advances by one.** Fixed-width `u64` spent eight bytes
to say *"+1"*. Delta-varint spends one.

**`position` ascends within a posting run and resets at the next**, so the delta is taken *within*
each run — the boundaries come from `position_at`, which is why that section is written first and
read first. A global delta would go negative at every run boundary.

Both sections are still **exactly zero bytes when positions are off**, which needed an explicit
guard: writing the element count unconditionally made the "off" case eight bytes instead of nothing,
and `an_index_without_positions_costs_no_position_bytes` caught it immediately.

## Measured, same corpus and same bin as `p45`

25,979 real rows, 140,640 token occurrences:

| | `p45` fixed width | **`p54` delta-varint** | |
|---|---|---|---|
| artifact, no positions | 2,378,890 B | 2,378,910 B | |
| artifact, with positions | 4,156,942 B | **2,755,971 B** | |
| **cost of phrase support** | **+74.7 %** | **+15.9 %** | **4.7x smaller** |
| position sections | 1,778,052 B | **377,061 B** | |
| bytes per occurrence | 12.64 | **2.68** | |

**Correctness is untouched and re-verified the same way**: 980 hits over 184 real phrases checked
against raw fixture text by an arm that shares no code with the engine — 0 wrong, 0 drift.

## The reader got stricter, not looser

Variable-width encoding removes the length check that fixed width gave for free — `position_at` was
validated as *"exactly `(postings + 1) * 8` bytes"*, and that arithmetic no longer exists. So the
count is written explicitly and checked against the posting count, the first offset must be 0,
deltas are unsigned so monotonicity is structural, and every accumulation is `checked_add`.

That care is not ceremony. `p45` recorded why this is the section where corruption is worst:

> an offset array one entry short makes `position_of` hand back the **next** posting's run, and the
> verifier agrees with whatever it is given.

`a_truncated_position_offset_array_is_refused` still passes, now against the varint form.

## A small durability fix

`an_older_format_version_is_named_rather_than_called_garbage` hard-coded the current magic, so it
failed on this bump for no reason other than the bump. It now compares against `MAGIC` itself — the
next format change will not have to remember to edit it.

## Still open

- **Positions are all-or-nothing across fields.** Storing them for a title and not a description is
  still impossible, and that is what a prose corpus would want.
- **The posting list itself is still fixed width** — `u32` doc plus `f32` sat, 8 bytes, no delta
  encoding. Doc ids are monotone within a list, so the same idea applies and would shrink the
  largest section in the file. It is a much bigger change: the postings are the hot path, and
  varint decoding on every scan is a latency cost where the offsets were only a size cost.
