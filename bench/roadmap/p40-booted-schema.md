# P40 — searching every database in the house

**Tier:** T1 · **Bin:** `booted-schema` (new) · **Corpus:** `~/.booted/schema.json`, read not committed
**Status: PASS, 2026-09-06. 2,600 tables, 54 databases, 5 machines, 30.0 M estimated rows.**

The goal says the engine should work with *all* the apps and databases. `booted` is the catalogue
that knows what those are — a schema crawl of every Postgres in the house, banked to
`~/.booted/schema.json`. Indexing that bank is both a new consumer and the most direct test of the
claim.

**Nothing is committed.** The bank is read from the user's own machine and the bin skips cleanly
when absent, exactly as the presyo and DepEd corpora are handled. No schema, comment or row count
enters the repository.

## A corpus shape nothing else here has

| | |
|---|---|
| tables | **2,600** |
| databases | **54** |
| machines | **5** (this Mac, bygelo, aipo, advo, geldan-pc) |
| tables with a prose comment | 1,702 |
| estimated rows catalogued | **29,994,659** |
| build | 0.06 s, 4,894 terms, 1.2 MB |

Short, **highly repetitive** names — `id`, `created_at`, `user_id` appear in nearly every table —
with a long prose comment attached. That is the opposite of a product catalogue, and it is the shape
that breaks BM25 tuned for product names.

It also answers the billion-row question with real numbers rather than ambition: **the largest table
in the house is `bygelo/presyo.price_event_2026_04` at 1,205,889 rows.** Every real table is inside
what `p7` already measures.

## The questions a developer actually asks

| question | how it is answered | result |
|---|---|---|
| *"Which table is `life_record`, and where?"* | name search, scored against the **set** of same-named tables across databases | **100 %** rank-1 |
| *"...and I typed it wrong"* | one-character typo | 73.7 % rank-1 |
| *"Where do we store scraped articles?"* | the table's own COMMENT, with every word that also appears in its name removed | **100 %** in top 10 |
| *"Which databases have a table matching `id`?"* | facet tally over the database field | tna 242, fourlinq 157, blueos 145 |
| *"Show me matching tables over 100,000 rows"* | numeric range over `estimatedRowCount` | 1.2 M row partitions surfaced |

The comment result is the striking one. **Removing the name words from the query is what makes it a
real test** — a hit cannot be the name matching by accident, so 100 % means the prose comment alone
locates the table. That is the query a developer actually has at 2 a.m.

Latency: exact p50 **9 us**, typo p99 **484 us**, against a 5 ms interactive bar.

## What it found in the engine

Exact-name rank-1 first measured **86.4 %**, below the 95 % bar. Rather than accept or explain it,
the bin prints the losing pairs — and every one has the same shape:

```
session                -> local/sideout.session_resource
user                   -> mac/akonga.user_agent
change_order           -> advo/fourlinq.change_order_line
account                -> local/wheresthefx_dev.account_preference
notification_delivery  -> local/sideout.notification_delivery_attempt
```

**A short exact name loses to a longer relative** whose comment and column list repeat the term.
BM25 sums term frequency across fields and has no notion that *the first field is exactly the
query* — a distinct intent, and the one a developer typing a table name has.

That finding produced `p41-exact-field.md`, which adds the signal. With it, this bin reads
**100.0 %**.

## Honest limits

- **One machine's bank.** The crawl includes five hosts, but it is a snapshot from one laptop; a
  stale bank indexes stale schema and nothing detects that here.
- **Typo rank-1 is 73.7 %**, much lower than on product names. Schema identifiers are short, share
  long prefixes (`price_event_2026_04` vs `_05`), and a one-character corruption often lands
  exactly on the part that distinguishes siblings. Not investigated.
- **No code graph.** `booted` also records which files and routes touch each table; indexing that
  would answer *"where is this table used"*, and it is not done.
- **Comments are documentation, not data.** They are indexed as prose; nothing checks whether they
  are accurate or current.
- **Table-level only.** One document per table. Column-level documents would answer *"which table
  has `organization_id`"* directly rather than through the merged `about` field.

## Reproduce

```sh
cargo run -p index-bench --release --bin booted-schema
BOOTED_SCHEMA=/path/to/schema.json cargo run -p index-bench --release --bin booted-schema
```
