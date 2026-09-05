# Build or buy — crates.io audit

> Measured 2026-09-05 against the crates.io API (downloads, latest stable, last publish date).
> Download counts are the only public maturity signal that cannot be spun, and `recent` (90-day)
> separates *alive* from *historically popular*. This file closes two open `ROADMAP.md` Triage
> rows with numbers instead of opinion.

## The rule

> Reimplementing something with **8-digit recent downloads** costs weeks and produces something
> worse. Reimplementing something with **3-digit recent downloads** costs weeks and produces
> something with a maintainer.
>
> `index` writes code only where a measured requirement is not met by an alive crate. Everywhere
> else it writes *integration*, and the integration is the product.

---

## Verdicts

### BUY — alive, load-bearing, do not reimplement

| Crate | Version | Recent (90 d) | Last publish | Role in `index` |
|---|---|---|---|---|
| `memchr` | 2.8.3 | **338,189,350** | 2026-07-08 | SIMD substring/byte search; the `teddy` multi-pattern backend |
| `aho-corasick` | 1.1.5 | **231,332,344** | 2026-08-03 | multi-pattern exact match (already a rejected-to-build row) |
| `unicode-normalization` | 0.1.25 | 94,044,927 | 2025-10-30 | NFKC/NFD — the base of the normalization layer all four apps hand-rolled |
| `rkyv` | 0.8.18 | 36,906,254 | 2026-08-05 | zero-copy index deserialization; the browser byte-budget lever |
| `roaring` | 0.11.5 | **10,490,499** | 2026-08-12 | compressed doc-id bitmaps for filters/facets; onegrid's `bitmapOp` kernel |
| `deunicode` | 1.6.2 | 13,224,828 | 2025-04-27 | ASCII transliteration fallback |
| `bitpacking` | 0.9.3 | 5,371,409 | 2026-01-08 | SIMD-BP128 posting-list compression |
| `daachorse` | 5.0.0 | 3,234,687 | 2026-08-08 | double-array Aho-Corasick, ~12 B/state — presyo already uses the JS equivalent for brand scanning |
| `strsim` | 0.11.1 | 213,736,424 | 2024-04-02 | edit distances for the verify stage |
| `fastembed` | 6.0.2 | 1,840,616 | 2026-08-27 | ONNX embedding runtime if a real model is ever needed |

### BUY — alive, and these are the interesting ones

| Crate | Version | Recent | Last publish | Why it matters here |
|---|---|---|---|---|
| **`sucds`** | 0.9.1 | **332,674** | **2026-08-29** | Succinct data structures — Elias-Fano, rank/select, **wavelet trees**. Actively maintained. **This overlaps `crates/index-core/src/wavelet.rs` directly.** Our hand-rolled `BitRank` uses a u32-per-word `cum` array (~50 % overhead, a known open item); a two-level rank is exactly what `sucds` ships. |
| **`vers-vecs`** | 1.10.2 | 93,006 | 2026-08-10 | Second actively-maintained succinct library. Gives a cross-check oracle for whichever we pick. |
| **`charabia`** | 0.10.0 | 258,076 | 2026-08-13 | **Meilisearch's own tokenizer.** Multilingual segmentation + normalization, the exact layer `demand.md` Finding 2 says four repos hand-rolled. Buying this and layering PH-specific rules on top is far cheaper than writing a tokenizer. |
| **`model2vec-rs`** | 0.2.1 | 104,293 | 2026-05-23 | Official Rust static-embedding runtime. Young (0.2.x) but alive and growing. The only CPU-viable dense arm ([`relevance.md`](relevance.md) §3). **Risk: pre-1.0, single upstream.** |
| **`whatlang`** / `lingua` | 0.18 / 1.8 | 930,617 / 582,280 | 2025-10 / 2026-03 | Language detection — needed to decide whether a query is Tagalog, English, or Taglish before choosing an analyzer. |
| **`pgrx`** | 0.19.2 | 490,093 | 2026-07-30 | The Postgres-extension path (what ParadeDB uses). Alive and versioned aggressively. |
| **`napi`** | 3.12.2 | 13,928,885 | 2026-08-21 | Node bindings; the path into presyo, sisia, profstopick, onegrid |
| `tantivy` | 0.26.1 | 3,795,280 | 2026-04-21 | The reference implementation and the oracle to measure against — **not necessarily a dependency** (see below) |

### BUY, BUT MIND THE DATE — stale yet dominant

