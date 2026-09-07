# Changelog

All notable changes to `index`. The project is pre-release and unversioned, so entries are keyed to
**roadmap items** (`## pNN — title`) rather than to versions; the newest work sits under
`[Unreleased]` until it is given an item number. Newest first. See
[ROADMAP.md](ROADMAP.md) for the tiered plan and [docs/roadmap-rejected.md](docs/roadmap-rejected.md)
for what was deliberately ruled out.

## [Unreleased]

### Added — the two test gaps the honest-gaps list named (2026-09-08)

- **`FmIndex::locate` for an absent pattern.** The contract half `count` had and `locate` did not:
  a pattern that occurs nowhere returns an EMPTY list, not garbage positions. Asserted for absent
  single bytes, long patterns, a pattern longer than any text run, and a near-miss whose absence is
  verified against brute force first — the test asserts locate's honesty, not the corpus's (the
  first draft got that backwards and its "absent" near-miss genuinely occurred elsewhere).
- **`PgmIndex` ≡ `PlaIndex` result equivalence.** The recursive PGM is a wrapper around the same
  leaf PLA, so any key one finds and the other misses is a recursion bug. Compared on every present
  key across four distributions and three epsilons, and on absent keys probed inside, below and
  above the range — both must refuse, and refuse the same way.

Also noted: `crack::tests::correct_across_limit_settings` alone accounts for ~171 s of the
`index-core` suite — pre-existing, worth a look some day, unchanged here.
### Added — CI (2026-09-08)

