# P17 — deriving the aliases, and the control that proves it

**Tier:** T1 · **Bin:** `cargo run -p index-bench --release --bin presyo-expand`
**Status: MEASURED, 2026-09-05. +23.9 pt held out = 71 % of the achievable gain, 0.0 damage.**

## The three-step chain this completes

1. **`p15-presyo-catalog.md`** found the project's only unsaturated workload: category retrieval at
   61.7 % precision@10, because `"Baking Needs"` holds *Camote Powder* and *Sago Tapioca*, which
   share no token with the query. It proposed a query-time alias table **on a hunch**.
2. **`p16-biasd-entity.md`** measured what an alias table is worth using `biasd`'s curated political
   gazetteer — **+14.9 points hit@1, and 0.7 % → 95.5 % on queries with no shared token** — and
   stated plainly that this was a **ceiling, not a forecast**, because those aliases were handed
   over while presyo's would have to be derived.
3. **This file derives them**, and asks whether the derivation actually works.

## The mechanism

For each category, score every term by how concentrated it is inside that category — in-category
rate against overall rate — and append the top *k* to the query. `"Baking Needs"` becomes
`"Baking Needs" + {torani, sweetleaf, monkfruit, lakanto, allspice, …}`.

No model, no embeddings, no new storage: one pass over the catalogue and a lookup table.

## The leak this design exists to avoid

Deriving `"Baking Needs" → {tapioca, …}` from the same category assignments the run is scored
against is **using the answer key**, and reports a large win by construction. It is the trap
`p13-presyo-prior.md` avoided with a held-out split and the one `p12-maphy-place.md` fell into.

So products split in half by index parity:

- expansions derived from the **train** half only (120,839 products);
- the index contains the **test** half only (120,838 products);
- relevance is test-half membership.

**No product contributes both to a term's weight and to the score it earns.** 123 categories have
100+ products on both sides.

## Measured

| query | precision@10 | Δ |
|---|---|---|
| category name alone | 55.7 % | — |
| + top 3 derived terms | 76.7 % | +21.1 pt |
| + top 5 derived terms | 77.2 % | +21.5 pt |
| + top 10 derived terms | 78.6 % | +22.9 pt |
| **+ top 20 derived terms** | **79.6 %** | **+23.9 pt** |
| + top 5 **random** terms *(control)* | 25.5 % | **−30.2 pt** |
| + top 20 **random** terms *(control)* | 19.8 % | **−35.9 pt** |
| + top 5 derived, **brands removed** | 70.2 % | +14.5 pt |
| + top 20 derived, **brands removed** | 67.0 % | +11.3 pt |
| **category indexed as a field** *(upper bound)* | **89.5 %** | **+33.8 pt** |

*(The 55.7 % baseline is not directly comparable to `p15`'s 61.7 %: this indexes only the test half
over 123 categories at a 100-product threshold. All comparisons here are internal.)*

## The control is the point

Adding twenty terms to a query changes what it matches. **If a random expansion of the same size,
drawn from the same vocabulary, helped as much, the gain would be "longer queries retrieve more" and
the derivation would be decoration.**

Random expansion instead **costs 35.9 points**. The spread between derived and random is roughly
**60 points**, so the derivation is unambiguously doing the work.

This is the same role the `rankings` column played in `p13-presyo-prior.md`: build the measurement
that can distinguish the boring explanation from the interesting one, *before* reporting the
interesting one. It is the third bench in the repo designed that way in advance.

## How much of the achievable gain does this capture?

Indexing the category name on every product is close to cheating — the label becomes part of the
document — but it is the right **upper bound**: it says how much of the gap is "the label is simply
absent from the text" rather than something no expansion could reach.

| | precision@10 | share of achievable gain |
|---|---|---|
| category name alone | 55.7 % | — |
| derived expansion | **79.6 %** | **71 %** |
| derived, brands removed | 70.2 % | 43 % |
| category indexed *(upper bound)* | 89.5 % | 100 % |

**Derived expansion captures 71 % of the achievable gain with no new data, no model and no storage.**
The remaining 9.9 points are not vocabulary: even with the label indexed, precision stops at 89.5 %
because a product whose *name* contains the category's words outscores a product that merely belongs
to it. That residual is BM25 field weighting, not semantics, and it is a different problem.

## The brand caveat, tested rather than asserted

`p17` first recorded that most derived terms are brand names and flagged the obvious risk: an
expansion built from history cannot cover a brand it has never seen. The catalogue carries a
`brand_name` column, so that caveat is **testable**, and it was tested by striking every brand token
out of the expansion:

