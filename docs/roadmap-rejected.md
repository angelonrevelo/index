# Roadmap — rejected ideas (idempotency anchor)

Read this **first** on every `/roadmap` run. Anything here was killed with a verbatim reason; do not re-surface it without new evidence that overturns the reason.

---

## 2026-06-19 — initial greenfield validation sweep

**Vectors as a physical ordered-key range scan** (i.e. flatten embeddings to a Hilbert/Z/SFC key and answer ANN as a key-range scan).
→ REJECTED. Killed three independent ways: (1) SFC locality collapses past ~8–10 dimensions for information-theoretic reasons — adjacent points in N-D land arbitrarily far on the curve; no clever curve fixes it. (2) RaBitQ and friends are distance *estimators* used inside IVF, not order-preserving keys — sorting by them craters recall. (3) The "How good are multi-dimensional learned indices?" survey shows learned MD indices lose to plain R-trees beyond ~10D. The field's SOTA for vector+range (UNIFY, VLDB) keeps vectors in a proximity graph and treats range as a filter — it explicitly does NOT flatten to one key.

**Viewport as a universal *physical* range key across all modalities.**
→ REJECTED as stated; REFRAMED to a *logical pre-filter envelope* (P3). A viewport is a coarse bounding region that becomes a range pre-filter feeding whichever engine owns the modality — not a single physical key spanning vectors+text+spatial.

**Naive database cracking** (reorganize the array on every range query, no randomization).
→ REJECTED. Two fatal properties: (a) it physically reorders the array *on read queries*, turning every SELECT into a writer — brutal under concurrency; (b) it converges well only on near-random workloads and degrades on *sequential/clustered* queries, which are the natural real-world pattern. ~18 years with effectively no mainstream productization is itself a signal. Only **stochastic / forced-randomization** cracking, with a benchmark proving it never does worse than a full scan on adversarial input, is on the roadmap (P1).

**Build our own string / term-dictionary fuzzy index.**
→ REJECTED. BurntSushi's `fst` crate already does fuzzy-term lookup (Levenshtein automaton × FST, ed≤2) over a compressed structure, battle-tested at 1.6B keys. Learned indexes are independently documented to lose on string keys (Tsinghua Finding 7). Use `fst`/`tantivy-fst`; scope strings out of the learned core.

**Build our own multi-pattern exact matcher.**
→ REJECTED. `aho-corasick` (contiguous-NFA, cache-tuned) and `daachorse` (double-array, 3-5× faster, ~12 bytes/state) already exist and are well past what a from-scratch impl would reach. Depend on them.

**Ship the RMI artifact as a runtime component.**
→ REJECTED as a component. RMI is static (no updates) and its headline speed depends on an expensive per-dataset hyperparameter search (CDFShop exists precisely because hand-tuning is infeasible) — disqualifying for an adaptive, updatable core. Allowed ONLY as a static speed-ceiling baseline in benchmarks.

---

## 2026-09-05 — demand-led re-baseline sweep

Evidence for every entry is in [`docs/research/`](research/). Eleven repos surveyed by source
inspection, six web-research lanes, one cross-model X/practitioner sweep, one crates.io audit.

**A standing correction to how this file is read.** The 2026-06-19 entry "Build our own string /
term-dictionary fuzzy index" was **correct to reject building an FST** and was then used to conclude
that *typo tolerance* was out of scope. That does not follow, and the mistake cost the project its
largest available win for nine months. From now on every rejection must answer:
**does this scope out the PROBLEM, or only one SOLUTION to it?**

**Depending on `pgm-extra` or `pgm_index`.**
→ REJECTED, and this closes the long-open Triage row. `pgm-extra` has **134 downloads in the last 90
days** (498 all-time); `pgm_index` has 80. There is no production usage to inherit and no maintainer
community to fall back on. Combined with Finding 0 — no consumer has a `u64 → position` workload —
the answer to "depend or build" is **neither**.

**Adopting the `seismic` crate for learned-sparse retrieval.**
→ REJECTED. The *algorithm* is real and excellent (185 µs/query at MRR@10 40.27 on splade-v3). The
*dependency* is not: **16 downloads in 90 days, no publish since 2025-03-05.** Adopting it means
adopting unmaintained research code. Revisit only via an inference-free-query variant.

**Learned sparse (SPLADE et al.) in v1.**
→ REJECTED for v1. Real gains on e-commerce (+27.5 % nDCG@10 on Amazon ESCI vs BM25) but it needs a
~100 M-parameter query encoder in-process and ~4.7× index blowup. The one thing that could displace
the lexical arm is an **inference-free query side** (opensearch-doc-v3-gte, 54.6 BEIR, zero
query-side model) — that specific combination is the reopening condition.

