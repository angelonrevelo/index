# P49 — "works on any database", and what that turned out to mean

**Tier:** T1 · **Crate:** `index-cli` · **Binary:** `index`
**Status: SHIPPED, 2026-09-06. Zero new dependencies. 23 CLI tests, `scripts/cli-smoke.sh` in CI.**

The request was "something that works on any database". The obvious reading is a driver per
database. That is the one thing that cannot work on *any* database, because it only ever works on
the ones somebody wrote a driver for.

## The claim, and the price

> **If your database can print rows, this can index them.**

No driver, no connection string, no dialect, no per-database integration. Every database already
ships a client that prints CSV, TSV or JSON, and every change-data-capture tool already emits
newline-delimited JSON:

```sh
psql -c "COPY (SELECT sku,name,brand FROM product) TO STDOUT (FORMAT csv, HEADER)" \
  | index build -d data/ --schema 'sku:0:0.6,name:3:0.4,brand:1:0.6' --key sku --facet brand

sqlite3 -header -csv app.db 'SELECT ...'  | index build -d data/ ...
mysql -B -e 'SELECT ...'                  | index build -d data/ --tsv ...
mongoexport --type json                   | index build -d data/ --jsonl ...
curl -s /api/product | jq -c '.[]'        | index build -d data/ --jsonl ...
```

**The price is that the pipe IS the integration.** Credentials, pagination, restarts and
back-pressure belong to whoever runs the command; the tool cannot resume a stream it did not start.
That is the trade, and it is stated in the binary's own `--help` rather than discovered: universal
reach and zero dependencies, against convenience for any one database.

**Verified against a real database, not a fixture.** DuckDB and PostgreSQL 18 were both already on
the build machine; the worked example in `CHANGELOG` is a DuckDB table piped straight in, searched
with a typo, then updated through `apply` and diffed against a rebuild.

## Zero dependencies meant writing three readers

`index-text` has two dependencies, both audited. The CLI has **one**: `index-text`. So the CSV, TSV
and JSON readers are hand-written — about 400 lines — and each earns its place:

- **CSV** must honour RFC-4180 quoting, because `psql COPY ... FORMAT csv` quotes commas and
  newlines inside addresses, and a `split(',')` shreds them. `scale` already carries that scar.
- **JSON** is not optional, because Debezium, wal2json and Mongo change streams are all JSON. It
  parses one line into dotted paths (`after.sku`) in a single pass with no document tree, which is
  *less* code than walking one would be. `null` is an **absent** path rather than the string
  `"null"` — otherwise every nullable column indexes a word no row really contains.
- **TSV** is what `mysql -B` prints.

A record is a `name -> value` map, never a positional tuple, which is what lets one set of flags
(`--key`, `--facet`, `--field`) serve all three formats.

## Refusing beats guessing, in three places

**A ragged row is an error.** A loader that silently drops rows it could not parse builds an index
quietly missing data, and the only symptom is a search that does not find something — which is
indistinguishable from the engine being bad at its job.

**An unknown `op` word is an error.** Guessing "upsert" for a word we do not understand would apply
a delete as an insert.

**`apply` on a keyless collection is refused**, rather than silently doing nothing addressable.

## A collection is a directory, and why

Deletes have to persist, and a tombstone lives inside a segment's own bytes. Applying a change
stream therefore rewrites the segments it touched *and* appends a new one — two files changing at
once, which a single-file index cannot express without rewriting everything on every change, the
exact cost `Searcher` exists to avoid.

```
data/  0000.idx  0001.idx  0002.idx
```

Loaded in name order, which is insertion order, which is what makes newest-wins key resolution
correct. **Zero-padded**, because a `1.idx, 10.idx, 2.idx` scheme loads out of order at the tenth
segment and silently inverts newest-wins — resurrecting superseded rows.
`segments_load_in_insertion_order_past_nine` writes twelve of them and requires the survivor to be
the twelfth.

Every write is a temp file renamed into place, because a process killed mid-write must not leave a
half-written segment that then refuses to open and takes the whole collection with it.

**The single-artifact case is a collection with one segment**, so shipping to a browser is still
"copy `0000.idx`".

## What it cannot do, stated in `--help` rather than discovered

**Compaction is a rebuild.** The engine does not store field text — that is *why* the index is small
— so it cannot regenerate a segment from itself. `index build` against the source of truth is the
compaction path, and `index stat` says when it is worth doing.

## Still open

- **No `--limit`/`--offset` or resumability**, so a very large table is one long pipe. Chunking it
  is the caller's job, and `apply` is the mechanism for doing it incrementally.
- **`search` prints TSV and cannot filter**, so facets, ranges, phrases and paging are reachable
  from Rust and the hosts but not from the shell. The engine has all of it; the CLI exposes the
  smallest surface that proves the pipeline works.
- **No `--dry-run`** on `apply`, which is the flag anyone pointing this at production would want
  first.
