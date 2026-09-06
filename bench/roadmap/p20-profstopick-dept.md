# P20 — a third corpus, a refuted prediction, and a benchmark error worth 9.4 points

**Tier:** T1 · **Bin:** `cargo run -p index-bench --release --bin profstopick-dept`
**Status: MEASURED, 2026-09-05. Prediction refuted; a methodology error found and corrected.**

> **Numbers on this page were corrected by `bench/roadmap/p21-pool-eviction.md`** — a score-ordered
> candidate pool was evicting perfect-bucket documents before the bucket-first final sort. See p21
> for the full before/after table.

## What this row was for

`p18-blead-industry.md` produced a claim *with a condition*: `learn_expansion` generalizes **where a
facet's members reuse vocabulary**. presyo's grocery categories retained 68 % of their gain on
held-out documents, blead's business names 33 %.

A third corpus that merely repeated the claim would add little, so this one was chosen because the
condition makes a **falsifiable prediction** about it. profstopick's research pack carries **2,253
Ateneo course titles across 92 departments**, where the department is the course-code prefix and is
not in the title:

```
HSCI 60I  ->  FUNDAMENTALS OF GLOBAL HEALTH
MATH 21   ->  MATHEMATICAL ANALYSIS I
```

Course titles reuse vocabulary heavily — chemistry courses say *chemistry*, *organic*, *laboratory*.
**The prediction, written into the bin before it was run: retention should resemble presyo's 68 %,
not blead's 33 %.**

## What happened instead

| | precision@10 |
|---|---|
| plain | **99.2 %** |
| `learn_expansion(dept, 20)` | ~~98.3 % (−0.8 pt)~~ **100.0 % (+0.8 pt)** since `p41` |

Held out: plain 99.7 %, expansion 61.1 % (−38.6 pt).

**The prediction is refuted, but not in a way that tests the condition** — because the baseline is
**already saturated at 99.2 %**. There is no gain to retain, so retention is undefined here, and
this corpus cannot adjudicate the vocabulary-reuse hypothesis either way. Reporting "0 % retention,
condition refuted" would be reading a ratio whose denominator is meaningless.

## The error that 99.2 % exposed

A plain lexical index should not score 99.2 % on a query like `"MATH"` when only 19.6 % of titles
contain their department code. Chasing that produced the finding:

> **A field with `boost 0.0` contributes no score, but still lets a document MATCH.**

A matching document is assigned `typo_bucket` 0 — **the primary sort key** — so it outranks every
document that does not match at all, regardless of scoring weight. Verified directly:

```
schema: title (boost 3.0), facet (boost 0.0)
doc 0: "camote powder" / "bakingneeds"
query "bakingneeds" -> 1 hit: doc 0, score 0.0000, bucket 0
```

`p15`, `p18`, `p19` and this bin all used `boost 0.0` on the facet field believing it made the field
inert for retrieval. **It does not.** It makes the field score-free, not match-free.

### What it cost, measured

On presyo, with **no expansion at all**:

| index | precision@10 |
|---|---|
| name + brand only | 61.7 % |
| **+ boost-0.0 category field** | **71.1 %** |

**9.4 points came from the field's presence.** `p15` compared an expansion arm that carried the
facet field against a baseline that did not, and credited expansion with all 34.9 points.

**The corrected figure is +25.5 points from expansion** (71.1 % → 96.6 %), and `p15` has been
updated. The feature still works and the gain is still large; the headline was overstated by 9.4.

**`p18` is unaffected**: both of its arms carried the industry field at boost 0.0, so its +20.4 was
a fair comparison already. `p19`'s no-op result is likewise unaffected — it compared two indexes
that both carried the field.

## The other finding: expansion can hurt

Setting the contamination aside, `99.2 % → 88.3 %` is a fair comparison — both arms carry the same
schema — and it says something useful:

**Expansion should not be applied to a facet that already retrieves well.** Adding twenty terms to a
query that was already returning the right documents can only displace them. That is an adoption
rule the earlier corpora could not have produced, because both had a real gap to close.

The mechanism of the harm is **not diagnosed here** — expansion terms carry `EXPANSION_DISTANCE = 1`
and should rank below exact matches, so the drop is larger than that ordering predicts. Recorded as
open rather than explained away.

## What this changes

- **`p15`'s headline is corrected**: +25.5, not +34.9.
- **A guard is needed**: `learn_expansion` should be measured against a *same-schema* baseline, and
  an adopter should check whether their facet already retrieves before enabling it.
- **The vocabulary-reuse condition from `p18` is neither confirmed nor refuted.** It remains the
  best available explanation of the presyo/blead difference and it is still untested on a third
  corpus, because this one could not test it.

This is the seventh methodology error this project has caught in itself, and the most expensive: it
changed a number that had been reported repeatedly. It was found by **a baseline being implausibly
high**, which is the same tell as every previous one — a number sitting suspiciously close to a
structural constant of the data.

## Not measured

- **A corpus that can actually test the condition.** It needs an unsaturated baseline and a facet
  whose members either do or do not reuse vocabulary. profstopick was chosen for the second property
  and failed on the first.
- **Why expansion costs 10.8 points here** when its terms are priced below exact matches.
- **Whether a `boost 0.0` field should match at all.** Arguably the engine is wrong, not the
  benchmark — but changing match semantics is a ranking change for every consumer and is not made
  casually at this hour.

## Reproduce

```sh
cargo run -p index-bench --release --bin profstopick-dept
cargo run -p index-bench --release --bin presyo-catalog   # the corrected three-row baseline
```