`.github/workflows/ci.yml` — the P3 row open since 2026-06-19. clippy with `-D warnings` (this
repo's gate), workspace tests, the explicit wasm32 build `index-accel` needs, every shippable
artifact through `scripts/build-wasm.sh`, both Node smokes, the Python ctypes host and the CLI
smoke. Every step was run locally before the file was committed; the browser checks are excluded on
purpose, with the reason written in the file. Green means nothing until the first push runs it, and
the ROADMAP's honest-gaps line now says exactly that.
### Tried and reverted — p83: bucket-tiered enumeration (2026-09-08)

The one lever `p47` named for the 5 ms typo bar, built (250 lines over `search_opt`: per-shape
intersection arms covering buckets 0-2, block-skipping drives, early exit on a completed sweep),
exact everywhere it ran (the exhaustive oracle caught a duplicate-admission defect mid-build, worth
reading the doc for), and **slower everywhere it mattered**: +8-14 % across `scale`'s ladder at 1 M,
neutral on `real-million`'s real 1.2 M. The abort that bounds a tier walk forfeits the
exhaustiveness its early exit needs, and an unpruned intersection loses to the scan's own block-max
pruning on every fat shape. Reverted; the measurement closes the lever. Four attacks (`p27` x2,
`p29`, `p47`, `p83`) now agree: the tail is the cost of the ranking rule, and the remaining work is
pricing, not search.

### Decided — the typo bar (2026-09-08)

With the last lever measured closed, the bar resolves the way `p55` priced: **the 5 ms guarantee
stands at the sizes consumers run** (every real corpus <= 241 K documents passes on a quiet
machine), the published envelope beyond it is **5.3 ms at 1.2 M real products** (down from 8.26 via
`p69`/`p82`; the recombined stress ladder stays `OVERALL: FAIL` by design), and hosts above ~250 K
that need sub-5 ms get `search_capped` with its agreement table now measured on REAL 1 M vocabulary
(cap 8: 4.73 ms at 98.80 % top-10 agreement). The bar is not raised; its failing row is labelled as
the stress shape it is.
### Changed — p82: one expansion per segment, not two (2026-09-08)

`p52` named the recovery and left it: "the first pass could hand its expansion to the second
instead of the second re-deriving it. That is where the ~2x goes." `plan` is now split into an
expand phase (`Index::expand_query` — the dictionary walk, no cap, no weights) and a weigh phase
(`Index::weigh` — cap, IDF, weights), and `Searcher::stat_for` hands each segment's stat-pass
expansion back to that same segment's weigh phase. Collection-wide statistics now cost **~1.0x on
the typo tail, down from 1.8-2.2x**, at every rung past 10 segments, with ranking bit-identical —
asserted by a new differential test that compares score BITS between the re-derived and the
handed-over path across exact, typo, compound-split, typeahead and learned-expansion queries.
`want_text` keeps the single-index path's zero-cost; `ExpansionEmit::learned` keeps the learned
terms out of the collection-wide `df` sum exactly as before. ABI unchanged (14). The full
measurement: [`bench/roadmap/p82-expansion-handoff.md`](bench/roadmap/p82-expansion-handoff.md).

### Revalidated — every bench re-run against its published number (2026-09-08)

24 of 26 runnable benches reproduce or hold their documented red, run one at a time on a quiet
machine (the sweep itself failed `alec-surface` under concurrent load and passed it quiet — the
repo's "interleave or do not claim a delta" rule, re-earned). Findings: the ROADMAP's "stays red at
512 decoys" sentence was stale (`prune-consistency` passes everywhere since `p25`/`p26` — fixed in
place); `presyo-catalog`'s typo tail straddles the 5 ms bar across runs, which is exactly the
decision the next-list row 5 exists to end; `real-million`'s input is not on disk and needs a live

### Added — the image tier (2026-09-06)

**`index-image`, a new crate.** The engine now indexes pixels alongside text, and the architectural
claim is narrow enough to be falsifiable: *no shipping system answers text, facet, numeric-range,
vector and perceptual-hash predicates in one query plan over one index, selecting top-k once.*
**That claim was RETRACTED the same day** — it had been tested only against photo apps, and Vespa
and Lucene both refute it. See `docs/research/image.md` §3. What survives is embeddability and
licence, not a novel query model.
Evidence for that claim, and for every decision below, is in
[`docs/research/image.md`](docs/research/image.md) — a seven-lane sweep of the FOSS photo-search
field, embedding and ANN state of the art, on-device inference, lossless compression, perceptual
hashing, video retrieval, and the face-recognition licence and legal landscape.

- **`hash`** — aHash, dHash and pHash at 64 bits, plus a PDQ-*shaped* 256-bit hash (documented as
  **not** bit-compatible with Meta's PDQ, so it does not interoperate with ThreatExchange
  blocklists). Thresholds are the measured ones, each carrying the number that justifies it, and the
  hash's *limits* — rotation past ~5 degrees, crops past ~5% — are asserted by test rather than
  described in a comment.
- **`color`** — sRGB to OKLab from Ottosson's matrices, deterministic seeded k-means++ palette
  extraction, and 77 perceptually-even buckets in OKLCH. Near-zero chroma is routed to dedicated
  neutral buckets, because a grey has no meaningful hue and letting numerical noise assign it one is
  the classic colour-search bug.
- **`meta`** — format sniffing, dimensions, JPEG quantisation tables, embedded EXIF thumbnails, and
  a TIFF/EXIF IFD parser for both byte orders including GPS. Built for **hostile input**: bounds-
  checked reads, a cyclic-IFD guard, capped allocation, no `unwrap`. Proven by truncation at every
  length, cyclic and out-of-range pointers, absurd component counts, and a seeded sweep of 4,000
  random buffers plus 2,000 bit-flipped mutations of valid fixtures.
- **`vector`** — a quantised column storing three tiers from one push (1-bit binary, int8 with a
  per-vector scale, optional f32), searched as **binary popcount shortlist → int8 rerank → exact**.
  No ANN graph: below ~1 M vectors a graph costs build time, memory, recall and mutability and buys
  nothing, and it would break the live-update property `searcher` already has.
- **`digest`** — SHA-256, verified against the FIPS 180-4 vectors and cross-checked byte-for-byte
  against `sha256sum`. Cryptographic rather than fast because it serves as both the dedup key and
  the proof a lossless transcode round-tripped 1:1, and both are adversary-facing in a scraped
  corpus.
- **`image`** — the fusion layer. Hard predicates (facet, range) are pushed into the existing text
  pass; the vector and hash arms each over-generate candidates, are re-filtered against those hard
  predicates, and are then normalised, combined and top-k-selected **once**. Every hit carries a
  `Why` naming which signals found it, and agreement breaks ties.

### Notable decisions

- **`index-image` ships no model, no ONNX runtime and no image decoder.** An embedding is an
  **input** (`&[f32]`). The licence spread across image encoders is a minefield — SigLIP and DINOv2
  are Apache-2.0, MobileCLIP2 is an Apple sample licence, Jina CLIP v2 is CC BY-NC, DINOv3 is a
  bespoke gated licence — and keeping the model out is what preserves the crate's near-zero
  dependency footprint, its WASM shippability, and its MIT OR Apache-2.0 licence against seven AGPL
  incumbents. The cost is stated rather than hidden: **this crate cannot embed an image for you.**
- **No image codec was written.** "Byte-exact but smaller" is lossless compression and is bounded by
  information theory; the measured ceiling is JPEG XL's ~20%, which fails outright on ~1% of real
  JPEGs, and libjxl already has it under BSD-3. Lepton, PackJPG, brunsli and FLIF all attempted this
  and are dead or absorbed. Meanwhile roughly 30% of a scraped corpus is duplicate bytes, so dedup
  beats the codec — the engine ships the content address that *proves* the round-trip and leaves the
  transcode to the host.
- **Face support is scoped to the clustering primitive, with no bundled weights, permanently.**
  InsightFace's face weights are research-only despite its MIT code badge; EU AI Act Art. 5(1)(e)
  absolutely prohibits building facial-recognition databases by untargeted scraping; and the UK
  Upper Tribunal named *clustering facial vectors* as the triggering processing step. See
  [`bench/roadmap/p64-face-cluster.md`](bench/roadmap/p64-face-cluster.md).

### Measured, on a real 17,311-file web scrape

`crates/index-bench/src/image_corpus.rs` runs the whole tier against a genuine scrape nobody curated
for it — 17,311 files, 2,838 MB — and reports `OVERALL: PASS` with one verdict explicitly withheld.
Baseline committed under [`bench/runs/2026-09-06-baseline/`](bench/runs/2026-09-06-baseline/).

- **0 panics, 0 unreadable, 0 decode failures** across all 17,311 files, including the 5,304 that
  are not images at all.
- Cheap tier **2.049 ms p50 / 112.0 B per image** — both `p57` budgets hold.
- **100% EXIF stripping**: 6 files in 17,311 carry an APP1 block, none parse to non-empty EXIF.
  This confirms the research prediction exactly.
- **29.82% of all files are redundant exact copies** (42.69% among images alone). That lands on the
  ~30% the literature reports for LAION-2B, from an entirely independent scrape — and it is the
  strongest evidence for this release's "dedup beats the codec" conclusion, since a hash lookup
  removes 29.82% of bytes against a ~20% ceiling for the best lossless transcode.
- Fused query over 12,007 documents, 100 queries at k=20: **0 short pages, 0 filter leaks, 0
  uncorroborated hits.**
- Latency at 12,007 documents: text **2 µs** p50 (flat from 1,000 docs), vector **192 µs**, fused
  **623 µs**. The vector arm scaling linearly is what an exact scan predicts and what `p58` chose
  over a graph deliberately.
- Index is **2,938.8 B/image**, 14.96% of the bytes it indexes.

**Then a real encoder ran, and nothing is withheld.** `scripts/embed-corpus.py` produced **12,007
real CLIP ViT-B/32 embeddings in 133 s — 90 img/s on an RTX 2060 SUPER** — and the benchmark went to
**8 / 8 PASS**:

- **`p58` recall@10 = 0.9870** against the exact oracle. `docs/research/image.md` §4 had image-
  embedding recall as `UNVERIFIED`: every published binary-quantisation figure is for *text*. This
  is now measured rather than borrowed from another modality, and it lands inside Qdrant's
  0.98–0.9966 text band.
- **`p59` acceptance 3 HOLDS** — fused nDCG@10 **0.2932** > vector-only **0.2910** > text-only
  **0.0203**. It needed a second labelled set to be measurable at all: the obvious one, built from
  exact-duplicate groups, is **degenerate**, because identical bytes give identical embeddings and
  the vector arm scores 1.0000 by construction. The set that votes is near-duplicates with exact
  duplicates removed. **The margin is thin — +0.0021, ~0.7 % relative over 100 queries** — which is
  reported as thin rather than dressed up; the text arm is weak here because a CDN URL path is a
  poor caption.
- Real embeddings cost **2.4x** the synthetic ones (fused p50 1,467 µs vs 623 µs), because a
  42.69 %-duplicate corpus clusters densely and the Hamming shortlist carries more ties into the
  rerank. A benchmark that had quietly used synthetic vectors would have understated its own cost by
  more than double.

**`p61` acceptance 4 closed with a real transcode**: `cjxl`/`djxl` 0.12.0, **2,000 / 2,000 JPEG
restored byte-exact** by SHA-256. Both published claims then failed to reproduce on this corpus —
JPEG XL saved **6.65%** against a claimed 13–22%, and **0.00%** of files were refused against a
claimed ~1% — while the generic-compressor control reproduced at **1.06%**. At 6.65% the transcode
is worth less than advertised and dedup measured 29.82%, so the content address is roughly **4.5x**
the bigger win.

**A correction worth recording: the dedup rate cannot be sampled.** A 1,500-file systematic sample
put exact duplicates at 9.00%; the full corpus measures 29.82%. Striding across a corpus strides
across duplicate clusters, so the sampled figure was wrong by a factor of three rather than merely
imprecise. Every other sampled number held.

The same fused query returns identical documents in identical order in Rust, Node, **headless
Chromium 147**, and **Python `ctypes` using the standard library alone**. The C ABI went 11 → 12 and
49 → **61 symbols**, still with no `wasm-bindgen`; the artifact grew **+14.3% gzipped** for the whole
tier, reported as the regression it is.

### Fixed

- `js/browser-check.mjs` had been passing green against a **stale ABI-2 WASM module from
  2026-09-05** cached in the gitignored `artifact/web/`. It now serves from the live tree, so the
  browser check tests the module actually being built.

### Roadmap

New rows `p57`–`p65` under [`ROADMAP.md`](ROADMAP.md) Part III, each with a falsifiable benchmark:
cheap tier, vector column, fusion, real corpus, content address, image ABI, video-as-shots, the
face primitive, and `p65` — the field-budget collision the corpus run surfaced, where an image
document wants seven columns and `index_text::MAX_FIELD` allows four.




















## p81 — reading the incumbent's source, and finding it had already fixed half of what we claimed (2026-09-07)

A side-by-side comparison page against profstopick's search. Building it required reading their
**current** matcher rather than the production measurement this repo keeps quoting, and the two no
longer agree.

### Fixed — a claim this repo had been making that is no longer true

`bench/roadmap/p68-opfs-tier.md` published `name order reversed | miss | hit`, and `README.md`
attributed profstopick's zero-result rate to *"mostly name-order misses that now resolve."*

**profstopick fixed the name-order half themselves.** `src/lib/search-match.ts` carries a
backtracking distinct-token assignment — and their own comment cites the 2026-08-17 measurement as
the reason they added it. Verified against their code, ported line for line into
`js/profstopick-match.mjs`: `raphael abacan` reaches `ABACAN, RAPHAEL` in *their* matcher, with no
help from us.

Their matcher also folds diacritics (`pena` → `PEÑA-REYES`) and strips punctuation. It is a good
typeahead. **What survives as a genuine gap is the misspelling half**, which a prefix/substring
matcher cannot reach by construction.

### Verified — the headline benchmark table is unaffected

Checked before touching it. `PrefixBaseline` in `real_corpus.rs` requires every query token to match
somewhere in any order, so it already handles reversed names, and it has no edit distance anywhere.
The published comparison is a *typo* set, so **engine 99.9 % vs baseline 14.9 % typo hit@10 stands
unchanged.**

### Two findings about our own side

- **`prefix: true` is not optional.** The first version of the page omitted it, which matches the
  last token as a complete term and so finds nothing for any half-typed query: `pen` 0 instead of 6,
  `gar` 0 instead of 10, `cru` 0 instead of 10. That was a demo bug that badly understated the
  engine.
- **`index` alone is a downgrade for this typeahead.** Even with prefix on, its fuzzy expansion
  outranks the obvious prefix hits on short fragments: `pen` leads with `TEH`, `gar` with `GARDON`,
  `mar` with `BANARIA`, `joh` with `CO, SR. MA. ANICIA`. Their matcher leads correctly on all four.

### Added

- **`js/compare.html`** — profstopick's matcher, `index`, and both merged, running live in one tab
  on the same 1,322-professor snapshot.
- **`js/profstopick-match.mjs`** — a line-for-line port of their matcher. A port, not a paraphrase:
  if it and theirs disagree, this file is wrong.
- **`js/hybrid.mjs`** — the merge. Literal matches rank above fuzzy ones, because a character the
  user typed is evidence and an edit the engine guessed is inference. Neither source is re-scored.
  **Over 16 queries its top result is the better of the two on every one** — the only configuration
  strictly better than what profstopick ships today.

The two are complementary structurally, not incidentally: **theirs does infix and `index` cannot**
(`gracia` → `DIVINAGRACIA`; a term dictionary holds that as one token, so no FST walk reaches it,
and `index` offers `GARCIA` at distance 1 — the wrong professor), while **`index` does typos and
theirs cannot**.

### Honest gaps

- **The merge is a demo.** Not in CI, no test, only a 16-query check run by hand.
- **The 40.8 % figure should stop being quoted as a current number.** It measures a matcher that has
  since changed. Nobody has re-measured profstopick's zero-result rate against their current code,
  and this repo should not imply otherwise. The measurement remains valid as history and as the
  reason their fix exists.
- **`index` cannot do infix.** Not a bug to file but a property to state; reaching `DIVINAGRACIA`
  from `gracia` needs a suffix automaton or n-gram terms, both of which cost bytes.

## p78-p80 — the browser payload, the last allocation, and a default that had to be inverted (2026-09-07)

The close of the fan-out, and the point at which the box finally went quiet enough to measure
properly. Two lanes landed; one earlier lane was **partially reverted** on the evidence.

### p78 — the browser payload nobody was watching

`README.md` had claimed **173,700 bytes** for the WASM module for a long time. The real figure was
**604,184** — a 3.5x growth, found only because a literal transcript in the README was *re-run*
instead of hand-edited. The browser tier is this repo's uniquely defensible position, so nobody
watching that number was a gap in the gate, not merely in the docs.

- `.cargo/config.toml` scopes `-C strip=symbols` to the `wasm32` triple, removing a **46,513-byte
  `name` section** of Rust symbol names only a profiler reads. Deliberately a cargo config rather
  than a `[profile]` key: `strip = true` at the workspace root applies to *every* target and would
  cost `cargo test --release` its symbolised backtraces while buying zero bytes natively.
- Measured on the merged tree: `index_wasm` **559,970 raw / 203,769 gzipped** (code now 93.3 % of
  the module), `index_geo_wasm` 77,065 / 31,072, `index_accel` 5,946 / 2,802.
- **The finding is larger than what shipped.** `core::slice::sort` is **199,029 bytes — 38.3 % of
  all code — across 146 monomorphizations**; all of `index-text` is 148,753 by comparison.
- The lane's own main hypothesis **failed**: rewriting every `sort_by` to `sort_unstable_by` bought
  9,249 bytes and was *worse* gzipped, because `ipnsort` is nearly as big as `driftsort`. The cost
  is the number of distinct `(T, comparator)` pairs, not stability.
- `opt-level="z"` was the smallest on the board (477,121) and **2.1x slower per query**.
  `opt-level=2` was the near miss — speed-neutral *in wasm* — rejected because a target-wide
  rustflag also hits `index-accel`, where `bitmap_op` collapses **7,344 → 2,408 M rows/s**, and the
  obvious carve-out does not work: rustflags are appended *after* cargo's own `-C opt-level`.
- Ships `scripts/wasm-section.mjs`, a dependency-free section reporter, so the number stops being
  invisible.

### p79 — the allocation was jointly owned

`p75` named `Vec<Token>` as the last large build item and listed three shapes to fix it. **The
measurement picked none of them.** Reusing the vector and freeing the token strings is worth 180 ms
of a 1,536 ms item — 12 %. The other 1,356 ms is 13.69 M malloc/free pairs.

And the buffer **alone measured 32 ms slower** in the real build. The old map insert *moved* the
token's string in, so the free belonged to the map: one malloc in the analyzer, one free in the map.
Reusing the buffer without changing the map merely relocates the malloc into the clone. **Either
half alone removes one end of a pair and therefore removes nothing** — which is why the lane needed
`index.rs` as well as `analyze.rs`.

`tokenize` is unchanged as public API (a three-line wrapper), so `p75`'s four `mod legacy`
equivalence tests pass **unmodified, with no adapter**.

### p80 — the query fan-out is opt-in now, because the default was 2.4x slower

`p74` shipped threaded per-segment queries **enabled by default** and noted its speedup had not been
independently reproduced. Reproducing it inverted the default.

`segment-scale` on presyo's 241,789 real products, alternating `INDEX_PARALLEL=0` and `=1` so both
arms meet the same machine, at the rungs the gate actually selects:

| segments | serial p50 | threaded p50 | |
|---|---|---|---|
| 25 | 5,141 / 5,792 us | **12,528 us** | **2.2-2.4x SLOWER** |
| 50 | 14,235 / 11,296 us | **26,966 us** | **1.9-2.4x SLOWER** |

25 x 241,789 = 6.0e6 clears the 4.5e6 gate, so **the threaded column is literally what `p74` shipped
as the default.** Both measurements are correct — `p74`'s were taken on a quiet box, and this
machine was running its owner's ordinary applications. `p74` predicted the pathology precisely and
then defaulted the other way. **The error was treating contention as an exceptional condition worth
a kill switch, when for an embedded library it is the ordinary one.**

`INDEX_PARALLEL=1` is now an opt-in rather than a kill switch, plus `Searcher::set_parallel(bool)`
and `Searcher::parallel()`. `PARALLEL_WORK_MIN`, the work estimate, the claim queue and the
segment-index sort are untouched: nothing about `p74`'s machinery was wrong, only its default.

### The build number, finally measured on a quiet box

Every speedup in `p73`-`p79` was reported as the lane's own, taken before merge, because three to
four build-heavy agents held cores throughout. With the fan-out finished the machine went quiet, and
the pre-`p73` tree (`0073828`) was rebuilt and re-run beside the merged one. **Minimum of three runs
each side, `bin/scale`:**

| documents | pre-`p73` | now | speedup | bytes |
|---|---|---|---|---|
| 61,467 (real) | 671 ms | **223 ms** | **3.01x** | identical |
| 250,000 | 2,945 ms | **778 ms** | **3.79x** | identical |
| 1,000,000 | 12,404 ms | **3,180 ms** | **3.90x** | identical |

This **supersedes the ~3.6x figure** recorded in the `p74`-`p77` entry below, which was computed
against an 18,436 ms baseline measured while the box was loaded. It also settles a question the
earlier entries could not: the typo p99 at 1 M is **14,146 us before and 14,432 us after** — the
build work did not touch query latency in either direction.

### Fixed

- **`README.md` claimed the browser tier had no persistence**, three lanes after `p68` shipped OPFS
  and one after `p72` shipped range reads.
- **`README.md` claimed no WASM artifact existed.** One has existed and been gated in CI for a while.
- **`README.md` claimed the 5 ms typo bar "is met on every corpus of real documents."** `p55` had
  already corrected that claim in the roadmap; the README kept it. It holds to ~250 K; `p56`
  measured 8.29 M real rows at 33 ms.
- **The README scaling table was stale in every column** — 796 ms / 10.2 MB at 61,467 against a
  measured 223 ms / 5.2 MB.
- **`scripts/build-wasm.sh` said "C ABI v13, 69 symbols."** `ABI_VERSION` is 14 and the module
  exports 81 `idx_*` of 84 total, counted out of the binary.
- **~20 harness environment variables were documented nowhere.** Now in `bench/README.md`, with the
  library's single variable in the README's new Configuration section.

### Honest gaps

- **`pool-audit` is not in CI.** The ranking gate that every merge in this repo is checked against
  runs by hand, because it needs sibling corpora that are not vendored. Nor are
  `host/python/index_ffi.py`, `js/demo.mjs` or `js/accel-bench.mjs`.
- **`p74`'s 2.9x tail win has been reproduced by nobody but the lane that found it.** It is gated
  behind an opt-in and remains unconfirmed on a second quiet measurement.
- **The `docs/benchmarks.md` headline grid still predates `p69`.** Regenerating it needs a live pull
  from presyo; the fixture is not committed. It carries a staleness banner rather than a fix.
- **`core::slice::sort` at 38.3 % of the wasm module is untouched.** `p78` sketches the collapse —
  pack `(f32 score, u32 doc)` into one `u64` whose natural order is the ranking order — and nobody
  has built it. It is the largest single lever left in the repo.
- **Safari is untested** for the OPFS tier; only Chromium is checked.

## p74-p77 — concurrency on both sides, the tokenizer, and a geo bug that answered confidently (2026-09-06)

The second half of the fan-out, after the free-model tier's quota ran out and the remaining lanes
were relaunched on a different worker. **Every lane was gated here on the real tree**, and in three
cases that gate caught something the lane could not: two lanes numbered their roadmap doc `p74`
(already taken, and colliding with each other), and one lane's `pool-audit` had silently run **three
of six query sets**, because a temp worktree's relative corpus path misses presyo.

### p74 — the query fans across segments

`Searcher` spreads its per-segment pass across OS threads. **Tail-led, and the doc says so**: at 25
segments the p50 is a wash (1.22x) while the p99 comes down **2.9x**; at 50 segments 2.5x and 2.9x.
Rungs below the gate run identical code in both arms, which makes them a control rather than a
result.

The gate is a **work estimate** — `segment_count * doc_count >= 4_500_000` — not a segment count: 50
segments of 200 documents is fifty times nothing and fifty spawns, and one huge segment cannot be
split at all. Forcing threads at 10 segments measured 883 → 1,473 us, a **loss**, and the gate
excludes exactly that rung.

`INDEX_PARALLEL=0` is a kill switch, and it is earned: **under machine contention the threaded path
is pathological** — a 50-segment p50 of 36,271 us, 4x worse than serial. It is also what a WASM
build or a server that already owns its core budget wants.

### p75 — the tokenizer, and two suspects the measurement killed

`p73` named this as the next lane. It profiled first, and **the profile killed both mechanisms the
brief proposed**: "skip re-lowercasing already-lowercase text" is worth **0.004 %** (the corpus is
97.7 % ASCII but only 3,270 bytes of 86 MB are already lowercase), and "avoid the per-token heap
allocation" — the obvious suspect — is **170 ms of 6,757, i.e. 2.5 %**.

The cost was two *whole-field* allocations and char-at-a-time scanning. The `Vec<char>` is deleted:
token text is a contiguous **byte range** of the folded string, which is structural rather than
incidental, because the only rewrite the split makes is a separator becoming `.` (all one byte, so
length-preserving) and both lookarounds are `is_ascii_digit`, which no multi-byte character can
satisfy.

**Analyzer 6,757 → 3,642 ms. With `p73`, a million-document build has gone 18,436 → ~5,478 ms — 3.4x
— with the serialized bytes unchanged throughout.**

The token stream is *proven* unmoved rather than assumed: the pre-change functions are copied
**verbatim** into a `mod legacy` and asserted equal over 54 cases — combining and precomposed
accents, six non-Latin scripts, emoji with regional indicators, a 100,000-character mixed-script
field, and **every ASCII byte 0..127**.

### p76 — `index-core` primitives, and a main hypothesis that failed

First, a fact the crate had never stated: **`index-core` is an unused research crate.** Nothing in
`index-text`, `index-geo`, `index-image`, `index-cli` or `index-wasm` links it; only `index-bench`
does. That makes its numbers the only thing it produces, so every structural choice in it has to be
measured rather than plausible. Four were not.

- **The wavelet documented `n·H₀` and delivered `n·log σ`.** Huffman-shaped over the present
  alphabet: FM bits/char on Zipf text **12.350 → 8.417** and **12.165 → 6.631**, −32 % to −46 %. The
  before column barely moves with entropy; the after column tracks H₀.
- Suffix array O(n log²n) → O(n log n): **2.0–3.6x** build.
- Rank directory packed to `{u32, u32}` per 256 bits: speed-neutral, **1.25 bits stored per payload
  bit instead of 1.50**.
- **The lane's own main hypothesis failed**: interleaving the rank directory into a 64-byte cache
  line is **2x slower at every size** (32 KiB 5.82 → 15.23 ns/rank), because `i / 448` sits on the
  dependency chain of every rank and the side directory was never actually missing. Reverted, and
  recorded.

### p77 — the geo tier: a correctness bug, a 24x build, and a conclusion overturned

**The bug is worth more than the speedups.** `segment_cell` casts with `(v.floor() as i64)`, and
**Rust saturates a NaN cast to 0**. A segment meeting a NaN vertex rasterizes into column 0, the
cells it really crosses are never marked boundary, the interior fill claims them, and they then
answer with a stored polygon id **having run no geometry at all** — no error, no signal. Measured:
NaN and `NEG_INFINITY` each produced **300 disagreements over 40,401 probes**; `INFINITY` produced 0
purely by luck, saturating the other way. `geo_build` is documented as parsing network data.

**A published conclusion is overturned.** `p9` concluded over-fetch was *"dominated by point
clustering rather than by cover coarseness"*. Wrong — the cover kept pending cells on a **stack** and
emitted whatever was on it when the budget ran out. Best-first splitting gives **16.46x → 1.14x
over-fetch at the same budget, with fewer ranges, at identical 100 % recall.** It also corrects the
Hilbert story: over-fetch is now equal between curves, so Hilbert's entire advantage is 1.8x fewer
ranges — fewer HTTP range requests, not a locality claim. `p9` and `p10` carry supersede notes.

Build **7,002 → 285 ms (24.6x)** with every cell count and interior/boundary split byte-identical.
Memory 323 KB and 8,077 allocations → **143 KB and zero**. New: `polygon_in_view`, and
`to_bytes`/`from_bytes` in the same section-table shape as `index-text`, which is what `p68`/`p72`
need to reach this tier.

### Honest gaps

- **Every speedup in `p73`–`p77` is the lane's own, taken uncontended before merge.** Correctness was
  verified here in every case — tests, `pool-audit` 6/6, byte-identity — but the *timings* could not
  be, because three to four build-heavy agents held cores throughout. **One re-run on an idle box is
  owed**, covering these and the `docs/benchmarks.md` grid together.
- **The repo asserts two spawn costs that differ ~6x** — `p73` measured `thread::scope` at ~87 us per
  thread, `p74` at ~550 us for a one-thread scope, on the same machine. Both thresholds stay safe
  under either reading, but one measurement should settle it.
- **`p74` and `p73` were measured separately and now compose.** A process that builds and queries
  concurrently can oversubscribe in a way neither lane measured alone.
- **`index-geo` and `geo_join.rs` are now two implementations of one row-sweep algorithm**, and only
  the library half has unit tests.
- **The query at geo's actual operating point did not get faster.** Everything at L=6–8 landed inside
  the noise; the narrower key pays only at L=10–12, which is not where it runs. Reported as a
  non-result rather than quietly omitted.

## p69-p73 — the fan-out: half the bytes, a parser, a range tier, and a build that was mispriced (2026-09-06)

A parallel fan-out across isolated worktrees, part of it on a free non-Anthropic model until that
tier's quota ran out mid-run. **Every lane was gated here on the real tree before merge** — a
worker's report is a claim about the tree, not the tree — and three of the five merges required
fixes the lane could not have made, because the format changed underneath it.

### p69 — delta-varint the posting lists

The posting list was the largest section in the file and still fixed width. Doc ids ascend within a
term's list, so their deltas varint-encode well.

- **Bytes per document 106.4 → 60.0** at 100 K, 105.4 → 54.2 at 250 K, 101.3 → 51.5 at 500 K.
  profstopick's shipped artifact went **386,043 → 220,471 bytes**.
- **The tail improved with the size rather than paying for it** — typo p99 at 250 K went
  5,183 → 3,286 us. Smaller postings mean more of a list per cache line.
- The count is written **per list**, not once per section, so a single posting span still decodes on
  its own. That is the range-read property `p72` consumes.
- **Fixed before merge:** the lane grew `MAGIC` to nine bytes, which breaks the fixed head a range
  reader depends on — you cannot know the head's size until you have read the version out of it. The
  magic is held at eight bytes forever as `IDXTXT10`, dropping the `E` rather than the invariant.

### p70 — a query parser

`p45` left `search_phrase` treating the whole query as one phrase, so `red "ice cream"` was
inexpressible. `parse()` separates terms, quoted phrases and exclusions — a pure function over a
string, no index access, no new dependency, with a fuzz loop asserting it never panics.

**Its own test caught a real bug:** `flush` cleared the negation marker unconditionally and an
opening quote also flushes, so a negated phrase lost its marker and was filed as a *required* phrase
— the exact inversion, silently.

### p71 — the TOAST placeholder, refused by name

`p67` measured that every table with a TOAST relation in the estate holds TOASTed data (7.8 GB in
presyo alone), and that logical decoding emits a placeholder rather than the value for an untouched
TOASTed column. `--require` guarded the *empty* case; the placeholder is a **sentinel string**, so it
waved it through. `--placeholder VALUE` now refuses the record naming the field and the key, and
`index stat` reports empty-value counts per field so blanking that already happened is detectable.

### p72 — the range tier: query an index without reading it

`p68` left the range path unwired; `p56` measured a 10 M index at ~910 MB, which a tab cannot hold.

- **A prefetch plan, not a lazy handle.** `createSyncAccessHandle().read()` is worker-only and
  synchronous and cannot be awaited from inside a WASM call without JSPI or cross-origin isolation.
  A plan is also inspectable — the host can count the bytes, and the byte count is the claim.
- Measured in a real Chromium: opened from 296 B + resident sections, **82,785 B of 220,470 read =
  37.5 %**, **4/4 hits identical** to a whole-file open, **0 network calls**.
- Answers are exact, not approximate: where a tie-break would need a `df` the planner has not read,
  the call verifies and returns `u32::MAX` so the host falls back to a full open.
- **Fixed before merge:** unfetched terms were given a zero-length span, which `p69` had just made
  invalid — an empty list is now the byte `0x00`, and a zero-length span is unreadable rather than
  empty. Also the Python host's ABI pin was not moved 13 → 14.

### p73 — the build was never bound by the thing everyone threaded

`p56` closed with *"no concurrency"*, which reads as an invitation to thread `build()`. Profiling
first showed `build()` and `rebuild_meta()` were **2.6 s of a 24.3 s** million-document build, so
threading them alone caps at 12 %. The real cost was the builder's inner
`BTreeMap<u32, [u16; MAX_FIELD]>` — 15 M node allocations sorting data that already arrived sorted.

- Append-only posting columns plus threading on `std::thread::scope`, **no new dependency**.
- **1,000,000 documents: 18,436 → 9,732 ms (1.89x)**, and **bytes identical at all five scale rungs**.
- A test builds a corpus that *asserts* it crosses the threading threshold, then rebuilds it
  genuinely serial via a `cfg(test)` thread-local and compares `to_bytes()`.
- **Corrects the lane's own 2.38x claim**: it branched from a pre-`p69` base, so its "before" column
  was slower than this tree's.

### Rejected in the same fan-out

- **prefix-bleed** — gave every strict prefix extension +1 edit distance so an exact term outranks
  its own extensions. Tests passed, clippy clean, principled. But `typo_bucket` is the *primary* sort
  key, so the +1 demoted legitimate typeahead: **maphy-place hit rate 76.2 % → 46.9 %**, a 29-point
  regression — and it **did not move the metric it targeted** (sisia prefix bleed stayed at 1,391
  extra rows against LIKE's 295), because the change reorders results rather than removing them.

### Honest gaps

- **The free-model tier ran out mid-fan-out.** Two lanes returned with no changes for that reason,
  not because the work was done; they were relaunched on a different worker.
- **`p70`'s parser is not wired to the ABI.** It parses and is tested; `idx_searcher_*` still takes a
  pre-split clause set.
- **`p72`'s 37.5 % is one artifact and four queries.** The fraction should fall sharply with file
  size, and that is unmeasured.
- **`p73` leaves tokenization as the largest single build cost** — 6.9 s of the remaining 9.7 s at
  1 M — and query remains single-threaded.

## p56, p67, p68 — the browser tier, the scaling grid, and the connector priced (2026-09-06)

### p68 — the OPFS tier (see also `js/opfs.mjs`, `js/opfs-worker.mjs`, `js/opfs.html`)

- **Shipped the tier `docs/research/landscape.md` §7.5 says nobody occupies**: an index living in the
  browser's own filesystem, queried with **zero network calls**, and range-readable so a file larger
  than the tab's memory can still be opened. It is open architecturally, not by neglect — Tantivy's
  WASM RFC has been open since 2019 and is read-only, LanceDB closed WASM as "not planned", Orama has
  no wasm32 target, and Meilisearch and Elasticsearch are servers.
- **0.086 ms per query in-tab against a 2.300 ms empty localhost round trip — ~27x faster than a
  server that does no work.** The comparison is deliberately the friendliest possible server: no TLS,
  no queue, no distance, no work.
- **296 bytes to learn the layout of the whole file** (0.077 % of it), via a sync access handle in a
  worker. That is what the fixed section table in `format.rs` was always for; nothing had exercised
  it from a browser until now.
- Replaces profstopick's 2,505,813-byte JSON shard — 95.6 % of the 5 MB localStorage quota — with
  **15.4 % of that**, and resolves the name-order misses behind its 40.8 % production zero-result rate.
- `node js/opfs-check.mjs` drives a real Chromium and is in CI. Fixed a missing `await` in `drop()`
  that leaked an unhandled rejection on every first visit.

### p56 — 10 M: the capacity holds, the bar was never at a million

- **8,290,639 real product rows indexed** (presyo `raw_product` + `extraction_insight`, one pipe).
  84 s, 729 MB, **91.2 B/doc — bytes per document IMPROVE with scale** (106.4 → 91.2).
- **The 5 ms typo-p99 bar holds to ~250,000 documents.** Not 1 M, where `p7` set it. Two independent
  corpora agree: this ladder crosses between 100 K and 250 K, and presyo's real 241,677-product
  catalogue measures 4.54 ms. **The bar was set 4x too high, not the engine 1.7x too slow.**
- **Capacity was never the ceiling; latency is.** Median stays under a millisecond to a million and
  reaches 3 ms at eight.
- **10 M of real text does not exist in this estate** and was not manufactured: `p55` measured
  recombination overstating the tail ~2x, so padding to a round number would report a worse answer
  than admitting the ladder stops at 8.29 M.

### docs/benchmarks.md — the grid nobody in this category publishes

- The survey found that **almost nobody publishes p50/p99 at 100 k / 1 M / 10 M** in the embedded
  category, and that **no search product publishes an end-to-end index-visibility number at all**.
  Both are now published here, including a §6 that collects every failure in one place.

### p67 — §7.1 and §7.2 priced against the estate

- **Measured 255 real tables across 8 databases: all 255 are `REPLICA IDENTITY DEFAULT`, none
  `FULL`.** That is normally a connector's most invasive prerequisite; `p50`'s state-based design
  never needs a before-image, so the estate is consumable as-is. Not luck, but not foreseen either.
- **Found a latent correctness bug in what `p50` shipped.** Every table with a TOAST relation in the
  estate holds TOASTed data — 7.8 GB in presyo alone — and logical decoding emits a **placeholder**
  for a TOASTed column an update did not touch. `apply` replaces whole documents and the engine
  stores no field text, so **a partial update is not expressible**: an unrelated UPDATE would
  silently blank the column and search would stop finding the row.
- **Added `index apply --require NAME`**, which refuses such an upsert with the key and input path.
  It is a guard, not a fix — the real answer is a producer that re-SELECTs the row on update, which
  is exactly what "correctness-first" means in §7.2 and is unbuilt. Gated both ways in `cli-smoke`.
- **Noted the prerequisite nobody mentions**: the estate runs `wal_level = replica`, and logical
  decoding needs `logical` plus a server restart.
- **§7.1 (reading Postgres' own storage) rejected**, recorded in `docs/roadmap-rejected.md`: it costs
  the browser tier, it inherits rather than fixes the 54 ms → 188,442 ms GIN problem, and the
  category has four competing extensions, none in core and none on RDS.

## p52-p55 — the segmentation ceiling, keys on the ABI, varint positions, and a real million (2026-09-06)

### p52 — collection-wide statistics

- **`p38` and `p50` hit the same wall from opposite directions and neither fixed it**: each segment
  scored IDF against its own collection statistics, so a term was rare or common according to the
  shard holding the row rather than the corpus. A three-row delta scores a term at 0.98 where a
  4,001-row base scores 7.89.
- **The obvious fix made it worse.** Correcting only `doc_count` left `df = 1` against a corpus of
  200 and inflated small segments instead of correcting them —
  `ranking_skew_is_bounded_for_a_small_delta` rejected it in twenty minutes. **A partial correction
  of a ratio is not a partial improvement.**
- **Summing `df` can only be keyed on the term's TEXT**, because a term id means something different
  in each dictionary. The FST stores no strings, but its stream already yields the key bytes during
  traversal and the expansion was discarding them. The single-index path never pays.
- **Broad-query agreement 51-64 % -> 84-87 %, selective rank-1 96-98 % -> 99.0-99.8 %, and now FLAT
  in segment count** — two segments and fifty give the same quality, which is what `p38` could not
  achieve by tuning. On a change stream the decay flattens: rank-1 fell 9 points over 8,000
  operations and now falls 3.3; overlap fell 21 and now falls 7.5.
- **Costs +13 % to +25 % on p50 at the recommended segment counts and ~2x on the typo tail**, timed
  interleaved in one process because the machine was too noisy for cross-run comparison.
  `Searcher::set_collection_stat(false)` trades it back; on by default.

### p53 — keys reach the C ABI

- **`p48` left keys unreachable from anything but Rust**, which meant a browser or Python service
  could search a live collection and never update it: the only deletion the ABI offered takes a
  dense ordinal assigned at insertion that no database row carries.
- **Eight symbols, ABI 12 -> 13, 69 total.** An update needs none of them beyond the build-time key:
  it is a delta plus `idx_searcher_push`, which retires the shadowed row itself.
- **Three behaviours pinned because each is a trap**: `doc_of_key` finds DELETED documents (that is
  what you need in order to delete them); deleting an absent key returns 0 rather than erroring (a
  replayed stream re-delivers deletes, and replay is a pipe consumer's only recovery); every failure
  returns a sentinel rather than trapping.
- Gated in all three places — a Rust ABI test, `js/smoke.mjs` through the raw ABI (113 checks), and
  `host/python/index_ffi.py` — because `p44` already learned what an ungated host does.

### p54 — delta-varint positions

- **`p45` published what was wrong with phrase support and declined to guess at the fix**: the offset
  array cost 2.2x the positions it addressed, because most (term, document) pairs carry one position
  and each paid an eight-byte offset to say so.
- **Phrase support now costs +15.9 % of artifact size instead of +74.7 %** — position sections
  1,778,052 B -> 377,061 B, 12.64 -> 2.68 bytes per occurrence, a 4.7x reduction. Correctness
  re-verified identically: 980 hits over 184 real phrases, 0 wrong.
- **The reader got stricter, not looser.** Variable width removes the length check fixed width gave
  for free, so the count is explicit and checked, the first offset must be 0, deltas are unsigned so
  monotonicity is structural, and every accumulation is `checked_add`.
- Format `IDXTEXT8` -> `IDXTEXT9`. Also made the format-version test compare against `MAGIC` rather
  than a hard-coded string, so the next bump need not remember to edit it.

### p55 — the 5 ms bar at a real million, and a correction

- **`p47` named the missing input and `p51` found it by accident**: presyo's `raw_product` holds
  4,524,754 real rows. One `psql | index build` pipe later the question is answerable.
- **The bar fails on real data too: 8.12 / 8.69 / 8.84 ms at a real million** (202,727 terms), against
  13.58 ms on the recombined corpus (41,069 terms, held fixed). **Recombination overstated the tail
  by ~1.6x but did not invent the failure.**
- **This corrects `p47`**, which wrote "on every corpus of real documents this project has, the bar
  is met" and had it repeated into README and ROADMAP. That was true only because the largest real
  corpus was 241,677 documents; the separation it drew was between corpus SIZES and was mistaken for
  a separation between real and synthetic data.
- **The expansion cap, priced on real vocabulary**: cap 8 -> 6,961 us at 99.45 % top-10 agreement,
  cap 4 -> 6,273 us at 96.90 %, cap 2 -> 5,199 us at 92.70 %. **Nothing reaches 5 ms.** The bar stays
  red and unraised for the fourth document running, but is no longer arguable.

## p51 — the claim, tested against every database in the estate (2026-09-06)

- **Swept 122 databases across 5 machines and 3 engines**: PostgreSQL 16/17/18, SQLite and DuckDB,
  on Windows, macOS and three Linux hosts, reached over Tailscale by SSH. **86 indexed, 36 verified
  empty, 0 failures, 2,226,041 rows.** Evidence in `bench/evidence/p51-database-sweep.tsv`.
- **Every index was built by the local Windows binary from rows streamed over SSH**, so the data
  crossed an OS boundary, a CPU-architecture boundary and a network on the way in. Nothing was
  installed on any remote host except a 90-line script that prints rows.
- **Typo tolerance held on 86 of 86** probes that returned hits. Where it did not fire the token was
  numeric, which is never fuzzy-matched by design.
- **Found and fixed a real bug: real databases are not valid UTF-8.** Thirteen SQLite databases
  aborted the build with `stream did not contain valid UTF-8`, because one stray byte anywhere in a
  120,000-row export destroyed the whole thing. The reader now takes bytes, repairs invalid
  sequences to U+FFFD, counts them, and reports the count — repaired rather than skipped, because
  dropping the row builds an index quietly missing data; counted rather than silent, because the
  operator's export pipeline is probably wrong. All 13 now index.
- **Verified continuous sync against a live production database** (`bygelo/blead_scraper`, 4,000 real
  schools over Tailscale): membership exact after an update, a delete and two inserts, and 21 of 21
  real queries agreed with a full rebuild on rank-1 and on the whole top-10. Disagreement appears
  only at k=25, and the score gap shows why — 1.31 against 7.01 for the same row — because the delta
  segment holds three documents and computes its statistics over three. `p38`'s effect at its
  pathological limit.
- **Recorded the sweep's own three bugs**, because each looked like a product failure: an unquoted
  `-F|` that the local shell read as a pipe (17 false "no text table" results), Python's `cp1252`
  stdout on Windows feeding the CLI non-UTF-8, and `ssh` inside a `while read` loop eating the
  loop's stdin. None were visible without running against real data.

## p48, p49, p50 — any database, over a pipe, and kept current (2026-09-06)

Asked for "something that works on any database", with continuous sync. Those two answers together
rule out the obvious design: a CDC daemon needs a driver per source, and a driver only ever works on
the databases somebody wrote one for. So the pipe is the integration, in both directions.

### p48 — document keys

- **The engine could not say which row it meant.** `Searcher::delete` takes a dense global ordinal
  assigned at insertion, and no database row carries one, so *"the row with `sku = 'A-1'` changed"*
  was inexpressible. `p37` shipped live updates and `p38` measured them, but both were about
  *appending*, which needs no identity.
- **Added `IndexBuilder::with_key`**, `Index::doc_of_key` / `key_of` / `keyed_count`,
  `Searcher::doc_of_key` / `delete_key` / `key_of`. Format `IDXTEXT6` → `IDXTEXT7`, section table
  17 → 18 spans. No ABI change.
- **The invariant everything rests on: at most one live document per key.** Segments are immutable,
  so an update can only mean *append the new version and retire the old one*. `Searcher::push` does
  the retirement, because a caller who forgets gets no error — just a collection accumulating every
  historical version of every row and returning all of them.
- **A blank key is NO key, not a key of `""`.** Two such rows would both answer to the empty key and
  a change stream would update an arbitrary one. They stay searchable and become unaddressable, and
  `keyed_count()` drops below `live_count()` so an operator can see it.
- **Keys are positional, so the reader counts them against `doc_count`.** A truncated key array
  shifts every later key onto the wrong document; `doc_of_key` then resolves a real key to a
  real-but-wrong row and a change stream updates the wrong record. Corruption that looks exactly
  like working software, so `a_short_key_array_is_refused` requires an error.
- **Disambiguated the "NOT PRESENT, DELIBERATELY: an internal->external document id map" comment**,
  which reads like a prohibition on exactly this. That one is a docID *reordering* permutation for
  ranking, measured worse in `p7`; a key is an application identifier and touches no ranking.

### p49 — the `index` CLI

- **New crate `index-cli`, one dependency: `index-text`.** The CSV, TSV and JSON readers are
  hand-written, so the tool a server runs has no supply chain of its own.
- **`index build` / `apply` / `search` / `stat`**, all over stdin. Verified against a real DuckDB
  database on the build machine, not a fixture.
- **A record is a name→value map, never a positional tuple**, which is what lets one set of flags
  (`--key`, `--facet`, `--field`) serve CSV, TSV and JSONL alike. JSON nesting addresses as
  `after.sku`; `null` is an **absent** path rather than the string `"null"`, or every nullable
  column would index a word no row contains.
- **Refusing beats guessing**: a ragged row is an error (a loader that drops rows builds an index
  quietly missing data), an unknown `op` word is an error (guessing "upsert" would apply a delete as
  an insert), and `apply` on a keyless collection is refused.
- **A collection is a directory of zero-padded segments**, because a tombstone lives inside a
  segment's bytes and applying changes rewrites some files while appending another. Zero padding is
  load-bearing: `1.idx, 10.idx, 2.idx` loads out of order at the tenth segment and silently inverts
  newest-wins, resurrecting superseded rows. Every write is a temp file renamed into place.
- **Compaction is a rebuild**, stated in `--help` rather than discovered: the engine does not store
  field text, which is why the index is small, so it cannot regenerate a segment from itself.

### p50 — the change stream, and the bug its bench found immediately

- **`cdc-equivalence` reported 212 keys resolving wrongly on its first run** — a real bug in `apply`.
  Upserts were batched into one new segment while deletes tombstoned existing ones, so **every
  delete in a batch was applied before any upsert in it**, whatever order they arrived. A stream
  saying `upsert k` then `delete k` ended with `k` present. Nothing threw; membership silently
  diverged from the source of truth, which is the exact failure the feature exists to prevent.
- **Fixed by collapsing the stream per key.** A change stream is a sequence of *states*, so a key's
  last record decides it. That makes the order between upserts and deletes irrelevant **by
  construction** rather than by careful sequencing, and a thousand updates to one row become one
  document. `scripts/cli-smoke.sh` drives the hazard through the real binary and is now in CI.
- **Membership is exact and gated: 8,000 operations, 0 keys wrong, 14,288 = 14,288.**
- **Ranking drifts, and is measured rather than gated** — 93 % rank-1 agreement with a rebuild after
  the first delta, 86.7 % when `needs_compaction` fires, 84.0 % after 8,000 operations. Two
  documented causes compaction removes together: a deleted row still counts toward document
  frequency until a rebuild, and each segment scores against its own statistics (`p38`).
  **This is the first measurement of what deleting and updating cost as distinct from appending** —
  `p38` measured appends only.
- **The metric was wrong first, and `p38` had already paid for that lesson.** Exact top-10 sequence
  equality read 15 % and looked catastrophic; `p38` had recorded that number as *"true and
  useless"* on broad queries, because near-ties make the order arbitrary. Switching to selective
  queries and `p38`'s metrics gave the table above. The retired metric is still printed, so a reader
  can see why it was retired rather than take it on trust.
- **The gate is set below the observed value on purpose.** A bar fitted to today's measurement only
  ever says "nothing changed"; this one is a regression detector, and what the number means is
  argued in the roadmap document instead.

## p47 — the typo tail: one lever that worked, two that did not (2026-09-06)

- **`eff` is now exactly the final comparator.** It was built from the raw score while `rank_cmp`
  compares the quantized one, so two documents whose scores differ but quantize equal were ordered
  by score in the ranking pool and by document id in the answer. The union with the scoring pool was
  quietly covering for it. `eff_induces_exactly_the_final_ranking` pins the property and fails on
  the old formula.
- **Consequence, and the reason it was worth doing:** the ranking pool now provably holds the top
  `k` by the final comparator, so the scoring pool contributes no document that survives truncation.
- **The scoring pool is sized to `k`** instead of `max(3k, 32)`. Recall is no longer its job; its
  only job is to supply the pruning threshold, and a tighter pool raises that threshold and engages
  more block skipping. **Exact p50 at 1 M fell 35 % (820 → 529 us), typo p50 22 %, presyo real-query
  p99 12 % (1,213 → 1,062 us)** — with `pool-audit` still 0.00 % in all six cells, `real-corpus`
  still 100 % recall@10, `presyo-catalog` still 97.0 % precision@10, `presyo-expand` byte-identical.
- **The bucket-floor block skip is no longer gated on the score threshold.** Sound now that `eff` is
  exact; measured **latency-neutral**. It ships because it is free and strictly more general, not
  because it helped. Its first version was a 25 % regression, because ungating ran an O(terms) loop
  on every candidate — fixed by computing how large the bucket floor would have to be before looking
  for it. **A sound bound evaluated in the wrong order is a pessimization.**
- **The 5 ms typo-p99 bar is met on every corpus of real documents** — 4.54 ms at presyo's 241,677
  products, 1.81 ms at 61,467 real schools. It still fails on `scale`'s recombined rows (6.16 ms at
  250 K, 13.58 ms at 1 M), which hold vocabulary fixed while multiplying documents. The bar stays
  red and unraised; what is new is that the failing rows are separated from the passing ones by
  whether the documents are real.
- **The median moved 35 % and the tail did not move at all.** Three independent attacks (`p27`
  seeding, `p29` expansion, `p47` bounds) now agree the 1 M tail is not reachable by tightening an
  existing bound.

## p46 — the sort tail, and an algorithm that is usually worse (2026-09-06)

- **Added a second `search_sorted` arm** that walks the numeric column in value order and stops
  after `k` matches, recovering the early exit `p32` correctly showed a posting scan cannot have.
- **It is usually worse**: 1,579 us p50 against the scan's 19 us on presyo's real queries, because
  most of them are selective and "walk until `k` match" walks nearly the whole column. So neither
  arm ships alone — **both ship and the cheaper is chosen** from posting-list lengths the index
  already has, before either runs. The estimate is biased toward the scan, which is the arm that has
  always been correct here.
- **The tail nearly halves and the median does not move**: typo-free sort p99 2,615 → 1,387 us over
  four consecutive runs, p50 unchanged at 19 us.
- **Ties are the trap.** Stopping at exactly `k` returns an arbitrary subset of the documents
  sharing the `k`-th value and looks completely plausible; the walk continues while the value is
  unchanged, then truncates.
- **`numeric_order` is derived at load, never serialized** — it is a sort of a column the index
  already stores. **No format change and no ABI change.**
- **Two implementations of one answer is a bug factory**, so both are held to being the same answer:
  a unit test over 6 queries × 7 values of `k` × 2 directions on a fixture with heavy ties, and
  `facet-shop` repeating it on 336 real queries × 2 directions every run — **0 disagreements**.
  `Index::search_sorted_arm` is public so that claim is checkable from outside the crate.

## p45 — phrase queries (2026-09-06)

- **Added phrase queries**: `IndexBuilder::with_position`, `Index::search_phrase`,
  `Searcher::search_phrase`, `idx_build_position` / `idx_search_phrase` /
  `idx_searcher_search_phrase`. ABI 10 → 11, 49 symbols. Format `IDXTEXT5` → `IDXTEXT6`.
- **The field is packed into the position** (`field << 16 | token_index`), so a phrase cannot span a
  field boundary — there is no separate check for that, it falls out of the packing.
- **Positions are opt-in and the default costs zero bytes**, asserted rather than claimed. They are
  the only build option whose cost scales with token occurrences instead of documents.
- **An index without positions returns nothing rather than falling back** to a term search, because
  bag-of-words rows are indistinguishable from phrase rows once they are in the result buffer.
- **A phrase is exact** — no typo correction, no prefix expansion. A token absent from the
  dictionary matches nothing, the same direction as an include clause whose values are all unknown.
- **The phrase is a filter, not a ranker**: applied after scoring at the same two sites the facet
  clause uses, so a phrase hit carries the score the same query would have given it.
- **Measured on 25,979 real rows: +74.7 % artifact size — and the delta is not where it was
  expected.** 140,640 occurrences at one `u32` each is ~563 KB; the other ~1.2 MB is the offset
  array, because most (term, document) pairs carry exactly one position. **The addressing costs 2.2×
  the data it addresses.** Delta/varint encoding is the obvious next move and is left undone rather
  than guessed at.
- **Verified against an arm that shares no code with the engine** — raw fixture text, no tokenizer,
  no dictionary, no postings — one-way, in the direction where its crudeness cannot raise a false
  alarm. 980 hits over 184 real phrases: 0 wrong, 0 empty, 0 drift.
- **The reader validates `position_at` hard**, because every failure there is silent: an offset
  array one entry short hands the verifier another posting's positions and it agrees with them.

## p44 — the filter bar reaches the searcher, and a host that was never gated (2026-09-06)

- **Added `idx_searcher_search_clause` and `idx_searcher_search_page`.** ABI 9 → 10, 46 symbols.
  The Rust `Searcher` had both since `p43`; nothing across the ABI did, so the only surface with
  more than one segment was the only surface without a filter bar.
- **The unknown-value rule is per segment**, which is the whole reason it is shaped that way:
  a value known only to one segment draws nothing from the others on an include and removes nothing
  from them on an exclude. Asserted from Rust, JavaScript and Python against two segments that
  intern the same label at different ids.
- **Fixed: an out-of-range facet slot ignored the rule it was built to obey.** A clause naming a
  slot the index does not have made the whole filter unsatisfiable, *including an exclude* — so
  `1!=discontinued` would silently delete every row from a segment built before the slot existed.
  A slot with no values has no *known* value, so it now takes the same two answers as an unknown one.
- **Fixed: `js/index.mjs` — the host applications import — sat at `ABI_VERSION = 2` while the module
  was at 9.** Every `build()` and `open()` through it threw. It survived because `js/smoke.mjs`
  instantiates the WASM module directly, so 34 green host checks never touched it. **A host that is
  not gated is not shipped, only published**: the smoke test now imports it the way a consumer would.
- **`host/python/index_ffi.py` had no clause or paging binding at all**; "both hosts pass" had meant
  "both hosts pass the checks they have". Added, with a `Clause` type mirroring `FacetClause`.
- **Both hosts now refuse a facet value containing `|`** rather than encoding it, because the spec
  grammar cannot express one and the failure is a filter that silently means two values.

## p43 — the rest of the filter bar, and paging (2026-09-06)

- **Added `FacetClause`**: OR within a clause, AND across clauses, NOT. That is how a filter bar
  behaves — ticking two brands widens, ticking a brand and a category narrows. Closes the gap `p30`
  named. `Index::search_clause`, `Searcher::search_clause`, `idx_search_clause`.
- **The rule that decides whether a filter is safe**, stated and asserted in both directions: an
  **include** whose values are all unknown matches **nothing**; an **exclude** whose values are all
  unknown excludes **nothing**. Reversing them makes a filter silently return the whole corpus,
  which looks exactly like a working search. Same asymmetry for a document with no value: kept by
  an exclude, dropped by an include, so an unbranded row survives "not Colgate".
- **Added paging** (`search_page`, `idx_search_page`), closing the gap `p39` named. **Cost grows
  with `offset`, not `k`** — the engine over-fetches and drops the prefix, because rank order is
  only known once everything above the page is scored. Documented on the method rather than left to
  be found in production.
- **`pages_partition_the_ranking`** asserts what matters: three pages of ten equal one request for
  thirty, no document appears on two pages, past the end is empty rather than wrapped. Verified
  across segments too, where a segment's 3rd-best can be the page's 1st.
- **Replaced `search_opt`'s eight positional parameters with a `Scan` struct.** Clippy flagged 8/7;
  the struct was the right answer rather than an `allow`, because phrase support is next and would
  have made it nine.
- **ABI 8 -> 9, 44 symbols.** 125 tests. No regression: `pool-audit` 6/6 zero cells, `real-corpus`,
  `facet-shop`, `booted-schema`, `alec-surface` all PASS.

## p42 — long documents, and a total-order bug (2026-09-06)

- **Indexed `alec`'s web-surface scrape**, the largest data file in the house and the first
  LONG-document corpus: **119,179 rows x 1,000 characters, 123.6 MB of text**, read from disk and
  never committed. Every prior corpus is short-field.
- **It panicked on the first run**: `user-provided comparison function does not correctly implement
  a total order`. `rank_cmp` — the single ranking comparator, used by both the pruned and the
  exhaustive path — treated scores within a relative tolerance as equal, and **a tolerance is not
  transitive**: `a ~ b` and `b ~ c` does not give `a ~ c`.
- **Short fields never exposed it.** The chain only matters when one result set holds many scores
  spaced finer than the tolerance; six-word product names never produce that, and 119 k HTML bodies
  sharing a **7,459-term vocabulary** produce it immediately.
- **Fixed by quantizing instead of comparing differences.** `canon_score` rounds each score onto a
  grid, making the comparison a pure function of one value, so transitivity holds by construction.
  `rank_cmp` is now plain lexicographic ordering on `(bucket, quantized score, doc)`.
- **Added `rank_cmp_is_a_total_order`** — antisymmetry and transitivity checked exhaustively over
  scores one ULP apart, then sorted — and `the_score_grid_matches_the_documented_tolerance`, which
  pins the grid to `SCORE_TIE_REL` and checks the quantization is monotone. 121 tests.
- **The gates hold on the new shape**: exactness against brute force is **0 differences, 0 lost
  documents**, which is the first evidence `p24`-`p26` generalise beyond short fields.
- Reported without celebration: the index is **114 % of the source text**, and an 8-word body query
  costs **12 ms p50** against 365 us for a short one.

## p40, p41 — the house's databases, and the ranking signal they exposed (2026-09-06)

- **Added `booted-schema`**, indexing the schema crawl `booted` keeps of every Postgres in the
  house: **2,600 tables, 54 databases, 5 machines, 30.0 M estimated rows**, read from
  `~/.booted/schema.json` and **never committed**. Built in 0.06 s, 1.2 MB.
- It answers the questions a developer has at 2 a.m.: find a table by name (**100 %** rank-1),
  find one from its **comment with every name word removed** (**100 %** in top 10, so a hit cannot
  be the name matching by accident), facet by database, range by row count. Exact p50 **9 us**.
- **Grounded the billion-row question**: the largest real table in the house is
  `bygelo/presyo.price_event_2026_04` at **1,205,889 rows**. Everything real is inside what `p7`
  already measures.
- **It found a ranking gap.** Exact-name rank-1 was 86.4 %, and every miss had one shape: a short
  exact name losing to a longer relative whose comment repeats the term (`session` ->
  `session_resource`, `user` -> `user_agent`). BM25 cannot see that field 0 IS the query.
- **Added `INEXACT_FIELD_KEEP`** — a demotion of the inexact, never a boost of the exact, which is
  the third signal shaped that way after priors and anchoring and always for the same reason:
  `max_score` bounds assume 1.0, so a factor in `(0, 1]` keeps pruning valid by construction.
- **The consistency tests caught a real bug in it.** The factor belongs at three scoring sites and
  I added it to two, missing the hot inline scan loop; `maxscore_agrees_with_exhaustive_or` failed
  with a score ratio of exactly 10/9, which named the missing factor precisely.
- **Nothing regressed, two things improved.** `booted` exact-name 86.4 % -> **100 %**;
  `profstopick-dept` learned expansion **98.3 % (a documented -0.8 pt loss) -> 100 % (+0.8)**;
  `pool-audit` still 0.00 % on all six sets; `facet-shop`, `real-corpus`, `sisia` all PASS. The
  presyo baseline rose 75.3 % -> 76.0 %, shrinking expansion's credit to +21.0 pt.

## p39 — highlighting (2026-09-06)

- **Added `Index::highlight` / `Searcher::highlight` / `idx_highlight`** — byte ranges of a row that
  matched a query, which is what a UI bolds. **ABI 7 -> 8, 42 symbols**, working from Rust,
  JavaScript and Python.
- **The caller passes the text back**, because the index stores no field text. That is the right
  shape for an embedded engine rather than a concession: the application owns the rows, and a second
  copy kept only to highlight them is the duplication this engine exists to avoid.
- **Spans come from the same analysis the index used**, so a typo-corrected hit highlights the real
  word — `highlight("Colgte", ..)` marks **Colgate**.
- **Added `fold_with_origin` and `tokenize_span`.** Folding is not length-preserving, so offsets in
  the folded string say nothing about the caller's text. Two implementations of the tokenizing rules
  can drift and a drifted highlight marks the wrong words, so `tokenize_span_matches_tokenize` pins
  them over thirteen inputs chosen for where the rules bend.
- **Wrote a use-after-free and caught it.** The first JavaScript check reused a handle closed 25
  lines earlier; the module did not trap, `node` just hung with no output. A freed handle is the one
  class of mistake this ABI cannot check for a host, and the header says so.

## p38 — what incremental updating costs (2026-09-06)

- **Measured the limit `p37` named.** On presyo's 241,789 real products, split into a 60 % base plus
  deltas: **latency is the cost, ranking is not.** p50 goes **10 us -> 2,326 us across 1 to 50
  segments (232x)**, while what a shopper sees first stays right **96–98 %** of the time.
- **Practical guidance it produces**: build time is flat regardless of split, so segments are cheap
  to make; keep the count in single digits (1 -> 10 segments is 10 -> 322 us, both interactive);
  rebuild when `needs_compaction()` turns on.
- **Got the metric wrong twice before publishing it.** The first version reported exact top-10
  sequence equality and said segmentation destroys ranking (19.3 % at two segments). Two corrections
  followed: the query set was single first words, and **70.3 % of those have a top-10 whose scores
  span under 5 %** — near-ties whose order was never stable. Then the selective arm still read
  32.4 %, so the raw rankings were printed: **the same documents, with ranks 2 and 3 trading places
  because their scores differ by 0.07.** Headline metrics are now set overlap and rank-1.
- **Third time a frightening number turned out to be the measurement** (`p34` timer resolution,
  `p35` buffer aliasing, now this). Recorded as a rule: before publishing a number that says
  something is broken, print the raw rows it came from.

## p37 — incremental updates, reachable from every host (2026-09-06)

- **Corrected a wrong answer of mine.** I reported "no incremental updates, `Index` is immutable" —
  but `searcher.rs` is titled *"Multi-segment search — adding documents without rebuilding"* and
  predates this session. I audited `Index`'s API and never looked at `Searcher`'s.
- **Found the real gap, which was worse.** `Searcher` exposed only `search` and `search_prefix`, so
  everything from `p30`–`p32` (facets, conjunctions, ranges, histograms, sort) was `Index`-only:
  an application could have **live updates or shopping filters, not both**. And `Searcher` was **not
  in the C ABI at all** — incremental updates were unreachable from JavaScript, Python or a browser.
- **Gave `Searcher` the full query surface** and **added 15 C ABI symbols** (`idx_searcher_*`).
  **ABI 6 → 7, 41 symbols.**
- **Facet tallies merge by VALUE, not by interned id.** Segments intern labels independently, so the
  same integer means different things in each; merging on it would add unrelated categories together
  and report a plausible wrong number. A test builds two segments that genuinely disagree about ids.
- **Proven end to end from Rust, JavaScript and Python**: append a segment, typo-search across
  segments at global ordinals, tally by value, conjunctive filter, delete, live count.
- **Python host gained `Live` and `Index._take()`** — zeroing a wrapper whose handle the ABI has
  consumed, which is the one memory bug a `ctypes` host can still write.

## p36 — one build for every artifact (2026-09-06)

- **Added `scripts/build-wasm.sh`**, closing the limit `p35` named. It builds all four shippable
  WebAssembly artifacts side by side into `dist/`, each under its own `CARGO_TARGET_DIR` — because
  `RUSTFLAGS` is not part of the target path, so the `simd128` analytics build was silently
  overwriting the baseline one and comparing them meant rebuilding between runs.
- Reports raw **and gzipped** sizes, since gzipped is what a browser downloads:
  `index.wasm` 374,382 / **144,780**; `index-geo.wasm` 57,037 / **22,489**;
  `index-accel.wasm` 6,342 / **3,086**; `index-accel.simd.wasm` 7,325 / **3,358**.
- Both analytics artifacts re-verified from `dist/`: 14,586 vs **236,995** M rows/s on `bitmap_op`.
- `dist/` is gitignored — reproducible from source, and `index.wasm` alone is 374 KB.
- **Audited `index-core` for the gap that caught `index-accel`, and found none**: cracking has `p1`,
  PLA has `p0`, the FM-index is exercised by `fuzzy-decision`/`p2`, and the wavelet tree is internal
  to the FM-index. Checked rather than assumed, and reported as no-gap rather than padded into one.

## p35 — simd128 measured (2026-09-06)

- **Ran `p34`'s named follow-up.** First answer: `-C target-feature=+simd128` changes nothing —
  `bitmap_op` 14,092 -> 13,651 M rows/s, every other kernel flat. That answer was **wrong**, and the
  reason is the finding.
- **The bench was aliasing its own buffers**: `og_bitmap_op(0, presence, mask, N, mask)` passed the
  output as the second input. Overlapping buffers force any compiler to assume aliasing, which is one
  of the two standard reasons a byte loop fails to vectorise — so measuring only that form confounds
  "wasm has no SIMD" with "this call cannot be vectorised at all".
- **With distinct buffers, `simd128` is 9–16× on `bitmap_op`** across three interleaved rounds
  (baseline steady at 14,500–14,700 M rows/s; SIMD 128,000–236,000). That lifts it from **4 % of
  native to roughly 45 %**. The other five kernels are unchanged, as expected — they are gated by
  dependent loads and hash probes, not byte-parallel work.
- **Both forms are now measured** by `js/accel-bench.mjs`, which also takes an artifact path so two
  builds can be compared.
- **Not shipped, deliberately.** onegrid already ships `PROBE_SIMD` and `detectCapability()` returns
  `{ wasm, simd, thread }` — the socket exists and nothing uses the answer — and their
  `bundle-budget.json` excludes the `.wasm` from the budget, so the 983 extra bytes are not
  constrained. Building both and letting their probe choose is their call, not one to take for them.

## p34 — the analytics kernels measured inside WebAssembly (2026-09-06)

- **Closed `p33`'s stated limit** ("these are host measurements, not wasm; nothing here claims a
  browser number") with `js/accel-bench.mjs`: the real 6,342-byte artifact, instantiated in Node and
  driven exactly as onegrid's host drives it — module never allocates, host bump-allocates above
  `og_heap_base()`, memory grown once up front so no typed-array view is ever detached.
- **WebAssembly keeps 73–93 % of native on five of six kernels.** A million rows aggregated in
  **2.8 ms**, filtered in **5.6 ms**, grouped in **4.6 ms**, in a browser tab from a 6 KB artifact.
- **`bitmap_op` collapses to 4 % of native** (14.1 G vs 354 G rows/s) and it is the expected one: it
  is the only byte-parallel kernel, so it is the only one a native compiler auto-vectorises and the
  wasm32 build cannot. That is what "no SIMD" costs, isolated. `simd128` is the obvious follow-up
  and is not run.
- **Correctness is checked in the wasm tier too** — filter count, aggregate COUNT, and top-k
  ordering against JavaScript — because a native pass cannot catch a defect that exists only at
  32-bit pointer width, which is the gap `p33`'s correction left open.
- **Found and fixed a measurement defect in this bin, twice.** `bitmap_op` first printed
  `0.00 ms / 215,519 M rows/s` — a single shot below timer resolution reported to six figures — then
  still printed `0.00` after averaging, because the display carried two decimals. Both arms now
  average 200 repetitions and print three. The wasm figure moved **3×** once averaged.

## p33 — the analytics half, finally measured (2026-09-06)

- **Audited roadmap coverage and found the hole**: `index-accel` — onegrid's ratified `AccelModule`
  ABI, 773 lines, seven analytics kernels — had **0 roadmap documents and 0 bench bins**, against 33
  and 12 for the text engine.
- **Corrected the first draft of `p33`, which overstated its own findings.** It called the `u32`
  host-pointer truncation "a defect it found" — but `docs/integration.md` already documents that
  exact failure, `STATUS_ACCESS_VIOLATION` and all, in a section explaining why host unit tests were
  deliberately not written. It also counted "3 tests vs 111" as neglect, when those three are
  deliberately pointer-free and correctness was proven by onegrid's own harness (294/294 against the
  real compiled module, their generators). A rediscovery presented as a discovery.
- **Added `accel-kernel`**, a differential bench checking every kernel against an independent
  reference over randomised inputs including missing values, `NaN`, `-0.0`, ±infinity, duplicate
  keys, empty selections and `k > n`. **All seven kernels, 2,400 trials, 0 disagreements.**
  `sort_pass` is checked for stability *and* for being a permutation; `group_code` and
  `group_combine` are compared as partitions in both directions; and three deliberately
  over-filled hash tables all returned `-1` rather than wrapping — a table that wrapped would
  corrupt a group-by rather than fail it.
- **Introduced `AccelPtr`** = `u32` on wasm32 (ratified ABI unchanged, byte for byte) and `usize` on
  the host, so the kernels can be exercised natively at all. This **reverses a documented decision**
  not to host-test; the argument is in `p33`, as is the residual cost — host tests now use 64-bit
  pointer arithmetic while the shipped artifact uses 32-bit, so a wraparound defect would not surface
  natively. The wasm path stays covered by onegrid's harness.
- **Throughput at 1 M rows**: aggregate **387 M rows/s**, group_code **305**, top_k(100) **291**,
  filter_mask **192**. `sort_pass` is 10.6 M rows/s (94 ms) — a full stable merge sort, ~20 passes,
  and not a defect, but worth knowing before calling it per keystroke.
- **Both first-run failures were in the bench's own reference, not the kernels** — an `NaN == NaN`
  comparison and a mis-assigned group code — and that is recorded rather than quietly fixed:
  an oracle needs checking as hard as the thing it checks.

## p32 — sort by value (2026-09-06)

- **Added `search_sorted` / `search_sorted_filtered`**, the gap `p31` named: order results by a
  numeric column instead of relevance, with the whole filter bar applied first. `idx_search_sorted`
  in the ABI (**5 -> 6**, 26 symbols). No format change — it reads the column `p31` already stores.
- **Sorting gives up pruning, and that is the whole story.** Relevance ordering is what lets the
  engine stop early; a numeric order gives it nothing to stop on, because the cheapest item may
  match the query worst. Cost tracks the query's match count, not `k`.
- **Measured on 336 real queries over 241,677 products: p50 18 us, p99 2,521 us — 8x the tail of a
  ranked `search` (315 us).** The predicted shape, published rather than hidden behind the p50.
- **Absent values are excluded** from the order, as from ranges and histogram buckets: one rule,
  three places. **`k` truncates after ordering**, so it returns the k cheapest, not k arbitrary
  matches. **Ties break by relevance then document id**, so equal-priced pages are stable and
  sensibly ordered.
- **Verified two ways**: order (both directions), and membership against `search_range` — a
  different code path — because a result that merely is in order proves nothing about what it
  dropped. 60 queries, 0 wrong.

## p31 — numeric ranges (2026-09-06)

- **Added numeric columns and range faceting**, the gap `p30` named: `with_numeric(field)` parses a
  field once into an `f64` column; `search_range(q, k, slot, lo, hi)` filters on a half-open range;
  `range_tally(q, slot, &edge)` returns a histogram over **every** matching document; and
  `search_filtered(q, k, &facet, &range)` evaluates a whole filter bar — brand AND category AND size
  — in one pass.
- **Half-open ranges** `lo <= v < hi`, so adjacent buckets in a slider never both claim the boundary
  and the counts printed beside them cannot exceed the result set.
- **Absent is absent, not zero.** Unparseable text becomes `NaN`, which is in no range and in no
  bucket. Stated consequence: histogram counts sum to *at most* the match count.
- **`f64`, not `f32`**: 24 bits of mantissa stops being exact for minor-unit prices above ~16.7 M.
- **C ABI 4 -> 5** (25 symbols): `idx_build_numeric`, `idx_search_range`, `idx_range_tally`,
  `idx_numeric_slot_count`. Both hosts exercise them, including the boundary and absent-value cases.
- **Format `IDXTEXT4` -> `IDXTEXT5`**, 13 -> 15 spans. Values are written as raw IEEE-754 bits so a
  `NaN` marker survives verbatim.
- **Measured on presyo's 241,677 real products** (`size_value`, since the export has no price):
  **+17.55 B/doc** for two facet slots and one numeric column, **60 histograms 0 wrong** against both
  the range filter and the match count, and a six-bucket histogram at **179 us p99**.

## p30 — faceting, single then multi-field (2026-09-06)

- **Added faceted search**, which the engine had no form of: `IndexBuilder::with_facet`,
  `Index::search_facet`, `Index::facet_tally`, `facet_label`/`facet_of`. Shopping search is filter
  and count, not only ranking, and `facet` previously meant only "a field used to learn expansions".
- **Multiple facet fields with conjunctive filtering**: `search_facet_all(q, k, &[(slot, value)])`
  is brand AND category in one pass, not an intersection of result sets. `with_facet` is callable
  once per field, and call order is slot order.
- **Exposed it through the C ABI** — `idx_build_facet`, `idx_search_facet`, `idx_search_facet_all`,
  `idx_facet_tally`, `idx_facet_count`, `idx_facet_slot_count` — available to every language, not
  only Rust. **ABI 2 -> 4.** `js/smoke.mjs` and `host/python/index_ffi.py` both exercise it, check
  for check, including the conjunction.
- **Format `IDXTEXT2` -> `IDXTEXT4`**, 11 -> 13 spans. The span count did not change for the
  multi-slot step but the *encoding* of the two facet spans did, and the rule is "bump on any
  incompatible change" -- an old reader would read a slot count as a label count. The bump was forced by the existing
  `TABLE_BYTE` assertion, which is exactly what it is for.
- **Measured on presyo's 241,677 real products**, two slots (**19,793 brands, 146 categories**):
  **+9.55 B/doc** (28.2 -> 30.5 MB), tallies **exact on 40/40** queries vs exhaustive scoring,
  **0 short pages and 0 leaks** across 104 hardest filter-then-rank cases, **0 wrong** on 80
  conjunction pairs, and a full tally at **88 us p99** — fast enough for every keystroke.
- **Fixed a latent panic in `Reader::need`**: `self.p + n` overflows on a corrupt length prefix, in a
  format contracted never to panic. Now `checked_add`, with both length-prefixed sections bounding
  the claimed count by what the span can hold before allocating.
- **Fixed a trap in the first `idx_build_facet`**: it swapped through `Schema::new(vec![])`, which
  asserts. A panic in a WASM export kills the instance. Added non-consuming `set_facet_field`.

## p29 — the bounded-error lever, priced (2026-09-06)

- **Added `Index::search_capped(query, k, cap)`** — opt-in bounded-error search that caps how many
  dictionary terms each query token expands to. Default `search` is unchanged at `MAX_EXPANSION`.
  A query-time parameter threaded through `plan`/`emit`, so no serialization or threading change.
  The error is one-sided: expansions are ordered by edit distance then rarity, so a lower cap drops
  the vaguest matches first and never reorders what it keeps.
- **Priced it at 1 M documents.** Best trade is **cap 4: typo p99 16.4 ms -> 11.0 ms (-33 %) for
  1.9 % of answers changing**. Below 4 the curve is not monotone — accuracy falls and latency stops
  improving.
- **CORRECTS `p27`.** `p27` blamed the tail on typo expansion. At **cap 1 — no expansion at all —
  typo p99 is still 12.4 ms**, so expansion is only ~24 % of it. The rest is the cost of exact
  bucket-first ranking: a corrupted token leaves the pool a high-bucket worst member either way, so
  the gates stay shut.
- **No arm measured today meets `p7`'s 5 ms bar**, including `p24` at 9.4 ms, which predates every
  correctness fix. The bar stays red and unraised, now explained rather than merely observed.
- **The sweep caught a defect in itself**: it first reported nanoseconds labelled `us` (`time_each`
  returns ns; the main table divides by 1,000). Found by a control row timing plain `search` through
  the same harness, which now stays permanently.

## p28 — "any language" made real (2026-09-06)

- **Added `include/index.h`**, a hand-written C header for all 15 ABI symbols, documenting the
  memory model, the no-trap error model, and threading. Any language with a C FFI binds against it.
- **Added `host/python/index_ffi.py`**, a ctypes host in the standard library alone — no build step,
  no dependency — with a 14-check self-test mirroring `js/smoke.mjs` assertion for assertion, plus a
  real-corpus bench.
- **Extended `js/smoke.mjs` from 7 checks to 21.** It now builds an index from JavaScript, searches
  it, corrects two typos, serializes, reopens, and confirms identical answers. Its old comment
  claimed a JS host "cannot" build an index; `idx_build_new`/`_add`/`_finish` are exported precisely
  so it can, and that path had **no host-side test at all**.
- **Fixed two source files that were byte-level binary.** `crates/index-wasm/src/lib.rs` and
  `js/index.mjs` held **literal NUL bytes** in char/string literals (NUL is the ABI wire separator).
  `file` reported both as `data` and `grep` refused them. Replaced with `' '` — same value, both
  files now `UTF-8 text`. No Rust test covers the separator; it exists only at the ABI boundary,
  which is now tested from both tiers.
- **Measured from Python via ctypes** on 25,979 real business records, recombined: **1,039,160 rows,
  exact p50 551 us, p99 4.0 ms**, including full marshalling and hit unpacking. Explicitly *not*
  comparable to `p7-scale.md` — a small vocabulary over long postings is an easier shape, and the
  document says so.

## p27 — bucket-aware champion seeding, tried and reverted (2026-09-06)

- **Built the lever `p26` named for the typo tail, measured it, and reverted it.** Seeding one
  champion term per query *group* instead of one per query improves the ranking pool's worst bucket
  (median 6 -> 4 across 19,993 real queries) but does not move the fraction saturated at **bucket 0**
  (1.7 % -> 1.8 %), which is what actually gates pruning.
- Costs +31 % exact p50 at full budget. Also tried at **equal** seeding budget to separate seed
  quality from seed count: that is *worse* (+96 %), which settles it — concentrating the budget on
  the most discriminative term is the better design and was not an accident.
- **Consequence:** the 17 ms typo p99 at 1 M is not a pruning problem. Both `p26` bounds are exact
  and have no slack. Fixing the tail requires changing what is ranked (adaptive expansion caps, or a
  bounded-error mode with a stated guarantee) — product decisions, not optimizations.

## p26 — one exact bound replaces three approximations (2026-09-05)

- **Replaced all three pruning-soundness predicates with the exact one.** `eff` is exactly
  lexicographic `(bucket, score)`, so a document with `score <= S` and `bucket >= B` has
  `eff <= S - B * bucket_scale`; skipping is sound exactly when that cannot beat the ranking pool's
  worst `eff`. `prune_is_sound` (worst bucket == 0) and `p25`'s `bucket_floor > worst_bucket` were
  both sufficient-but-not-necessary special cases of it.
- **Added a block-range bucket floor**: a group is unreachable inside `[candidate, range_end)` when
  every one of its terms has its cursor past `range_end`, so the whole range has a bucket floor.
- **Measured why the strict form was useless**: `range_floor > worst_bucket` fires on 0.02 % of
  evaluations because floor and worst bucket are the *same distribution* (both mean 3.3, median 3,
  max 24 over 200 k evaluations). The pre-instrumentation hypothesis — that `range_floor` would be 0
  on typo queries — was wrong on all 200 k.
- **Against `p25`** at 1 M, interleaved medians: exact p50 **1,108 -> 476 us (-57 %)**, typo p50
  -37 %, typo p99 -16 %.
- **Against `p24`**, the last incorrect build: exact p50 **at parity** (504 -> 498 us), typo p50
  +17 %, typo p99 +83 %. **Exact-match retrieval is now correct at no measurable cost.**
- **Recorded a failing bar honestly.** `scale` is `OVERALL: FAIL` — typo p99 17 ms against `p7`'s
  5 ms bar at 1 M. Not a regression from this work: `p24` measures 9.4 ms in the same interleaved
  run and is also over. The bar is left red rather than raised.

## p25 — retrieval is exact on every measured corpus (2026-09-05)

- **Located the residual** `p24` left open, via a new `INDEX_DUMP_LOSS=1` mode on `pool-audit` that
  prints `search` beside `search_exhaustive_unpooled` for losing queries. It is the same pruning
  defect at a **third** site: the MaxScore essential/non-essential partition, which removes
  documents from the candidate *enumeration* rather than from a pool.
- **Fixed it with a bucket floor.** A document matching none of the remaining essential terms misses
  every query group wholly inside them, so its bucket has a computable floor; demotion is safe once
  that floor is strictly worse than the ranking pool's worst member. Unlike `p24`'s
  `prune_is_sound` — which is correct here but costs **20.7x**, measured — the floor holds exactly
  in the case that makes the naive gate expensive.
- **`pool-audit` now reads 0.00 % on all six real query sets** (presyo product / category / 3-word
  tail, blead, maphy, profstopick), rank-1, top-10 and actually-worse. Presyo product queries across
  one day: **28.20 % → 9.33 % → 2.23 % → 0.00 %**.
- **Shrank `rank_cap` from `k.max(16)` to `k.max(1)`.** The new bound compares against the ranking
  pool's worst member, so an oversized pool suppresses demotion for nothing. Typo p99 at 1 M:
  30.2 ms → **17.0 ms**, audit unchanged.
- **Cost:** +14 % p50 / +6.5 % p99 on real presyo queries (six interleaved pairs, medians); 2.25x on
  the adversarial `scale` corpus, which ties scores near the threshold so nothing can be skipped.
- **Corrected `p24`**: the count of presyo queries losing a better-matching document was **90**, not
  41. The percentages were right; that count was stale.
- Retires `p23`'s open puzzle — the byte-identical rank-pool sweep was evidence that enumeration,
  not pool size, was the lever.

## [Unreleased]

### Fixed - 2026-09-05 (the pruning defect, closed)

- **`prune-consistency` is GREEN at every decoy count.** The engine pruned by score and ranked by
  bucket-then-score, discarding documents that matched more of the query. Two earlier repairs were
  priced at 10-30x (one `eff`-ordered pool) and "still red from 512" (two pools).

- **The fix gates pruning rather than replacing it.** Score-based skipping is not wrong, it is
  *conditionally sound*:

  > Skipping on a score bound is valid exactly when the ranking pool is **full and its worst member
  > has bucket 0** - then no unseen document can have a better bucket, so entry requires beating a
  > score, which is what block-max skipping actually tests.

  ```rust
  fn prune_is_sound(rank_pool: &BinaryHeap<RankCandidate>, rank_cap: usize) -> bool {
      rank_pool.len() >= rank_cap && rank_pool.peek().is_some_and(|w| w.0.bucket == 0)
  }
  ```

  No new structure, no reordering, no threshold arithmetic. The condition is monotone, so it flips
  on once per query and stays on.

- **Real-data degradation across the day:**

  | corpus | original | two pools | **+ gate** | lost-better-match |
  |---|---|---|---|---|
  | presyo / product names | 28.20 % | 9.33 % | **2.23 %** | 1,136 -> 221 -> **41** |
  | presyo / 3-word tails | 18.21 % | 4.35 % | **3.17 %** | 729 -> 66 -> **50** |
  | blead / business names | 1.66 % | 0.18 % | **0.02 %** | 72 -> 3 -> **0** |
  | maphy, profstopick | 0.09 / 0.04 % | 0.00 % | **0.00 %** | -> **0** |

  **12.6x fewer degraded queries than this morning on presyo; three of five corpora clean.**

- **Cost, measured interleaved: ~8 % p50, ~19 % p99.** The absolute numbers in that A/B are not
  comparable to earlier runs - the machine had been benchmarking for hours and both arms ran ~4x
  their usual - which is why only the interleaved delta is quoted. Interleaving is the house rule
  now because a warm-versus-cold comparison already discarded a working fix once.

- **2.23 % remains and is NOT explained.** `p23`'s sweep showed pool size is not the lever, so it
  should not be assumed to be the same defect. It needs its own reproduction.


### Fixed - 2026-09-05 (the pool defect is real, measured on production data, and repaired)

- **`pool-audit` bench: the `p22` defect happens constantly on real data.** `p22` reverted two
  working repairs because the defect "had not been observed on any real corpus". No consumer bench
  was **built** to observe it - they score precision@10 and hit@1 against labels, and a pool eviction
  swaps one correct document for a less-correct one *inside* the labelled set, which those metrics
  cannot see. Comparing `search` against brute force instead:

  | corpus / query set | queries | rank-1 | top-10 worse | lost better-matching doc |
  |---|---|---|---|---|
  | presyo / product names | 4,028 | 0.00 % | **28.20 %** | **1,136** |
  | presyo / 3-word tails | 4,004 | 0.00 % | **18.21 %** | **729** |
  | blead / business names | 4,330 | 0.00 % | 1.66 % | 72 |

  **Rank 1 was never wrong**, which is exactly why it hid.

- **The latency figure that justified the revert was a measurement error.** +50-75 % compared a
  thermally loaded run against a cold baseline from hours earlier. Interleaved A/B, three pairs, one
  session: **exact p50 +1.7 %, typo p50 +3.0 %, typo p99 +6.3 %** - and one pair returned a *lower*
  p99 with the fix on.

- **The two-pool repair is restored.** A score-ordered pool owning the pruning threshold, plus a
  `k`-sized pool ordered by final rank, merged and deduplicated.

  | corpus | worse before | worse after | lost-better-match |
  |---|---|---|---|
  | presyo / product names | 28.20 % | **9.33 %** | 1,136 -> **221** |
  | presyo / 3-word tails | 18.21 % | **4.35 %** | 729 -> **66** |
  | blead | 1.66 % | **0.18 %** | 72 -> **3** |
  | maphy, profstopick | 0.09 / 0.04 % | **0.00 %** | -> **0** |

  **3x fewer degraded queries, 5-11x fewer lost documents, for 2-6 % latency.** 103 tests green,
  `real-corpus` and `sisia-catalog` still PASS.

- **On the real workload the fix is free within noise.** The 2-6 % figure came from the `scale`
  bench (DepEd names, 41,069 terms); presyo has **117,472 terms** and longer queries. Timing 4,028
  real presyo product names: **p50 +0.3 %, p99 +1.2 %** - indistinguishable from noise, because long
  queries make the scoring loop dominate one extra small-heap operation.

  The first attempt at this measurement was **biased again**: running OFF-then-ON reported OFF at
  171 us against ON at 144 us, i.e. the fix making things *faster* by 15 %. That was warm-up - the
  first process of each pair pays cold-cache costs and OFF always went first. Alternating the order
  collapsed it. **Third ordering artifact of the day: an A/B without alternating order is not an
  A/B.**

- **On the real presyo workload the fix is free within noise.** 4,028 real product-name queries,
  A/B'd with the order **alternated** to cancel warm-up bias: p50 144.3 -> 144.7 us (**+0.3 %**),
  p99 1,006 -> 1,019 us (**+1.2 %**). The 2-6 % from the DepEd scale bench is a conservative upper
  bound that does not transfer - presyo's long product names over a 117,472-term dictionary make
  scoring dominant, so one small-heap operation per candidate vanishes into it.

  Order mattered: running *without* first in every pair read 171/169 us for that arm against 144 us
  for the other, purely cold-start. **Third time measurement order nearly produced a wrong delta**;
  the first cost a working fix a whole round.

- **The ranking pool saturates at `k`, measured.** Swept from `k` to `16k` against real presyo
  queries: **byte-identical** results at every size (376 worse, 221 losing a better-matching
  document, same score gaps). That settles the sizing question *and* attributes the residual 9.33 %
  definitively - if it were eviction, a bigger pool would move it; it does not move at all, so
  everything still missing is discarded **before scoring**.

- **`p22` remains red at 512 decoys** (was 32). The residual is documents block-max skipping
  discards *before* scoring, reachable only by the single-`eff`-pool design that genuinely costs
  10-30x. A bucket-aware pruning bound is still the real repair and still unattempted.

- **Two methodology lessons, the second expensive.** "No consumer bench detected it" is not evidence
  of absence when none was designed to. And **never compare a warm measurement to a cold baseline** -
  that error threw away a correct fix for an entire round. Interleave, or do not claim a delta.


### Measured and reverted - 2026-09-05 (two fixes for the pruning defect, both too expensive)

- **Both plausible repairs for the `p22` defect were implemented against its acceptance test and
  then backed out.** The numbers are recorded so the next attempt starts from evidence.

  | | `p22` first failure | exact p50 @1M | typo p99 @1M |
  |---|---|---|---|
  | baseline | 32 decoys | 274 us | **6.0 ms** |
  | fold bucket into a monotone score | **never fails** | 8,051 us | **57.5 ms** |
  | two pools (score + rank) | 512 decoys | 400-530 us | 9-10.5 ms |

- **Attempt 1 is correct and 10-30x slower, for a structural reason.** `eff = score - bucket*scale`
  makes the pool's ordering identical to the ranking's, and `p22` then passes at every decoy count -
  but the pruning threshold is the pool's worst member, so an `eff` threshold sits far below any
  score and block skipping never engages. **Pruning by score is only sound when ranking is by
  score.**

- **Attempt 2 keeps the speed and closes 16x of the gap.** A score-ordered pool (owning the
  threshold, so skipping is unchanged) plus a `k`-sized rank-ordered pool, merged at the end. All
  103 tests green. It cannot close the hole because block-max skipping discards documents *before*
  scoring, so neither pool sees them - exactly the part attempt 1 fixed.

- **Reverted because the defect has not been observed on any real corpus.** `real-corpus`,
  `sisia-catalog`, `presyo-catalog`, `maphy-place`, `blead-industry` all pass; it reproduces only on
  a corpus built to trigger it. The latency cost is **unconditional - every query pays**. Trading
  50-75 % of tail latency for a partial fix to an unobserved defect is a bad trade at this evidence
  level, and a good one the moment it shows up in production.

- **A dedup bug in attempt 2 is worth remembering**: it binary-searched a vector it was
  simultaneously pushing to, so the tail was unsorted and a document could be admitted twice. It
  surfaced as duplicate hits in `block_max_pruning_agrees_with_exhaustive_or_at_scale` - the test
  caught it immediately, which is the argument for that test existing.


### Found - 2026-09-05 (a pre-existing correctness bug, reproduced)

- **`prune-consistency` bench - the pool defect is GENERAL, not expansion-specific.** `p21` asserted
  it; this reproduces it, with no expansion involved:

  ```
     decoys   truth rank 1   search() rank 1   agree
         16           3000              3000     yes
         32           3000              3200      NO
       2048           3000              3200      NO
  ```

  **The break is at 32 decoys, exactly the pool size** (`(k*3).max(32)`, k=10). Once enough
  high-scoring worse-bucket documents exist to fill the pool, the correct answer is evicted before
  the bucket sort runs. **This has been true for as long as the pool has existed.**

- **`Index::search_exhaustive_unpooled`** added as ground truth: score every matching document, sort
  by the engine's own comparator, no pool and no pruning. Neither `search` nor `search_exhaustive`
  could serve - **both apply the same pool**, as `search_exhaustive`'s own comment says.

- **Getting the corpus right took three attempts, and the two failures are the finding.** Putting
  the discriminating term in one document, then in 400, both **passed for the wrong reason**:
  **to be bucket 0 a document must match every group, and the group the decoys miss is precisely the
  one carrying the IDF**, so the bucket advantage and the score advantage normally move together.
  The bug needs the discriminating term to be nearly worthless - which is exactly what a common word
  is.

  The triggering shape is not contrived: a query with one common word, full matches long, near
  misses short and numerous. That is `"iphone 15 pro case"` - the shopping search this project
  exists for.

- **The bucket-aware pruning bound is no longer optional.** It is the repair for a demonstrated
  correctness bug rather than a refinement, and `p22` is its acceptance test. Expected-red and
  gate-excluded until the fix lands.


### Diagnosed - 2026-09-05 (the pool multiplier is a workaround, and the real defect is named)

- **No pool multiplier is correct, and the sweep proves it.** In-sample precision@10 against the
  multiplier:

  | mult | presyo | profstopick | blead |
  |---|---|---|---|
  | 3 | 96.5 % | 87.5 % | 84.8 % |
  | 6 | **97.0 %** | 92.2 % | 89.6 % |
  | 24 | 97.0 % | 96.7 % | **95.6 %** |
  | 48 | 97.0 % | **99.4 %** | 95.6 % |

  **presyo saturates at 6, blead at 24, profstopick is still climbing at 48.** On presyo, 24 buys
  nothing over 6 and costs ~200 us per query. `EXPANDED_POOL_MULT` now carries this table in its doc
  comment rather than presenting 24 as principled.

- **The actual defect, named: the engine PRUNES by score and RANKS by bucket-then-score, and those
  disagree.** Block-max pruning skips work whose score cannot beat the pool's worst score, but the
  final ordering puts `typo_bucket` first - so a document that would rank top can be pruned for a
  score that has nothing to do with how it will be ranked. Expansion made this acute; it did not
  cause it.

- **The obvious repair does not work, and why is recorded so it is not attempted twice.** The pool's
  worst score *is* the pruning threshold, so a bucket-ordered heap has no valid threshold at its
  root; tracking the true minimum separately preserves correctness but drives the threshold toward
  zero - because bucket-0 low-score documents are exactly what the change retains - disabling block
  skipping entirely. **Pruning by score is only sound when ranking is by score.**

### Fixed - 2026-09-05 (pool eviction: an engine bug that changed six published numbers)

- **The candidate pool is ordered by SCORE; the final ranking is ordered by `typo_bucket` FIRST.**
  A document with a perfect bucket but a modest score was evicted before the bucket sort ever saw
  it - and learned expansion made it far worse, because it adds many terms whose matches are scoring
  competitors that were not there before.

  Diagnosed by widening the pool and watching the loss vanish: on profstopick, expansion read 88.3 %
  at the default pool and climbed monotonically to **100.0 %** as it grew. **The documents were
  always ranked correctly; they were being thrown away before the sort.**

- **Fixed by widening the pool when, and only when, expansion fires** (`k*24` vs `k*3`). Tied to
  expansion rather than raised globally because `real-corpus` and `sisia-catalog` never expand and a
  wider pool would cost them tail latency for nothing - both unchanged, as are `maphy-place` and the
  WASM smoke test. **Not free where it fires**: a browse query now costs ~700 us against 163 us for
  a lookup.

- **Six published numbers changed:**

  | bench | measurement | before | after |
  |---|---|---|---|
  | p15 presyo | in-sample | +25.5 pt | +25.9 pt |
  | p17 presyo | held-out | +23.9 pt | **+21.5 pt** |
  | p18 blead | in-sample | +20.4 pt | **+30.7 pt** |
  | p18 blead | held-out | +6.7 pt | **-1.5 pt** |
  | p20 profstopick | in-sample | -10.8 pt | **-2.2 pt** |
  | p20 profstopick | held-out | -38.6 pt | -34.7 pt |

- **The corrected numbers produce an adoption rule, sharper than the claim they replaced:**
  1. facet already retrieves well -> **do not expand**;
  2. gap + members reuse vocabulary -> **expand**, 83 % of the gain survives on unseen documents;
  3. gap + members do NOT reuse vocabulary -> **expand only with frequent rebuilds**. blead's
     **+30.7 in-sample is the largest gain of the three corpora and its held-out value is negative**
     - an adopter measuring only in-sample would have chosen it as the best fit.

- **Two deliberate practices made this findable**, and both are worth keeping: `p20` recorded the
  anomaly as **undiagnosed** rather than explaining it away, and the in-sample/held-out split
  existed at all - the artifact hurt held-out arms far more, so a project measuring only in-sample
  would have seen nothing wrong.


### Corrected - 2026-09-05 (a benchmark error worth 9.4 points)

- **`boost 0.0` does not make a field inert.** It contributes no score, but a document matching a
  term there is still a MATCH - and a match earns `typo_bucket` 0, **the primary sort key**, so it
  outranks every document that does not match at all. Verified directly:

  ```
  schema: title (boost 3.0), facet (boost 0.0)
  doc 0: "camote powder" / "bakingneeds"
  query "bakingneeds" -> 1 hit: doc 0, score 0.0000, bucket 0
  ```

- **`p15`'s headline is corrected: +25.5 points from expansion, not +34.9.** It compared an
  expansion arm carrying a boost-0.0 category field against a baseline with no such field. Measured
  on presyo with **no expansion at all**, adding the field alone moves 61.7 % -> **71.1 %**. `p15`
  now shows all three rows and the fair baseline is the middle one.

- **`p18` and `p19` are unaffected** - both of their arms carried the facet field, so those
  comparisons were fair. `p18`'s +20.4 and `p19`'s no-op stand.

- **Caught by a baseline being implausibly high**, which is the same tell as every previous
  methodology error in this repo. Seventh so far, and the most expensive: it changed a number that
  had been reported repeatedly.

### Measured - 2026-09-05 (third corpus: prediction refuted, and a new adoption rule)

- **`profstopick-dept` bench - 2,253 Ateneo course titles across 92 departments.** Chosen because
  `p18`'s vocabulary-reuse condition made a **falsifiable prediction** about it, written into the
  bin before running: course titles reuse vocabulary heavily, so held-out retention should resemble
  presyo's 68 % rather than blead's 33 %.

- **The prediction is refuted, but the corpus cannot test the condition.** Its plain baseline is
  **already 99.2 %** - saturated - so there is no gain to retain and retention is undefined.
  Reporting "0 % retention, condition refuted" would be reading a ratio with a meaningless
  denominator. **The vocabulary-reuse condition remains neither confirmed nor refuted.**

- **A new adoption rule, which the earlier corpora could not have produced: expansion can HURT.**
  99.2 % -> 88.3 % (-10.8 pt), a fair same-schema comparison. **Do not apply `learn_expansion` to a
  facet that already retrieves well** - adding twenty terms to a query that was already returning
  the right documents can only displace them. The mechanism of the harm is larger than the
  `EXPANSION_DISTANCE = 1` ordering predicts and is **recorded as undiagnosed** rather than
  explained away.


### Measured - 2026-09-05 (held out on blead: the generalization claim gets its condition)

- **`learn_expansion` generalizes where a facet's members REUSE VOCABULARY - and that condition is
  now measured, not guessed.** `p18` reported blead in-sample only; the held-out split it named as
  missing has been run.

  | corpus | in-sample gain | held-out gain | held-out retains | staleness cost |
  |---|---|---|---|---|
  | presyo - grocery products | +34.9 pt | +23.9 pt | **68 %** | ~11-17 pt |
  | **blead - business names** | +20.4 pt | **+6.7 pt** | **33 %** | **13.7 pt** |

  **The staleness cost is comparable (13.7 vs ~17 pt) but what survives is not.** Half of
  `Baking Needs` and the other half share brands, product types and sizes, so a term learned from
  one predicts the other; two logistics firms share almost nothing but legal suffixes.

- **This narrows the claim and makes it more useful.** Not "it generalizes" but "it generalizes
  where a facet's members reuse vocabulary" - and **an adopter can check that on their own data
  before adopting**, using exactly the in-sample/held-out split these benches run, which needs no
  labels beyond the facet they already have.


### Measured - 2026-09-05 (categorizer sweeps, and a no-op worth running)

- **The operating curve, not the single number, is the deliverable.** `k` and the confidence
  threshold were chosen rather than measured, so both were swept.

  | threshold | accuracy | share kept |
  |---|---|---|
  | take everything | 68.0 % | 100 % |
  | 0.6 | 81.0 % | 68.7 % |
  | 0.8 | 89.5 % | 46.8 % |
  | **unanimous** | **95.7 %** | **22.1 %** |

  **At unanimity the index is right about 95.7 % of the products it will speak up for** - 22 % of
  the catalogue, good enough to apply automatically. The application picks the point; the vote share
  comes free with the prediction.

- **`k` is a coverage dial, not an accuracy dial.** Overall accuracy peaks at k=10 and is flat
  either side (66.7 % - 68.0 %), but confident accuracy climbs monotonically 73.8 % -> 85.0 % from
  k=3 to k=40 while confident share falls 86.3 % -> 50.4 %. A larger neighbourhood makes agreement
  rarer and more meaningful.

- **`learn_expansion` on the classifier is an exact no-op (-0.0 pt), and it was run anyway.**
  Predicted from the design: expansion fires only when the whole query IS a facet value, and a
  product name never is. Run because the prediction is falsifiable in a useful direction - **a
  mechanism that had fired there would have meant the strict trigger leaks**, which is `p17`'s
  damage gate checked from the opposite side.

### Measured - 2026-09-05 (the analyzer half of the goal)

- **`presyo-categorize` bench - the index as a classifier, filling a 21 % data-quality hole.** `p15`
  recorded that **50,607 of presyo's 241,677 products (20.9 %) are `Uncategorized`** and left it
  alone. This asks the inverse of every question measured so far: not "find the products in this
  category" but **"what category is this product?"**

  The method needs nothing new - search the name against products whose category is known, take a
  **majority vote over the top-10 neighbours**. kNN classification where **the index is the model**:
  no training, no embeddings, no second system.

  | | accuracy | share of predictions |
  |---|---|---|
  | all predictions | 68.0 % | 100 % |
  | **confident only (>=60 % agree)** | **81.0 %** | 68.7 % |

  **The confidence threshold is worth 13 points and costs nothing** - the vote share is a by-product
  of the prediction, so an app takes the confident 69 % automatically and queues the rest.

- **Applied to the real hole: 100 % get a proposal, 46 % confidently, at 2,169 products/second**
  single-threaded. The *lower* confidence there is correct behaviour rather than a disappointment:
  products left uncategorised are plausibly the ones that were hard to categorise to begin with, and
  a method equally sure about both sets would be the suspicious result.

- **Validation is deliberately conservative.** Accuracy is measured only on labelled products
  **withheld from the index**, never on the unlabelled rows - their true categories are unknown, so
  no accuracy is claimed for them and none is estimated.

- **The misses are mostly taxonomy boundaries, and that is measured rather than asserted.** Of 1,284
  misses, **188 (14.6 %) name a category sharing a word with the true one** - `Spirits` for
  `Liquor`, `Fresh Meat` for `Frozen Meat`, `Fresh Seafood` for `Frozen Seafood`. Counting those as
  acceptable would read 72.7 %, and **that number is deliberately not claimed**: whether `Spirits`
  may stand in for `Liquor` is presyo's call about its own taxonomy, and a bench that grades itself
  generously on someone else's category scheme is measuring its own opinion.


### Measured - 2026-09-05 (learn_expansion generalizes)

- **`blead-industry` bench - the feature works on a corpus it was not designed for.**
  `learn_expansion` was built against presyo's grocery catalogue; a feature measured only on the
  data it was built for has proved very little. `blead`'s lead store supplies a second corpus with
  the same failure shape and a completely different vocabulary: **25,979 real Philippine business
  names** tagged with an industry, where nothing in `10K EAST CONCRETE MIX SPECIALIST, INC.` says
  *Wholesale/Retail*.

  | corpus | documents | facet values | plain | learned | delta |
  |---|---|---|---|---|---|
  | presyo - grocery products | 241,677 | 145 | 61.7 % | 96.6 % | **+34.9 pt** |
  | **blead - business names** | **25,979** | **27** | **64.8 %** | **85.2 %** | **+20.4 pt** |

  `Construction` and `Public Admin` go from **0 %** to 80 % and 100 %. **MRR reaches 1.000** - after
  expansion every one of the 27 industries has a correct result at rank 1.

- **Comparable by construction.** Label leakage is 11.8 % here against presyo's 13.8 %, so the label
  is independent of the retrieval signal to nearly the same degree and a difference in outcome
  cannot be blamed on an easier label. Same metric, same arms, same `boost 0.0` facet field.

- **The smaller gain is explained, not glossed.** blead's 27 industries average ~960 members and
  lump unrelated firms together (`"Other Services"` reaches only 60 %, `"Real Estate"` 50 %);
  business names carry less signal than product names; and the achievable ceiling for this corpus
  was not computed, so 85.2 % may already be near it. **Two corpora is enough to say "not
  presyo-shaped"; it is not enough to say "general".**


### Added - 2026-09-05 (learned query expansion, in the engine)

- **`IndexBuilder::learn_expansion(facet_field, top_k)` - the engine now bridges vocabulary
  mismatch by itself.** For each distinct value of a low-cardinality field (category, department,
  tag), it learns the terms most concentrated in the documents carrying it; a query that **is** one
  of those values is expanded with them. An application gets this by naming a field - no pipeline,
  no model, no external service.

  On presyo's 241,677 real products: **61.7 % -> 96.6 % precision@10 in-sample**, 145 facet values
  learned, **+3.4 s build**, category-query latency **316 us** vs 178 us for an exact product query.

- **In-sample vs held out, labelled rather than blurred.** The 96.6 % is measured on the same
  catalogue the table was learned from. `p17-presyo-expand.md`'s **79.6 %** held out half the
  products. Both are real and they answer different questions - deployment condition for existing
  products, versus behaviour for products added after the table was built. **The ~17-point gap is
  the cost of a stale table**, and it is what should decide re-derivation cadence.

- **Emitted as an alternative inside every query group, not as a group of its own.** `bucket_of`
  SUMS a penalty per unsatisfied group, so a dedicated expansion group would make a document that
  matches only expansion terms pay `MISSING_TERM_PENALTY` for every original token - ranking a
  genuine category member BELOW a document that merely shares one word with the category's name.
  As alternatives, one expansion hit satisfies the group it sits in.

- **Priced with the existing typo dial.** `EXPANSION_DISTANCE = 1` reuses `typo_penalty^distance`
  for weight and `bucket_of` for ordering, so an expansion match ranks below an exact match and
  above nothing - without a second ranking mechanism to keep consistent.

- **The trigger is strict**, and `p17` measured why: a loose rule fires on 19.7 % of ordinary
  product queries and costs 2.0 points of exact-product hit@1, because `Signature Select Ice Cream
  Butter Pecan` contains the category `Cream`. Asserted by
  `expansion_fires_only_on_an_exact_facet_value`.

- **Format: `IDXTEXT2` carries 11 sections** (176-byte table). A learned expansion is a ranking
  signal, so an artifact that dropped it would rank worse than the index it was built from -
  "browse got worse after we deployed", which is close to undebuggable. Fourth time this rule has
  applied. Asserted by `a_learned_expansion_survives_a_round_trip`.

### Fixed - 2026-09-05

- **A fixed learning threshold silently learned nothing for small facet values.** Requiring a term
  to appear in 5+ of a value's documents means a value with a handful of documents yields an **empty
  table** - the feature looks broken rather than inapplicable. The threshold now scales,
  `(n/4).clamp(2, 5)`, so a large value behaves exactly as when `p17` measured it.


### Measured - 2026-09-05 (the vocabulary gap, largely closed)

- **`presyo-expand` bench - derived aliases are worth +23.9 points, held out.** `p15` found the
  61.7 % category-retrieval gap and proposed a query-time alias table **on a hunch**; `p16` measured
  the ceiling on curated aliases; this derives them from presyo's own catalogue and asks whether the
  derivation works.

  For each category, score terms by concentration inside it and append the top *k* to the query.
  One pass over the catalogue, a lookup table, **no model and no new storage.**

  | query | precision@10 | delta |
  |---|---|---|
  | category name alone | 55.7 % | - |
  | **+ top 20 derived terms** | **79.6 %** | **+23.9 pt** |
  | + top 20 **random** terms *(control)* | 19.8 % | **-35.9 pt** |

- **The control is the point.** Adding 20 terms changes what a query matches; if random terms helped
  equally, the gain would be "longer queries retrieve more" and the derivation would be decoration.
  Random expansion instead **costs 35.9 points** - a ~60-point spread. Third bench in this repo
  designed *in advance* to separate the boring explanation from the interesting one.

- **Leak-free by construction.** Expansions come from a train half (120,839 products); the index
  holds only the disjoint test half (120,838). No product contributes both to a term's weight and to
  the score it earns. Deriving from the same assignments being scored would report a large win by
  construction - the trap `p13` avoided and `p12` fell into.

- **Caveat recorded rather than discovered later: most derived terms are brand names**, not concepts
  (`"Soup Mixes"` -> *knorr*, *campbell*; `"Baking Needs"` -> *torani*, *lakanto*). The mechanism
  largely learns brand→category association - real signal in a grocery catalogue, but it cannot
  cover a brand it has never seen, and per-category quality varies (`"Pet Accessories"` picked up
  *eyewear*, *35mm*, *sweden* and still improved on aggregate).

### Measured - 2026-09-05 (sizing the gain, and testing the brand caveat)

- **Derived expansion captures 71 % of the achievable gain.** Indexing the category name on every
  product - `p15`'s option 2, and close to cheating since the label becomes part of the document -
  is the right upper bound. It reaches **89.5 %**.

  | | precision@10 | share of achievable gain |
  |---|---|---|
  | category name alone | 55.7 % | - |
  | **derived expansion** | **79.6 %** | **71 %** |
  | derived, brands removed | 70.2 % | 43 % |
  | category indexed *(upper bound)* | 89.5 % | 100 % |

- **The residual 9.9 points are not vocabulary.** Even with the label indexed, precision stops at
  89.5 % because a product whose *name* contains the category's words outscores one that merely
  belongs to it. That is BM25 field weighting, a different problem - and it makes `p15`'s option 3
  (dense/hybrid retrieval) a weaker case than when it was listed.

- **The brand caveat was tested, not just carried.** `p17` flagged that most derived terms are brand
  names and that an expansion built from history cannot cover an unseen brand. The catalogue has a
  `brand_name` column, so striking every brand token out measures it: **brand-free expansion still
  captures 43 % of the achievable gain (+14.5 pt)**. Not pure brand memorisation; a category of
  entirely unseen brands degrades toward +14.5 rather than to zero. Brand-free is also better at
  k=5 (70.2 %) than k=20 (67.0 %) - once brands are gone, depth pulls in noise.

### Gated - 2026-09-05 (expansion damage, caught and fixed)

- **The damage gate caught a real regression before it shipped.** Query expansion is built for
  *category* queries, but a shipped expander does not know what kind of query it was handed. Fired
  loosely - whenever a query *contains* a category's words - it triggers on **19.7 %** of ordinary
  product queries and costs **2.0 points of exact-product hit@1**:

  ```
  Signature Select Ice Cream Butter Pecan 1.5Qt   matched category "Cream"
  Moringa-O2 Shampoo Herbal 200ml                 matched category "Shampoo"
  Huggies Dry Diapers Pants Double XL x 22pcs     matched category "Diapers"
  ```

  Every casualty is a product whose name legitimately contains a category word, buried under that
  category's other members. **The feature would have made browse better by making search worse.**

- **The fix is one comparison.** Requiring the query's token set to *equal* a category's rather than
  contain it fires on **zero** product queries and costs **+0.0 points**, while retaining the full
  +23.9 on genuine category queries.

  | trigger | hit@1 | fired on |
  |---|---|---|
  | plain | 99.2 % | - |
  | loose (contains) | 97.3 % | 596 of 3,021 |
  | **strict (equals)** | **99.2 %** | **0** |

  Shippable design, both sides measured: **+23.9 points on browse, 0.0 damage on lookup.**

### Housekeeping - 2026-09-05

- **Stopped a bad background sweep.** A repo-wide grep for search-shaped code ran 31 minutes and
  reached ~15 % of the tree, and its pattern (`\bsearch\(`) matched `.search()` on any JS string -
  scoring unrelated repos identically to the ones that mattered. Direct row-count inspection had
  already superseded it. **Disk size and keyword counts were both bad proxies for "has a corpus".**


### Measured - 2026-09-05 (what an alias table is worth)

- **`biasd-entity` bench - p15's proposed fix, now with evidence.** `p15` ranked a query-time
  alias/expansion table as the first fix for its 61.7 % category gap, **on a hunch and with no
  ground truth**. `biasd` - a PH news aggregator that resolves political entities in article text -
  has exactly that ground truth: **4,560 real entities and 7,217 alias queries**, with **292 (4.0 %)
  sharing no token at all with their canonical label**.

  | | hit@1 | hit@10 |
  |---|---|---|
  | canonical label only | 71.6 % | 94.1 % |
  | **label + aliases indexed** | **86.5 %** | **99.8 %** |
  | *on the 292 disjoint queries* | | |
  | canonical label only | 0.0 % | **0.7 %** |
  | **label + aliases indexed** | 43.5 % | **95.5 %** |

  `"Bobby"` -> Alberto Pacquiao, `"Queenie"` -> Alexandria Gonzales, `"Baham"` -> Abraham Kahlil
  Mitra. Filipino nicknames - nothing to match, and no tokenizer or typo policy reaches them. **The
  same failure class p15 found in presyo's categories.**

- **Stated as a ceiling, not a forecast.** biasd's aliases are curated; presyo's would have to be
  derived from co-occurrence, which starts from a strictly harder position. And arm B indexes
  aliases where p15's option 1 would expand the query - different mechanisms, different costs, only
  the first measured.

- **A finding for biasd itself: arm A fails an 80 % hit@1 bar at 71.6 %.** An index of canonical
  political names is not adequate for what newspapers actually print; more than a quarter of real
  surface forms miss the right entity. biasd already carries the surface lists, so this quantifies
  what skipping them costs.

### Found, not yet benchmarked - 2026-09-05

- **`blead/data/lead-store.db`** - 30,687 distinct real business names with industry, city, address
  and PSIC code, and visible near-duplicates (`10K EAST CONCRETE MIX SPECIALIST, INC.` vs
  `10K CONCRETE MIX SPECIALIST INC.`). A seventh app, entity-dedup shaped.
- **A repo sweep found nothing else.** Several repos looked large (hobbycat 1,468 MB, trin 763 MB)
  but the size was `node_modules`: the databases hold **16 and 10 rows**. Disk size was a bad proxy
  for corpus size and row counts should have been checked first.


### Measured - 2026-09-05 (241,677 real products, and a conclusion corrected)

- **`presyo-catalog` bench - the first workload in this project that is NOT at ceiling: 61.7 %
  precision@10.** Built on `presyo/data/endless-prep/catalog-active.csv`, presyo's active catalogue
  exported from production: **241,677 real products**, four times the largest real corpus this
  project had.

- **This corrects a conclusion I had stated four times.** After `p13`/`p14` I wrote that every
  measurable workload was at ceiling and that "everything this repo can learn from static corpora,
  it has learned". **Wrong** - and wrong because I had not looked hard enough for corpora. Each
  earlier bench asked a question whose answer was already in the document text; this one does not.

  | bench | query | result |
  |---|---|---|
  | `p6-real-corpus` | a school's own name | 99.8 % |
  | `p8-sisia-catalog` | a course's own code | 100 % |
  | `p13-presyo-prior` | a listing's own name | 98.4 % |
  | `p14-presyo-broad` | a word the product name repeats | 97.2 % |
  | **`p15-presyo-catalog`** | **a category the name never contains** | **61.7 %** |

  Saturation was a property of the questions, not of the engine.

- **The failure mode is vocabulary mismatch, and no existing feature can fix it.** `"Baking Needs"`
  should return *Camote Powder*, *Mung Beans*, *Sago Tapioca*, *Cream of Tartar*, *Hotcake Mix* -
  **not one shares a token with the query**. Six categories score 0 %. BM25F tuning, typo tolerance,
  prefix anchoring and static priors all operate on tokens that must match something.

- **Label independence is what makes it a real test.** Only **13.8 %** of products contain their own
  category name, against `p14`'s `gold_product_type` labels which retailers repeat in the product
  name - which is why `p14` could only measure lexical precision.

### Corrected - 2026-09-05 (p7-scale was generous)

- **Recombination understates typo p99 by ~1.9x.** `p7-scale` reaches 250 K and 1 M by recombining
  61,467 real schools and flags the caveat; p15 quantifies it. Recombination stalls at **41,069
  terms** because it cannot invent vocabulary, while a real catalogue of similar size carries
  **117,472** - nearly 3x the dictionary a fuzzy expansion must search.

  | | p7 @250 K (recombined) | p15 @241,677 (real) |
  |---|---|---|
  | distinct terms | 41,069 | 117,472 |
  | typo p99 | 3.08 ms | **5.72 ms** |

  The 5 ms bar is missed at **a quarter of a million real rows**, not at a million. An honest
  downgrade of a number this project has been quoting, from better data rather than a code change.


### Verified - 2026-09-05 (no scale regression from today's scoring changes)

- **Three additions to the hot scoring path cost nothing measurable at 1 M documents.** Static
  priors, prefix anchoring and deletion each add per-candidate work inside the retrieval loop, and
  the day's correctness benches (`real-corpus`, `sisia-catalog`, `maphy-place`) say **nothing** about
  latency at scale. `p7-scale` was re-run before calling the changes done.

  | | recorded | after |
  |---|---|---|
  | exact p50 @1M | 1.14 ms | 274 us |
  | typo p50 @1M | 1.26 ms | 598 us |
  | typo p99 @1M | 9.15 ms | 6.01 ms |
  | bytes/doc @1M | 157.2 | **161.2** |

  Latency is same-or-better and is **not** claimed as a speedup - the two runs are from different
  sessions and are not controlled against each other. The supported claim is the negative one.

- **The index grew exactly 4.0 B/doc, and that is the whole story.** `first_term` is one `u32` per
  document. Priors were uniform and nothing was deleted, so **both wrote zero-length sections** -
  the two features that were not used cost nothing at all, which is what lets them ship without
  taxing an adopter who never calls them.


### Measured - 2026-09-05 (the broad-query workload, and the end of static priors)

- **`presyo-broad` bench - the labelled workload `p13` said did not exist.** `p13` claimed building
  one required somebody to decide what the right answer to "milk" is. **That was half wrong**:
  presyo's gold fixture already carries a curated taxonomy (`gold_brand`, `gold_product_type`), so
  the labels were in the file. **79 broad queries** (32 brand, 47 category), mean **53 relevant
  listings each**. The index sees only `raw_name`; labels come from gold metadata that is not
  indexed.

  Metric is **precision@10**, not recall - recall@10 is meaningless when 53 listings are relevant,
  and reporting it would repeat the `"CITY "` mistake from `p12`.

  | prior | precision@10 | delta |
  |---|---|---|
  | none | **97.2 %** | - |
  | source quality | 95.8 % | **-1.4 pt** |
  | popularity | 94.7 % | **-2.5 pt** |

- **Static priors are settled: they ship OFF.** On the workload they were supposed to be for, they
  do not merely fail to help - they **actively degrade precision by up to 2.5 points**, pulling
  well-stocked or clean-named products above ones that match the query. `p13` showed a prior buying
  nothing; `p14` shows it costing something. Two independent measurements, one answer.

- **The tail was checked before being called a target, and it was the labels.** `"chocolate"` scores
  40 % precision@10, which looks like the obvious next ranking target. **65 listings say "chocolate"
  in the retailer's name; only 14 carry it in `gold_product_type`.** "Nestle Chuckie Chocolate Milk
  Drink" is typed "Milk Drink", so the label scores it irrelevant while a shopper plainly wants it.
  The engine was right and the label was wrong - **true precision is HIGHER than 97.2 %**, and the
  tail is not work to be done.

  Sixth methodology catch in this repo, second caught *before* publishing. The habit is now
  explicit: **check whether a bad number is the system failing or the measurement lying, before
  assigning work to it.**

- **Every measurable consumer workload is at ceiling** - `p6` 99.8 %, `p8` 100 %, `p13` 98.4 %,
  `p14` 97.2 %. Further ranking work would be tuning against noise. The roadmap's next row is no
  longer a feature: it is an adopter running this against production traffic.


### Measured - 2026-09-05 (static priors: the answer is no, here)

- **`presyo-prior` bench - the last unproven feature, settled.** Static priors were measured on
  presyo's **500 gold cross-store clusters, 2,440 real listings, 13 retailers**, with the prior
  fitted on a train half and scored on a **held-out** half. Fitting and scoring on the same clusters
  would have made the prior a compressed copy of the answer key and reported a win by construction.

  | prior | recall@10 | hit@1 | rankings |
  |---|---|---|---|
  | none | 98.4 % | 99.4 % | - |
  | source quality (any spread) | 98.4 % | 99.4 % | **1,227 moved** |

  **+0.00 points.** The retailer quality spread is real (0.80 to 0.99 across sources) and the prior
  demonstrably applies - it reordered 1,227 held-out queries - it simply buys nothing.

- **The `rankings` column is the point.** "No effect" and "no benefit" produce identical metric
  columns, and the wrong one of those is the one that silently ships. Measuring whether the feature
  changed anything is what makes the negative result trustworthy rather than a possible bug report.
  This is the first bench in the repo designed *in advance* to distinguish them, which is the habit
  the previous five methodology failures were arguing for.

- **The cause is headroom, not the feature.** 98.4 % recall@10 leaves 1.6 points in the entire
  workload, and a prior cannot add a token a listing does not have. So the corpus **cannot answer
  the question** rather than answering it negatively: a prior earns its place on broad or browse
  queries where candidates are near-tied, and presyo's gold set has ground truth only for the
  specific-product task.

### Changed - 2026-09-05

- **Static priors are now labelled narrow rather than unproven**, on evidence. They stay shipped -
  correct, cheap, serialized, safe by construction - and `ROADMAP.md` now tells an adopter to leave
  them off until their own numbers say otherwise.
- **The next roadmap row is no longer engine work.** Every consumer benchmark in this project
  (`p6` 99.8 %, `p8` 100 %, `p13` 98.4 %) is at or near ceiling on the task it measures, so further
  ranking work would be tuning against noise. The blocker is a **labelled broad-query set**, which
  means someone deciding what the right answer to "milk" is.


### Added - 2026-09-05 (deletion)

- **`Searcher::delete` / `undelete` - the index can finally forget.** One bit per document, empty
  when nothing is deleted, routed to the owning segment by global ordinal. `live_count()` and
  `deleted_count()` report the split; `doc_count()` still counts tombstones.

  This was the last correctness gap against presyo, which soft-merges products via `superseded_by`
  (36,260 merged rows) and filters `WHERE superseded_by IS NULL` on public reads. An index that
  could not forget kept serving merged duplicates until the next rebuild.

- **Deletion does not rewrite postings**, and the cost is stated rather than hidden. A deleted
  document stops appearing at once but still counts toward document frequency and average field
  length until a rebuild - Lucene's behaviour between merges, and right here for the same reason:
  rewriting postings per delete turns an O(1) operation into an O(index) one. After many deletions
  IDF reflects a collection larger than the live one, so near-ties can reorder; the result SET is
  always correct.

- **`needs_compaction()` counts deletions, not just additions.** A single segment that has forgotten
  a third of its documents has skew 0.0, so a signal watching only skew would report it healthy
  forever while it drifted arbitrarily far from a clean rebuild. For presyo, deletions are the
  likelier driver of the two.

- **Filtering happens at heap INSERTION, not at candidate selection.** The candidate step is
  followed by cursor advances the MaxScore loop depends on; skipping it early would either
  desynchronize them or duplicate the advance. Scoring a deleted document and discarding it is a
  little wasted work in exchange for one obviously correct place to filter.

- **Format: `IDXTEXT2` carries 10 sections** (160-byte table). Deletions MUST persist - an artifact
  that dropped them would resurrect every deleted document on reload, which for presyo means the
  merged duplicates reappearing. Asserted by `deletions_survive_a_round_trip`.


### Added - 2026-09-05 (anchored prefix matching)

- **Typeahead now knows the user is typing from the START of a name.** `p12-maphy-place.md` found
  every typeahead miss to be a prefix ending part-way through a second token: `"DEL C"` plans as
  exact-`del` plus prefix-`c*`, and since `del` is common and `c*` matches nearly everything,
  `DEL CARMEN` never ranked. A tokenized query had lost a string-level fact.

  Storing each document's `first_term` (term id of the first token of field 0, **four bytes per
  document**) restores it. In prefix mode a document that does not anchor keeps `UNANCHORED_KEEP`
  of its score. **Typeahead hit@10 94.6 % -> 96.4 %**; `real-corpus` and `sisia-catalog` unchanged,
  which is the check that matters for a global scoring change.

  Applied as a **demotion of the non-anchored**, never a boost of the anchored - the same rule
  static priors follow: a scoring factor above 1 would let a true score exceed the block maxima the
  pruner trusts and silently drop valid results, only on corpora large enough for pruning to
  engage. Fires only for multi-token prefix queries: `"carmen"` must still find `DEL CARMEN`
  unpenalized, and for a single token the prefix term IS the whole query.

- **Format: `IDXTEXT2` grew to 9 sections** (later 10; see the deletion entry above). `first_term` is serialized because
  anchoring is a *ranking* signal - an artifact that dropped it would rank worse than the index it
  was built from, and the symptom ("search got slightly worse after we deployed") is close to
  undebuggable. Asserted by `prefix_anchoring_survives_a_round_trip`.


### Added - 2026-09-05 (static priors)

- **`IndexBuilder::add_with_prior` - query-independent per-document importance.** Normalized into
  `(0, 1]` at build time, which removes a correctness trap **by construction**: retrieval prunes
  against upper bounds (block maxima, and the non-essential bail comparing `score + optimistic
  remainder` to the threshold), so a multiplier above 1 would make true scores exceed those bounds
  and the pruner would silently discard documents that belong in the result - only on corpora large
  enough for pruning to engage. Scaling by the largest prior keeps every applied factor `<= 1`, so
  no bound needed patching and the pruning code is untouched. Only prior *ratios* affect ranking, so
  nothing is lost.

  A prior scales the score and deliberately **does not touch `typo_bucket`**, which stays the
  primary sort key: an important document matched through a typo must still lose to an exact match
  on an unimportant one. Asserted in `a_prior_cannot_outrank_an_exact_match`.

- **Format `IDXTEXT1` -> `IDXTEXT2`**, section table 7 -> 10 spans (112 -> 160 bytes) across the
  day: prior, then `first_term`, then the deleted bitmap. One unreleased version absorbed all
  three. The magic was
  bumped rather than the span appended silently, because a reader expecting seven spans would
  mis-parse the eighth as posting data and fail as garbage results rather than as an error. A
  uniform prior writes a zero-length span - the absence of a prior and a uniform prior are the same
  thing, and neither pays `doc_count` floats to say nothing.

### Added - 2026-09-05 (maphy place search: the fifth app)

- **`maphy-place` bench.** `place-index.ts` scores every entry and alias on every keystroke with
  four exact-substring predicates, so **the score of a misspelling is exactly zero**.

  | query class | engine@10 | maphy@10 |
  |---|---|---|
  | exact place name | 100.0 % | 100.0 % |
  | typeahead (answerable) | 94.6 % | **100.0 %** |
  | one-character typo | **99.7 %** | 9.3 % |

  **maphy returns an empty list for 90.7 % of one-character-typo queries.** Latency is a tie at this
  pool size (27.5 vs 31.3 us p50); claiming a win there would be dishonest.

### Fixed - 2026-09-05

- **A benchmark reported a spurious engine loss and it moved the roadmap.** The typeahead row used
  each name's first five characters as the query, so **all 40 municipalities named `CITY OF ...`
  issued the identical query `"CITY "`** and all 17 regions issued `"REGIO"`. No ranker can put 40
  documents in 10 slots, so 137 of 1,067 queries measured which arbitrary ten a tie-break picked.
  Excluding them moved the engine 85.2 % -> 94.6 % and maphy 90.4 % -> 100 %.

  From the spurious loss, *static priors* had been named the next roadmap row. Sweeping the prior's
  strength from 0 to 1.0 then moved typeahead hit@10 **not at all** - identical at every setting,
  including off - which is what sent the investigation to the real cause. **A fix that does not move
  the number was never addressing the number.** The real gap is anchored prefix matching, now the
  next row; the priors are kept but relabelled as unproven.

- **An exclusion filter silently did nothing.** It keyed on `normalize("CITY ")`, which trims the
  trailing space, against a map built from 5 chars of the normalized full name (`"city "`). The
  filter excluded 45 queries instead of 137 and the numbers barely moved, which looked like the
  hypothesis being weak rather than the filter being broken.

### Honest open-items - 2026-09-05

- **Static priors have a rationale and no evidence.** Every consumer has a query-independent
  importance signal, but no benchmark yet shows the feature improving ranking on one.
- **The barangay tier (~42,000 entries) is not on disk**, so the latency claim is untested at
  maphy's shipped worst case; the measured pool is ~40x smaller.
- **Anchored prefix is unbuilt.** It is the actual fix for the remaining typeahead gap.


### Added - 2026-09-05 (maphy place search: the fifth app)

- **`maphy-place` bench - maphy's place search reproduced and measured, and the result is mixed.**
  `apps/web/src/components/map/lib/place-index.ts` scores every entry and every alias on every
  keystroke with four exact-substring predicates, so **the score of a misspelling is exactly zero**.

  | query class | engine@10 | maphy@10 |
  |---|---|---|
  | exact place name | 100.0 % | 100.0 % |
  | typeahead (first 5 chars) | 85.2 % | **90.4 %** |
  | one-character typo | **99.7 %** | 9.3 % |

  **maphy returns an empty list for 90.7 % of one-character-typo queries.** That is the win, and it
  is not close.

  **The engine LOSES typeahead**, and the reason is worth more than the win: maphy's tie-break ranks
  by administrative level, a domain prior BM25F cannot express. A hand-rolled ranker that encodes
  domain knowledge beats a general one that does not, and no BM25 tuning fixes it - the engine needs
  a **static prior** hook. This is the cleanest evidence for that roadmap row the project has, and
  it came from losing.

  **Latency is a tie** at this pool size (27.8 vs 34.0 us p50; maphy is faster at p99). A linear
  scan over 1,067 short strings is not slow, and saying otherwise would be dishonest.

### Honest open-items - 2026-09-05 (place search)

- **The barangay tier is not on disk.** ~42,000 more entries is maphy's shipped worst case and the
  only place the latency claim can be tested; `apps/web/public/data/place/` is empty in this
  checkout. The measured pool is ~40x smaller, so the latency row is a floor on the gap, not the
  gap.
- **A metric, not the engine, produced a spurious FAIL.** Exact-name rank 1 first read 91.8 % and
  failed a 95 % gate. **137 of 1,067 entries (12.8 %) are duplicate names**, so demanding one
  specific row at rank 1 demands the impossible and would score a perfect ranker at ~87 %. Measured
  against the set of same-named places, both systems get 100 %. Fourth time in this repo a
  methodology was wrong before the code was - and every one was caught by a number sitting
  suspiciously close to a structural constant of the data.
- **Adopting the engine here is a trade, not an upgrade**: a typeahead regression for a typo win.
  That is maphy's product decision, not this bench's.


### Added - 2026-09-05 (the browser point-in-polygon number)

- **`index-geo` + `index-geo-wasm` + `js/geo.mjs` - point location in a browser at 3.61 M points/s,
  ~27x a JavaScript scan.** `docs/research/geometry-sota.md` found the geometry stack largely solved
  and named one hole with **no published figure in any language**: point-in-polygon at scale in a
  browser. `bench/roadmap/p10-geo-join.md` filled the native half; `p11-geo-wasm.md` fills this one.

  The finding is that **WASM is at parity with native** - 4.43 M points/s in Node and 3.61 M in real
  headless Chrome, against 4.50 M for the same index in native Rust, and against DuckDB's 2.02 M
  native *multicore* reference. A browser costs about 20 %, not a factor. The reason is structural
  rather than a compiler win: most queries touch no geometry at all, so what crosses into WASM is a
  binary search over a `u64` array.

  **22,334 bytes gzipped**, against DuckDB-WASM's 149.4 MB npm package. Hand-written raw-pointer C
  ABI, no wasm-bindgen, same convention as `index-wasm` - so one artifact serves Node, the browser,
  an edge worker and a native FFI.

- **The gate now runs it.** `geo-bench` is wired into CI as a **correctness** gate, not a
  performance one: it throws unless the WASM index and a pure-JS scan return the identical polygon
  for every one of 53,715 real points. CI runners are too noisy to gate on timings; the agreement is
  what has caught every bug in this line of work.

### Fixed - 2026-09-05

- **A MultiPolygon is not a polygon with holes.** The JS fixture loader treated the first ring of
  each polygon as its outer boundary and every later ring as a hole - correct for a simple polygon,
  **wrong for an archipelago**, where each island is a second *outer* ring. It silently turned most
  of the Philippines into holes and reported 34,726 POIs inside a province where the truth is
  51,732.

  The per-point assert did not catch it, because both JS arms shared the loader and were wrong
  together. The **cross-tier** comparison against the independent Rust implementation did.
  **Two agreeing implementations that share an input parser agree about the parser, not about the
  answer.** `js/geo.mjs` now takes `{ pt, outer }` rings explicitly and its doc comment names the
  trap.

### Honest open-items - 2026-09-05 (browser tier)

- **Chrome only.** No Safari, no Firefox. The ABI is deliberately fixed-width - `i32` offsets, no
  memory64, no relaxed SIMD - because relaxed SIMD is Safari-flag-gated and memory64 is
  Safari-unsupported. "Designed for it" is not "measured on it".
- **No SIMD, no workers.** The crossing test is a natural fit for vectorization and is unexercised;
  everything measured runs on one thread.
- **The favourable regime is many small polygons.** 88 large provinces give only 4.8x, because a
  bbox-prefiltered scan over 88 candidates is already cheap. Stated as a rule with a condition
  rather than as a single number.


### Added - 2026-09-05 (geometry: point-in-polygon as an index)

- **`geo-join` bench - point-in-polygon over real maphy geometry, 28.2x over a naive scan and 9.1x
  over a bbox-prefiltered one.** `docs/research/geometry-sota.md` surveyed the field and found the
  cloud-native geometry stack largely solved - FlatGeobuf's bbox-over-HTTP, PMTiles tile addressing,
  COPC octree range reads, meshopt as the vertex codec - and one hole with **no published numbers in
  any language**: point-in-polygon at scale in a browser. This is the first half of filling it.

  Both sides of the join are real files: **53,715 POIs** and **88 unclipped PSGC-coded provinces**
  (991 rings, 37,507 vertices) or **2,454 municipal polygons** decoded from maphy's own PMTiles.
  Provenance and rebuild commands are in `bench/fixture/README.md`.

  Two index shapes were built, and they give opposite answers, which is the finding:
  a **cell index** (rasterize to a Hilbert-ordered grid; interior cells answer with zero geometry)
  wins on **many small polygons** - 11.93 ms, 126 KB - while a **vertex index** (each cell stores
  the polygons containing its centre plus the boundary segments crossing it, answered by a local
  parity walk) wins on **few large ones** - 11.65 ms against the cell index's 17.72 ms. Measuring
  only one dataset would have produced a confident and wrong general claim in either direction.

- **`sfc-2d` now runs on real coordinates**, closing the gap `p9-sfc-2d.md` recorded against itself.
  Real and synthetic points on an identical grid give **identical range counts** and over-fetch
  within 7 %, so the synthetic stand-in was honest and the earlier conclusion is unchanged.

### Honest open-items - 2026-09-05 (geometry)

- **The browser number is still unmeasured.** `geo-join` is native Rust. The vacuum the research
  identified is specifically a WASM/browser figure, and this table does not get to claim it. The
  native throughput is 4.50 M points/s single-threaded against DuckDB's 2.02 M points/s native
  multicore reference - different hardware, different data, and DuckDB solves the harder general
  case.
- **Vertex dedup does not pay on this data: 1.09x.** Shared-vertex topology is the premise of
  TopoJSON's arc sharing. On maphy's real province geometry it collapses 37,507 vertices to 34,442,
  flat from a 340 m quantization down, because adjacent provinces were simplified independently and
  do not share vertex coordinates. Most duplicate instances are ring closures. Recorded as a
  refuted hypothesis rather than built on.
- **41,966 barangays is an extrapolation.** The trend from 88 to 2,454 polygons (1.9x to 28.2x) is
  strongly favourable but unrun at maphy's real target size.
- **Still no head-to-head against an R-tree.** `p9`'s caveat stands: sufficient is not better.
- **Three correctness bugs, all caught by asserting equality with the scan over every point.**
  Boundary and interior cells are not mutually exclusive when neighbouring polygons have slivers
  between them (28 wrong answers); a cell centre can be inside several overlapping polygons at once
  (1,496); and a parity walk is undecidable when the ray passes exactly through a vertex (1 in
  53,715, now detected and deferred to a full test). Sampled agreement would have missed the third.


### Added - 2026-09-05 (incremental update)

- **`index-text::searcher` - multi-segment search, so documents can be added without a rebuild.**
  `docs/adoption.md` named index immutability as the real gate on presyo, whose daily scrape
  processes 2.08 M raw observations; a rebuild is ~15 s at a million documents. A `Searcher` holds
  ordered immutable segments, new documents go into a small cheap one, and queries run across all
  segments and merge with the same comparator the single-segment path uses.

  The design fits this project specifically: **the application's database is the source of truth and
  the index is derived**, so compaction is not a merge of segments - it is a rebuild from rows the
  app already has, on whatever schedule it likes, while new rows become searchable immediately.
  `skew()` reports how much of the collection lives outside the largest segment and
  `needs_compaction()` is advisory, because this crate does not know when an application can afford
  a rebuild and silently blocking a write to compact would be worse than saying so.

  Global document ordinals never move when a segment is appended, so an application may store them.

### Honest open-items - 2026-09-05

- **Segmented scoring perturbs the order of near-equal documents, and the test says so.** BM25 needs
  collection-wide statistics; a segment only knows its own. Measured on a 195+5 split: a
  **selective** query identifies the same document as a full rebuild, while a **broad** query
  matching most of the collection can reorder its near-ties. The result SET is preserved either way.
  This is the standard cost of segmented search - Lucene's IDF is per-shard for the same reason -
  and it is asserted per query class rather than averaged into a single flattering number.
- **Deletion is not implemented.** `Searcher` closes additions only. presyo soft-merges products via
  `superseded_by` (36,260 merged rows), so an index that cannot forget will serve merged duplicates
  until compaction. A per-segment deleted bitmap is the next row.
- **A test fixture, not the engine, was wrong.** The first version of the skew corpus ended every
  document with the word `pack`, and `pack` vanished as a term - because the analyzer correctly
  merged `3 pack` into the quantity `3pc`. The engine was right; the fixture was misleading, and the
  comment now says so.


### Added - 2026-09-05 (sisia-app, on sisia-app's own data)

- **`index-bench` bin `sisia-catalog`** + `bench/roadmap/p8-sisia-catalog.md`. **sisia was written
  off one round too early.** The corpus this repo has benchmarked from the start declares
  `"source": "sisia class_section_all"` in its own metadata — it *is* sisia's registrar table,
  exported through profstopick's research pack: **2,253 distinct course-code x title pairs** of real
  sisia data, on disk the whole time.

  Its catalog search (`Course.ts`) is `course_code LIKE ?` plus `LOWER(title) LIKE ?`, ordered by
  code, with no relevance ranking. Reproduced as the baseline rather than strawmanned.

  | | engine | sisia's shipped LIKE |
  |---|---|---|
  | exact code hit@1 | 100.0 % | 100.0 % |
  | **out-of-order title words hit@10** | **91.2 %** | **0.0 %** |

  A substring `LIKE` matches one contiguous run, so a two-word query in the wrong order matches
  nothing across 2,038 real course titles — and no parameter changes that, because the limit is the
  operator. It is the same failure class sisia documented at `driveHybridSearch.ts:88-93`, where AND
  semantics "matched almost nothing" and switching to OR then over-matched.

### Honest open-items - 2026-09-05

- **A non-win, kept rather than deleted.** On the prefix-bleed set the engine is **not** better than
  sisia's `LIKE`, and two attempts to construct a metric where it was are recorded in the bench doc.
  The first measured rank: both reach 100 %, because `ORDER BY course_code` sorts `CHEM 399.1` above
  `CHEM 399.11` by lexicographic luck, so sisia's documented bug does not manifest as a ranking
  failure on this corpus. The second measured extra rows returned and the engine came out **worse**
  (1,391 vs 295) — because it *ranks* where `LIKE` *filters*, so that metric was measuring recall
  and calling it imprecision. **For exact course-code lookup, sisia's `LIKE` is adequate here.**
- **sisia's hybrid path is untouched.** The `ts_rank_cd` sparse arm fused with pgvector by RRF k=60
  and reranked by Vertex needs a database, a corpus and an API key this machine does not have. The
  engine's sparse arm is shaped to drop into it (`query -> (id, rank)[]`), but that is asserted, not
  measured.


### Added - 2026-09-05 (champion lists)

- **Static pruning via champion lists.** Per term with `df >= 512`, the positions of its 64
  highest-scoring documents are precomputed; a query seeds its top-k heap from **one** term's
  champions - the most discriminative that has a list - so the threshold is already high when the
  scan begins. Champions are real documents scored exactly as the main loop scores them, so no
  answer changes; the main loop declines to insert a seeded document twice.

  | | exact p50 @1M | typo p50 @1M | typo p99 @1M |
  |---|---|---|---|
  | before | 475-580 us | 809-875 us | 6.02-6.40 ms |
  | after | **279-314 us** | **643-687 us** | 6.36-7.24 ms |

  **Median latency improved ~1.7x; the tail did not move.** Kept because throughput follows the
  median, and reported honestly because the tail did not.

### Honest open-items - 2026-09-05

- **The 1 M tail cannot be resolved at the precision the bar demands.** Three identical runs of the
  same binary measured **6.36, 6.72 and 7.24 ms** - a 0.9 ms spread on a 5 ms bar. Any p99 claim at
  this scale that omits that spread is overclaiming, so the row stays FAIL rather than being
  declared met by picking the best run.
- **Champion lists raise the threshold; they do not make block maxima informative.** Those are two
  different problems and only the first is solved. On a query scoring against one common term, a
  document's score tracks its length and a block of 128 arbitrary documents almost always contains a
  short one, so a legitimately-high block maximum cannot be skipped no matter how high the threshold
  is. Recursive graph bisection remains the technique that attacks the real cause.
- **Two seeding designs were measured, not guessed.** Seeding from every query term measured
  **7.44 ms p99** - worse than no seeding - because it scored `terms x 64` candidates with a binary
  search each and deduplicated with a linear `contains` over a growing vector.


### Added - 2026-09-05 (the browser tier, and presyo at catalogue scale)

- **`js/browser.html` + `js/browser-check.mjs` - the engine running in a real browser.** Node proves
  it works outside Rust; a browser is a different claim: no `fs`, the index arrives over HTTP, the
  query runs on the main thread beside a render loop, and it is the only place
  `instantiateStreaming` and its hard `application/wasm` MIME requirement are exercised. It is also
  the tier profstopick actually ships to.

  The page drives the **raw C ABI**, not `js/index.mjs`, so a failure cannot be hidden by the Node
  wrapper. Measured in headless Chromium 147: 244,200-byte module, 380,564-byte index, 1,322
  documents, **52.7 ms compile+instantiate, 16.8 ms open+parse, and 37 microseconds per query on the
  main thread** with typo tolerance intact. Playwright is deliberately not a dependency - the check
  borrows it from a sibling checkout and exits 2 with instructions if it cannot, rather than
  pretending it ran.
- **presyo compared at 260,000 rows**, their production order of magnitude. The 1,940 real gold rows
  stay exactly as exported; the rest are distractors **recombined from real presyo tokens**, so
  every token is real and every row distinct. Whole-document replication was deliberately avoided -
  it produces near-identical copies, the corpus shape that most distorts top-k pruning, a mistake
  this project already made once and recorded in `bench/roadmap/p7-scale.md`.

  | 260,000 rows | recall@10 | hit@1 | mean latency |
  |---|---|---|---|
  | clean, presyo shipped SQL | 100.0 % | 99.4 % | 61.40 ms |
  | clean, index engine | 100.0 % | **100.0 %** | **0.34 ms** |
  | one typo, presyo shipped SQL | 99.8 % | 97.6 % | 54.01 ms |
  | one typo, index engine | **100.0 %** | **99.0 %** | **0.42 ms** |

  **Recall held at 134x the haystack and hit@1 went up**, with queries staying sub-millisecond. A
  distractor can only ever hurt the score - ground truth is still `gold_product_id`, so a recombined
  row can never be counted as a hit.

### Honest open-items - 2026-09-05

- **The browser tier has no persistence.** The index is re-fetched on every load; OPFS caching
  (ROADMAP P8) is unbuilt, and Safari evicts script-written storage after 7 days regardless, so any
  cache needs a rebuild path.
- **The 260 K presyo run is padded.** Only 1,940 rows are real exported production data; it does not
  populate their `search_text` column or their ~296 K aliases, and says nothing about their real
  catalogue's term distribution.
- **Nothing is deployed.** Three worktree branches, no PR against any app.


### Added - 2026-09-05 (presyo, head to head with the shipped implementation)

- **`scripts/compare_index_engine.{sh,ts}` in a presyo worktree** - a disposable `postgres:16-alpine`
  loaded with 1,940 real cross-store listings from their frozen production gold-cluster export
  (2026-06-13), with **their own `searchProduct` imported and called**: the real 7-lane `pg_trgm`
  `UNION ALL` with the inline `ts_rank` rescore, over the trigram indexes migrations 004 and 012
  create. Same rows, same queries, both answers printed.

  The task is presyo's hardest: cross-store product identity. The query listing is held out and not
  loaded, so a hit can only come from a different retailer's wording. Ground truth is
  `gold_product_id`.

  | | recall@10 | hit@1 | mean latency |
  |---|---|---|---|
  | clean, presyo shipped SQL | 100.0 % | 99.4 % | 46.72 ms |
  | clean, index engine | 100.0 % | **99.8 %** | 0.09 ms |
  | one typo, presyo shipped SQL | 99.8 % | 97.6 % | 43.39 ms |
  | one typo, index engine | **100.0 %** | **99.0 %** | 0.14 ms |

  **Read honestly: presyo's SQL is good.** `pg_trgm` handles a single mistyped character almost
  perfectly at this corpus size; the engine's margin is narrow (+0.2 points of typo recall, +1.4 of
  typo hit@1). The latency gap includes a loopback round trip to a container, so it demonstrates
  "in-process beats a database round trip", not a better query planner. And 1,940 rows is not their
  260 K-product catalogue.

