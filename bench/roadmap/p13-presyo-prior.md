# P13 — does a static prior actually pay?

**Tier:** T1 · **Bin:** `cargo run -p index-bench --release --bin presyo-prior`
**Status: MEASURED, 2026-09-05. Answer: not on this workload — and the reason is headroom, not the feature.**

## Why this row existed

Static priors were the only feature in the engine with a rationale and no evidence, and they got
there badly: built because `bench/roadmap/p12-maphy-place.md` appeared to show they were needed, when
that benchmark was measuring unanswerable queries. Sweeping the prior's strength moved the number it
was built for **not at all**.

`docs/adoption.md` still argues every consumer has a query-independent importance signal — presyo
store trust, profstopick rating counts, sisia catalog level. **Arguing is not measuring.** This
measures it on the strongest signal available.

## The workload

`presyo/tests/fixtures/recall-gold-cases.json` — **500 gold cross-store clusters, 2,440 real
listings across 13 retailers**, exported from production 2026-06-13. Every listing in a cluster is
the same physical product at a different retailer, and the task is the one presyo performs: given one
retailer's listing, find the same product at the others.

Name quality really does differ by source, which is what makes a source prior plausible:

```
ever_ph     Fiesta Sweet Spaghettipid 850g
metromart   Fiesta Sweet Spaghettipid 850 g
pickaroo    Fiesta Sweet SpaghetTipid 850 g
waltermart  Fiesta Small Sweet Spaghettipid | 850g
```

Fitted per-retailer quality, from the train half only:

| retailer | quality | listings |
|---|---|---|
| pickaroo | 0.989 | 639 |
| ever_ph | 0.984 | 142 |
| landmark_direct | 0.978 | 186 |
| puregold | 0.975 | 74 |
| shopsuki | 0.969 | 100 |
| waltermart | 0.944 | 18 |
| suysing | 0.894 | 56 |
| mfr_cdo | 0.802 | 6 |

A genuine spread: 0.80 to 0.99.

## The design decision that makes it honest

**The prior is fitted on a TRAIN half and scored on a HELD-OUT half**, split by cluster parity.
Fitting per-source quality on the same clusters it is scored against would make the prior a
compressed copy of the answer key and would have reported a win by construction. Documents are all
2,440 listings either way — only the queries and the fitting are split, because the index a user
searches contains everything.

## Measured — held-out test half

| prior | recall@10 | hit@1 | rankings |
|---|---|---|---|
| none (baseline) | 98.4 % | 99.4 % | — |
| source quality, spread 0.25 | 98.4 % | 99.4 % | 1,227 moved |
| source quality, spread 0.5 | 98.4 % | 99.4 % | 1,227 moved |
| source quality, spread 1.0 | 98.4 % | 99.4 % | 1,227 moved |
| source quality, spread 2.0 | 98.4 % | 99.4 % | 1,229 moved |

**Best held-out gain: +0.00 points.**

## The column that makes this a finding instead of a bug report

`rankings` exists because **"the prior has no effect" and "the prior has no benefit" are different
statements, and without measuring the first you cannot claim the second.** A feature that silently
fails to apply produces exactly the same two metric columns as a feature that applies and does not
help — and the wrong one of those is the one that ships.

It reordered results for **1,227 of the held-out queries**. It is applied, it is not a no-op, and it
still moves neither metric.

Every previous methodology bug in this repo was found because a number looked structurally
suspicious. This is the first one designed *in advance* to be distinguishable, which is the habit the
earlier five were arguing for.

## Why it does not pay here

**Headroom.** The baseline is 98.4 % recall@10 and 99.4 % hit@1 — 1.6 points of room in total. A
prior cannot add a token a listing does not have, and matching a *specific* product across retailers
is decided by the words in the name. There is nothing for a query-independent nudge to fix.

So the honest reading is that **this corpus cannot answer the question rather than answering it
negatively.** A prior earns its place where candidates are otherwise near-tied — a broad or browse
query, "milk", "shampoo", a category page — and presyo's gold set carries ground truth only for the
specific-product task. Building the workload that could show a win means labelling broad queries,
and nobody has.

## What this changes

- **Static priors stay shipped and stay labelled unproven**, now on evidence rather than on the
  absence of it. `add_with_prior` is correct, cheap, serialized, and safe by construction
  (normalized into `(0, 1]` so pruning bounds stay valid). An adopter should be told to leave it off
  until their own numbers say otherwise.
- **The claim in `docs/adoption.md` is narrowed.** "Every consumer has an importance signal" is true;
  "therefore the engine needs to accept one" does not follow for retrieval tasks that are already at
  ceiling.
- **The next honest step is a labelled broad-query set**, not more engine work.

## Not measured

- **Broad / browse queries.** The workload where a prior should win, and there is no ground truth
  for it in any consumer repo.
- **Other consumers' priors.** profstopick rating counts and sisia catalog level are untested; both
  face the same ceiling problem, since `p6` and `p8` already report 99–100 % on their exact tasks.

## Reproduce

```sh
cargo run -p index-bench --release --bin presyo-prior
INDEX_CORPUS_DIR=/path/to/checkouts cargo run -p index-bench --release --bin presyo-prior
```
