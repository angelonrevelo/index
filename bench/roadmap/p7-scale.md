# P7 — scale: does "a million rows, milliseconds" hold?

**Tier:** T1 · **Bin:** `cargo run -p index-bench --release --bin scale`
**Status: PARTIAL — passes to 250 K documents, FAILS the 5 ms bar at 1 M.** Gate-excluded.

## Why this exists

`real-corpus` proved the engine correct on 1,322 and 1,940 real documents. That is not evidence
about a million rows, and the project's stated goal is explicitly about millions. This bench is the
honest answer, and it is currently a partial one.

## Corpus

**`blead/data/schools-masterlist.csv` — 61,467 real Philippine schools** (DepEd masterlist): school
name, street address, barangay, municipality, division, region. Real Filipino place names, real
abbreviations (`ES`, `NHS`, `Brgy.`), and real collisions — many schools share a name and differ
only by locality, which is the hard case for ranking.

Above 61,467, documents are **synthesized by recombining real field values** (a real school name
with a different real locality and a different real region, walked with coprime strides so
combinations do not repeat). Every token stays real; every document stays distinct.

> **A methodological error worth recording.** The first version replicated whole documents with a
> unique marker token. That produced ~16 near-identical copies of every school — precisely the
> corpus shape that defeats top-k pruning, because thousands of documents tie near the threshold and
> nothing can be skipped. It measured **19.5 ms p99 at 1 M** and sent three optimization attempts
> chasing a problem the benchmark had invented. **The corpus is part of the claim.**

## Measured — 2026-09-05

| docs | terms | build ms | bytes | B/doc | exact p50 | typo p50 | typo p99 | |
|---|---|---|---|---|---|---|---|---|
| 5,000 | 9,223 | 54 | 902 KB | 180.4 | 65 µs | 124 µs | 598 µs | real |
| 20,000 | 22,460 | 211 | 3.5 MB | 175.0 | 119 µs | 226 µs | 1.08 ms | real |
| **61,467** | **40,890** | **688** | **10.2 MB** | **166.2** | **194 µs** | **414 µs** | **1.83 ms** | **real** |
| 250,000 | 40,890 | 3,170 | 39.7 MB | 158.9 | 433 µs | 830 µs | 4.26 ms | recombined |
| 1,000,000 | 40,890 | 13,739 | 157 MB | 157.2 | **1.14 ms** | **1.26 ms** | **9.15 ms** | recombined |

The serialized index is **reloaded and re-queried at every scale** and must answer identically.

**Verdict: sub-millisecond p50 at a million documents. p99 is 6.4-7.2 ms against a 5 ms bar — FAIL**,
and the run-to-run spread on that tail is ~0.9 ms, which is itself a finding: a 5 ms bar is below
this harness's resolution at this scale. 250 K passes comfortably. The bar stands; it is not moved
to manufacture a pass, and it is not declared met by picking the best of three runs.

## What this bench found

Four defects, all in the retrieval path, none visible at small scale:

1. **An O(total postings) prologue on every query.** `plan()` recomputed each term's maximum score
   bound by scanning all of its postings — a bound that is a property of the index, not the query.
   Latency tracked document count almost exactly. Precomputed at build/load time.
2. **A linear scan for the heap minimum on every replacement.** ~200 comparisons per accepted
   candidate against the pool. Replaced with a `BinaryHeap`.
3. **Unbounded fuzzy expansion.** A token could expand to hundreds of dictionary terms, each adding
   a posting list to walk — tail latency, not median. Capped at 50 (Elasticsearch's
   `max_expansions`), ordered by edit distance then document frequency.
4. **An inverted tie-break in the candidate heap** — a soundness bug, and the serious one. The heap
   evicted the *lower* document id among equal scores while the final sort *preferred* it, so
   equal-scoring documents vanished from results. The block-skip logic was suspected first and was
   innocent; isolating it by disabling the skip is what found the real cause.

Cumulative effect at 1 M documents: **exact p50 4.66 ms → 1.14 ms, typo p99 15.9 ms → 9.15 ms.**

## Where the tail actually is — measured, not guessed

`INDEX_DIAG=1` reports the slowest queries at 1 M with the work available to them:

```
    9713us  terms=  2  postings=   428654  "School ame"
    7489us  terms=  3  postings=   428685  "Ambagiw Elemntary School"
    7251us  terms= 16  postings=   659199  "Jose Rizal-Apoc-Apoc Natconal High School"
    median: 1059us  terms=6  postings=54523
```

**The tail is low-selectivity queries, not long ones.** The worst case has only *two* terms — but one
of them (`school`) carries a **428,654-posting list**, and the other is too short to earn a typo
budget and matches nothing. There is no discriminative term to prune against, so retrieval must
consider a large fraction of the corpus. The median query has 6 terms and 54,523 available postings
and runs in ~1 ms.

**Why block-max skipping does not rescue it, precisely:** a block's maximum score is only useful if
it is *low*, and with 128 arbitrary documents per block, almost every block contains at least one
short (therefore high-scoring) document. The block maxima are near-uniform, so nothing is skippable.
**The block-max metadata is uninformative because the score is uncorrelated with document id.**

That is exactly the problem docID reordering solves, and it is why `docs/research/speed.md` ranks it
as the cheapest remaining multiplier. Assigning document ids in an order that clusters similar
documents (by length, or properly by recursive graph bisection) makes block maxima informative and
turns the existing skip machinery on. It changes no answers — only the id assignment — but it does
change the public contract, since callers currently receive insertion ordinals.