### Honest open-items - 2026-09-05

- **sisia-app could not be reached at all.** Its catalog is a gitignored `sisia.db` that lives on the
  VPS, and its hybrid retrieval needs Postgres + pgvector + a Gemini key. A genuine gap, recorded
  rather than papered over with a proxy measurement.
- **presyo's input contract is deliberately left unsatisfied.** It pins
  `productSearchToken('Coca-Cola 1.5L') === ['coca','cola','1.5l']`; the analyzer produces `1500ml`
  instead, because canonicalizing the unit is what makes `1.5L`, `1500ml` and `1.5 liters` a single
  token - a property presyo itself measured as worth **+4.6 pp recall**. Satisfying it verbatim
  would be a regression, so it is stated rather than quietly bent.
- **Nothing is deployed.** Three worktree branches, no PR, no browser execution.


### Added - 2026-09-05 (onegrid's kernels, and a second app contract satisfied)

- **`index-accel` - onegrid's ratified `AccelModule` ABI, implemented.** `demand.md` Finding 4
  recorded that onegrid had ratified an acceleration ABI, written the JavaScript reference backend,
  written the property-based differential harness that proves an accelerated backend identical to
  it, budgeted the artifact - and shipped **no module**. `packages/wasm/crate/` did not exist and
  every test ran against `createFakeAccelModule()`. **The socket was cut and empty.**

  All seven kernels (`og_sort_pass`, `og_filter_mask`, `og_group_code`, `og_group_combine`,
  `og_aggregate`, `og_bitmap_op`, `og_top_k`) plus `og_abi_version` and `og_heap_base`, in
  **6,342 bytes** of `no_std`, zero-allocation WebAssembly. `no_std` is structural rather than
  stylistic: **the JavaScript host owns the heap**, bump-allocating above `og_heap_base()`, so a
  Rust allocator in the same linear memory would hand out addresses the host believes it owns.
  `top_k` marks consumed survivors with `i32::MIN` in the caller's scratch buffer for the same
  reason.

  **Result: onegrid's `packages/wasm` suite passes 294 / 294** driven by the real module, including
  a copy of their `differential.property.test.ts` whose only edit is the backend under test. Their
  generators are deliberately hostile - weighted toward NaN, `-0`, infinities, duplicates, ties and
  zero-length columns, because ties are where a stable sort and a group-by are actually interesting.
