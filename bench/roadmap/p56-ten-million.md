# P56 — 10 M: the capacity holds, the latency bar was never at a million

**Tier:** T1 · **Bin:** `real-million` · **Corpus:** presyo `raw_product` + `extraction_insight`
**Status: MEASURED, 2026-09-06. 8.29 M real documents index fine. The 5 ms bar holds to ~250 K, not
1 M and not 10 M.**

`p55` settled that the typo bar fails at a real million and corrected `p47` for having framed the
failure as a recombination artefact. It left the obvious next question: **is the engine unproven
above a million because it breaks there, or because nobody had the rows?**

## The rows

`p51` found presyo's `raw_product` (4,534,478) and `extraction_insight` (3,756,722) while sweeping
the estate. Together, **8,290,639 real product names** — one `psql | index build` pipe, 34 seconds
of transfer.

**10 M of real text does not exist on any of the five machines.** It is not manufactured here:
`p55` measured recombination overstating the typo tail by ~1.6x, so padding to a round number would
produce a worse answer than admitting the ladder stops at 8.29 M.

## Capacity: not the ceiling

| documents | terms | build | bytes | **B/doc** |
|---|---|---|---|---|
| 100,000 | 49,540 | 0.9 s | 10.6 MB | 106.4 |
| 1,000,000 | 151,866 | 9.0 s | 94.4 MB | 94.4 |
| 2,000,000 | 356,062 | 18.7 s | 183.7 MB | 91.8 |
| 4,000,000 | 1,234,225 | 44.4 s | 374.5 MB | 93.6 |
| **8,000,000** | **1,263,840** | **84.2 s** | **729.4 MB** | **91.2** |

**Nothing broke.** Build time is linear, and **bytes per document IMPROVE with scale** — 106.4 to
91.2 — because the dictionary amortises while postings grow. A 10 M index extrapolates to ~910 MB,
which is a number, not a wall.

That matters for `p68`'s tier specifically: 910 MB is far past what a tab will hold in memory, and
exactly what OPFS range reads are for. `js/opfs.html` opens an index by reading **296 bytes** to
learn its section table.

## Latency: this is the ceiling, and it is much lower than anyone said

| documents | typo p50 | **typo p99** | 5 ms bar |
|---|---|---|---|
| 100,000 | 329 us | **2,057 us** | **PASS** |
| **250,000** | 644 us | **5,183 us** | **just over** |
| 500,000 | 766 us | 5,601 us | FAIL |
| 750,000 | 901 us | 6,737 us | FAIL |
| 1,000,000 | 928 us | 6,947 us | FAIL |
| 2,000,000 | 1,204 us | 10,683 us | FAIL |
| 8,000,000 | 3,024 us | 33,619 us | FAIL |

> **The 5 ms typo-p99 bar holds to roughly 250,000 real documents.**

Two independent corpora now agree on that number: this ladder crosses between 100 K and 250 K, and
`p47` measured presyo's 241,677-product catalogue at **4.54 ms — PASS**. `p7` set the bar at a
million. **It was never achievable there**, on any corpus, and four documents spent effort on the
assumption that it nearly was.

**The p50 is the better news and has been ignored.** It stays under a millisecond to a million and
reaches only 3 ms at eight. What degrades is the tail.

## Where the tail actually comes from

Vocabulary, not documents. Across the ladder it grew **49,540 → 1,263,840 terms (25x)** for 80x the
documents, and the p99 tracks the vocabulary curve rather than the document curve — a 2x document
step from 250 K to 500 K costs only 8 % more tail, while the 2 M → 4 M step, where vocabulary jumps
3.5x, costs 68 %. That is `p5`'s finding at production scale: **fuzzy cost scales with vocabulary,
not corpus size.**

The expansion cap at 8 M, priced on real vocabulary:

| cap | typo p99 | top-10 identical | rank-1 identical |
|---|---|---|---|
| 16 (default, exact) | 32,501 us | 100 % | 100 % |
| 8 | 25,405 us | 98.90 % | 99.30 % |
| 4 | 23,678 us | 95.95 % | 97.35 % |
| 2 | 15,253 us | 90.10 % | 93.20 % |

Nothing approaches 5 ms, and at this scale even cap 2 — 10 % of top-10 answers changed — leaves
15 ms.

## The verdict, stated as a product number

**The engine indexes at least 8.29 M documents at ~91 B/doc and answers the median query in 3 ms.**
Its interactive typo bar holds to **~250 K documents**. Above that it still works and still returns
exact answers; it stops being sub-5-ms.

That is a real product statement, and it is the first one in this repo that names a *document count*
rather than defending a bar. `p7`'s 5 ms at 1 M stays red, but it should now be read as a target that
was set 4x too high rather than an engine that is 1.7x too slow.

## Still open

- **10 M unmeasured, and honestly so.** The estate holds 8.29 M rows of real text. Closing the gap
  means finding a real corpus, not padding this one.
- **No concurrency.** Every number here is single-threaded. A 33 ms tail at 8 M is one core; nothing
  in the engine currently shards a query across more.
- **The `to_bytes()` path materialises the whole artifact** — 729 MB in memory at 8 M. Fine on a
  workstation, and the reason the CLI streams rows in but not out.