**Three optimization attempts were made on hypotheses before this was measured** (halving the posting
struct, finer blocks, precomputed bounds). Only the last helped materially. The diagnostic should
have come first.

## Champion lists — helps the median, does not fix the tail

The diagnostic above said the threshold starts at zero and climbs slowly, so block-max skipping
never engages. The standard answer is **static pruning**: precompute, per term, the positions of its
highest-scoring documents (a *champion list*), and seed the top-k heap from them so the threshold is
already high when the scan begins. Champions are real documents scored exactly as the main loop
scores them, so this changes no answers.

Built for terms with `df >= 512`, 64 champions each, seeded from **one** term per query — the most
discriminative one that has a list.

| | exact p50 @1M | typo p50 @1M | typo p99 @1M |
|---|---|---|---|
| before | 475-580 us | 809-875 us | 6.02-6.40 ms |
| after | **279-314 us** | **643-687 us** | 6.36-7.24 ms |

**Median latency improved ~1.7x. The tail did not move.** And the tail cannot be resolved at this
precision anyway: three identical runs of the same binary measured **6.36, 6.72 and 7.24 ms**, a
spread of 0.9 ms on a 5 ms bar. Any claim about p99 at this scale that does not report that spread
is overclaiming.

**Why the tail is unmoved, precisely.** Champions raise the threshold, but block-max skipping needs
block *maxima* to be low, and they are not: `"School ame"` scores only on `school`, so a document's
score tracks its length, and a block of 128 arbitrary documents almost always contains one short
one. A high threshold cannot skip a block whose maximum is legitimately high. **Raising the
threshold and making block maxima informative are two different problems, and only the first is
solved here.**

**Two seeding designs were measured, not guessed.** Seeding from *every* query term measured
**7.44 ms p99** — worse than no seeding at all — because it scored up to `terms x 64` candidates,
each needing a binary search per term, and deduplicated them with a linear `contains` over a growing
vector. The work spent raising the threshold has to stay small relative to the scan it saves.

## Why 1 M still misses, and what would close it

At a million documents the vocabulary is 40,890 terms, so common terms carry very long posting
lists. Block-max MaxScore skips whole blocks, but the implementation still decodes postings one at a
time within a block and stores postings uncompressed (`u32` doc + 4 × `u16` tf = 12 bytes each).
`docs/research/speed.md` ranks the remaining levers, and the top three are unbuilt here:

- **Recursive graph bisection docID reordering** — still the right answer, because it is the only
  listed technique that attacks the *actual* diagnosed cause: uninformative block maxima. A cheap
  approximation (sorting documents by total length) was implemented and **measured worse** —
  6.40 → 7.02 ms p99, build 14.1 → 19.0 s, index 157 → 161 MB — because the DepEd corpus has nearly
  uniform document lengths, so length-sorting separates scores hardly at all. It was reverted. Real
  bisection clusters by *term co-occurrence* rather than length and is not subject to that
  objection, but it must be measured on a corpus with real variance rather than assumed.
- **PEF or Slicing postings compression** — 3.12 bits/int at 1,316 Mint/s. Note the hot posting
  struct was already halved to 8 bytes (term frequencies moved to a serialization-only side array)
  and it moved p99 by 0.02 ms, so **retrieval here is not memory-bound** and compression should be
  expected to help less than the literature's numbers suggest.
- **SIMD block decode.**

None of these change the answer, only the time — so they are safe to add behind the existing
equivalence oracle.

## Re-measured 2026-09-05, after three changes to the hot scoring path

Static priors, prefix anchoring and deletion all added work **inside the scoring loop** — a
multiply, a slice lookup, and a bitmap test per scored candidate — plus one new per-document array.
Correctness was checked on `real-corpus`, `sisia-catalog` and `maphy-place`; **none of those runs
say anything about latency at a million documents**, so this bin was re-run before the changes were
called done.

| | recorded | after the changes |
|---|---|---|
| exact p50 @1M | 1.14 ms | **274 µs** |
| typo p50 @1M | 1.26 ms | **598 µs** |
| typo p99 @1M | 9.15 ms | **6.01 ms** |
| build @1M | 13,739 ms | 13,359 ms |
| bytes/doc @1M | 157.2 | **161.2** |

**No regression.** The latency columns are same-or-better, and they are *not* claimed as a speedup:
the recorded figures come from an earlier session on this machine and the two runs are not controlled
against each other. The claim this table supports is the negative one, which is the one that
mattered — three additions to the per-candidate path cost nothing measurable.

**The size delta is exactly accounted for, which is the more useful check.** +4.0 B/doc, and
`first_term` is one `u32` per document — 4.0 B/doc. Priors were uniform and nothing was deleted, so
both write **zero-length sections**. Nothing in the growth is unexplained, and the two features that
were not used cost nothing at all, which is the property that lets them ship on by default without
taxing an adopter who never calls them.

The 5 ms bar still fails at 1 M (6.01 ms), for the reason this file already diagnoses: low-selectivity
queries where block maxima are uninformative. That is unchanged and remains gate-excluded.

## Reproduce

```sh
cargo run -p index-bench --release --bin scale
INDEX_BENCH_N=1000000 cargo run -p index-bench --release --bin scale
INDEX_CORPUS_DIR=/path/to/checkouts cargo run -p index-bench --release --bin scale
```