- **profstopick's full suite run against the search adapter: 1,922 / 1,958.** The 4 failures fail
  identically on their untouched `main` (verified by running them there), so the adapter breaks
  nothing.
- CI now builds both wasm artifacts.

### Changed - 2026-09-05

- `docs/integration.md` now covers both applications, with what each proves and what it does not.

### Honest open-items - 2026-09-05

- **There are deliberately no host unit tests for the accel kernels, and the reason is structural.**
  Every pointer in that ABI is a `u32`, because on wasm32 a pointer *is* an offset into linear
  memory; on a 64-bit host an address does not fit in 32 bits, so a test passing
  `vec.as_mut_ptr() as u32` silently truncates and the kernel reads garbage - which is exactly what
  the first version did, faulting with `STATUS_ACCESS_VIOLATION` rather than failing an assertion.
  Simulating linear memory with an arena would mean giving every kernel a base parameter the real
  ABI does not have, i.e. testing a different function from the one that ships. Correctness is
  proven where the module runs, by the consumer's own harness.
- **presyo and sisia-app still have data-level evidence only.** presyo's contract test and sisia's
  sparse arm both require a live Postgres this machine does not have; what has been measured is
  their exported corpora (`real-corpus`, 100 % cross-store recall@10, zero size violations).
- **Nothing is deployed.** Both integrations are throwaway branches. No browser has executed either.


