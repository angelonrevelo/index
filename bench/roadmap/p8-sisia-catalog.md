# P8 — sisia-app's catalog search, on sisia-app's own data

**Tier:** T1 · **Bin:** `cargo run -p index-bench --release --bin sisia-catalog`
**Status: PASSING, 2026-09-05** — with one honest non-win recorded below.

## sisia was written off too early

`docs/integration.md` said sisia-app was unreachable: its catalog is a gitignored `sisia.db` on a
VPS, and its hybrid retrieval needs Postgres + pgvector + a Gemini key. That was half right and half
an oversight.

The corpus this repo has been benchmarking since the beginning —
`profstopick/research-pack/ateneo-professor-course.json` — declares in its own metadata:

```json
"source": "sisia class_section_all"
```

**It is sisia's registrar table**, exported through profstopick's research pack. 1,322 instructors
and **2,253 distinct course-code × title pairs** of real sisia data, on disk the whole time.

## The defect under test

`sisia-app/apps/api/src/models/Course.ts:541`, in its own words:

> *"was matched with `LIKE 'CHEM 10%'`, which bled into `CHEM 107` (a different course)"*

Its catalog search is `c.course_code LIKE ?` plus `LOWER(c.title) LIKE ?` with `%` wildcards,
ordered by course code, with **no relevance ranking** — SQLite, no FTS5. That baseline is reproduced
here rather than strawmanned, exactly as `real-corpus` reproduces profstopick's shipped matcher.

This corpus contains **155 codes that are a strict prefix of another code**, so the case is real.

## Measured — 2026-09-05

```
2,253 distinct course_code x title pairs · 2,525 terms · dict 16,204 B · build 4 ms
155 of them are a strict PREFIX of another code

                              engine        sisia's shipped LIKE
  exact code   hit@1         100.0%      100.0%
  PREFIX-BLEED clean           0.0%        0.0%
  PREFIX-BLEED extra rows      1391         295
  title words  hit@10         91.2%        0.0%   (2,038 queries, words reversed)
  latency  p50 2,570 ns   p99 5,150 ns
```

## The win: out-of-order title words, 91.2 % vs 0.0 %

A student types the words they remember, in the order they remember them. `LOWER(title) LIKE '%q%'`
matches **one contiguous substring**, so a two-word query in the wrong order matches nothing — it
scores **0.0 %** across 2,038 real course titles, and no amount of tuning changes that, because the
limitation is the operator rather than a parameter.

This is the same class of failure sisia already documented elsewhere in its own stack:
`driveHybridSearch.ts:88-93` records that `websearch_to_tsquery`'s AND semantics *"was too
restrictive (queries like 'math 31.2 long test' matched almost nothing)"*, forcing a switch to OR —
which then over-matched and required the subject-gate hack at `:101-106`. Term-level matching with
per-field scoring is the thing that removes both problems at once.

## The non-win, kept rather than deleted

**On the prefix-bleed set the engine is not better, and two attempts to construct a metric where it
was are recorded here.**

The first measured **rank**: both reach 100 % exact-code hit@1, because `ORDER BY course_code` sorts
`CHEM 399.1` above `CHEM 399.11` — the shorter code wins the tie by lexicographic luck. sisia's bug
does not manifest as a ranking failure on this corpus.

The second measured **extra rows returned**, and the engine came out worse: 1,391 unrequested courses
against the baseline's 295. That is not a defect either — the engine *ranks* where `LIKE` *filters*,
so it fills the remaining slots of a top-10 with related courses, which is what a search box should
do. **The metric was measuring recall and calling it imprecision.**

The honest conclusion: **for exact course-code lookup, sisia's `LIKE` is adequate on this corpus.**
The engine's value to sisia is the title-word case, and that is what the pass criterion tests.

## What this still does not cover

sisia's *hybrid* retrieval — the `ts_rank_cd` sparse arm fused with a pgvector dense arm by RRF
k=60, plus the Vertex cross-encoder — is untouched. That path needs Postgres with pgvector, the
`drive_content` corpus, and a Gemini key, none of which are on this machine. The engine's sparse arm
is shaped to drop into it (`query → (id, rank)[]`), but that remains asserted rather than measured.

## Reproduce

```sh
cargo run -p index-bench --release --bin sisia-catalog
INDEX_CORPUS_DIR=/path/to/checkouts cargo run -p index-bench --release --bin sisia-catalog
```
