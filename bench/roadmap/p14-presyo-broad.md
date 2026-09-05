# P14 — the broad-query workload, labelled

**Tier:** T1 · **Bin:** `cargo run -p index-bench --release --bin presyo-broad`
**Status: MEASURED, 2026-09-05. Saturated too — and static priors actively hurt.**

## Why this row existed

`bench/roadmap/p13-presyo-prior.md` found static priors worth +0.00 points on presyo's cross-store
matching, diagnosed the cause as headroom (98.4 % baseline), and concluded that the workload worth
optimizing is **broad and browse queries** — where dozens of candidates are near-tied and the
ordering *is* the product.

It then claimed the blocker was a labelled query set that **required somebody to decide what the
right answer to `"milk"` is**. That claim was half wrong, and this bin exists because of it:
presyo's gold fixture already carries a curated product taxonomy. The labels were in the file.

## The labels, and why they are not a judgement call

- **brand query** — `"Purefoods"`; relevant = every listing whose cluster has that `gold_brand`.
- **category query** — `"milk"`, `"cheese"`, `"hotdog"`; relevant = every listing whose
  `gold_product_type` contains that word.

**79 labelled broad queries** (32 brand, 47 category), mean **53 relevant listings each**. The index
sees only `raw_name`; the labels come from gold metadata that is **not indexed**, so relevance is
decided by presyo's taxonomy and retrieval has to earn it from the retailer's text.

recall@10 is meaningless when 53 listings are relevant — no ranker fits them in ten slots, and
reporting it would repeat the mistake `p12-maphy-place.md` made with `"CITY "`. The metric is
**precision@10**: *is what I got back actually about this?*

## Measured

| prior | precision@10 | MRR | Δ |
|---|---|---|---|
| **none (baseline)** | **97.2 %** | 0.972 | — |
| source quality (p13's prior) | 95.8 % | 0.985 | **−1.4 pt** |
| popularity (retailers carrying) | 94.7 % | 0.949 | **−2.5 pt** |

## Two conclusions, one of them definitive

**1. Static priors are settled for presyo: leave them off.** They do not merely fail to help on
broad queries — the workload they were supposed to be *for* — they **actively degrade precision**,
by up to 2.5 points, pulling well-stocked or clean-named products above ones that actually match the
query. `p13` showed a prior buying nothing; this shows it costing something. Combined, that is a
complete answer for this consumer rather than an open question.

**2. Broad queries are saturated as well: 97.2 %.** Every workload this project can measure on real
consumer data is at or near ceiling — `p6` 99.8 %, `p8` 100 %, `p13` 98.4 %, `p14` 97.2 %.

## The tail was checked before being called a target — and it is label noise

The mean hides a tail: `"chocolate"` scores 40 %, `"pork"` 50 %. The obvious move is to name that
tail as the next ranking target. **It would have been wrong.**

**65 listings say "chocolate" in the retailer's name; only 14 carry it in `gold_product_type`.**
`Nestle Chuckie Chocolate Milk Drink` is typed `Milk Drink`, so the label scores it *irrelevant* for
the query `"chocolate"` — while a shopper plainly wants it. The engine returned the right thing and
the label marked it wrong.

**So true precision is higher than 97.2 %, not lower**, and the tail is not work to be done.

This is the sixth methodology catch in the repo and the second caught *before* publishing rather
than after. The habit that produced it is now explicit: **check whether a bad number is the system
failing or the measurement lying, before assigning work to it.**

## What survives, honestly

The original claim in `p13` — that a genuinely useful broad-query set needs a human judgement — is
**narrowed but not removed**. `gold_product_type` is a *canonical type*, not a tag set: it cannot
express "this is a chocolate product". A label set that could would need someone to decide whether
`"milk"` should return a yoghurt drink, and no data in any consumer repo answers that.

What this bin establishes is narrower and still worth having: **on the labels presyo already owns,
broad queries are not where the engine is weak either.**

## What this means for the project

The engine is not the bottleneck on any workload this project can currently measure. That is a real
finding rather than a failure to find work:

- **Do not ship static priors on by default.** Two independent measurements say leave them off.
- **Further ranking work would be tuning against noise** — every measurable consumer task is at
  ceiling, and the one visible tail turned out to be the labels.
- **The remaining lever is not relevance, it is reach**: the engine's advantages that are *not*
  saturated are latency at scale (`p7`, 1 M documents), portability (`p11`, browser point location),
  and incremental update plus deletion — none of which a precision metric measures.

## Not measured

- **Intent-labelled broad queries.** Needs a human decision; named in `p13`, still open.
- **Behavioural labels.** Click or purchase data would sidestep the judgement call entirely, and no
  consumer repo has it exported.

## Reproduce

```sh
cargo run -p index-bench --release --bin presyo-broad
```
