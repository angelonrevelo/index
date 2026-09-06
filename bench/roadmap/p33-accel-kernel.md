# P33 — the analytics half, measured natively (and a correction to this document)

**Tier:** T1 · **Bin:** `accel-kernel` (new) · **Crate:** `index-accel`
**Status: PASS, 2026-09-06. All seven kernels covered — 2,400 differential trials, 0 wrong.
This document also corrects its own first draft; see the CORRECTION section.**

## Why this exists

The goal names a *"database engine/analyzer"* and *"the best analytics/metrics across the board"*
alongside search. `index-accel` is that half: onegrid's ratified `AccelModule` ABI, 773 lines, seven
kernels — sort, filter, group, group-combine, aggregate, bitmap, top-k — behind nine C exports.

An audit of roadmap coverage found `index-accel` had **0 roadmap documents and 0 bench bins**,
against 33 and 12 for the text engine. That gap is real and this document closes it.

## CORRECTION — what the first draft of this document got wrong

The first draft also counted **3 tests against the text engine's 111** and called the pointer
problem below "a soundness defect it found". **Both framings were wrong, and the repository's own
documents say so.** They are corrected here rather than quietly edited, because the error is the
instructive part.

**1. The `u32` truncation was already known, and deliberately decided.**
`docs/integration.md` carries a section headed *"Why there are no host unit tests for the kernels"*:

> On a 64-bit host an address does not fit in 32 bits, so a test passing `vec.as_mut_ptr() as u32`
> silently truncates and the kernel reads a garbage address — **which is exactly what the first
> version did, faulting with `STATUS_ACCESS_VIOLATION`** instead of failing an assertion.

The crate's own test module repeats it. My bench's guard did not discover a hazard; it *reproduced a
documented one*. Presenting a rediscovery as a discovery is the kind of thing this roadmap exists to
prevent, and it survived a full write-up before being caught.

**2. The three tests are not neglect.** They are deliberately **pointer-free** — the version
constant, bitmap length rounding, `-0.0` normalisation — because the recorded decision was to prove
correctness *where the module actually runs*, by the consumer's own harness, rather than natively.
The "3 vs 111" comparison implied nobody had bothered. Someone had, and had written down why.

**3. Agreement with onegrid's specification is already established.** The first draft listed "the
reference is mine, not onegrid's" as an open limit, which reads as though the port is unvalidated
against the spec. It is validated: `docs/integration.md` records onegrid's own suite at **294/294**
against the real compiled module, including `differential.real-module.test.ts` — their property test
with a single line changed, the backend swapped from `createFakeAccelModule()` to the real one, and
every generator, property and assertion theirs.

## The pointer change, and the decision it reverses

```rust
#[cfg(target_arch = "wasm32")]      pub type AccelPtr = u32;    // the ratified ABI, byte for byte
#[cfg(not(target_arch = "wasm32"))] pub type AccelPtr = usize;  // host builds address real memory
```

**This reverses a considered decision, so the argument for it belongs here rather than in a commit
message.** `docs/integration.md` rejected host testing because simulating linear memory "would
require giving every kernel a base parameter the real ABI does not have, i.e. testing a different
function from the one that ships."

That objection is aimed at a **base parameter**, which changes a kernel's call signature and its
address arithmetic. A type alias does neither: the kernel bodies are character-for-character
identical, and on wasm32 `AccelPtr` *is* `u32`, so every exported signature is unchanged and
onegrid's host binds exactly as before. Both targets are built to confirm it.

**The residual cost is real and should not be waved away**: host tests now exercise 64-bit pointer
arithmetic while the shipped artifact uses 32-bit. A defect that depends on 32-bit wraparound would
not be caught natively. That is precisely why the wasm path remains covered by onegrid's harness,
and why this bin is offered as *additional* coverage rather than as a replacement for it.

## Differential verification

Every kernel is checked against an **independent reference written in the bench** from the ABI's
documented semantics, over randomised inputs that deliberately include what breaks analytics code:
missing values, `NaN`, `-0.0`, ±infinity, duplicate keys, empty selections, and `k > n`.