### Added - 2026-09-05 (measured against an application's own contract)

- **`docs/integration.md`** - the engine run against **profstopick's `test/search-name-order.test.mjs`,
  unmodified, with only the import line redirected**, on a git worktree branch that leaves its `main`
  untouched. That test encodes a production measurement from 2026-08-17 (158 hits vs 109 misses, a
  40.8 % miss rate) and six explicit survival assertions. **Result: 9 of 9 pass**, including all
  three production failures. It passed 7 of 9 first; both failures produced engine fixes.
- **In-process index building in the WASM ABI** (`idx_build_new` / `idx_build_add` /
  `idx_build_finish` / `idx_build_free` / `idx_serialize`, ABI v2). Without it a host could only
  open a prebuilt blob, which means adopting the engine required a Rust build step - a far larger
  ask than `npm install`. A Node or browser host can now index its own data directly, and serialize
  the result to cache it.
- **Compound splitting.** A token absent from the dictionary that splits cleanly into two dictionary
  terms is replaced by those two, preferring the **rarest** pair (a split into two common terms is
  usually an accident: `therapist` -> `the` + `rapist`). Closes `math30.23` for profstopick and an
  entire category of presyo's frozen fixture - `cocacola`, `bearbrand`, `luckyme`, `pancitcanton`.
  Tried **only after exact and fuzzy have both failed**: attempting it earlier measured **7.12 ms
  p99 against 6.37 ms** at a million documents, because a typo'd token often happens to split into
  two real terms and taking that split discards the correction.