| Crate | Version | Recent | Last publish | Assessment |
|---|---|---|---|---|
| **`fst`** | 0.4.7 | **5,928,891** | **2021-06-06** | Five years without a publish, six million downloads a quarter. This is *finished* software, not abandoned software — the FST format is a closed problem and BurntSushi's is the canonical one. **Still the right dependency** for the term dictionary (2.7 B/key, mmap'd, flat RSS). Risk is real but low: no publishes also means no breakage, and the code is vendorable. |
| **`tantivy-fst`** | 0.5.0 | 4,204,036 | 2023-11-21 | Tantivy's maintained fork of `fst`. **If we depend on one, this is the one** — it is the version with an active downstream consumer. |
| **`levenshtein_automata`** | 0.2.1 | 4,432,581 | 2021-05-18 | The Schulz–Mihov DFA builder that Tantivy uses for `FuzzyTermQuery`. Same profile as `fst`: stale, dominant, finished. Together with `tantivy-fst` this is the **entire typo-tolerance stack** from [`relevance.md`](relevance.md) §7, already written and battle-tested. |
| `rust-stemmers` | 1.2.0 | 9,200,332 | **2019-11-17** | Snowball stemmers. Alive by usage, dead by maintenance, and **has no Filipino stemmer** — nor does anything else, because Tagalog infixation/circumfixion/reduplication defeats Snowball's model. Use for English fields only. |
| `rapidfuzz` | 0.5.0 | 1,781,382 | 2023-12-01 | Fast edit-distance kernels. Fine as a verify-stage dependency. |

### DO NOT BUY — measured as effectively unused

| Crate | Version | Recent | Last publish | Verdict |
|---|---|---|---|---|
| **`pgm-extra`** | 1.3.0 | **134** | 2025-12-29 | **Closes the open Triage row.** 498 downloads *all-time*. There is no production usage to inherit and no maintainer community to fall back on. |
| **`pgm_index`** | 0.3.2 | **80** | 2025-08-15 | Same verdict. |
| **`seismic`** | 0.2.1 | **16** | 2025-03-05 | **Corrects [`relevance.md`](relevance.md) §2.** The research sweep reported "`cargo add seismic`" as the Rust learned-sparse path. The crate exists but has **16 downloads in 90 days and no publish since March 2025** — it is a paper artifact, not a dependency. Any learned-sparse work would mean adopting unmaintained research code. This strengthens the "no learned sparse in v1" call. |
| `symspell` | 0.5.2 | 23,221 | 2026-03-22 | Alive but unnecessary — `tantivy-fst` + `levenshtein_automata` cover the same ground with 190× the usage and better memory behaviour at our corpus sizes. |

---

## The two Triage rows this closes

**1. "Depend on `pgm-extra` vs build PGM from scratch."** *(ROADMAP.md → Triage)*

**Answer: neither.** `pgm-extra` has 134 recent downloads — there is nothing to inherit. And per
[`demand.md`](demand.md) Finding 0 and [`claim.md`](claim.md) §1, more PGM is not what any consumer
needs. The `PgmIndex` already in `crates/index-core/src/lib.rs` is retained as a *fence-pointer
component*, which is the only role the literature supports, and no further investment is scheduled.

**2. "Reinvent string fuzzy / term dictionary."** *(docs/roadmap-rejected.md — REJECTED)*

**The rejection was right and the conclusion drawn from it was wrong.** Rejecting *building* an FST
does not scope *typo tolerance* out of the project — it scopes it in, as integration.
`tantivy-fst` (4.2 M recent) + `levenshtein_automata` (4.4 M recent) are the exact stack
Meilisearch and Tantivy ship. The work is the **length gates, first-char protection, numeric-token
exemption, lazy firing, and typo-as-ranking-bucket** — i.e. the policy layer above the automaton,
which is where every production engine's actual behaviour lives and which no crate provides.

## The one place a from-scratch scorer is justified

Everything above says "buy". [`relevance.md`](relevance.md) §1 is the exception, and it is a
narrow, measurable one:

- Tantivy hardcodes `K1 = 1.2; B = 0.75` as module constants with **no public API** (issue #2924,
  open).
- Lucene and Tantivy quantize the field norm to **one byte**, so on 3–5-token product titles
  distinct lengths collapse to the same norm and the length signal is destroyed *before* `b` can
  act.
- BM25F and per-field `b` are mutually exclusive in Elasticsearch's `combined_fields`.

A scorer that stores **exact field lengths as `u16`** and implements **BM25F with blended IDF** is
a few hundred lines, is testable against Anserini's published defaults (k1 = 0.9, b = 0.4), and is
the *only* identified capability that no alive crate provides and that a measured workload
(presyo's 60-query fixture, dominated by short titles) demands.

**That is the build. Everything else is integration.**

## Re-audit trigger

Re-run this audit before promoting any roadmap tier, and specifically if:

- `fst` / `tantivy-fst` / `levenshtein_automata` recent downloads fall below ~1 M (signal that the
  ecosystem moved),
- `model2vec-rs` fails to reach 1.0 or stops publishing (it is the youngest load-bearing dep),
- `sucds` publishes a two-level rank that beats our `BitRank` on `bits_per_char` (then delete ours).