**A sidecar process (local daemon over HTTP/gRPC/unix socket).**
→ REJECTED, and the usual reason is wrong. The transport is cheap: unix-domain-socket RTT is
**7.7 µs**, 1–3 % of a sub-millisecond query. The **codec** is the cost — `serde_json` deserialize of
a 6 MB payload is **103.59 ms** against **5.6 ns** for an rkyv archive read. And the two engines
worth running as sidecars **cannot be embedded at all**: Meilisearch's real `milli` carries
`publish = false` (crates.io `milli` is a 0.1.0 placeholder from 2020-07-15), and Typesense/Qdrant
are server-only by architecture. Their footprints settle it — Meilisearch **305 MB RSS for a 9.1 MB
corpus**, Typesense a **380 MB image at 2–3× searchable-field bytes, hard-resident**. presyo's box
already runs Node, Nginx and a 15 GB Postgres.

**An mmap-first index format.**
→ REJECTED as the base assumption; allowed only as a native optimization layered on range reads.
**wasi-libc's `mmap` is a fake** — its own source comment: *"It just allocates memory with malloc and
reads and writes data with pread and pwrite."* Emscripten does the same, and the browser cannot have
mmap by design (no MMU exposure). So an mmap-based index **does not fail to port to WASM; it
silently reads the entire index into linear memory with no way to opt out** — the worst failure mode
available. Lance is the proof of the alternative: exactly one `mmap` reference in the whole repo, in
test-data generation.

**`memory64` / wasm64.**
→ REJECTED. Costs **10 % to over 100 %** (wasm32 on a 64-bit host elides bounds checks via a 4 GiB
guard reservation) and is **absent from Safari entirely**. Stay on wasm32 and shard — but use `u64`
on-disk offsets, because 32-bit `usize` was the named blocker that killed Tantivy's browser PR.

**A Vercel Edge artifact.** → REJECTED. 1/2/4 MB gzipped by plan, and `runtime = 'edge'` is
**unsupported from Next.js 16.3**.

**A Fastly Compute artifact.** → REJECTED. 50 ms CPU per request and **a fresh sandbox per individual
request** — no amortization of index load, which is the entire economics of an embedded index.
CloudFront Functions (10 KB, no WASM, no network) is categorically out.

**A WASM Component / WIT distribution.**
→ REJECTED for now. The Component Model is **Phase 1** — a Bytecode Alliance convention, not a
browser feature; no browser loads components natively. **`cargo-component`'s last release was
2025-04-07 — 17 months stale.**

**`wasm-bindgen` for the browser binding.**
→ REJECTED. Every `String`/`Vec<u8>` crossing is an O(n) copy plus allocation. A hand-written
raw-pointer C ABI — which onegrid has already ratified as `ACCEL_ABI_VERSION = 1` — generalizes
unchanged from WASM to native FFI, and is one artifact instead of two.

**Block-Max WAND as the top-k algorithm.**
→ REJECTED in favour of **block-max MaxScore**. BMW is the famous name and is excellent on sparse
queries (8.1× over exhaustive OR, 23.7× after docID reassignment), but it **inverts on dense query
vectors**: on SPLADE in PISA, BMW 681 ms vs exhaustive OR 553 ms vs **MaxScore 220 ms**. MaxScore
needs no per-document heap sorting, degrades better at large k, and Lucene and PISA converged on it
independently.

**VByte posting-list compression.** → REJECTED. Pareto-dominated on both axes: 8.81 bits/int at
1,042 Mint/s, against PEF's 3.12 bits/int at 1,316 Mint/s.

**Training an entity-matching model (Ditto / RoBERTa / R-SupCon).**
→ REJECTED. The WDC Products benchmark shows fine-tuned matchers fall off a cliff on unseen
entities (Ditto 79.16 seen → 70.24 unseen; R-SupCon 81.88 → 57.23; under cross-dataset transfer
Ditto collapses to 48.74), while **zero-shot GPT-4 holds 89.61**. A grocery catalogue with weekly new
SKUs is *permanently* in the unseen regime — which is exactly the in-sample-100 % → out-of-sample-82 %
correction presyo already recorded against itself. At 0.006 ¢/pair, adjudicating 100 K pairs costs
about $6. **The engineering is blocking; the matcher is a solved economic problem.**

**Counterfactual / unbiased LTR before there is traffic.**
→ REJECTED for now. SIGIR'24 reproducibility (Baidu-ULTR) found ULTR methods **robustly improve
click prediction but do not consistently improve ranking**, with results dominated by choice of
ranking loss rather than the debiasing method.

