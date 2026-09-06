# P28 — "any language" made real: a C header, a Python host, and a builder path that was never tested

**Tier:** T1 · **Bins:** `js/smoke.mjs` (extended), `host/python/index_ffi.py` (new)
**Status: DONE, 2026-09-06.**

The goal says the engine should serve *"webapps/websites/apps in general in any language (low or
high level)"*. `docs/research/portability.md` reasoned about how that would work and
`crates/index-wasm` already built a native cdylib whose own description claimed "native FFI" — but
nothing shipped a C header, and no non-JavaScript host had ever been run. The claim was
architectural, not demonstrated.

## Three real defects found on the way

**1. The builder ABI was exported and never exercised from a host.** `js/smoke.mjs` opened with:

> This builds a tiny index in Rust-free JavaScript terms — it cannot, so instead it asserts the
> module's ABI surface and its refusal behaviour.

That was false: `idx_build_new` / `_add` / `_finish` / `idx_serialize` are all exported, precisely so
a host *can* build in-process. The comment had outlived the code, and the entire build path — the
thing that makes "no upload, works on your existing architecture" true — had no host-side test.

**2. Two source files were byte-level binary.** `crates/index-wasm/src/lib.rs` and `js/index.mjs`
contained **literal NUL bytes** inside char and string literals, because NUL is the wire separator
for field specs and document values. Valid in both languages, and invisible: `file` reported both as
`data`, `grep` refused them without `-a`, and any tool that sanitises control characters would
silently break the ABI for every host. Replaced with the two-character escape `'\0'` — same value,
and both files are now `UTF-8 text`.

**3. Nothing caught either, because the separator has no Rust test.** All 103 Rust tests pass with
the separator broken; it only exists at the ABI boundary. That is now covered from both tiers.

## What was added

| | |
|---|---|
| `include/index.h` | Hand-written C header for all 15 symbols, with the memory, error and threading models written down. |
| `host/python/index_ffi.py` | A ctypes host in the standard library alone — no build step, no dependency — with a 14-check self-test and a real-corpus bench. |
| `js/smoke.mjs` | Extended from 7 checks to 21: builds an index from JavaScript, searches it, corrects two typos, serializes, reopens, and confirms the reopened index answers identically. |

The Python self-test mirrors the JavaScript one assertion for assertion, so a divergence between the
WASM tier and the native tier shows up as one tier failing a check the other passes.

The header records the one place the tiers genuinely differ:

> **Native hosts own their own memory and pass their own pointers straight in.** `Index::from_bytes`
> parses into owned structures, so a Python `bytes` or a Go slice can be passed directly and freed
> on return. `idx_alloc` / `idx_free` exist for WASM hosts, which cannot otherwise reach into the
> module's linear memory, and native hosts may ignore them.

It also records the trap that cost real time here: ctypes defaults an undeclared `restype` to a
32-bit `int`, which **truncates every returned pointer** on a 64-bit build and yields a
plausible-looking non-null value that segfaults on use. Declaring `restype` is not tidiness.

## Measured — from Python, through ctypes, on real rows

`blead-lead.tsv`, 25,979 real Philippine business records, three fields. Every figure includes the
full round trip: argument marshalling, the FFI call, and unpacking every hit into a Python object.

| rows | terms | build | exact p50 | exact p99 | typo p50 | typo p99 |
|---|---|---|---|---|---|---|
| 25,979 (real) | 20,617 | 258 ms | **66 us** | 329 us | 326 us | 1,132 us |
| 259,790 | 20,617 | 2,819 ms | **298 us** | 1,855 us | 214 us | 1,950 us |
| **1,039,160** | 20,618 | 12,301 ms | **551 us** | **4,042 us** | 327 us | 4,001 us |

**A million rows, sub-millisecond median, from Python.**

**These numbers are not comparable to `p7-scale.md`, and the difference is not in the engine's
favour.** That bench runs 41,069 terms over real DepEd school names; this one recombines 25,979
business names by suffixing an integer, so the term dictionary barely grows (20,617 to 20,618) while
the posting lists lengthen. Long postings over a small vocabulary is an *easier* shape than a large
vocabulary — it is why the typo p99 here is 4 ms against `scale`'s 17 ms. The right reading is
**"the FFI boundary is not where the time goes"**, which is what this bench was built to show. It is
not a second, more flattering measurement of the engine.

## Reproduce

```sh
cargo build -p index-wasm --release                        # native cdylib
python host/python/index_ffi.py                            # 14 checks
python host/python/index_ffi.py --bench 40                 # ~1 M rows

cargo build -p index-wasm --release --target wasm32-unknown-unknown
node js/smoke.mjs                                          # 21 checks
```
