# P16 — entity linking from name variants, and what an alias table is actually worth

**Tier:** T1 · **Bin:** `cargo run -p index-bench --release --bin biasd-entity`
**Status: MEASURED, 2026-09-05. Aliases are worth +14.9 points, and 0.7 % → 95.5 % where it matters.**

## Why this row existed

`bench/roadmap/p15-presyo-catalog.md` found the project's first unsaturated workload — 61.7 %
precision@10 on category retrieval, because `"Baking Needs"` contains *Camote Powder* and *Sago
Tapioca*, which share no token with the query. It proposed three fixes and ranked a **query-time
alias/expansion table first**, on the grounds that `AliasTable` already exists and needs no model.

**That proposal had no evidence behind it**, and presyo has no alias ground truth to supply any.
`biasd` does.

## The corpus

`biasd` is a PH news aggregator that resolves political entities mentioned in article text. Its
gazetteers record, per person, the **surface forms that actually appear in print**:

```text
label    Abdulghani Salapuddin
surface  Abdulghani A Salapuddin | Salapuddin, Abdulghani | Salapuddin, Gerry
         Gerry Salapuddin | Gerry
```

**4,560 real entities** (3,387 from `maphy-elections`, 1,173 from `bettergov.ph` open congress data)
and **7,217 alias queries**. **292 of them (4.0 %) share no token at all with their canonical
label** — the hard core, reported separately because an average over 7,217 would bury them.

## The two arms

- **A — canonical label only.** What an app has before it knows about aliases.
- **B — label plus surface forms as a second field.** What an app does once it knows them.

Arm B is not a clever technique. Its value is as a **ceiling**: the most an alias table can possibly
buy, on aliases that were *handed over* rather than derived.

## Measured

| | hit@1 | hit@10 |
|---|---|---|
| A: canonical label only | 71.6 % | 94.1 % |
| **B: label + aliases indexed** | **86.5 %** | **99.8 %** |

### The 292 lexically disjoint queries

| | hit@1 | hit@10 |
|---|---|---|
| A: canonical label only | **0.0 %** | **0.7 %** |
| **B: label + aliases indexed** | **43.5 %** | **95.5 %** |

Unreachable without aliases, rank 1 with them:

```
"Booby / Bob"  ->  Abdullah Dimaporo
"Baham"        ->  Abraham Kahlil Mitra
"Companero"    ->  Alan Peter Cayetano
"Compañero"    ->  Alan Peter Cayetano
"Bobby"        ->  Alberto Pacquiao
"Queenie"      ->  Alexandria Gonzales
```

These are Filipino nicknames. **No tokenizer, no typo policy and no amount of BM25 tuning reaches
them**, because there is nothing to match — and that is precisely the failure class `p15` found in
presyo's categories.

## What this establishes for p15

**An alias table is worth +14.9 points of hit@1 overall, and takes the impossible cases from 0.7 %
to 95.5 % hit@10.** That is a large, real, measured gain on the exact mechanism `p15` proposed on a
hunch — so option 1 is now an evidence-backed row rather than a guess.

Two honest limits on transferring it:

1. **biasd's aliases are curated; presyo's would have to be derived.** These came from
   `maphy-elections` and a public congress dataset. A co-occurrence-derived table over presyo's
   category→product data starts from a strictly harder position, so **this is a ceiling, not a
   forecast.**
2. **Arm B indexes the aliases rather than expanding the query.** Those are different mechanisms
   with different costs — indexing grows the corpus, query expansion grows the query — and only the
   first is measured here. `p15`'s option 1 is the second.

## A finding for biasd itself

**Arm A fails an 80 % hit@1 bar at 71.6 %.** An index of canonical political names is *not adequate*
for resolving what a newspaper actually prints — more than a quarter of real surface forms fail to
land the right entity first. biasd already carries the surface lists, so the fix is available to it;
this quantifies what skipping it costs.

## Not measured

- **Query-time expansion**, as opposed to index-time alias fields. The cheaper mechanism, unmeasured.
- **Derived aliases.** Everything here uses curated ones.
- **Precision damage.** Adding aliases to the index can only help *these* queries by construction;
  whether it degrades ordinary label queries is not measured, and it should be before shipping.
- **`blead`'s `lead-store.db`** — 30,687 real business names with visible near-duplicates
  (`10K EAST CONCRETE MIX SPECIALIST, INC.` vs `10K CONCRETE MIX SPECIALIST INC.`). A seventh app
  with an entity-dedup workload, found in the same sweep, still unbenchmarked.

## Reproduce

```sh
cargo run -p index-bench --release --bin biasd-entity
```