**Full-catalogue phonetic matching (Soundex / Metaphone).**
→ REJECTED at catalogue scale, ACCEPTED for the brand dictionary only (< 50 KB). Filipino
orthography is phonemic, so users spell English brands by ear (`kolgate`, `nesle`, `kokakola`) —
phonetic substitutions that edit distance ≥ 2 rejects. Applied to the full catalogue it is far too
lossy to use as a general recall device.

**Fuzzy-matching numeric tokens.**
→ REJECTED, permanently, as a correctness rule. `300g` matching `800g` is a correctness bug in a
price-comparison app that presents as a relevance bug. This mirrors Algolia's
`allowTyposOnNumericTokens`, which must be off.

**Building our own tokenizer.**
→ REJECTED. `charabia` (Meilisearch's, 258 K recent downloads, published 2026-08-13) does
multilingual segmentation. The PH-specific rules layer on top; that layer is the contribution.

**Spatial / viewport (old P3) and federated multi-modal (old P4) as scheduled tiers.**
→ DEFERRED, not rejected. No repo in the survey has a spatial-range or filtered-ANN workload its
current stack fails at; maphy serves 41,966 barangays from a 1.2 MB gzipped index. They return when
a consumer measures a failure. `bench/roadmap/p3-sfc-dimensionality.md` stays on disk because it
settles the question cheaply if the topic reopens.

---

## 2026-09-06 — image-tier sweep

Evidence for every entry: [`docs/research/image.md`](research/image.md).

**Writing an image codec, or any lossless compressor for pixels.**
→ REJECTED, permanently. "Byte-exact but smaller" is lossless compression and is bounded by
information theory. An already-JPEG corpus is already entropy-coded — zstd and brotli win **1–3%**
on it. The measured ceiling is JPEG XL's lossless JPEG transcode at **~20%** (13–22%), which
**fails outright on ~1% of real JPEGs**, and libjxl already ships it under BSD-3. Everyone who
attempted this independently is dead or absorbed: **Lepton** archived 2023-02-14, **PackJPG** last
released 2016-01-22, **brunsli** survives only as JXL's internal transport, **FLIF/FUIF** folded
into JPEG XL. Neural lossless compression (L3C, CALLIC, FLLIC) is GPU-only with no deployable
decoder. Meanwhile **~30% of a scraped corpus is duplicate bytes**, so dedup beats the codec by a
wide margin. The engine ships the SHA-256 content address that *proves* a round-trip was 1:1 and
leaves the transcode to the host.

**Taking a libjxl dependency.**
→ REJECTED. There is no pure-Rust JXL *encoder*; adopting it would put a C++ toolchain inside a
crate whose entire thesis is that it has none. Also moot for browser delivery: Chrome 145 merged a
Rust decoder but it is **still behind a flag as of Chrome 151**, Firefox 152 ships it **disabled by
default**, and only Safari 17+ is on by default (without progressive decode).

**An HNSW / IVF / any ANN graph index.**
→ REJECTED at this scale, DEFERRED above it. Measured: brute force over **1 M × 384-d runs at 79.7
QPS single-thread** (12 ms) on a laptop. 30 k images at 512-d is **1.9 MB as binary codes** — a
popcount scan over that is sub-millisecond. A graph costs build time, memory, recall and mutability
and buys nothing below roughly 1 M vectors, and it would break the no-rebuild live update
`searcher` already has. Returns as its own row, with its own evidence, if a corpus crosses that
line — not pre-emptively.

**Bundling an embedding model, an ONNX runtime, or an image decoder in `index-image`.**
→ REJECTED. The licence spread is a minefield that moves yearly: SigLIP and DINOv2 are Apache-2.0,
**MobileCLIP2 is an Apple sample licence**, **Jina CLIP v2 is CC BY-NC 4.0**, **DINOv3 is a bespoke
gated Meta licence**. Keeping the model out is what preserves the crate's near-zero dependency
footprint, its WASM shippability, and its MIT OR Apache-2.0 licence against seven AGPL incumbents.
An embedding is an **input**. The cost is stated rather than hidden: the crate cannot embed an image
for you.

**Bundling any face detection or recognition weight.**
→ REJECTED, permanently. **InsightFace's code is MIT but its weights (`buffalo_l`, `antelopev2`,
ArcFace, AdaFace's official checkpoints) are research-only and require a separate paid commercial
licence** — the MIT badge does not cover the weights, and this is the most common mistake in the
field. Ultralytics YOLOv8/v11-face is AGPL-3.0. Separately, EU AI Act **Art. 5(1)(e)** absolutely
prohibits building facial-recognition databases by untargeted scraping (no proportionality test, no
exception), every European Clearview decision rejects the "publicly available photos" defence, and
the UK Upper Tribunal named **clustering facial vectors** as the triggering processing step. Google
paid **$100 M** under BIPA for face-grouping inside users' own libraries. `p55` ships the clustering
arithmetic over caller-supplied vectors and nothing else.

**Claiming PDQ compatibility for the 256-bit hash.**
→ REJECTED as a claim. The implementation is PDQ-*shaped* — same construction, not bit-identical
(Meta additionally applies a Jarosz box blur and tent-filter decimation). A code from it cannot be
matched against a ThreatExchange blocklist: comparison would sit at the 128/256 chance level and
silently find nothing, which is worse than not offering it. Documented as incompatible rather than
approximately compatible.

**Frame-by-frame video indexing.**
→ REJECTED as framing, goal kept as `p54`. Keyframe-aware selection uses **63–99% fewer frames than
uniform sampling** and scores **better** (R@1 63.9% on MSR-VTT); retrieval accuracy flattens past
**2 fps**. A 10-minute video is ~18,000 frames and ~120–170 shots. Sampling one frame per shot is a
~100x reduction the literature says costs nothing.

**Bundling an H.264 or HEVC decoder.**
→ REJECTED on patent grounds. A BSD-2 licence on a pure-Rust H.264 decoder does **not** remove the
patent obligation — patents cover the technique, not the implementation. HEVC runs ~$2.07/unit
across the pools. AV1 is royalty-free by AOMedia commitment; a shipped product decodes through the
OS/browser (`WebCodecs`, ~95.5% coverage) or AV1.

**Error Level Analysis and PRNU as ingest-tier signals.**
→ REJECTED for bulk ingest. Standalone ELA is not a reliable "edited: yes/no" test — the recent
literature only reports gains when it is fed as one feature into a CNN, which concedes the point.
PRNU costs **seconds per image** plus a reference set per candidate camera, and is destroyed by the
resize/recompress cycles every social-sourced image has already been through. Both are lab
forensics, not a cheap tier.

**Asserting a single near-duplicate rate for a corpus.**
→ REJECTED as a reporting practice. Published methods span **3% to 37%** on comparable web corpora
— a 12x spread — so any single headline number is a methodology choice wearing the costume of a
fact. `p52` prints the count at every threshold, with the threshold beside it.

**Exact facet counts beside an active vector arm, as a differentiator (`p66`).**
→ **REJECTED 2026-09-06, the same day the row was written.** The row was created after retracting
`p59`'s novelty claim, as the one property that appeared to survive: every purpose-built vector
engine degrades facet counts when a vector arm is active (Vespa documents *"Grouping counts are not
accurate when using nearestNeighbor"*; Elasticsearch aggregations collapse to top-`k`; LanceDB
refuses `limit`/`offset` with an aggregate). It was written with an acceptance item requiring the
comparison class be verified against SQL engines **before** any claim. That check killed it.

**Plain SQL has the property for free.** The standard computes aggregates at step 4 and `LIMIT` at
step 9, so a `GROUP BY` is already exact over the full `WHERE`-filtered set. And pgvector's HNSW
index is consulted *only* for an `ORDER BY <distance> LIMIT k` branch, so an aggregate branch is
never routed through it — **`hnsw.ef_search` truncation cannot reach the counts.** Postgres
satisfies it in both exact and HNSW mode; DuckDB VSS, sqlite-vec and ClickHouse follow structurally.
**Solr satisfies it as well**, in the rerank configuration, and that is a search engine rather than a
database.

The engines that degrade do so because their faceting is defined over the **ANN result set** instead
of a predicate match set — a product-category design choice, not a law. Only a *performance* claim
survives (one fused pass vs a two-branch plan), and it is not worth a row until measured against
Postgres in both modes; `WITH ... AS MATERIALIZED` arguably makes the SQL plan one pass too.

**The pattern is the finding, and it is recorded here deliberately.** Three separate novelty claims
in the image tier were probed against the right comparison class, and all three collapsed to the
same residue — **embeddable, dependency-free, MIT/Apache, browser-capable**. The fusion query model
(`p59`) fell to Vespa and Lucene. The perceptual-hash predicate fell to Vespa, Elasticsearch and
LanceDB, which all ship Hamming distance on binary vectors. Facet counts under a vector arm (`p66`)
fell to Postgres and Solr. **The engineering in this tier is sound; the novelty was not there, and
this file is where that is written down rather than discovered later by a reviewer.**
