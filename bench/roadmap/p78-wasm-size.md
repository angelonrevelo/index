# p78 — the browser payload nobody was watching

## The regression

`CHANGELOG.md` records the WASM module at **173,700 bytes**. Nothing in the repo ever printed that
number again, so nothing noticed when it became:

```
$ cargo build -p index-wasm --target wasm32-unknown-unknown --release
$ node js/demo.mjs
  wasm module   604,184 bytes
```

**3.5x.** The browser tier is this engine's one genuinely uncontested position — `docs/research/landscape.md`
§7.5 argues no competitor can follow it into OPFS and range reads — and a 604 KB download is the
argument against itself.

## Where the bytes were

Measured with `scripts/wasm-section.mjs`, added by this item so the number is never invisible again.

```
$ node scripts/wasm-section.mjs target/wasm32-unknown-unknown/release/index_wasm.wasm
  raw 604,184 bytes   gzipped 217,229 bytes

  section                bytes    share
  code                     520307    86.1%
  custom:name               46513     7.7%
  data                      34203     5.7%
  export                     1708     0.3%
  function                    673     0.1%
  type                        360     0.1%
  custom:target_features      151     0.0%
  element                     143     0.0%
  custom:producers             79     0.0%
  global                       27     0.0%
  table                         7     0.0%
  memory                        5     0.0%
```

Attributing the 520,307 bytes of code to their origin (`--function`, against an unstripped build):

```
  origin                 bytes    share
  slice::sort              199029    38.3%
  index-text               148753    28.6%
  core/alloc/std            41124     7.9%
  index-wasm                31414     6.0%
  tantivy-fst               23259     4.5%
  index-image               20245     3.9%
  levenshtein               16666     3.2%
  hashbrown                 13815     2.7%
  core::fmt                  9962     1.9%
  dlmalloc                   7759     1.5%
  (other)                    5972     1.1%
  panic                      2303     0.4%
```

**`core::slice::sort` is a third of the entire module** — 199,029 bytes across **146**
monomorphizations, 14 of them `driftsort` (stable) at ~8.3 KB each and 10 `ipnsort` (unstable) at
~5.2 KB each. Rust's post-1.81 sorts are enormous per instantiation, and this engine sorts in
47 places in `index-text`, 11 in `index-image` and 8 in `index-core`.

## What shipped

`.cargo/config.toml`:

```toml
[target.wasm32-unknown-unknown]
rustflags = ["-C", "strip=symbols"]
```

|                        | before  | after   |          |
|------------------------|---------|---------|----------|
| `index_wasm.wasm`      | 604,184 | 557,441 | -7.7%    |
| gzipped                | 217,229 | 203,199 | -6.5%    |
| `index_geo_wasm.wasm`  |  57,037 |  48,992 | -14.1%   |
| gzipped                |  22,494 |  20,057 | -10.8%   |
| `index_accel.wasm`     |   6,342 |   5,946 | -6.2%    |
| gzipped                |   3,086 |   2,817 | -8.7%    |

It removes the `name`, `target_features` and `producers` custom sections — 46,743 bytes of debug
metadata a browser never reads. All 84 exports (81 `idx_*` functions) survive byte-identically and
`js/demo.mjs` measures 45.1 us/query after against 45.4 us/query before.

It is a **cargo config and not a `[profile]` key on purpose**: a profile applies to every target, so
`strip = true` in the root `Cargo.toml` would also strip the native test binaries and cost a failing
`cargo test --workspace --release` its symbolised backtrace, buying nothing (a native `.dll` has no
wasm `name` section). Scoping it to the triple leaves the native build byte-for-byte unchanged.

## What did not work

Every figure below was built and weighed. `-C strip=symbols` is held constant so the rows compare.

