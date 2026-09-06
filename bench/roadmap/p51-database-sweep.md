# P51 — the claim, tested against every database in the estate

**Tier:** T1 · **Evidence:** `bench/evidence/p51-database-sweep.tsv`
**Status: 122 databases, 86 indexed, 36 empty, 0 failures. One real bug found and fixed.**

`p49` claimed *"if your database can print rows, this can index them"* on the strength of one DuckDB
table and a CSV fixture. That is an assertion, not a measurement. This is the measurement.

## What was swept

| | |
|---|---|
| **machines** | 5 — Windows (`geldan-pc`), macOS (`gelos-macbook-pro`), 3× Linux (`bygelo`, `aipo`, `advo`) |
| **reached over** | Tailscale, by SSH. Nothing was installed on any remote host except a 90-line Python script that prints rows. |
| **engines** | PostgreSQL 16 / 17 / 18, SQLite, DuckDB |
| **databases** | **122** — 39 PostgreSQL, 83 SQLite files |
| **indexed** | **86** |
| **empty** | 36, each verified to have no table over 30 rows — scaffolds, migration shells, browser-profile artefacts |
| **failed** | **0** |
| **rows** | **2,226,041** (capped at 120,000 per table) |

Every index was built by the **local Windows binary** reading rows streamed over SSH. The rows crossed
an operating-system boundary, a CPU-architecture boundary and a network on the way in, and nothing in
the engine knew or cared.

## The one that mattered: `advo`, `aipo`, `bygelo`, `mac` — one command each

```sh
ssh bygelo 'sudo -u postgres psql -d presyo -c "COPY (SELECT ...) TO STDOUT (FORMAT csv, HEADER)"' \
  | index build -d data/ --schema '...' --key sku
```

Real results, unedited from the evidence file:

| database | table | rows | terms | probe | typo |
|---|---|---|---|---|---|
| `bygelo/presyo` | `raw_product` (4.5 M) | 120,000 | 135,183 | `Pringles` 10 | `Pringlse` 10 |
| `bygelo/blead_scraper` | `school` | 4,145 | 38,992 | `Maximo` 2 | `Maxiom` 2 |
| `geldan-pc/life` | `life_record` | 120,000 | 457,722 | `Novacane` 10 | `Novacaen` 10 |
| `mac/biasd` | `article` | 27,735 | 118,535 | `welcomes` 10 | `welcomse` 10 |
| `mac/blead/school-crm.db` | `school` (SQLite) | 60,922 | 258,004 | `Apaleng` 1 | `Apalegn` 2 |
| `aipo/tna` | `admin_audit` | 120,000 | 120,502 | `rafaelo` 4 | `rafaeol` 4 |

**Typo tolerance held on 86 of 86** probes that returned hits: every one also matched the same word
with two characters transposed. Where it correctly did *not* — `sisia`'s GPS device IDs, `tripi`'s
UUIDs — the token is numeric, and numeric tokens are never fuzzy-matched by design
(`dict::numeric_tokens_are_never_fuzzy_matched`). That is the rule working, not failing.

## The bug: real databases are not valid UTF-8

**Thirteen of the SQLite databases aborted the build outright:**

```
index: stream did not contain valid UTF-8
```

The reader took `String`, so a single stray byte anywhere in a 120,000-row export destroyed the
entire build. That is the wrong answer for a tool whose whole claim is "any database" — a `TEXT`
column in SQLite holds whatever it was handed, a `latin-1` dump is common, and a Windows export is
`cp1252` unless something says otherwise.

Now the reader takes **bytes**, repairs invalid sequences to U+FFFD, **counts** them, and the CLI
reports the count:

```
warning: 412 lines contained bytes that were not valid UTF-8 and were repaired;
         if that is unexpected, the export is probably not UTF-8
```

Repaired rather than skipped, because dropping the row would build an index quietly missing data.
Counted rather than silent, because a word containing the bad byte will not match what the source
held, and the operator should know their export pipeline is wrong.
`invalid_utf8_is_repaired_and_counted_rather_than_fatal` pins it. **All 13 now index.**

## Continuous sync, verified against a live production database

`p50` measured change-stream convergence against a model. This ran it against `bygelo/blead_scraper`
over Tailscale: 4,000 real schools indexed, then a change stream of one update, one delete and two
inserts — all rows taken from the database itself.

**Membership was exact**: the deleted key returns 0 rows, the updated key returns exactly 1, the
inserted keys are present. **21 of 21 real queries agreed with a full rebuild on both rank-1 and the
whole top-10.**

Disagreement appears only deeper, at `k=25`, and the cause is visible in the scores: the renamed
school scores **1.31 in the incremental collection and 7.01 in the rebuild**, because it now lives in
a *three-row* delta segment whose collection statistics are computed over three documents instead of
4,001. That is `p38`'s per-segment-statistics effect, reproduced on production data at its
pathological limit — a three-row delta is the smallest and therefore worst possible segment.

## The sweep's own errors, recorded because they were mine

The harness was wrong three times before it was right, and each failure looked like a product bug:

- **`-F|` unquoted** made the local shell treat the separator as a pipe, so every remote query
  returned nothing and 17 databases reported "no text table". The tool was fine.
- **Python's stdout on Windows is `cp1252`**, so the SQLite exporter was itself emitting non-UTF-8.
  That masked the real bug above — the CLI *was* also too strict, but the harness was feeding it
  garbage first, and the two had to be separated before either could be fixed.
- **`ssh` inside a `while read` loop consumes the loop's stdin**, so the Mac sweep processed exactly
  one database and stopped. `ssh -n` fixes it.

**None of these were visible without running against real data**, which is the argument for the
sweep existing at all.

## Limits

- **No MySQL, MongoDB or ClickHouse in this estate.** A scan of every `.env` in 151 local repos found
  no connection string for any of them, so the claim is verified for PostgreSQL, SQLite and DuckDB
  and remains *untested* for the rest. The mechanism is format-level, not engine-level, so there is
  reason to expect it to hold — but that is an expectation, not a measurement.
- **120,000 rows per table**, so `presyo`'s 4.5 M-row `raw_product` was sampled, not indexed whole.
- **One table per database**, chosen by row count with a preference for human-meaningful column
  names. A real adoption would pick the table and the columns deliberately.
- **The 36 empty databases were verified empty, not indexed.** They are scaffolds and test shells;
  counting them as successes would be dishonest, and counting them as failures would be wrong.