**Brand-free expansion still captures 43 % of the achievable gain (+14.5 points).**

So the mechanism is *not* purely brand memorisation — non-brand terms carry substantial signal on
their own — but brands account for roughly a third of the effect. A category of entirely unseen
brands would degrade toward the +14.5 figure rather than to zero, which is a much better failure
mode than the caveat implied.

One detail worth keeping: brand-free expansion is **better at k=5 (70.2 %) than at k=20 (67.0 %)**.
Once the brands are removed, going deeper pulls in noise, so the right depth depends on what the
expansion is allowed to contain.

## What was actually derived, and why that is a caveat rather than a triumph

```
"Baking Needs"     -> torani, sweetleaf, sison, monkfruit, lakanto, neco, allspice, 9inch
"Soup Mixes"       -> ladle, campbell, 298g, soup, sampalok, miso, knorr, tom
"Pet Accessories"  -> muffy, kaviar, eyewear, 35mm, handcraft, technica, evita, sweden
"Native Deli"      -> italfood, antiche, sebaste, antica, grappa, poli, corvinia, bindi
```

**Most of these are brand names, not category concepts.** The mechanism is largely learning
*brand → category* association, which is real signal in a grocery catalogue — Knorr really does
concentrate in soup — but it carries two limits worth stating rather than discovering later:

- **It will not cover a brand it has never seen.** A new supplier's products are invisible to an
  expansion derived from history, which is exactly when a catalogue changes most.
- **Per-category quality varies a lot.** `"Soup Mixes"` picked up *soup*, *miso*, *sampalok*,
  *knorr* — genuinely useful. `"Pet Accessories"` picked up *eyewear*, *35mm* and *sweden*, which
  are noise, and it still improved on aggregate. The average is real; it is not uniform.

## The gate: does it damage ordinary product queries?

Everything above expands a *category* query, which is the case the mechanism was built for. **A
shipped expander does not know what kind of query it was handed**, and `"milk"` is both a category
and something a shopper types when they want one specific carton. So: real product names as queries,
asking for the product itself back.

| trigger | hit@1 | hit@10 | fired on |
|---|---|---|---|
| plain query (no expansion) | 99.2 % | 100.0 % | — |
| **loose** — query *contains* a category's words | 97.3 % | 99.4 % | 596 of 3,021 (19.7 %) |
| **strict** — query **is** a category | **99.2 %** | **100.0 %** | **0** |

**The loose trigger costs 2.0 points of exact-product hit@1**, and the casualties say exactly why:

```
Signature Select Ice Cream Butter Pecan 1.5Qt          matched category "Cream"
Moringa-O2 Shampoo Herbal 200ml                        matched category "Shampoo"
Krem-Top Krem Top Creamer 170g                         matched category "Creamer"
Huggies Dry Diapers Pants Double Extra Large x 22pcs   matched category "Diapers"
Rexona Deodorant Roll-On Men Quantum 25ml              matched category "Deodorant"
```

Every one is a product whose name legitimately contains a category word. Expanding it buries the
exact match under the category's other members — **the feature would have made browse better by
making search worse**, which is the trade this gate exists to catch before it ships rather than
after.

**The fix is one comparison.** Requiring the query's token set to *equal* a category's, rather than
merely contain it, fires on **zero** product queries and costs **+0.0 points**, while still firing
on genuine category queries — so the +23.9 above is retained in full.

So the shippable design is: **+23.9 points on category/browse queries, 0.0 points of damage on
product lookup, gated by "expand only when the query IS a category".** Both halves measured.

## Where this leaves the gap

| | precision@10 |
|---|---|
| `p15` baseline, category name alone | 61.7 % |
| `p17` derived expansion (internal baseline 55.7 %) | **79.6 %** |

The gap `p15` opened is **substantially but not fully closed**, by the cheapest of the three
options it listed, with no model and no new storage — and now with evidence at every step rather
than a hunch at the first one.

## Not measured

- **`AliasTable` integration.** The mechanism is simulated at query-construction time, not wired
  into the analyzer's existing alias machinery. Engineering, not evidence.
- **Option 3 from `p15`** — dense or hybrid retrieval. Options 1 and 2 are now both measured; this
  is the only one left, and the 9.9-point residual it would target is BM25 field weighting rather
  than vocabulary, so it is a weaker case than when `p15` listed it.
- **Where a per-corpus artifact lives.** Derivation is one pass over the catalogue, so it can be a
  build step — but this repo has no home for per-corpus data yet.

## Reproduce

```sh
cargo run -p index-bench --release --bin presyo-expand
```
