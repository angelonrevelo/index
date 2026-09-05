# P15 — 241,677 real products, and the first workload that is not at ceiling

**Tier:** T1 · **Bin:** `cargo run -p index-bench --release --bin presyo-catalog`
**Status: MEASURED, 2026-09-05. Category retrieval 61.7 % — the first real gap this project has found.**

> **This file corrects a conclusion.** After `p13` and `p14` I wrote that "every workload this
> project can measure on real consumer data is at or near ceiling" and that "everything this repo
> can learn from static corpora, it has learned." **That was wrong**, and it was wrong because I had
> not looked hard enough for corpora. `presyo/data/endless-prep/catalog-active.csv` — 30 MB,
> 241,793 rows — was on disk the whole time.

## The corpus

presyo's **active product catalogue, exported from production**: 241,677 usable rows with product
name, brand, type, variant, size, pack count, and an **assigned category**. Four times the largest
real corpus this project previously had (61,467 DepEd schools).

## Finding 1 — recombination was easier than reality, and by how much

`p7-scale.md` reaches 250 K and 1 M by *recombining* 61,467 real schools, and flags the caveat
itself. This quantifies it:

| | p7 @250 K (recombined) | p15 @241,677 (real) |
|---|---|---|
| distinct terms | 41,069 | **117,472** |
| index | 40.7 MB (162.9 B/doc) | 23 MB (101.3 B/doc) |
| build | 3.14 s | 0.4 s |
| exact p50 | 144 µs | 152 µs |
| **typo p99** | **3.08 ms** | **5.72 ms** |

**Recombination understates typo p99 by ~1.9×.** The vocabulary is the reason: recombining 61 K
schools cannot invent new terms, so it stalls at 41,069 no matter how many documents it emits, while
a real catalogue of similar size carries **117,472** — nearly 3× the dictionary, which is exactly
what a fuzzy expansion has to search.

So `p7`'s "FAILS the 5 ms bar at 1 M" was, if anything, generous. On real rows the bar is missed at
**a quarter of a million**: 5.72 ms. That is an honest downgrade of a number this project has been
quoting, and it comes from better data rather than from a code change.

## Finding 2 — the gap: 61.7 % precision@10 on category retrieval

`category_name` is **assigned**, not derived from the product text. Only **13.8 %** of products
contain their own category name, so the label is largely independent of the retrieval signal — the
thing `p14-presyo-broad.md` could not achieve with `gold_product_type`, which retailers repeat in
the product name.

**125 categories with 200+ products. precision@10 = 61.7 %, MRR 0.781.**

| category | precision@10 | products |
|---|---|---|
| Apparel | **0 %** | 210 |
| Baking Needs | **0 %** | 1,986 |
| Home | **0 %** | 299 |
| Native Deli | **0 %** | 660 |
| Pet Accessories | **0 %** | 2,550 |
| Soup Mixes | **0 %** | 273 |
| Canned Fruits | 10 % | 724 |
| Canned Seafood | 10 % | 353 |

**This is the first workload in the project that is not saturated**, and the failure is not a tuning
problem. Querying `"Baking Needs"` should return:

```
Fat & Thin Camote Powder 500g
Tian Seng Mung Beans Small Dice 100g
NVJ Sago Tapioca 200g
Mccormick Cream of Tartar 50g
Maya Hotcake Mix Fluffy N Tasty 350g
```

**Not one of those shares a token with the query.** This is vocabulary mismatch — the category is a
*concept* and the product name is a *thing* — and **no amount of BM25F tuning, typo tolerance,
prefix anchoring or static priors can bridge it**, because every one of them operates on tokens that
have to match something. It is the textbook limit of lexical retrieval, and it is sitting in the
middle of the shopping-search workload this project exists for.

## Why every earlier benchmark missed it

Each previous consumer bench asked a question whose answer was *already in the document text*:

| bench | query | result |
|---|---|---|
| `p6-real-corpus` | a school's own name | 99.8 % |
| `p8-sisia-catalog` | a course's own code | 100 % |
| `p13-presyo-prior` | a listing's own name | 98.4 % |
| `p14-presyo-broad` | a word from the gold type the name repeats | 97.2 % |
| **`p15-presyo-catalog`** | **a category the name never contains** | **61.7 %** |