- **Separator normalization between digits.** `30.23`, `30,23` and `30-23` become one token, so a
  code matches however it is punctuated. The cost is stated in the code and accepted: a hyphenated
  numeric range folds to a decimal.

### Fixed - 2026-09-05

- **The typo length gate did not hold in prefix mode - a real bug.** A one-character token fell
  through to the distance-1 automaton, and every single letter is one substitution away from any
  other, so `"c"` matched essentially the entire dictionary. `"jacob c"` therefore matched
  `Jacob, Precious` by spending `jacob` twice, which is the exact failure profstopick's contract test
  names as "how a name matcher turns into a fuzzy one". Fixed with a distance-0 prefix automaton
  below the 1-typo threshold, plus a regression test asserting `"c"` reaches `cruz` and `colgate`
  but not `jacob` or `precious`.
- **Query planning expanded twice per unmatched token.** Probing "is this reachable by fuzzy?" and
  then expanding again paid for two automaton traversals on exactly the tokens most expensive to
  traverse. Restructured to expand once and reuse the result: **7.40 ms -> 6.40 ms p99** at 1 M.
- `is_none_or` (stable 1.82) used against a declared MSRV of 1.74.

### Honest open-items - 2026-09-05

- **Nothing is deployed.** The profstopick branch exists to measure, not to ship: no PR, the app's
  other ~150 tests were not run against the adapter, its build pipeline still emits the JSON shard,
  and no browser has executed it. presyo, sisia-app and onegrid have data-level evidence only.
