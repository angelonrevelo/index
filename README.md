<p align="center">
  <img src="https://img.shields.io/badge/language-Rust-orange" alt="Rust">
  <img src="https://img.shields.io/badge/target-wasm32-654ff0" alt="wasm32">
  <img src="https://img.shields.io/badge/index--core-zero%20dependencies-brightgreen" alt="index-core has zero dependencies">
  <img src="https://img.shields.io/badge/status-research-lightgrey" alt="status: research">
</p>

# index

**Typo-tolerant search for the data your app already has — embedded, no search server required.**

Most apps that want good search end up renting it: Algolia, Elasticsearch, Meilisearch — another
server to run, another copy of the data to keep in sync. `index` is an attempt at the other way
round: a search engine that lives *inside* your program, builds from whatever your database can
print, and writes one index file that the command line, a server, a Node script and a browser tab
can all read.

- **Any database, over a pipe.** If it can print rows as CSV, TSV or JSON, it can be indexed — no
  driver, no connection string. A SQL dump file works too.
- **Forgiving queries.** `colgaye tothpaste` finds Colgate toothpaste. Quoted phrases, `-exclude`,
  facets, typeahead, and Filipino grocery aliases (`bigas` → rice) are built in.
- **Stays current.** Feed it a change stream (Debezium, wal2json) and it applies inserts, updates
  and deletes instead of rebuilding.
- **Runs in the browser.** The same index file is stored in the browser's own filesystem (OPFS) and
  searched with zero network calls per query.

> Status: a measured research engine, not a deployed product. It is benchmarked against the real
> data of several apps but not wired into any of them yet. The honest limits are listed in
> [docs/internals.md](docs/internals.md#honest-limits-today).

## Quick start

You need a Rust toolchain (1.74+).

```sh
cargo install --path crates/index-cli     # installs the `index` command

# build an index from Postgres, straight over a pipe
psql -c "COPY (SELECT sku,name,brand FROM product) TO STDOUT (FORMAT csv, HEADER)" \
  | index build -d data/ --schema 'sku:0:0.6,name:3:0.4,brand:1:0.6' --key sku --facet brand

# ...or from SQLite, MySQL, MongoDB, an API, or a dump file
sqlite3 -header -csv app.db 'SELECT ...'   | index build -d data/ ...
mongoexport --type json                    | index build -d data/ --jsonl ...
index build -d data/ --sql --table product --schema '...' --key sku < dump.sql

# search it
index search -d data/ 'colgaye tothpaste'
index search -d data/ 'red "ice cream" -discontinued'

# keep it current from a change stream
pg_recvlogical -S idx -f - --start -P wal2json | jq -c '...' \
  | index apply -d data/ --jsonl --key after.sku,before.sku
```

## Build and test

```sh
bash scripts/gate.sh          # the full gate: clippy, workspace tests, wasm, JS/Python hosts, CLI smoke
cargo test --workspace        # engine tests only
bash scripts/cli-smoke.sh     # the `index` CLI end to end
./scripts/build-wasm.sh       # the WebAssembly artifacts into dist/
node js/opfs-check.mjs        # the browser tier, in a real Chromium
```

On Windows without Visual Studio, use Rust's GNU toolchain — see
[docs/internals.md](docs/internals.md#windows--no-visual-studio).

## Configuration

The engine reads one environment variable; everything else is passed in code.

| variable | what it does |
|---|---|
| `INDEX_PARALLEL` | `1` lets a searcher spread per-segment work across threads. Off by default. |

The benchmark harness reads more; they are listed in [bench/README.md](bench/README.md).

## How it works

```
 database / dump / API ──► CSV | TSV | JSONL ──► index build ──► .idx file
 change stream (CDC)   ──────────────────────► index apply ──┘      │
                                                                     ▼
                      CLI  ·  Rust library  ·  C ABI (Python)  ·  WASM (Node, browser + OPFS)
```

Text goes through a normalizer, a typo-tolerant term dictionary (an FST searched with a Levenshtein
automaton) and BM25F field-weighted ranking. New rows land as extra segments with tombstones for
deletes, so updates never need a full rebuild until `index stat` says compaction is due.

| crate | role |
|---|---|
| `index-core` | dependency-free core structures (learned index, FM-index, cracking) |
| `index-text` | analyzer, term dictionary, BM25F, the index format |
| `index-cli` | the `index` command |
| `index-wasm`, `index-geo-wasm`, `index-accel` | WebAssembly builds |
| `index-geo`, `index-image` | geo and image tiers |
| `index-bench` | benchmark harness |

## Links

- [docs/internals.md](docs/internals.md) — full status, every measurement, limits and dependency policy
- [ROADMAP.md](ROADMAP.md) · [CHANGELOG.md](CHANGELOG.md) · [docs/roadmap-rejected.md](docs/roadmap-rejected.md)
- [docs/benchmarks.md](docs/benchmarks.md) — numbers, including what fails
- [docs/adoption.md](docs/adoption.md) — what shipping it into each app would take
- [docs/research/](docs/research/) — the evidence behind each roadmap row
