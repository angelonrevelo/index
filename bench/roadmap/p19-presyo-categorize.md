# P19 — the index as an analyzer: filling a 21 % data-quality hole

**Tier:** T1 · **Bin:** `cargo run -p index-bench --release --bin presyo-categorize`
**Status: MEASURED, 2026-09-05. 95.7 % accurate on the 22 % it is certain about; 81.0 % on 69 %.**

## Why this row existed

`bench/roadmap/p15-presyo-catalog.md` recorded, and did not pursue, that **50,607 of presyo's
241,677 products (20.9 %) are categorised `Uncategorized`**. That is a real data-quality problem
sitting inside the corpus this project has spent the day measuring, and it is the **inverse** of
every question asked so far: not "find the products in this category" but **"what category is this
product?"**

It also tests a different half of the stated goal — *"the best search engine **and database
engine/analyzer** one app could use"*. Retrieval has been measured to death; analysis had not been
measured at all.

## The method, and why it needs nothing new

Search a product's name against the products whose category *is* known, and take a **majority vote
over the top-10 neighbours**. That is kNN classification in which **the index is the model** — no
training, no embeddings, no second system, nothing the application does not already have.

## How it is validated, and what is deliberately not claimed

Predicting the 50,607 unlabelled rows proves nothing on its own: **there is nothing to check the
answers against.** So accuracy is measured on a **held-out slice of the labelled products** — 4,010
of them, removed from the index and predicted as if unknown.

Only then is the method turned on the genuinely unlabelled rows, and what is reported there is
**coverage and confidence, not accuracy**, because their true categories are unknown.

## Measured — 4,010 held-out labelled products

| | accuracy | share of predictions |
|---|---|---|
| all predictions | 68.0 % | 100 % (4,010 of 4,010 got one) |
| **confident only (≥60 % of neighbours agree)** | **81.0 %** | 68.7 % |

**A confidence threshold is worth 13 points**, and it costs nothing to apply — the vote share is a
by-product of the prediction. An application can take the confident 69 % automatically and queue the
rest for a human, which is the shape this feature would actually ship in.

### The misses are mostly taxonomy boundaries

```
Chivas Regal Scotch Whisky 18 Years Old 700ml   -> Spirits            (truth Liquor)
SM Bonus Chicken Leg Quarter 500g               -> Chicken & Poultry  (truth Fresh Meat)
SM Bonus Shrimp Raw Peeled 16-20 500g           -> Fresh Seafood      (truth Frozen Seafood)
Beef Stewcubes 500g                             -> Fresh Meat         (truth Frozen Meat)
Purefoods Ready to Eat Liempo Salt & Pepper     -> Ready to Eat       (truth Frozen Prepared Foods)
```

**Of the 1,284 misses, 188 (14.6 %) name a category that shares a word with the true one** —
measured, not asserted. Counting those as acceptable would put the figure at 72.7 %.

**That number is deliberately not claimed as accuracy.** Whether `Spirits` may stand in for `Liquor`
is presyo's call about its own taxonomy, not this benchmark's, and a bench that grades itself
generously on someone else's category scheme is measuring its own opinion. The 68.0 % / 81.0 %
figures are the honest ones; 72.7 % is context for reading them.

## The operating curve — which is the actual deliverable

`k` and the confidence threshold were chosen, not measured, so both were swept.

**Confidence threshold (k=10):**

| threshold | accuracy | share kept |
|---|---|---|
| 0.0 (take everything) | 68.0 % | 100 % |
| 0.4 | 71.8 % | 91.9 % |
| 0.5 | 76.0 % | 81.9 % |
| **0.6** | **81.0 %** | **68.7 %** |
| 0.7 | 85.5 % | 56.9 % |
| 0.8 | 89.5 % | 46.8 % |
| **1.0 (unanimous)** | **95.7 %** | **22.1 %** |

**This curve is worth more than any single number on it.** At unanimity the index is right about
**95.7 %** of the products it will speak up for, which is 22 % of the catalogue — good enough to
apply automatically. At 0.6 it is right 81 % of the time about 69 % of them, which is a review
queue. The application picks the point; the engine supplies the vote share for free.

**`k`, swept:**

| k | accuracy | confident accuracy | confident share |
|---|---|---|---|
| 3 | 66.7 % | 73.8 % | 86.3 % |
| 5 | 67.8 % | 76.2 % | 81.5 % |
| **10** | **68.0 %** | 81.0 % | 68.7 % |
| 20 | 67.3 % | 84.2 % | 59.5 % |
| 40 | 65.3 % | **85.0 %** | 50.4 % |

Overall accuracy peaks at k=10 and is flat either side, but **confident accuracy rises monotonically
with k while confident share falls** — a larger neighbourhood makes agreement rarer and more
meaningful. k is therefore a second coverage/precision dial, not an accuracy dial, and 10 is a
reasonable middle rather than an optimum.

## Does `learn_expansion` help the classifier? No, and that is the point

| | accuracy |
|---|---|
| plain index | 68.0 % |
| index with `learn_expansion(category, 20)` | 68.0 % (**−0.0 pt**) |

**Predicted from the design to be an exact no-op, and it is.** Expansion fires only when the whole
query *is* a facet value, and a product name never is.

It was run rather than asserted because the prediction is falsifiable in a useful direction: **a
mechanism that had fired here would have meant the strict trigger leaks**, which is the failure mode
`p17`'s damage gate was built to prevent. This is that gate checked from the opposite side.

## Applied to the real hole — 10,122 of the 50,607 Uncategorized

- **100 %** received a prediction.
- **46 %** of those met the confidence bar.
- **2,169 products/second**, single-threaded.

Where the confident ones would go:

| category | count |
|---|---|
| Baby Accessories | 683 |
| Prescription Drugs | 510 |
| Fresh Fruits | 213 |
| Pet Accessories | 166 |
| Fresh Vegetables | 141 |

**The confidence rate is much lower here (46 %) than on held-out labelled products (68.7 %), and
that is informative rather than disappointing.** Products left `Uncategorized` are plausibly the
ones that were hard to categorise in the first place — unusual names, thin descriptions, genuinely
ambiguous items — so a method that is *less* sure about them is behaving correctly. A method that
was equally confident on both sets would be the suspicious result.

## What this establishes

**A 21 % data-quality hole can be given a confident, checkable proposal by the index the application
already has**, at 2,169 products/second, with no model, no training and no second system. For roughly
half of it the engine will say "this is a Baby Accessory, and 6 of the 10 nearest products agree" —
and for the other half it will say so with less confidence, which is exactly the signal a human
triage queue needs.

## Not measured

- **Accuracy on the actually-Uncategorized rows.** Unknowable without someone labelling them. This
  file does not estimate it and the held-out figure should not be assumed to transfer, precisely
  because the confidence gap suggests those rows are harder.
- **The other 40,485 Uncategorized products.** Every fifth row was sampled to keep the run short.

## Reproduce

```sh
cargo run -p index-bench --release --bin presyo-categorize
```
