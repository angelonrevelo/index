# Landscape — the embedded / drop-in search engine field, September 2026

> Web sweep 2026-09-05. Every performance number carries a source URL + date. `UNVERIFIED` =
> searched, not found. Vendor-authored benchmarks are labelled as such.
>
> **The honest headline: almost nobody in the embedded category publishes p50/p99 at
> 100 k / 1 M / 10 M docs.** Only Quickwit and LanceDB Enterprise have real percentile tables. Any
> latency grid you see for Tantivy, Bleve, MiniSearch, Orama, FlexSearch or Pagefind is
> reconstructed, not published. That absence is itself an opportunity — see §7.

---

## 1. Who is alive, who is dead

**Dead or dying, and three of them still look alive:**

| Project | Status |
|---|---|
| **Stork** | **Dead.** Last commit 2023-07-01; [wind-down announced June 2023](https://github.com/jameslittle230/stork/discussions/360). Pagefind is the replacement. |
| **ZincSearch** | **Archived 2026-08-18** (last release 2024-01-14). |
| **Marqo (OSS)** | **Vendor-deprecated** (notice 2025-11-13, no Docker image since 2025-03-16) — still looks alive. |
| **MiniSearch** | **Stalled**, last commit 2025-09-16. The circulating "500 KB per 1000 docs" figure does not exist in the author's post. |
| **OramaCore** | AGPL-3.0, stalled (last commit 2026-04-14); its own README needs an A100/H100. |
| **lnx** | Dead, last release 2022-06-25. |
| **pgvecto.rs** | Deprecated in favour of VectorChord. |
| **Neon `pg_embedding`** | Dead end — its "20× faster than pgvector" compared HNSW to IVFFlat *before* pgvector shipped HNSW. |
| **InstantDB** | Sunsetting (services to 2027-08-31, team joined OpenAI). |

**Alive and relevant:** Tantivy 0.26.1, Lucene 10.5.1 (**there is no Lucene 11**), Quickwit 0.9.0
(**Datadog acquisition 2025-01-09 did not kill it — the Apache-2.0 relicense happened**), Bleve
2.6.1 (2026 added FAISS RaBitQ + GPU), sonic 1.8.1 (v1.8.0 added BM25-lite), Pagefind 1.5.2,
SQLite FTS5, DuckDB fts 1.5.5, Lance/LanceDB 11.0.0, **SeekStorm 3.3.8** (2026-09-03).

**SeekStorm deserves a note** — explicitly "in-process library & multi-tenancy server", indexes
"either in RAM or memory-mapped files". It is the closest existing thing to what `index` is
becoming. At **55 K lifetime downloads vs Tantivy's 17.6 M**, with vendor-authored benchmarks, it is
a **reference design, not a dependency**.

## 2. The numbers that actually exist

- **Pagefind** — the best number in the browser category: **10,000-page site, < 300 kB total network
  payload** ([pagefind.app](https://pagefind.app/)); 2,496 pages indexed in 2.357 s; v1.5 cut chunks
  ~45 %. *Alpine/musl users must be on 1.5.2 — 1.5.0 halved indexing speed.*
- **SQLite FTS5** — the best index-size number anywhere: 1,636 MiB email corpus →
  **743 MiB (`detail=full`) / 340 MiB (column) / 134 MiB (none)** = 45 % / 21 % / 8 %
  ([sqlite.org/fts5.html](https://sqlite.org/fts5.html)). **`detail=none` is a 5.5× index-size win
  if you don't need phrase queries.** *CVE-2026-11822 fixed in 3.53.2 — a hard version floor for
  untrusted files.*
- **LanceDB Enterprise**, 1 M × 1536-d: vector **p50 25 ms / p99 35 ms**; FTS **p50 26 ms / p99
  42 ms**; vector + broad filter p50 65 ms / p99 100 ms
  ([docs](https://docs.lancedb.com/enterprise/benchmarks)). **Tantivy was removed from LanceDB in
  2026** for a Lance-native BM25 index, explicitly to stop index/data drift.
- **Best independent datapoint in the survey** — DuckDB benchmarked Lance themselves: cold indexed
  vector search **12 ms vs Parquet 761 ms**
  ([2026-05-21](https://duckdb.org/2026/05/21/test-driving-lance)).
- **Typesense** — the only vendor benchmark with hardware and reproducible datasets: 2.2 M
  recipes / 4 vCPU **11 ms avg, 104 QPS, 900 MB RAM**; 28 M books 28 ms, 14 GB
  ([updated 2026-05-15](https://typesense.org/docs/overview/benchmarks.html)). Rows are **not
  version-stamped**.
- **sqlite-vec** — stable is **v0.1.9 (2026-03-31), still not 1.0**, repo quiet since 2026-05-18.
  Author's own 1 M-vector numbers: 3072-d float **8.52 s**, 192-d 192 ms, binary 124 ms —
  *"None of the float vectors at any dimension pass the 100 ms smoke test."* **Practical ceiling
  ~100 k vectors.**
- **Vespa's "12.9× vs Elasticsearch"** is written by its CMO, states it is not independent, and
  omits versions, hardware and absolute latencies. Discard.
- **Qdrant's benchmark page is ~2 years stale (2024).** Qdrant and Weaviate **both have no
  fuzzy/typo tolerance at all.**

## 3. Search inside Postgres — the category `index` is aiming at

**ParadeDB / pg_search 0.25.6 (2026-08-27), AGPL-3.0.** The mechanism is genuine, not marketing:
a native index access method via `pgrx` plus Custom Scan providers, queried with `@@@`, and
**Tantivy segments serialized into Postgres 8 KB blocks** via a custom `MVCCDirectory` — so they get
shared buffers, WAL through the generic buffer API, and physical replication
([2025-01-16](https://www.paradedb.com/blog/block-storage-part-one)). It is an LSM tree; merges run
in background workers.

**The benchmark retreat is the tell.** 2023: *"nearly identical to a dedicated Elasticsearch
instance."* 2026: they benchmark **only against Postgres FTS** — 1 M HN posts, 4 cores/8 GB, 50 QPS
open-loop: **p99 5.11 ms vs 238 ms (47×)**; 40-term rotation 3,373 vs 115 QPS (29×); ~3× the memory;
and **Postgres FTS is faster for 1–10-doc result sets**
([2026-06-02](https://www.paradedb.com/blog/benchmarker-iteration)). The one independent test
contradicts the old ES claim: **ES 360 vs ParadeDB 151 TPS at 1 client, 795 vs 184 at 10**
([inevolin](https://github.com/inevolin/ParadeDB-vs-ElasticSearch)) — ParadeDB wins ingest, loses
query throughput.

**Limits:** vertical only, "1–10 TB comfortable"; **all indexed columns fixed at CREATE INDEX —
adding one needs a full REINDEX**; **not on RDS**; vector search delegated to pgvector, not native.
Open bugs as of 2026-09-05 include a concurrent-insert panic (#6206), a segfault (#6168) and a
wrong-results hash-join bug (#6158). Company: $12 M Series A 2025-07-14, ~13 staff, no acquisition.

**The 2026 flank attack: Tiger Data's `pg_textsearch` v1.4.0 (2026-08-25)** — native Rust BM25,
on-disk paged memtable as LSM L0, WAL-logged via `GenericXLog`, Block-Max WAND — **under the
permissive PostgreSQL license**, against ParadeDB's AGPL. Vendor claims on MS-MARCO 138 M:
single-term **5.11 ms vs 59.83 ms**, throughput 198.7 vs 22.8 tps, index 17 GB vs 23 GB
([2026-03-31](https://www.tigerdata.com/blog/pg-textsearch-bm25-full-text-search-postgres)) —
**unrebutted and unreproduced.** It hit the same wall as ParadeDB: v1.2.0 "introduces proper support
for physical replication, which was essentially broken in previous releases."

> **The BM25-on-Postgres war fragmented rather than converged**: ParadeDB (AGPL), pg_textsearch
> (PostgreSQL license), VectorChord-BM25 (AGPL/ELv2 — honest enough to disclose that
> **Elasticsearch wins Top-10 at 341 vs 271.91 QPS** even while it wins Top-1000 ~3×), and pg_fts.
> **None is in core; none is on RDS or Cloud SQL.** Choosing one is a lock-in decision.

## 4. Why Postgres-native search fails — with numbers

**`ts_rank` is not BM25** and is missing three specific things: no IDF, no TF saturation, and no
corpus-average document-length normalization (the `normalization` bitmask normalizes by *this*
document's length, never `avgdl`). Postgres' own docs concede *"it is impossible to produce a fair
normalization"*. And `websearch_to_tsquery` — the only parser safe for raw user input — **refuses
prefix (`:*`) labels**, so you cannot do autocomplete through it.

**The structural failure is rank + LIMIT.** GIN produces a **bitmap, never an ordered iterator**: no
ORDER BY pushdown, no LIMIT pushdown, no block-max WAND, no early exit; posting lists hold only
6-byte TIDs, so **ranking always requires a heap fetch per candidate**.

| Case | Measurement |
|---|---|
| 12 M rows, `@@ plainto_tsquery ORDER BY timestamp DESC LIMIT 24` | **~54 ms without ORDER BY vs ~188,442 ms with it (~3,500×)**. Tom Lane in-thread: *"GIN and GIST indexes cannot be used by the ORDER BY"* |
| `ts_rank` sorting | **< 1 s → 25–30 s at 800,000 rows**; "significant deterioration around 1–2 M rows" |
| 100 M rows, filtered top-K | **37 s native FTS · 33 s with a partial index · 300 ms with block-max WAND** |
| `ts_headline` before the LIMIT | **200,000 re-parses to display ten snippets** — called *"the single most common performance bug in Postgres search implementations"* |
| `gin_fuzzy_search_limit` | **returns a random subset** — buys latency by discarding correctness |

**GIN write stalls are real.** `gin_pending_list_limit` defaults to 4 MB; the unlucky backend that
trips it blocks on the whole flush. Canonical case: ~20 M rows, most writes < 10 ms but **some
INSERT/UPDATE > 40 s, occasionally hitting a 100 s timeout**; dropping to 128 KB eliminated the
stalls, while `fastupdate=off` removed variance at **3–6× throughput cost**. GitLab filled the
pending list **every 2.7 seconds** at peak. **On insert-only tables autovacuum may never run, so
pending-list space is never reused** — *"horrible bloat"*. `fastupdate=on` degrades reads too: 10 M-row
log table **~41,301 ms → ~877 ms (~50×)** from a materialized tsvector column plus `fastupdate=off`.

**pg_trgm's failure modes are specific.** 10 M rows: `LIKE` **639 ms → 4.1 ms (~155×)**. But
**patterns under 3 characters extract no trigrams → guaranteed seq scan**, fatal for exactly the
type-ahead case. At low selectivity a trigram-indexed query still took **3 seconds** on 10 M rows,
with the planner **estimating 54 rows against an actual 51,874 (~960× under-estimate)**, poisoning
the join plan. And you must choose: **GIN loses string length so it can't order by similarity; GiST
can do `<->` KNN but is lossy.** No index does both.

**PG17/PG18 changed essentially nothing for relevance.** PG18 added parallel GIN builds and an
Estonian stemmer. **No ranking changes. No BM25. No top-k pushdown.** Its async I/O covers
sequential scans, bitmap heap scans and vacuum — **explicitly not index scans** — measuring 15.07 s
sync → 5.72 s io_uring cold. That helps FTS (GIN only produces bitmap heap scans) but it makes
fetching all N candidates *faster*, not *fewer*. **An O(matches) algorithm with a 2–3× constant is
still O(matches).**

> **This is the single most important external validation of presyo's measured pain.** Its 7-lane
> `pg_trgm` UNION, its inline `ts_rank`, its 17 s → 400 ms index-unreachable query and its 3–10 s
> home feed are not implementation mistakes. They are the documented, structural properties of
> Postgres FTS, hit by someone who did the work correctly.

**pgvector 0.8.6.** Read the changelog honestly: **three separate HNSW vacuum/corruption fixes
landed in five weeks mid-2026** (0.8.3 "index corruption with HNSW vacuuming" 2026-06-17; 0.8.4
"hnsw graph not repaired" 2026-06-30). Katz's dbpedia-1M numbers: **IVFFlat 8 QPS / p99 150 ms →
HNSW 253 QPS / p99 5.51 ms (~30×)**; binary quantization cuts build 474 s → 49 s and index
7.56 → 0.46 GiB. **halfvec is close to free.** **Binary quantization is not universally safe** —
SIFT-128 collapses to 2.42 % recall, GIST-960 to 0.00 %. Two traps: HNSW indexes only to 2,000 dims
for `vector`, and **iterative scans — the fix for filtered search returning 3 rows instead of 10 —
are off by default**.

> sisia-app hit the 2,000-dim ceiling for real: `migrations/042` records "No HNSW index: pgvector's
> HNSW caps at 2000 dimensions (we use 3072)", then `043` reverts 3072 → 768.

## 5. The sync problem — the actual reason people hate search infra

**Postgres logical replication.** Slot position persists **only at checkpoints**, so a crash
re-sends — **every sink must be idempotent upsert-by-key**. What it will not give you: **no DDL, no
sequences, no large objects**; a publisher-side schema change **errors the stream until fixed**;
without a replica identity there is no before-image, so **you cannot know which document to
delete**. Slot bloat dominates: `max_slot_wal_keep_size` defaults to **`-1` = unlimited**; PG18's
`idle_replication_slot_timeout` defaults to **`0` = disabled**. Write-heavy OLTP produces
**20–50 GB WAL/hour**. The nastiest case is *low* traffic — WAL is cluster-wide but slots are
per-database, so RDS's 5-minute heartbeat on 64 MB segments retains **~18 GB/day on an idle
database** ([Morling](https://www.morling.dev/blog/insatiable-postgres-replication-slot/)). Capping
the slot just chooses invalidation and a **full re-snapshot** instead of a full disk. **PG17 added
failover slot sync, but `sync_replication_slots` defaults to `off`.** And decoding cannot emit until
commit — **one 40-minute batch job = 40 minutes of index staleness.**

**Debezium** 3.6.2: default **5–15 k events/s**, tuned 30–80 k, ceiling 50–100 k before sharding.
**Backpressure is the killer** — a slow sink throttles the reader, WAL accumulates, and you are back
in slot bloat. The **TOAST `__debezium_unavailable_value` placeholder** for unchanged large columns
will **silently wipe that field in your index** if the sink upserts blindly. End-to-end p99 for a
Debezium→index pipeline: **UNVERIFIED — nobody publishes it.**

**Do not use LISTEN/NOTIFY to wake an indexer on a hot table.** `NOTIFY` takes
`AccessExclusiveLock` on the entire database during commit, serializing every commit. Recall.ai took
three outages 2025-03-19→22 with the signature of a mutex bottleneck — load spiking while **CPU and
I/O plummet** — and logs showing one process holding the lock **1015.921 ms** with hundreds queued
([2025-07-01](https://www.recall.ai/blog/postgres-listen-notify-does-not-scale)).

**The incumbents have publicly given up on this.** Meilisearch maintainer, **June 2025**: *"we've
identified some more pressing priorities at the moment, so the database sync tool has been
deprioritized for now"* ([discussion #765](https://github.com/orgs/meilisearch/discussions/765)) —
the request opened 2023-07-09 and is still unimplemented; their docs punt to Debezium. **Typesense
delegates to Airbyte and Sequin.** **Algolia keeps a copy of your data as records** and runs its
crawler on a schedule; **recrawl frequency and freshness SLA are not published**. Its silent
index-breakers: **record size 10 KB (Free) to 100 KB** — oversize records are rejected — 10,000
indexing ops per Unit, throttling at 100 pending requests.

**Sync engines, 2026 shakeout.** **Electric is joining Databricks (2026-08-11)** and publishes the
only reproducible sync benchmarks: with **optimizable where clauses (`field = constant`),
live-update latency is 6 ms flat regardless of shape count**; with non-optimizable clauses
(`ILIKE`), latency grows **linearly to ~0.1 s at 10,000 shapes**
([benchmarks](https://electric.ax/docs/reference/benchmarks)). **The predicate shape decides whether
fan-out is O(1) or O(shapes)** — the single most transferable lesson in this section. **Rocicorp
Zero GA'd at 1.0 in March 2026**; its docs warn **mutators run multiple times**. **PowerSync's
checkpoint model makes intermediate inconsistent states structurally impossible.**

**Index as materialized view.** Postgres core still has **no incremental refresh in 2026**.
**pg_ivm** 1.14's unsupported list is the disqualifier: **no window functions, no HAVING, no ORDER
BY, no LIMIT/OFFSET, no UNION/INTERSECT/EXCEPT**, and *"IVM is not effective when a base table is
modified frequently"*. **The cleanest published design is TimescaleDB continuous aggregates**: a
materialization threshold lags `now`, exploiting the fact that nearly all inserts carry recent
timestamps, so **most inserts do no invalidation work at all**. **The threshold *is* the staleness**
— and that generalizes: you buy write throughput by accepting a bounded lag window.

**Who solved it well: Convex**, by refusing to have two systems — its search index is *"automatically
reactive, consistent, transactional… They even include new documents created with a mutation"*
(capped at a 1,024-result scan). Then Electric (measured, predicate-aware), TimescaleDB (explicit
bounded trade), PowerSync (checkpoints).

**What breaks universally:** (a) **deletes** — ORM hooks miss bulk SQL, polling on `updated_at`
cannot see a deleted row, logical replication needs a replica identity; (b) **idempotency** — every
layer is at-least-once, so any design assuming exactly-once is wrong; (c) **the re-snapshot is the
real SLA** — slot invalidation, schema change, connector loss and sync-rule redeploy all converge on
"rebuild the index."

## 6. What "1 M rows, sub-10 ms" actually costs

**Nothing at 1 M rows is hard if the index is in page cache and the query is one term.** ParadeDB's
own pass 1 proves it: on 1 M HN posts a single repeated term gives **Postgres FTS 3,290 QPS at
1.49 ms median vs ParadeDB 3,290 / 1.33 ms — within ten percent.** The gap appears only when the
query set rotates (115 vs 3,373 QPS) or under open-loop offered load (p99 238 ms vs 5.11 ms).
**The closed-loop/open-loop distinction alone changes the answer by more than an order of
magnitude** — ParadeDB says so themselves.

So sub-10 ms at 1 M requires, concretely:

1. **The working set in RAM** (see [`speed.md`](speed.md) §3 for the DRAM-vs-NVMe arithmetic).
2. **A top-k algorithm, not a sort.** This is the entire delta between Postgres FTS and every real
   search engine, and why 37 s becomes 300 ms at 100 M rows.
3. **No network hop you don't control.** A 1 ms engine behind a 30 ms hop is a 31 ms product.
4. **An honest query mix.** Adrien Grand's critique of search-benchmark-game is definitive: **no
   index size or indexing time reported, all data in memory, no deletions, single-field queries,
   ~6 M docs, on an AVX-512 instance that favours vectorization**
   ([2025-05-12](https://jpountz.github.io/2025/05/12/analysis-of-Search-Benchmark-the-Game.html)).
   The benchmark is now maintained by **turbopuffer, a vendor**.

**Where each stack breaks at 1 M:** Postgres FTS — rank+LIMIT, at 800 k–2 M rows. pg_trgm — sub-3-char
patterns and low selectivity. SQLite FTS5 — `ORDER BY rank`, single-writer. sqlite-vec — its own
author says nothing float passes a 100 ms smoke test at 1 M. Tantivy/Lucene — segment count and merge
policy. HNSW — **filtered vector search is the unsolved case** everywhere except Vespa. **Fuzzy —
runtime scales with unique terms in the index, not document count**, so a large *vocabulary*, not a
large corpus, is what makes typo tolerance expensive.

> That last line is exactly what [`bench/roadmap/p5-fuzzy-term-feasibility.md`](../../bench/roadmap/p5-fuzzy-term-feasibility.md)
> measured independently: fuzzy p99 tracks vocabulary (0.66 ms at 12 K terms → 2.86 ms at 848 K),
> while document count is irrelevant.

## 7. Five gaps nobody fills well

1. **An embedded engine that reads the source database's own storage instead of copying it.** Every
   "no upload" claim today means "we copy your rows into our pages." ParadeDB and pg_textsearch put
   segments in Postgres blocks — a real improvement over a sidecar — but you still pay a full second
   copy, and **you cannot add an indexed column without a full REINDEX**.
2. **A first-party, correctness-first Postgres→index connector — with deletes.** Meilisearch
   **publicly deprioritized it in June 2025**; Typesense delegates; Algolia sells a crawler. The
   unowned hard parts are all *correctness*, not throughput: TOAST placeholders blanking fields,
   deletes needing a replica identity, DDL breaking the stream, re-snapshot as the only recovery.
3. **A published, bounded staleness contract.** TimescaleDB's materialization threshold and
   Electric's shape-predicate benchmarks are the **only two places in this entire survey where a
   vendor states how stale your index is and why. No search product publishes an end-to-end p99
   index-visibility number.** Not Debezium, not Algolia, not Meilisearch.
4. **Cheap fuzzy over a large vocabulary, and correct filtered ranking.** Weaviate, Qdrant and Marqo
   have **no typo tolerance at all**. Filtered top-k is equally open: Vespa integrates filtering into
   ANN traversal, pgvector 0.8 bolted on iterative scans that are **off by default**, everyone else
   post-filters and quietly loses recall.
5. **A browser/edge tier that shares one index format with the server.** Pagefind proves chunked
   static search works, but its index is build-time-frozen and bespoke. **LanceDB closed WASM as
   "not planned"; Tantivy's WASM RFC has been open since 2019 and is read-only; Orama's Rust core
   has no wasm32 target.** There is no engine where the same index file serves an OPFS-backed
   browser client, an embedded server process and an object-store cold tier — despite
   `opfs-sahpool` having removed the COOP/COEP and Safari blockers that were the standing excuse.

> Gaps **3, 4 and 5** are the ones this repo's evidence base already speaks to: it measures p99 by
> default, it has a measured fuzzy result, and it has a consumer (profstopick) whose whole problem
> *is* one index in a browser. Gaps 1 and 2 are bigger businesses and much harder.

## 8. Three things to act on immediately if any of this touches the stack

- **pgvector 0.8.0–0.8.2 with HNSW on a churning table** is exposed to three separate corruption
  bugs fixed in 0.8.3/0.8.4. presyo runs pgvector HNSW — **check the version.**
- **SQLite below 3.53.2** has an FTS5 memory-corruption CVE if it accepts untrusted database files.
- **Pagefind 1.5.0 on Alpine/musl** halves indexing throughput versus 1.5.2.

## Coverage caveats

The sweep hit a 200-call web-search ceiling. debezium.io returned 403 throughout, so its version and
throughput figures are snippet-sourced. Algolia's latency/SLA claims, a trigger-overhead benchmark,
Materialize/Feldera latency, and `REFRESH MATERIALIZED VIEW CONCURRENTLY`'s quantified cost are
**UNVERIFIED** and worth a second pass. Lucene's and Elasticsearch's nightly numbers live in JS
dashboards that do not render to a fetcher.
