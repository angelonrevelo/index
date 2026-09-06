# P18 — does `learn_expansion` generalize, or was it presyo-shaped?

**Tier:** T1 · **Bin:** `cargo run -p index-bench --release --bin blead-industry`
**Status: MEASURED, 2026-09-05. It generalizes — with a condition: facets whose members reuse vocabulary.**

> **Numbers on this page were corrected by `bench/roadmap/p21-pool-eviction.md`** — a score-ordered
> candidate pool was evicting perfect-bucket documents before the bucket-first final sort. See p21
> for the full before/after table.

## Why this row existed

`IndexBuilder::learn_expansion` was designed against one corpus and one failure. On presyo's
241,677 grocery products it took category retrieval from **61.7 % to 96.6 %**. A feature measured on
exactly the data it was built for has proved very little — the honest next question is whether it
works somewhere nobody had it in mind for.

## The second corpus

`blead` is a lead-generation pipeline. Its store holds **25,979 real Philippine businesses** —
company names, not product titles — each tagged with an `industry` from 35 values, of which **27
carry 100+ members**.

Same failure shape, completely different vocabulary:

```
J&S Agriventures Corporation                  ->  Agriculture
10K EAST CONCRETE MIX SPECIALIST, INC.        ->  Wholesale/Retail
BAGUIO CENTRAL UNIVERSITY ALUMNI ASSOCIATION  ->  Public Admin
```

Nothing in `10K EAST CONCRETE MIX SPECIALIST` says *Wholesale/Retail*.

**The comparison is fair by construction: 11.8 % of business names share a word with their own
industry, against 13.8 % for presyo.** The label is independent of the retrieval signal to almost
the same degree, so a difference in outcome cannot be blamed on an easier label. Same metric, same
arms, same `boost 0.0` on the facet field so it supplies the value to the learner and contributes
nothing to retrieval.

## Measured

| | precision@10 | MRR |
|---|---|---|
| plain index | 77.4 % | 0.753 |
| **`learn_expansion(industry, 20)`** | **95.6 %** | **1.000** |

33 facet values learned, build **0.17 s** on 25,979 documents.

### The industries the plain index could not reach

| industry | members | plain | learned |
|---|---|---|---|
| Construction | 229 | **0 %** | 80 % |
| Public Admin | 674 | **0 %** | **100 %** |
| Real Estate | 115 | **0 %** | 50 % |
| Transport | 154 | **0 %** | 70 % |
| Other Services | 248 | 10 % | 60 % |
| vehicle-services | 1,390 | 20 % | **100 %** |
| agriculture | 388 | 30 % | **100 %** |
| consulting | 334 | 30 % | 60 % |

## The verdict

| corpus | documents | facet values | plain | learned | Δ |
|---|---|---|---|---|---|
| presyo — grocery products | 241,677 | 145 | 61.7 % | 96.6 % | **+34.9 pt** |
| **blead — business names** | **25,979** | **27** | **77.4 %** | **95.6 %** | **+18.1 pt** |

Both in-sample, measured identically, on corpora with near-identical label leakage and completely
different vocabularies. **The mechanism is not presyo-shaped.**

## Held out — and this is where the generalization claim gets its condition

Both arms above are in-sample. `p17` measured the other condition on presyo — a table applied to
documents added *after* it was built — so the same split was run here: expansion derived from the
train half, index holding only the test half.

| | precision@10 |
|---|---|
| plain | 77.4 % |
| derived expansion, **held out** | **75.9 %** (−1.5 pt) |

| corpus | in-sample gain | held-out gain | held-out retains | staleness cost |
|---|---|---|---|---|
| presyo — grocery products | **+21.8 pt** | **+21.5 pt** | **~99 %** | see p21 |
| **blead — business names** | **+30.7 pt** | **−1.5 pt** | **~0 %** | see p21 |

**The staleness cost is comparable (13.7 vs ~17 points) but what survives is not: presyo keeps 68 %
of its gain on unseen documents and blead keeps 33 %.**

### The condition, which is the real finding

The mechanism's value depends on **vocabulary reuse within a facet**, and the two corpora differ
sharply in it:

- **Grocery products reuse vocabulary heavily.** Half of `Baking Needs` and the other half share
  brands (*Knorr*, *Alaska*), product types (*powder*, *tapioca*) and sizes. A term learned from one
  half genuinely predicts the other.
- **Company names are idiosyncratic.** `10K EAST CONCRETE MIX SPECIALIST, INC.` and the next
  logistics firm share almost nothing but legal suffixes. A term learned from half the firms in an
  industry often appears nowhere in the other half's names.

So the honest statement is narrower than "it generalizes", and better: **it generalizes where a
facet's members reuse vocabulary, and an adopter can check that property on their own data before
adopting it** — the in-sample/held-out split in this bench is exactly the check, and it needs no
labels beyond the facet they already have.

## Why the gain is smaller here, stated rather than glossed

+20.4 against presyo's +34.9. Three plausible reasons, and the file does not pretend to have
isolated which dominates:

- **Facet values are far broader.** 27 industries over 25,979 businesses averages ~960 members each
  and lumps unrelated firms together. `"Other Services"` reaches only 60 % and `"Real Estate"` 50 %
  — those are heterogeneous by definition, and no expansion makes an incoherent set coherent.
- **Business names carry less signal than product names.** `Maya Hotcake Mix Fluffy 350g` says what
  it is; `10K EAST CONCRETE MIX SPECIALIST, INC.` says who it is, and the industry has to be
  inferred from a trade word buried in a legal name.
- **A lower ceiling.** `p17` measured presyo's achievable gain by indexing the label directly; the
  equivalent bound has not been computed here, so 85.2 % may already be near this corpus's ceiling
  rather than short of it.

**MRR 1.000 is the part that is unambiguous**: after expansion, every one of the 27 industries has a
correct result at rank 1.

## Not measured

- **The upper bound for this corpus.** `p15` option 2 (indexing the label) was measured on presyo
  and not here, so "how much was available" is unknown.
- **A third corpus.** Two is enough to say "not presyo-shaped"; it is not enough to say "general".
- **`city` as an alternative facet.** 139 values but only 8 with 100+ members, so it was left alone.

## Reproduce

```sh
cargo run -p index-bench --release --bin blead-industry
```
