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
