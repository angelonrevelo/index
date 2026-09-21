# P93 — TIN's published workload, at the size this machine holds: ~10x behind on counts, phrase is the tail

**Tier:** T1 · **Check:** `cargo test -p index-text` · **Files:** `crates/index-bench/src/tin_shape.rs`,
`crates/index-text/src/index.rs` (`count_any`, `count_all`), `scripts/tin-corpus.py`
**Status: MEASURED, 2026-09-17, on 1/11 and 1/4.3 of TIN's 8.0 GB Wikipedia corpus. No like-for-like
row exists: the full corpus needs ~28 GB peak to build and this Mac has 24 GB.**

## What TIN published

PlanetScale's TIN (2026-09-16, `planetscale.com/blog/introducing-tin`): Postgres 18.6 in a container
limited to 8 vCPU / 32 GB on an i7i.8xlarge, ParadeDB's Benchmarker, 1,719 queries = substrings of
2–15 terms sampled from the corpus, each run as conjunction, disjunction and phrase.

| TIN workload | corpus | QPS | p99 |
|---|---|---|---|
| mixed; top-10 | Stack Exchange 85 GB | 199 | 256 ms |
| conjunction+phrase; top-10 | Stack Exchange 85 GB | 242 | 212 ms |
| disjunction; top-10 | Stack Exchange 85 GB | 148 | 324 ms |
| conjunction+disjunction+phrase; COUNT | Stack Exchange 85 GB | 179 | 438 ms |
| **disjunction; COUNT** | **Wikipedia 8.0 GB** | **10,260** | **2 ms** |

## What was built

- **`Index::count_any` / `count_all`** — exact `COUNT(*)` of documents holding any / every token, no
  typo expansion, deletions honoured. Union is a merge when sparse and a corpus bitmap when dense;
  intersection walks the rarest list and forward-binary-searches the rest. Pinned against a
  brute-force set over 600 random documents with deletions.
- **`tin-shape`** — their recipe: reservoir-sampled 2–15-token substrings (573 × 3 = 1,719), one
  warm-up pass, then N client threads looping for 30 s per workload; QPS, p50, p99, per-kind p99 and
  zero-result rate.

## Results — Apple M5 (4P + 6E), 8 threads, Wikipedia `20231101.en`

| workload | TIN 8.0 GB / 85 GB | **0.71 GB** (156,289 docs) | **1.84 GB** (468,867 docs) |
|---|---|---|---|
| mixed; top-10 | 199 QPS, 256 ms | 939 QPS, 42.2 ms | 499 QPS, 81.4 ms |
| conjunction+phrase; top-10 | 242 QPS, 212 ms | 847 QPS, 36.3 ms | 363 QPS, 86.4 ms |
| disjunction; top-10 | 148 QPS, 324 ms | 3,800 QPS, 10.4 ms | 1,756 QPS, 24.5 ms |
| conjunction+disjunction; COUNT | — | 13,815 QPS, 3.9 ms | 5,667 QPS, 8.4 ms |
| **disjunction; COUNT** | **10,260 QPS, 2 ms** | 9,925 QPS, 2.4 ms | 3,774 QPS, 6.0 ms |
| build / index / peak RSS | 8m10s, 50.7 GB (SE) | 20.1 s, 361.8 MB, 4.22 GB | 65.5 s, 949.7 MB, 6.54 GB |

Per kind at 1.84 GB, mixed top-10: conjunction p99 22.7 ms, disjunction 22.6 ms, **phrase 95.4 ms**.
Zero-result rate 0 % everywhere (every query is a substring of the corpus). 4 threads at 0.71 GB:
mixed 798 QPS / 22.9 ms, disjunction COUNT 5,834 QPS / 1.7 ms — the E-cores add throughput and cost
tail. Raw runs: `bench/runs/2026-09-17-tin-shape/`.

## Read it this way

- **The only comparable row is the Wikipedia COUNT, and it is not yet comparable.** At 1/11 of the
  corpus this engine matches TIN's QPS; 0.71 → 1.84 GB (2.6x) cost 2.6x QPS and 2.55x p99, i.e.
  **linear in postings**. Extrapolated to 8.0 GB (unmeasured): ~870 QPS, ~26 ms p99 — **~12x behind
  TIN**. That is the architecture TIN describes: they AND/OR page bitmaps and read exact per-term
  counts; this walks every posting of every term.
- **The top-10 rows are not comparable at all.** TIN's are Stack Exchange at 85 GB with the index
  1.6x RAM, through Postgres; these are 46x–120x less text, resident, in process. They say where the
  cost is, not who is faster.
- **Phrase is the tail**: 4x the conjunction p99 at both sizes. It is a filter over BM25F candidates
  with per-posting position checks, so a common-word phrase scores many documents that fail it.
- **Memory is the ceiling on this machine.** Peak RSS 6.0x → 3.5x the corpus; at 8.0 GB that is
  ~28 GB (extrapolated) on a 24 GB Mac already 6.9 GB into swap.

## Not done

Stack Exchange corpus, RAM-capped cold-read runs, the ParadeDB bridge under matching Docker limits,
concurrent updates. Phrase COUNT is wired in `tin-shape` (`Index::count_phrase`); it is still the
tail (20k-doc overlapping proxy: disjunction COUNT 566k QPS / 0.01 ms p99, mixed COUNT with phrase
2.9k QPS / 6.3 ms p99). That proxy is not the 8.0 GB Wikipedia bar.

```sh
hf download wikimedia/wikipedia --repo-type dataset \
  --include "20231101.en/train-0000[0-2]-of-00041.parquet" --local-dir data/
python3 scripts/tin-corpus.py data/20231101.en/*.parquet > wiki.tsv
INDEX_TIN_TSV=wiki.tsv cargo run -p index-bench --release --bin tin-shape
```