- **1 M still misses the 5 ms p99 bar (6.40 ms), and the cause is now measured rather than guessed.**
  The tail is low-selectivity queries - the worst case has two terms, one carrying a 428,654-posting
  list, with nothing discriminative to prune against. Block maxima are uninformative because the
  score is uncorrelated with document id, which is precisely what docID reordering fixes. Three
  optimization attempts were made on hypotheses before this was measured; the diagnostic should have
  come first.


### Added - 2026-09-05 (the engine leaves Rust)

- **`index-wasm` - the binding that makes the engine usable outside Rust.** A `cdylib` with a
  **hand-written C ABI and no wasm-bindgen**, so one artifact serves the browser, Node, edge workers
  *and* native FFI for Go/Python/PHP/Ruby. Deliberately the ABI shape onegrid already ratified on
  this machine (versioned, JS owns the heap via explicit alloc/free), so `index` drops into a socket
  that already exists rather than inventing a second one. **173,700-byte `.wasm`.** Three tests
  exercise the ABI exactly as a host does, and assert that every failure mode - null handle, bad
  magic, truncation, non-UTF-8 query - returns a sentinel rather than trapping, because a trap in
  WASM kills the whole module instance.
- **`js/index.mjs`** - the JavaScript host. Re-derives its typed-array views after every call,
  because `memory.grow()` detaches them **even at `grow(0)`**.