Saturation was a property of the questions, not of the engine. **The ceiling I reported four times
was the ceiling of the labels I had, and the fix was to go and find better data** — which the user
prompted by pointing at another repo.

## Closed: the engine now does this itself

`IndexBuilder::learn_expansion(facet_field, top_k)` implements the mechanism inside the engine
rather than in a benchmark. Measured on this same corpus:

> **CORRECTED 2026-09-05.** This table first read `61.7 % -> 96.6 %, +34.9 pt`. That compared an
> expansion arm carrying a `boost 0.0` category field against a baseline that had no such field —
> and **a boost-0.0 field scores nothing but still lets a document MATCH**, earning `typo_bucket` 0,
> the primary sort key. 9.4 of those points were the field's doing. See
> `bench/roadmap/p20-profstopick-dept.md`.

| index | precision@10 | |
|---|---|---|
| name + brand only | 61.7 % | no facet field at all |
| **+ boost-0.0 category field, no expansion** | **71.1 %** | **the fair baseline** |
| **+ `learn_expansion(category, 20)`** | **96.6 %** | **+25.5 pt from expansion** |

145 facet values learned, **+3.4 s of build time** on 241,677 products, and category-query latency
p50 **316 µs** against 178 µs for an exact product query — expansion adds terms, so it is not free.

**That 96.6 % is IN-SAMPLE and the label matters.** The table is learned from the same products it
is scored against, so it is *not* comparable to `p17-presyo-expand.md`'s held-out 79.6 %. The two
answer different questions and both are worth having:

- **in-sample (96.6 %)** — behaviour on the catalogue the table was built from, which is the
  deployment condition for every product already in it;
- **held out (79.6 %)** — behaviour for products added *after* the table was built, the condition
  that decays until it is rebuilt.

**The ~17-point gap between them is the cost of a stale table**, and it is the number that decides
how often derivation should re-run.

The category field carries **boost 0.0**, which was intended to make it inert for retrieval. **It
does not** — it makes the field score-free, not match-free — and that is why the fair baseline above
exists. The lesson generalizes past this bench: *"weight 0" is not "absent"* whenever a ranker sorts
on match-count before it sorts on score.

## What this changes on the roadmap

The next row is no longer "wait for an adopter". It is the gap above, and there are three known
approaches, in increasing order of cost:

1. **A synonym / expansion table at query time.** The analyzer already has `AliasTable`, applied at
   index *and* query time; `"Baking Needs"` → `{flour, sugar, tapioca, cream of tartar, …}` is the
   same mechanism as the existing Philippine grocery aliases, just sourced from the catalogue's own
   category→product co-occurrence rather than hand-written. Cheap, portable, no model.
2. **Category as an indexed field.** Trivially fixes this metric and is arguably cheating — it makes
   the label part of the document. Worth measuring precisely to quantify how much of the gap is
   genuinely semantic.
3. **Dense or hybrid retrieval.** `docs/roadmap-rejected.md` rejected vectors as a physical
   ordered-key range scan for sound reasons; that rejection was about *storage layout*, not about
   whether embeddings help recall. This is the first evidence in the project that they might.

**Option 1 first**, because it needs no model, no new storage, and reuses machinery that already
exists — and because measuring it tells you how much of the 38-point gap is vocabulary versus
genuine semantics.

## Not measured

- **The 50,673 `Uncategorized` products** (21 % of the catalogue) — excluded from labels here, and a
  real presyo data-quality problem in their own right.
- **`blead`'s `lead-store.db`** — 30,687 real business leads with names, emails, tiers and scores. A
  sixth app with an entity-search workload, found in the same sweep and not yet benchmarked.
- **1 M real rows.** This corpus is 242 K. presyo's full scrape is larger; only the active catalogue
  is exported here.

## Reproduce

```sh
cargo run -p index-bench --release --bin presyo-catalog
```