| attempt | raw | gzipped | why it was rejected |
|---|---|---|---|
| `opt-level = "z"` | 477,121 | 157,118 | **2.1x slower** — 108.9 us/query vs 51.1 at `3`, interleaved in one process. The best size on the board and not close to worth it. |
| `opt-level = "s"` | 549,975 | 181,295 | Slower *and* bigger than `opt-level = 2`. Strictly dominated. 53.7 us/query best-of-round vs 38.9. |
| `opt-level = 2` | 533,170 | 195,926 | **The near miss.** Speed-neutral in wasm (38.9 / 44.7 / 43.3 us best-of-round vs 42.0 / 49.9 / 44.6 at `3`) for a further 24,271 bytes. Rejected because a target-wide rustflag also hits `index-accel`, where `bitmap_op` collapses from 7,344 to 2,408 M rows/s and `filter_mask` from 153 to 92 — p35's headline kernel, 3x gone, for 4% of one module. A `[profile.release.package.index-accel] opt-level = 3` carve-out does **not** rescue it: rustflags are appended after the profile's own `-C opt-level`, so the flag wins (probed: accel still built at 4,719 bytes). Taking it through `[profile.release]` instead would work for accel but costs the native typo path ~5% (p50 70,202 ns vs 66,610 ns over three interleaved `real-corpus` runs) across five crates this lane does not own. |
| `opt-level = 1` | 768,116 | 230,398 | **27% bigger than `3`.** Less inlining leaves more functions standing. |
| `lto = "fat"` | 557,441 | 202,469 | Byte-identical to `thin` (477,129 vs 477,121 at `z`). `codegen-units = 1` has already done the work. |
| `panic = "abort"` | 533,170 | 195,926 | **Byte-identical.** `wasm32-unknown-unknown` is already abort-only; the `panic` bucket is 2,303 bytes total. |
| `-C inline-threshold=25` / `=50` | 557,441 | 202,456 | **Byte-identical to no flag.** Silently ignored under the new pass manager. |
| `-C llvm-args=-enable-merge-functions` | 557,441 | — | No effect, despite 14 near-identical quicksort bodies to merge. |
| `-C llvm-args=-vectorize-slp=false` | 533,170 | 195,926 | No effect. |
| `-C llvm-args=-max-jump-table-size=1` | 557,841 | — | 400 bytes *worse*. |
| `-C link-arg=` `-O2` / `-O4` / `--merge-data-segments` / `--lto-O3` | 557,441 | 202,456 | All four byte-identical. `wasm-ld` has nothing left to give. |
| `+bulk-memory` and friends | — | — | Already on. The `target_features` section reads `bulk-memory, bulk-memory-opt, call-indirect-overlong, multivalue, mutable-globals, nontrapping-fptoint, reference-types, sign-ext` under rustc 1.96. |
| `wasm-opt` | — | — | Not installed on this machine, and it is not going in as a build dependency. |
| stable sort -> unstable sort everywhere | 548,192 | 203,421 | **The hypothesis that failed.** Rewriting every `sort_by`/`sort_by_key` call site in `index-text`, `index-image` and `index-core` to its `_unstable` form bought 9,249 bytes and made the module *worse* gzipped. `ipnsort` is nearly as large as `driftsort`; stability is not the cost. Reverted. |

The one flag that is real and is not shipped: `-C llvm-args=-unroll-threshold=0` takes another
6,544 bytes (526,626 on top of `opt-level = 2`) at no measurable speed cost. It is left out because
it pins the artifact to an LLVM-internal flag name that no toolchain promises to keep, and 1.2% is
not worth a build that breaks on a `rustup update`.

## The lever that is left, and it is not a flag

`slice::sort` is 199,029 bytes — **more than a third of the code section, and larger than all of
`index-text`'s own logic.** The `_unstable` experiment proves the cost is not stability but the
*number of distinct `(T, comparator)` pairs*: 146 instantiations, each carrying its own copy of
driftsort or ipnsort plus `small_sort_network`, `median3_rec` and `insertion_sort_shift_left`.

The fix is to sort fewer *types*. A hit is a `(f32 score, u32 doc)` pair; packing it into a single
`u64` whose natural descending order is the ranking order lets every ranking sort in the engine
become one `<[u64]>::sort_unstable()` — one monomorphization for the whole crate instead of dozens.
The same trick applies to the facet, range and near-duplicate paths.

That work lives in `crates/index-text/src/index.rs`, `searcher.rs` and `crates/index-image/`, which
this lane does not own, so it is filed here rather than done. It is worth filing: if unification got
146 instantiations down to a handful it would be worth **six times** everything the flags above
bought put together, and it costs nothing at runtime — a packed `u64` sort is faster than a
comparator closure over a struct, not slower.

## Acceptance

```
cargo test --workspace --release                                      288 passed, 0 failed
cargo clippy --workspace --all-targets --release                      0 warnings
cargo build -p index-wasm --target wasm32-unknown-unknown --release   557,441 bytes
node js/smoke.mjs                                                     OVERALL: PASS
node js/image-smoke.mjs                                               OVERALL: PASS
python host/python/index_ffi.py                                       OVERALL: PASS
cargo run -p index-bench --release --bin pool-audit                   0 (0.00%), all 6 sets
node js/demo.mjs                                                      OVERALL: PASS
node js/accel-bench.mjs                                               OVERALL: PASS, bitmap_op 7,181 M rows/s
```

The export set is proven identical rather than assumed: 84 exports, 81 of them `idx_*`, diffed
between the 604,184-byte build and the 557,441-byte one with zero lines of difference.