| kernel | trials | wrong | what is checked beyond "same answer" |
|---|---|---|---|
| `bitmap_op` | 400 × 5 ops | **0** | tail bits past `bit_length` must be cleared, or two bitmaps of equal logical length stop comparing byte-equal |
| `filter_mask` | 400 × 12 ops | **0** | including `is_missing` / `is_present`, range, and set membership |
| `sort_pass` | 300 | **0** | **stability** *and* that the output is a permutation — a sort that loses or duplicates an index can still look ordered |
| `top_k` | 300 | **0** | `k > n` clamps; result equals the sorted prefix |
| `group_code` | 300 | **0** | compared as a **partition in both directions**, so different numbering passes and two groups folding into one fails |
| `group_combine` | 300 | **0** | fed from real `og_group_code` output, not synthetic integers, because that is the only way it is called; plus **3 deliberately over-filled tables**, all of which returned `-1` rather than wrapping |
| `aggregate` | 400 × 8 ops | **0** | sum, avg, count, distinct, min, max, first, last; empty selection writes nothing |

**2,400 trials, 0 disagreements.** The kernels are a faithful port.

The saturation case is worth its own line. The randomised trials never filled the hash table
(capacity 256, at most 200 rows), so the `-1` return — *this table cannot hold the answer* — was
reached by none of them. Three tables were then deliberately over-filled, and all three refused.
**A hash table that silently wrapped would corrupt a group-by rather than fail it**, which is the
worst outcome available to an analytics kernel: a wrong number that looks like a number.

## Both first-run failures were in the reference, not the kernel

The first run reported `group_code` 266/300 wrong and `aggregate` 86/400 wrong. Both were mine:

- **`aggregate`**: the diagnostic line read `got NaN, want NaN`. Summing a column containing both
  `+inf` and `-inf` gives `NaN` legitimately, in both implementations — and `NaN == NaN` is false.
  The comparison, not the kernel, was wrong.
- **`group_code`**: the reference gave a *repeated* key the index into its key list rather than the
  code that key was first assigned, which is a different partition whenever a missing row appears
  before a repeat.

This is `p11`'s lesson in a new costume — *two implementations that share an input agree about the
input, not the answer* — with a sharper edge: **an oracle needs checking as hard as the thing it
checks.** A differential harness with a wrong reference is worse than no harness, because it
manufactures confident false alarms. It is recorded here rather than quietly fixed because the next
person to write an oracle in this repo will be tempted to trust it.

## Throughput, 1,000,000 rows

| kernel | ms | M rows/s |
|---|---|---|
| `aggregate` (sum) | 2.58 | **387** |
| `group_code` | 3.28 | **305** |
| `top_k(100)` | 3.44 | **291** |
| `filter_mask` | 5.21 | **192** |
| `sort_pass` (full merge) | 93.96 | 10.6 |

Four of five kernels clear **190–390 M rows/s** — a million rows filtered, grouped, aggregated or
top-k'd in **2.6–5.2 ms**, single-threaded, on a machine that has been benchmarking all day.

**`sort_pass` is the outlier at 94 ms**, and it is not a defect: it is a full bottom-up merge sort
producing a stable permutation, `O(n log n)` with `log2(1e6) ≈ 20` passes over 1 M elements. A
storefront sorts a filtered page, not a million rows; onegrid's ABI exposes it as a *pass* precisely
so a host can interleave it with other work. Worth knowing before someone calls it per keystroke.

## Honest limits

- ~~**These are host measurements, not wasm.**~~ **Closed by `p34-accel-wasm.md`**, which measures
  the real artifact in WebAssembly: 73–93 % of native on five of six kernels, and 4 % on
  `bitmap_op`, the only one that auto-vectorises natively.
- **The reference here is mine, not onegrid's.** Agreement with the specification is established
  separately and more authoritatively — their harness, their generators, 294/294 — so this bin adds
  independent coverage rather than settling anything. Two references agreeing is worth more than
  either alone; one of them being the consumer's is worth more still.
- **Host pointer width differs from the shipped one.** See the correction above: 64-bit natively,
  32-bit in wasm, so a 32-bit-wraparound defect would not surface here.
- **Single-threaded, one shape of data.** Small integers with salted specials. Real analytics
  columns are wider and more skewed.

## Reproduce

```sh
cargo run -p index-bench --release --bin accel-kernel
cargo build -p index-accel --release --target wasm32-unknown-unknown   # ABI unchanged
```
