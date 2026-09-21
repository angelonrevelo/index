# P99 — grocery recall on presyo's judged set

**Tier:** T3 · **Status: SHIPPED 2026-09-22** · consumer: presyo (`scripts/product-search-judge.ts`,
`tests/fixture/product_search_judged.json`, 88 queries with checkable regex ground truth).

## Corpus

The Sep-18 pilot: the 34,102 product ids in `/var/lib/presyo/idx/build-20260918-064502/corpus.csv`
on `bygelo` (the live `mv_pilot_ready_product` has been at ~331 rows since Sep 19). A loopback
PostgreSQL 16 was loaded by read-only `COPY … TO STDOUT` from production
(`default_transaction_read_only=on`): `brand`, `category`, and the `product` / `product_alias` /
`mv_latest_price` rows of those ids (34,102 / 86,127 / 1,030,859), with the production trigram
indexes. `mv_pilot_ready_product` is a table of the 34,102 ids. Rows are today's, ids are Sep 18's.

Fidelity: with the engine at `a3a8a00` the replica reproduces INDEX_EVAL §7.1 — SQL p@10 83.4 %
(identical), engine 93.0 % (doc: 93.1), and the same 8 losses at the same numbers.

## Diagnosis (engine alone, `b` = typo_bucket)

| query | SQL → engine | cause |
|---|---|---|
| `lucky mee` | 100 → 50 | `mee` exists (Penang Prawn Mee, Ramyeon Mee); 3 letters is below the 1-typo gate and the lazy rule never fuzzes an existing word. `Lucky Me` rows b3 (missing `mee`), mee rows b3 (missing `lucky`); IDF of the rare word wins. |
| `sabong panlaba` | 90 → 60 | Table keys `sabon`; `sabong` (Tagalog linker) stays literal and matches dishwashing / bath soaps' `sabong panghugas` / `sabong pampaputi`. |
| `itlog` | 80 → 60 | `itlog` → `egg`; cartons named `Eggs` without `itlog` in the search text are unreachable (below gate, never fuzzed); egg pie / egg noodles fill the page. |
| `head and sholders` | 100 → 80 | `Head & Shoulders` has no `and` token: every H&S row b4 (missing `and` 3 + typo 1), `Head and Body Wash` b3. |
| `cdo corned beef` | 100 → 90 | Not the engine: `CDO Beef Loaf`'s search text says `corned beef` (b0). presyo's availability prior (68 vs 5 branches) moves it above the 10th relevant row. |
| `milk` | 50 → 40 | Not an engine defect: single generic term; `Milk Magic Milk Milk Chocolate` has tf 3. The judge excludes chocolate milks. |
| `laundry detergent` | 100 → 90 | Judge: `Breeze Laundry Powder Detergent ActivBleach` is excluded by the regex `bleach`. |
| `palmoliv` | 100 → 90 | `Honey Palmoliv Sh …` spells the query exactly and ranks 1 — exact beats typo by design. |

## Rules (each a Rust test, generic fixture, red on master)

1. **Linker** — `analyze::ligature`; `philippine_grocery` inserts `kapeng`, `sabong`, `asing`, …
2. **Optional connective** — `and at for of the with sa ng` beside a content word: scores if
   present, costs 0 if missing (`GroupKind::Optional`). Never the word being typed; an
   all-connective query keeps them required.
3. **Context correction** — a word of 3+ letters whose literal reading meets no other word in any
   live document, while the others meet each other, admits every one-edit neighbour that does,
   at distance 1. One conjunction check skips the whole step when the literal query already
   co-occurs.
4. **Number form of an alias word** — distance 1, IDF capped at the canonical's.

Red on `master` (7c7a6cf), green on the lane: `a_filipino_ligature_form_resolves_like_its_base`
(`kapeng barako` b3 → b0), `a_connective_missing_from_the_document_costs_nothing` (rank-1 was the
`and` row), `a_short_word_that_never_cooccurs_is_corrected_in_context` (top 3 were the `mee` rows),
`an_unknown_short_word_is_corrected_in_context`, `an_alias_word_matches_its_number_form_one_tier_below`,
`searcher_runs_the_grocery_recall_rules`. Guard green on both: `a_short_word_that_cooccurs_is_left_alone`.

## Result — judged, 88 queries, `--repeat 5`

| metric | engine `a3a8a00` | engine p99 |
|---|---|---|
| p@10 (engine + hydrate, as served) | 93.0 % | **94.8 %** |
| p@10 engine only | 91.7 % | 93.5 % |
| cover@20 | 94.1 % | 95.1 % |
| typo hit@1 | 88.9 % | 94.4 % |
| p@10 Filipino / typo | 76.3 / 95.6 % | 81.9 / 99.4 % |
| wrong-size, brand, generic, size | 0.7 %, 99.5, 96.0, 95.0 | unchanged |
| branch@10 | 90.25 | 90.16 |
| won / lost / tied vs SQL | 18 / 8 / 62 | **20 / 4 / 64** |

Every per-query p@10 change, old → new (SQL): `lucky mee` 50 → 100 (100), `sabong panlaba`
60 → 90 (90), `itlog` 60 → 100 (80), `head and sholders` 80 → 100 (100), `gamot sa lagnat`
30 → 60 (30), **`sabon` 100 → 90 (30)** — `Beauche Soaps Beauty 90g` (a bath soap) now ranks 1:
its text holds both `sabon` → soap and `Soaps`, and the two expansions sum. The fixture's
`\bsoap\b` does not match `Soaps`. It is still a win over SQL. No query the engine previously won
is lost. Engine latency in-process (laptop, relative only): p50 0.036 → 0.026 ms, p95 0.68 → 0.49 ms.

## Consumer benches, master vs lane (same machine, interleaved)

| bench | master | lane |
|---|---|---|
| `booted-schema` exact / typo rank-1 | 99.8 / 74.1 % | 99.8 / 74.4 % |
| `alec-surface` exact / typo rank-1 | 100 / 86.3 % | 100 / 90.7 % |
| `real-corpus` typo hit@1 (both corpora) | 99.7 / 99.0 % | 99.8 / 99.2 % |
| `presyo-catalog` name+brand p@10 | 61.7 % | 62.6 % |
| `profstopick-dept` derived expansion | 65.0 % | 67.8 % |
| `blead-industry` held-out expansion | 75.9 % | 80.7 % |
| `maphy-place` one-typo hit@1 | 89.0 % | 89.4 % |
| `biasd-entity` arm A (FAILs on master too) | 71.7 % | 71.8 % |
| `sisia-catalog`, `presyo-broad`, `yclap-species` | — | unchanged |
| `real-corpus` cross-store p50 / p99 | 7.3–8.3 / 20.5–23.4 µs | 8.0 / 26.7–27.2 µs |
| `presyo-catalog` typo p50 | 297–303 µs | 326–365 µs |

The latency cost is the conjunction checks; before the one-check short-circuit the cross-store
p99 was 80–100 µs.
