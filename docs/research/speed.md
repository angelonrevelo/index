# Speed — how the fastest retrieval systems actually get their speed

> Web research sweep, 2026-09-05, primary sources where reachable. `UNVERIFIED` = searched, not
> found. Several figures in the first draft of this sweep were **wrong and are corrected here**;
> the corrections are marked, because a research file that hides its own errors is worthless.

---

## 1. Dynamic pruning — the largest single lever, and it has a trap

**Block-Max WAND, the original numbers.** Ding & Suel, SIGIR 2011
([PDF](https://research.engineering.nyu.edu/~suel/papers/bmw.pdf)) — Gov2, 25.2 M docs, BM25,
k=10, single core, avg ms/query:

| | exhaustive OR | WAND | **BMW** | exhaustive AND |
|---|---|---|---|---|
| TREC 2006 | 225.7 | 77.6 | **27.9** | 11.4 |
| TREC 2005 | 369.3 | 64.4 | **21.2** | 6.86 |
| TREC 2006, docIDs reassigned | 210.6 | 50.1 | **8.89** | 6.56 |

**BMW is 8.1× over exhaustive OR, 23.7× after URL-lexicographic docID reassignment**, 2.8–3.0× over
WAND. The work-done table is the real story: exhaustive OR fully scores 3,815,676 documents; WAND
178,391 (4.6 %); **BMW 21,921 (0.57 %)**.

**Variable Block-Max WAND** (SIGIR 2017,
[PDF](https://pages.di.unipi.it/rossano/assets/pdf/papers/SIGIR17A.pdf)): C-VBMW40 is **~2× faster
than standard BMW128** *and* 3–5 % smaller. Gov2/Trec05 **2.1 ms vs 3.6 ms**; ClueWeb09 7.2 vs 12.6.

> **Budget the metadata.** The block-max side table is **15–42 % of the compressed index**
> (1.83 GiB on Gov2 at b=32). C-VBMW halves it. This is the cost nobody quotes alongside the
> speedup.

**Lucene's own gain is far smaller than the paper's**, because its exhaustive OR was already
block-optimized — and it decays hard with k
([ECIR 2020](https://cs.uwaterloo.ca/~jimmylin/publications/Grand_etal_ECIR2020.pdf)):

| ClueWeb09b, TREC05 | k=10 | k=100 | k=1000 |
|---|---|---|---|
| exhaustive OR | 331 ms | 371 | 521 |
| BMW | 96 | 137 | 370 |
| **speedup** | **3.4×** | 2.7× | **1.4×** |

*Correction to the first draft: "Lucene switched to MaxScore" was too strong.* Verified against
source at tag `releases/lucene/10.5.1` (2026-08-12): **both** `MaxScoreBulkScorer.java` and
`WANDScorer.java` exist — block-max MaxScore for top-level disjunction via `BulkScorer`,
`WANDScorer` for the general / min-should-match `Scorer` path. `INNER_WINDOW_SIZE = 4096`.

**Tantivy implements BMW** (`src/query/boolean_query/block_wand_union.rs`,
`block_wand_intersection.rs`, entry point `Weight::for_each_pruning`). Its "~2× faster than Lucene"
is a README claim only — **UNVERIFIED**.

**PISA vs Lucene head-to-head** ([arXiv 2110.11540](https://arxiv.org/pdf/2110.11540), 2021-10-28),
MS MARCO, k=1000: BM25 **Lucene 40.1 ms vs PISA 8.3 ms (4.8×)**; DeepImpact 244.1 vs 19.4
(**12.6×**); SPLADEv2 **2140.0 vs 220.3 (9.7×)**.

### The trap — pruning goes negative on dense queries

On SPLADE in PISA: **BMW 681 ms, exhaustive OR 553 ms, MaxScore 220 ms.** BMW works hard computing
skips that do not exist. Confirmed independently by BMP (SIGIR 2024,
[arXiv 2405.01117](https://arxiv.org/abs/2405.01117)): SPLADE k=10, **BMW 614.2 ms vs MaxScore
120.6 ms vs BMP 10.5 ms**.

> **Design consequence: implement MaxScore, not BMW.** No per-document heap sorting, better at
> large k, and it does not invert on dense query vectors. Lucene and PISA converged on it
> independently. BMW is the famous name; MaxScore is the safe default.

**The learned-sparse trajectory is ~3 orders of magnitude in two years** — PISA BMW ~100 ms →
MaxScore 75.7 → BMP 10.5 → **Seismic 187 µs** → Superblock Pruning 629 µs
([arXiv 2504.17045](https://arxiv.org/abs/2504.17045)) → Seismic v0.4.0 **185 µs** (2026-03-25). At
97 % accuracy Seismic scores **2,198 documents** where the graph baseline visits ~40,000; build time
**5 min vs 137–267 min**.

*Correction: the first draft called Seismic a "SIGIR 2024 best paper". The repo makes no such
claim and it could not be confirmed. Withdrawn.* And see
[`build-or-buy.md`](build-or-buy.md): the `seismic` crate has **16 downloads in 90 days** — the
algorithm is real, the dependency is not.

## 2. Posting-list compression — PEF and Slicing dominate the frontier

The authoritative table, Pibiri & Venturini, ACM CSUR
([arXiv 1908.10598](https://arxiv.org/abs/1908.10598)), i9-9900K, Gov2 = 5.3 B postings:

| Codec | bits/int | ns/int | Mint/s |
|---|---|---|---|
| BIC (interpolative) | **2.94** | 5.06 | 198 |
| **PEF (partitioned Elias-Fano)** | **3.12** | **0.76** | **1,316** |
| DINT | 3.53 | 1.13 | 885 |
| Opt-PFor | 3.63 | 1.38 | 725 |
| Opt-VByte | 3.89 | 0.73 | 1,370 |
| **Slicing** | 4.31 | **0.53** | **1,887** |
| QMX | 5.12 | 0.80 | 1,250 |
| Roaring | 6.63 | **0.50** | **2,000** |
| VByte | 8.81 | 0.96 | 1,042 |

**VByte is Pareto-dominated on both axes — do not ship it.** Roaring is the speed champion and the
space loser *for postings* (though best-in-class for bitmaps: 60 % of Concise's space on CENSUS1881;
Roaring+Run fixes sorted-data weakness at 0.34 vs 0.43 bits/value on WEATHERsort,
[arXiv 1603.06549](https://arxiv.org/abs/1603.06549)).

> **Non-obvious and directly actionable: prefix-sum costs 0.5 ns/int** and "sometimes dominates the
> cost of decoding the gaps." BIC, PEF, Roaring and Slicing avoid it entirely by storing *values*,
> not gaps. For any codec under ~1 ns/int, **half the decode bill is delta reconstruction, not
> unpacking.**

**Elias-Fano** costs at most `log₂(U/n) + 2` bits/element — under half a bit above the
information-theoretic bound — with O(1) access via select. Partitioning matters because EF and
bitmaps cross over with density (M=40, b=5: EF 5 bits vs bitmap 8; M=40, b=30: bitmap 1.33 vs EF 3);
PEF picks per partition by dynamic programming.

**SIMD-BP128** (Lemire & Boytsov, SPE 2015): GOV2 **2,500 Mint/s @ 7.4 bits/int** vectorized-delta
vs Simple-8b 780 Mint/s @ 4.6 — **> 3× faster decode**. **Stream VByte** (IPL 2018): **4.0 Bint/s**,
≥ 2.5× conventional VByte, at worst **70 % of `memcpy` speed** — and on compressible data it *beats*
memcpy because it reads fewer bytes.

**Newest, ECIR 2026 — DotVByte** ([arXiv 2602.05445](https://arxiv.org/abs/2602.05445), Rust, merged
into Seismic): **22 % space reduction at essentially uncompressed latency** (SPLADE 165 µs vs
uncompressed 157 µs @ 90 % recall), where ζ-codes are 5–8× slower.

**Recursive graph bisection docID reordering buys up to 27 % space and speeds decode** — a free
multiplier on any codec, and the source of BMW's 27.9 → 8.89 ms in the table above.

## 3. Where the wall actually is — memory, not algorithms

- DRAM random access **~10–60 ns**; a modern NVMe 4K random read **20–70 µs**
  ([simplyblock](https://simplyblock.io/glossary/nvme-latency/)) — **333–7,000× slower**. A cold
  10 ms budget buys roughly **150–500 random NVMe reads, total**.
- turbopuffer, same engine, same data, 1 M docs: **cold p50 874 ms vs warm p50 14 ms**; cold p90
  444 ms (vector) / 285 ms (BM25), warm p90 10 / 18 ms; **consistent reads have a ~10 ms floor**
  because S3 metadata is p50 10 ms ([architecture](https://turbopuffer.com/architecture)).
- Filters: Bloom/Cuckoo/Ribbon/Xor/binary-fuse buy **10–1000× on negative lookups** for a few
  bits/key.

## 4. Learned indexes in production — the 2026 verdict

Covered in full in [`claim.md`](claim.md) §1. In one line: **nobody has shipped one as a retrieval
algorithm**; MountDB (arXiv 2605.23815, May 2026) uses PGM as an *SST fence pointer* predicting a
block, and states plainly that "adoption in production systems remains limited." Updatable learned
indexes (ALEX, LIPP) beat ART/Masstree/HOT on > 80 % of the single-thread *data*-workload space, and
lose on writes and concurrency.

> **The one defensible open question left in this space**, and it is genuinely open: a
> **bounded-tail-latency updatable PLA benchmarked against `BTreeMap` and ART on the hard SOSD
> datasets (osm, genome)** is a more defensible contribution than another point on a QPS curve. The
> standing complaint in the literature is p99.9 under updates (VLDB 2022, still being papered at
> SIGMOD 2026) — and p99 measurement is the thing this repo already does better than the papers.
> That is a *research* row, not a product row, and the roadmap files it as such.

## 5. WASM

Expect **128-bit lanes and 85–95 % of native** for these kernels. Techniques 1, 2, 5, 7, 8, 9 in the
ranking below survive the port unchanged. (Full portability analysis in
[`portability.md`](portability.md).)

---

## Ranked — highest-leverage techniques for an embedded Rust engine at 1–10 M rows

| # | Technique | Expected speedup | Cost |
|---|---|---|---|
| 1 | Branchless + prefetched + batched PLA/fence lookup | 1.5–2.5× | 1–2 d |
| 2 | Zone maps / min-max block summaries | 10–100× on selective predicates | 2–3 d |
| **3** | **Top-k early termination via block-max MaxScore** | **3–8× (k=10); ~1.4× at k=1000** | 4–6 d |
| 4 | **PEF or Slicing postings codec** | 2–3× decode + 2.4× space vs VByte | 4–6 d |
| 5 | Binary fuse filter for negative lookups | 10–1000× on misses | 2 d |
| 6 | Vectorized execution, 2048-row batches | 10–100× vs row-at-a-time | 5–8 d |
| **7** | **Recursive graph bisection docID reordering** | **~3× on pruned queries, 27 % space** | **2–3 d** |
| 8 | 2 MB huge pages for the index arena | 1.15–1.35× | 0.5 d |
| 9 | Late materialization | 2–5× on wide rows | 3 d |
| 10 | SIMD predicates + `memchr`/Teddy | 4–16× on scan kernels | 3–5 d |

**#7 is the cheapest multiplier on the list** — pure preprocessing, zero query-path risk, and it is
what turns BMW's 27.9 ms into 8.89 ms.

## Withdrawn or unverified

Seismic's best-paper claim (**withdrawn**); Broder 2003 and Turtle & Flood 1995 original figures;
Lucene nightly absolute QPS (dashboard is JS, host DNS failure); Tantivy-vs-Lucene per-task numbers;
ANS-based index compression throughput; any > 10 Bint/s 2023–2026 SIMD claim. **Highest verified
decode throughput in this report remains Stream VByte at 4.0 Bint/s (2018).**