- **`js/demo.mjs`** - end-to-end proof against real data: a 174 KB module plus a 380 KB index
  answering typo'd Filipino name queries at **~20 us per query from JavaScript**, with exact,
  surname-only, lowercase, one-deletion, transposition and typeahead cases all resolving to the
  right professor.
- **`js/smoke.mjs`** + CI steps - the WASM artifact is built and loaded on every CI run, so "works
  in any language" cannot silently stop being true.
- **`index-bench` bin `emit-artifact`** - builds a shippable `.idx` from profstopick's real
  registrar snapshot and prints an explicitly **not** like-for-like size comparison against the
  2,505,813-byte JSON shard that application ships today.

### Fixed - 2026-09-05

- **`u32` sentinels were unrecognisable in JavaScript.** WASM has no unsigned 32-bit return type, so
  `u32::MAX` arrives as `-1` and the error check `n === 0xffffffff` never fired. Caught by the CI
  smoke test on its first run. Every boundary now coerces with `>>> 0`.
- **Per-posting scoring recomputed a query-independent value.** `pseudo_tf` ran a loop over fields
  with a float division for **every posting visited**; the saturated contribution depends only on
  the document and its field lengths. Precomputed at build and load time: exact p50 at 1 M documents
  **1.02 ms -> 590 us**.

### Changed - 2026-09-05

- **`MAX_EXPANSION` 50 -> 16.** Expansions are already ordered by edit distance then document
  frequency ascending, so the cap keeps the closest and rarest - the most discriminative and the
  cheapest to traverse. Measured: recall unchanged at 100 % on both production corpora, typo p99 at
  1 M **7.88 ms -> 6.39 ms**.
- `BLOCK` stays at 128; 64 was measured and made no difference (6.39 vs 6.40 ms), so the extra
  metadata is not worth carrying.

Cumulative at 1 M documents across this session: **exact p50 4.66 ms -> 590 us; typo p99 15.9 ms ->
6.39 ms.**

### Honest open-items - 2026-09-05

- **1 M documents still misses the 5 ms p99 bar** (6.39 ms). Remaining levers named and unbuilt:
  PEF/Slicing postings compression, recursive graph bisection docID reordering, SIMD block decode.
- **The WASM artifact is proven in Node, not in a browser, and not inside any application's own
  codebase.** No PR has been opened against presyo, profstopick, sisia-app or onegrid. Demonstrated
  on their data and through their ABI is not the same as adopted.
- **No incremental updates.** The index is immutable; adding a document means a rebuild.


### Added - 2026-09-05 (persistence, scale, CI)

- **`index-text::format` - the portable on-disk index.** Range-readable by construction: a fixed
  section table at the head, **every offset a `u64`**, and a cumulative posting-offset array so a
  reader can fetch **one posting list** with a single ranged request. `docs/research/portability.md`
  is why: wasi-libc's `mmap` is a fake that silently reads the whole file into linear memory, so an
  mmap-first format closes WASM permanently. Six tests, including a demonstration that a single-term
  read touches a small fraction of the file, byte-for-byte deterministic serialization (so an index
  can be content-hashed and cached immutably), and truncation/corruption handling that errors rather
  than panics.
- **`index-bench` bin `scale`** - 5 K to 1 M documents over **61,467 real Philippine schools** (DepEd
  masterlist). Real corpus at 61,467: **194 us exact p50, 1.83 ms typo p99, 10.2 MB**. At 1 M:
  **1.14 ms exact p50, 1.26 ms typo p50, 9.15 ms typo p99** - p50 is milliseconds, **p99 misses the
  5 ms bar and that row is recorded as FAIL**. Spec: `bench/roadmap/p7-scale.md`.
- **CI, finally** (`.github/workflows/gate.yml`) - open since 2026-06-19. Runs tests, doc tests and
  clippy with `-D warnings` on Linux, so "green" stops meaning "green on one Windows box". The
  `bench/roadmap/` items stay excluded, and the two benches needing sibling checkouts are built but
  not run rather than faked.

### Fixed - 2026-09-05 (four retrieval defects, found by scaling)

- **An O(total postings) prologue on every query.** `plan()` recomputed each term's maximum-score
  bound by scanning all its postings - a bound that is a property of the index, not the query. Query
  latency tracked document count almost exactly. Precomputed at build and load time.
- **A linear scan for the heap minimum on every replacement** (~200 comparisons per accepted
  candidate). Replaced with a `BinaryHeap`.
- **Unbounded fuzzy expansion.** One token could expand to hundreds of dictionary terms, each adding
  a posting list to walk - tail latency, not median. Capped at 50, Elasticsearch's `max_expansions`,
  ordered by edit distance then document frequency so the cap drops the vaguest and least
  discriminative matches rather than an arbitrary slice.
- **An inverted tie-break in the candidate heap - a soundness bug.** The heap evicted the *lower*
  document id among equal scores while the final sort *preferred* it, so equal-scoring documents
  disappeared from results entirely. The block-skip logic was suspected first and was innocent;
  disabling it to isolate the cause is what found the real one.
- **Non-associative float accumulation made result order depend on the optimizer.** MaxScore
  accumulates a document's terms in a different order than exhaustive scoring, and `f32` addition is
  not associative, so genuinely-tied documents could be transposed. Accumulation moved to `f64` and
  a single ranking comparator with a relative tie tolerance now falls through to the deterministic
  document-id tiebreak.

Cumulative effect at 1 M documents: **exact p50 4.66 ms -> 1.14 ms; typo p99 15.9 ms -> 9.15 ms.**

### Changed - 2026-09-05

- **Block-max MaxScore.** Per-block last-doc and max-score metadata (`BLOCK = 128`) with a
  block-skipping `seek`, so retrieval can skip a whole range of documents when even the optimistic
  bound cannot beat the threshold.
- **Candidate pool `max(k*5, 100)` -> `max(k*3, 32)`.** The pruning threshold is the *pool's* worst
  score, so every extra slot weakens skipping. Measured: identical recall on both production corpora
  (`real-corpus` stays at 100 %) with materially lower tail latency.
- **`search_exhaustive` now mirrors the pool semantics**, so the oracle tests the claim it should -
  *pruning does not change the answer* - and not a different claim, *the pool is large enough*,
  which is a recall question answered by production data rather than by an assertion.

### Honest open-items - 2026-09-05

- **A benchmark methodology error, recorded rather than quietly fixed.** `scale` originally
  replicated whole documents, producing ~16 near-identical copies of every school - the corpus shape
  that most defeats top-k pruning. It measured 19.5 ms p99 at 1 M and sent three optimization
  attempts chasing a problem the benchmark had invented. **The corpus is part of the claim.**
- **1 M documents still misses the interactive p99 bar.** The remaining levers are named and
  unbuilt: PEF/Slicing postings compression (postings are currently 12 uncompressed bytes each),
  recursive graph bisection docID reordering, SIMD block decode.
- **Above 61,467 documents the corpus is recombined, not observed** - real tokens, synthetic
  combinations. It measures posting-list and top-k scaling, not vocabulary growth on new text.
- **The engine is still not wired into any application.** No napi-rs or WASM artifact exists.


### Added - 2026-09-05 (the engine, and its first real-data proof)

**`index-text` — the retrieval engine itself.** The spine `ROADMAP.md` Part II describes, built and
measured against production data rather than specced. `index-core` remains dependency-free; the
engine depends only on `tantivy-fst` and `levenshtein_automata`, the exact stack Meilisearch and
Tantivy ship (4.2 M and 4.4 M recent downloads, audited in `docs/research/build-or-buy.md`).

- `analyze` - the single Unicode fold, tokenization, **unit canonicalization as integer arithmetic**
  (`1.5L` = `1500ml` = `1.5 liters` = `1,5L`, exact, no floating point), split-quantity merging
  (`850 g` -> `850g`, which real retailer feeds require), and curated alias tables including a
  Philippine grocery + Filipino/English starter set.
- `dict` - FST term dictionary plus **the typo policy**, which is the part no crate provides:
  <= 2 edits hard-capped, length gates at 0-3/4-7/8+, first character protected, **numeric tokens
  exempt from fuzzy matching**, and lazy firing that short-circuits on an exact hit.
- `index` - **BM25F with exact `u16` field lengths** (Lucene and Tantivy quantize the fieldnorm into
  one byte, which destroys the length signal on short product titles) and per-field `k1`/`b`
  (Tantivy's are compile-time constants, issue #2924). Retrieval is **block-max MaxScore, not BMW** -
  BMW inverts on dense queries (SPLADE in PISA: BMW 681 ms vs MaxScore 220 ms).
- `fuse` - **RRF at k=60 as a built-in**, the constant three of this machine's codebases converged on
  independently, plus tunable convex combination.

**`index-bench` bin `real-corpus`** - the engine against **two corpora exported from production
databases**, with the consumer's own shipped matcher reproduced as the baseline:

- **profstopick, 1,322 real Ateneo professors.** Typo hit@10 **99.9 % vs the shipped matcher's
  14.9 %**; zero-result rate **0.1 % vs 85.1 %**; MRR@10 0.998 vs 0.149; exact hit@1 99.8 %.
  29,881-byte dictionary (6.55 B/term), 5 ms build, p50 42.6 us / p99 202 us.
- **presyo, 500 gold cross-store clusters / 1,940 real retailer listings.** Given one store's raw
  name, find the same product in a *different* store: clean recall@10 **100.0 %** (hit@1 99.8 %),
  typo recall@10 **100.0 %** (hit@1 99.0 %). **Zero size violations** across the 478 queries stating
  a mass or volume. 6,780-byte dictionary, 2 ms build, p50 33.3 us / p99 86.5 us.
- `INDEX_DIAG=1` asserts MaxScore's pruned results are identical to exhaustive scoring (100.0 % vs
  100.0 %) - the only thing that makes the pruning worth having.

Spec, thresholds and provenance: `bench/roadmap/p6-real-corpus.md`.

### Fixed - 2026-09-05 (three defects only real data could find)

- **Unmatched query terms were scored as if they matched perfectly.** `typo_bucket` mapped a query
  group the document matched *not at all* to `0` - identical to a perfect match - so documents that
  silently **ignored** a query word outranked documents that **found it with one typo**. Cost:
  presyo typo recall@10 of **28.8 % where it should have been 100 %**, and profstopick typo hit@10
  of 46.4 % instead of 99.9 %. Exhaustive scoring measured 28.6 %, proving it was ranking rather
  than pruning. Fixed by `MISSING_TERM_PENALTY = 3`, strictly above the maximum edit distance.
  **No unit test on a small corpus can find this** - on a small corpus every document matches every
  token - which is the argument for the real-corpus bench existing at all.
- **A stated size lost to word overlap.** `Purefoods Honeycured Bacon Roll Pack 500g` returned the
  **250 g** listing, because the 500 g listing omitted two adjectives and three word-misses
  outweighed one size-miss. A price-comparison bug presenting as a relevance bug. Fixed by
  `MISSING_QUANTITY_PENALTY = 16` - a stated size is not just another word - and proven safe when no
  listing carries the requested size, since the penalty then cancels across all candidates.
- **`1.5 L` with a space did not canonicalize.** Real retailer feeds write `850g` and `850 g` for the
  same product across stores; without merging, the size stopped being one comparable token and the
  numeric guard had nothing to guard.
- **The size guard reported three violations that were not violations** - a *benchmark* bug. `6S` in
  `MILKMAN YOGURT DRINK STRAWBERRY 6S 100ML` parses as 6 pieces; comparing a pack count against the
  query's `100 ml` produced spurious failures. A benchmark that reports the wrong defect is worse
  than no benchmark, so it is recorded rather than quietly patched.

### Honest open-items - 2026-09-05

- **The engine has not been run inside any consumer application.** It is measured *against* their
  data, not wired into their code. No napi-rs or WASM artifact exists (ROADMAP P8/P9).
- **Nothing is persisted.** The index is rebuilt in memory every run; the portable on-disk format
  (ROADMAP P7) is not written, so none of the browser/edge/four-transport story is real yet.
- **`real-corpus` depends on sibling checkouts**, so it stays gate-excluded until the corpora or a
  fixture subset are vendored here. A gate that silently skips is not a gate.
- **Both corpora are small** - 1,322 and 1,940 documents. The 100 % figures are real but are not
  evidence about 260 K products or 10 M rows.
- No CI, still. `cargo test --workspace` is 55 tests green on one Windows box.


### Changed - 2026-09-05 (demand-led re-baseline)

**The project's thesis changed.** A full research programme - eleven applications on this machine
surveyed by source inspection, six web-research lanes, one cross-model X/practitioner sweep, and a
crates.io maturity audit - established that **the learned-index core solves a problem none of the
consumer applications has**. Their hot paths are text -> ranked documents, predicate -> row set, and
name -> canonical entity; a faster `u64 -> position` map appears in none of them. The literature
agrees independently (MountDB, arXiv 2605.23815: PGM is used as an SST fence pointer, not a
retrieval algorithm).

Nothing was deleted. The existing work is **re-scoped from thesis to component** - fence pointers,
succinct structures, an adaptive filter column, and a p99 measurement harness better than the ones
in the literature. `ROADMAP.md` was rewritten around what the applications measurably need:
normalization, a typo-tolerant term dictionary, BM25F, and a portable index format.

- `ROADMAP.md` - rewritten. Old P3 (spatial/viewport) and P4 (federated multi-modal) **deferred**,
  not cancelled: no surveyed repo has a workload their current stack fails at. New spine P3-P13,
  ordered by consumer pain closed per week of work. Adds a Positioning section.
- `README.md` - Status section now leads with the reframe rather than the learned-index claim.
- Governing rule adopted: **no roadmap row without a named consumer and a measurement that consumer
  already takes.**

### Added - 2026-09-05

- `docs/research/` - the evidence base, eight files, every performance claim carrying a source URL
  and a date, with UNVERIFIED marked explicitly:
  - `demand.md` - the eleven-app survey. What each corpus is, how it searches today, what it
    measured about its own pain.
  - `relevance.md` - ranking and fuzzy-matching SOTA; the BM25F/fieldnorm argument that justifies
    writing our own scorer; the production typo-tolerance consensus.
  - `speed.md` - dynamic pruning, posting-list compression, and a ranked list of the ten
    highest-leverage techniques. Includes corrections to its own first draft.
  - `landscape.md` - the embedded/drop-in engine field, the sync problem, why Postgres FTS fails
    structurally, and five unfilled market gaps.
  - `portability.md` - WASM, browser storage, native bindings, edge runtimes, in-database
    extensions, and a ranked ten-item distribution strategy.
  - `business.md` - who makes money selling search, verified pricing, and five ranked positions.
  - `claim.md` - what practitioners say publicly, with a credibility verdict on each.
  - `build-or-buy.md` - crates.io maturity audit; the rule and the verdicts.
- `bench/roadmap/p5-fuzzy-term-feasibility.md` + `index-bench` bin **`fuzzy-term`** - a measured
  feasibility result for the typo-tolerant term dictionary, built on `tantivy-fst` +
  `levenshtein_automata`. At profstopick's corpus size (11,949 labels / 12,029 distinct tokens):
  **96,547-byte term dictionary (8.03 B/key), exact p99 760 ns, fuzzy p99 663 microseconds, and
  typo recall 135 -> 1,999 of 2,000 (14.8x).** Holds to 861 K distinct terms. Verdict: **FEASIBLE.**
- `index-bench` now depends on `tantivy-fst` and `levenshtein_automata` for that spike only.
  **`index-core` remains dependency-free.**

### Fixed - 2026-09-05

- **Two open Triage rows closed with data instead of opinion.** `pgm-extra` has **134 downloads in
  90 days** (498 all-time) and `pgm_index` has 80 - there is no production usage to inherit, so the
  build-vs-buy answer is *neither*. And the `seismic` crate that the relevance research recommended
  for learned-sparse retrieval has **16 downloads in 90 days and no publish since 2025-03-05**; the
  algorithm is real, the dependency is not.
- **A rejected-ideas entry was producing a wrong conclusion.** Rejecting *building* an FST (June
  2026) was correct; concluding that *typo tolerance* was therefore out of scope did not follow, and
  it cost the project its largest available win. `docs/roadmap-rejected.md` now carries a standing
  question for every rejection: does this scope out the PROBLEM, or only one SOLUTION to it?
- **The first version of the `fuzzy-term` bench measured a 115-token vocabulary** and reported a
  flattering 0.03 bytes/key. Caught only because the number was implausibly good; the corpus now
  carries a realistic long tail. A benchmark's corpus is part of its claim.
- **One hypothesis refuted and recorded rather than quietly dropped:** prefix-anchoring is *not* a
  speed lever for fuzzy lookup (-22.7% to +9.6% effect on p50, no consistent sign). First-character
  protection is a precision and typeahead rule, not a performance one, and the roadmap must not
  claim otherwise.

### Honest open-items carried forward

- **`fuzzy-term` exits non-zero on purpose** - two documented reds: exact p99 1,380 ns against a
  1,000 ns bar at 848 K terms, and 8.03 B/key against an 8.0 bar on the 12 K name corpus. Neither is
  to be silently relaxed.
- **The `fuzzy-term` corpus is synthetic.** profstopick's real shard is not committed; generating it
  needs a database. The headline number is representative, not actual.
- **Still no CI** (open since 2026-06-19), so "green gate" means green on one Windows box.
- **No customer has asked for this.** Positioning is derived from other people's published pain.
  Three of the eleven surveyed repos declined the product outright, in writing.
- Real SOSD 200M datasets still not downloaded, blocking the P13 research row.
- Minor test gaps carried over: no `FmIndex::locate` test for an absent pattern; no `PgmIndex` =
  `PlaIndex` result-equivalence test.


### Added — 2026-06-19 (initial build: P0, P1, P2)

P0 — learned ordered index
- `index-core`: `PlaIndex` — piecewise-linear learned index over sorted `u64` keys with bounded
  `±ε` prediction and bounded last-mile search. Default build uses the **optimal convex-hull PLA**
  (O'Rourke / PGM) with exact `i128` geometry; `build_greedy` kept as the comparison baseline.
- `PgmIndex` — recursive multi-level variant (PLA over segment-start keys).
- `index-core::data` — deterministic, dependency-free generators (`sequential`, `uniform`,
  `lognormal`, PLA-hostile `hard`, byte-`gen_text`) + SOSD binary loader (`load_sosd_u64`).
- `index-bench` bin `beat-btreemap` — bytes/key + p50/**p99** vs `std::BTreeMap`, `rdtsc` clock,
  median-of-5. Result: PLA wins space/p50/p99 on all four distributions at n=1M and n=10M (ε=16).

P1 — adaptive database cracking
- `CrackerColumn` — naive + stochastic cracking; `query(lo,hi)` partitions toward the workload.
- `index-bench` bin `crack-converge` — convergence under random/sequential/ends workloads.
  Result: naive **fails** sequential (cum/scan 2.48×), stochastic **passes** (0.01×); random
  converges 599×.

P2 — compressed full-text + fuzzy
- `FmIndex` — BWT + backward search: `count`, `locate` (sampled SA), and `fuzzy_kmismatch`.
- `wavelet` — `BitRank` (popcount-prefix bit-rank) + balanced `WaveletTree` (rank/access in
  O(log σ)), making the FM-index succinct: ~5.9 / 9.7 / 14.6 bits/char at σ=4/26/256.
- `index-bench` bin `fuzzy-decision` — fuzzy-over-FM viability vs k and σ. Decided: viable to
  k≤2 at σ=26, only k≤1 at σ=256 (exponential-in-k blowup confirmed).

Tooling / docs
- Rust GNU toolchain adopted (no MSVC linker on the build box); documented in README.
- `ROADMAP.md`, `docs/roadmap-rejected.md`, `bench/README.md`, and five `bench/roadmap/*` specs.

### Changed — 2026-06-19
- FM-index rank backend swapped from a 256-wide checkpoint Occ table (168 bits/char, *larger than
  the text*) to a wavelet tree (≈ entropy + overhead).
- `beat-btreemap` default ε set to 16 after a sweep showed ε=64 loses p99 on irregular data at 10M.
- Timer upgraded from `std::Instant` (~100 ns granularity) to `rdtsc` with overhead calibration.

### Fixed — 2026-06-19
- Data generators (`gen_uniform`, `gen_lognormal`) had an O(n²) generate-sort-dedup retry loop that
  hung at n=10M on skewed data; replaced with O(n log n) increment/bump generation.
- `WaveletTree::rank` returned garbage (and caused OOB in fuzzy search) for symbols outside the
  alphabet; added an out-of-range guard.

### Honest open-items (not done this batch)
- **No CI** — there is no `.github/workflows`; tests/benches run only locally. A CI gate (with the
  `bench/roadmap/` exclusion intact) is unscheduled.
- **q-gram filter+verify fuzzy fallback (P2)** — specced, not implemented; needed for k≥2 at large σ.
- **Real SOSD 200M datasets** not bundled; benches run on synthetic stand-ins (loader works on real
  files via `beat-btreemap <file>`).
- **FM-index rank `cum` overhead** — u32-per-word (~50%); a two-level rank would trim it.
- **Minor test gaps**: no `FmIndex::locate` test for an absent pattern; no `PgmIndex` ≡ `PlaIndex`
  result-equivalence test (PGM is covered for correctness, not cross-checked against PLA).
- **P3 (spatial/viewport) and P4 (federated)** not started.
