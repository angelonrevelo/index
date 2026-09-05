# Claims — what practitioners say on X and in public, and whether it holds

> Sweep run 2026-09-05 through the `grok` CLI with live X + web access. This file records what
> people who ship search *claim*, with a credibility verdict on each. It exists so the roadmap
> can cite practitioner evidence without re-litigating it, and so hype is filed as hype.

---

## The one-line summary

> **The millisecond-at-millions club is not a secret algorithm. It is inverted indexes, pruning,
> RAM, or a hot cache.** Everything else is a bet on the *querier* (agents) or the *scorer* (late
> interaction), not on a new way to store postings.

## 1. Learned indexes — the verdict that reshapes this repo

**Almost nobody has shipped a learned index as the user-facing lookup path of a production search
engine.** A few database research systems put PGM/ALEX *under* LSM fence pointers. Search engines
still use B-trees, tries, and inverted indexes.

The most honest recent statement, **MountDB** ([arXiv 2605.23815](https://arxiv.org/abs/2605.23815),
May 2026):

> *"Despite these benefits, adoption in production systems remains limited, partly because learned
> indexes that support concurrency and persistence as effectively as, e.g., the B+-Tree, do not yet
> exist, while many research prototypes introduce substantial complexity."*

What MountDB actually did — a RocksDB fork: ALEX + "skeletonization" in the memtable; **PGM as SST
fence pointers**, predicting a *block* not a row, so still one I/O; up to 1.5× writes / 2.1× reads
vs SOTA LSMs; PGM + offsets < 172 KB per SST.

Wongkham et al. / VLDB experimental surveys: updatable learned indexes (ALEX, LIPP) beat
ART/Masstree/HOT on > 80 % of the single-thread data-workload space; **PGM wins pure-write because
it is LSM-shaped, not because the model is magic**; real datasets are "easy" (smooth CDFs); writes
and concurrency are where they lose. VLDB 2024's "Why Are Learned Indexes So Effective but Sometimes
Ineffective?" concludes you still need a cost model and last-mile binary search.

**Andy Pavlo did not spend 2024–2026 evangelising learned indexes.** His public output is "vector
DBs won't replace SQL" ([Firebolt, Jun 2024](https://www.firebolt.io/blog/vector-databases-wont-replace-sql---andy-pavlo)),
OtterTune's death, and joining ClickHouse. No thread of the form "we shipped ALEX in production
search" exists. What *did* ship adjacent: learned Bloom filters as research; ClickHouse / Umbra /
CedarDB shipping aggressive **traditional** indexes (sparse primary, skip indexes, now inverted
text).

> **Verdict, quoted:** *"If you are building a search engine in 2026, a learned index is a possible
> replacement for fence pointers over sorted doc-id/key arrays, not for postings or BM25. Nobody
> credible is using PGM/ALEX as the retrieval algorithm."*
>
> This independently confirms [`demand.md`](demand.md) Finding 0, arrived at from the opposite
> direction (what the house's apps need). Two independent lines of evidence, same conclusion.

## 2. The cost complaints are real and quantified

| Case | Numbers | Source | Credibility |
|---|---|---|---|
| Huntress: Elastic → ClickHouse | **$70 k/mo → $5 k/mo**, "same data, same queries", 4 M endpoints | [ClickHouseDB, 5 Apr 2026](https://x.com/ClickHouseDB/status/2040761699569991808) | Medium-high — vendor case study, SIEM/logs not product search; 14× is right for columnar vs inverted-index-on-JSON |
| Trip.com: 50 PB logs | "**Storage space savings exceeded 50 %… Query speed is 4 to 30 times faster than ElasticSearch, with a P90 of less than 300 ms and a P99 of less than 1.5 s**" | [ClickHouse blog, 12 Jun 2024](https://clickhouse.com/blog/how-trip.com-migrated-from-elasticsearch-and-built-a-50pb-logging-solution-with-clickhouse) | High |
| Workday, tens of PB | "**cut compute costs by 57 %**" *without leaving Elastic* — pet-clusters → "cluster of clusters" | [SRECon Americas, 27 Mar 2025](https://www.usenix.org/conference/srecon25americas/presentation/santos) | High — the "we kept Elastic and still had to invent an architecture" confession |
| Algolia per-search | "$0.50/1K searches seems cheap until you're doing 10 M searches/month = **$5 K/mo**" | [Meilisearch, 24 Oct 2025](https://x.com/meilisearch/status/1981647014925402324) | The multiplier is real — it *is* Algolia's business model. Competitive source. |
| Algolia vs Typesense | "For the equivalent workload that costs **$1,200 on Algolia**, Typesense Cloud runs around **$60 to $120**" | [veldsystems.com](https://veldsystems.com/blog/algolia-vs-typesense-vs-meilisearch), 2024 | Independent |
| Astro Vault, 500 k docs / 5 M searches/mo | Algolia **$3,000/mo** · Elastic Cloud **$1,500** · self-hosted OpenSearch **$800** | secondary | Directional |
| Self-managed ES TCO | **$200 k–$750 k/yr** (infra $50–300 k + 1–3 FTEs $150–450 k) | 2026 pricing roundups | Plausible for a serious cluster; **not** a $95 starter |

**And the non-dollar complaint, which matters more here:** dual-write CDC, mapping explosions, and
"the index is 5 minutes behind the DB" are the recurring reasons teams try `pg_search` /
Typesense-on-Postgres / FTS5. Simon Eskildsen (turbopuffer, ex-Shopify), on operating Elasticsearch:

> *"it's probably the worst database I've operated in my life and part of this company is my
> vendetta against it."*

Biased anecdote, widely echoed.

## 3. Search inside the primary database is the 2026 default

**ParadeDB `pg_search`** — the most serious "search that runs on your own database" of 2024–2026.
Built on **Tantivy via pgrx**. Claims: *"Query times over 1M rows are 20x faster compared to
tsquery and ts_rank"*; *"Indexing and search times are nearly identical to those of a dedicated
Elasticsearch instance"*; index layout is *"an LSM tree, where each segment consists of both an
inverted index and columnar index"*
([paradedb.com/blog/introducing_search](https://www.paradedb.com/blog/introducing_search)).
Independent measurement (Vineeth Pothulapati, 1.6 M products): ParadeDB **92 ms FTS vs Postgres
401 ms**; **fuzzy 139 ms vs 22,838 ms**. Caveat: you need `key_field` indexed or you pay a hidden
join tax (GitHub discussion #1628).

**sqlite-vec** (Alex Garcia) — *"1 million 128-dimensional vectors in just 17 ms; 500,000 with
960-dimensional vectors in 41 ms"*, *"written purely in C… run anywhere SQLite runs — including
WASM"* ([blog](https://alexgarcia.xyz/blog/2024/building-new-vector-search-sqlite/index.html)).
Technique is **brute-force KNN over chunked vector pages — no HNSW, and that is the point.**
Independent benches: ~45 ms at 1 M, **~400 ms at 10 M** — exactly what linear scan predicts. Simon
Willison documented Garcia's hybrid FTS5 + sqlite-vec via **RRF in pure SQL**.

**SQLite FTS5** — inverted index, `bm25()` ranking, `highlight()`/`snippet()`, recommended since
3.9.0 (2015). WASM: `fts5-sql-bundle` (~857 KB) enables it in sql.js. Turso's WASM FTS is gated
because Tantivy needs threads/mmap; they got 12 tests passing with `SingleSegmentIndexWriter`.
**Boring, which is the point.**

**DuckDB FTS** — `PRAGMA create_fts_index` + `match_bm25(id, query, k := 1.2, b := 0.75)`. QuackIR
(EMNLP 2025 industry) found it comparable to Anserini *if* you disable DuckDB's stemmer/stopwords
and set Anserini's BM25 defaults (k1 = 0.9, b = 0.4). First-party limitation: **the index is
table-based and must be rebuilt on mutation** — not an OLTP search engine.

**ClickHouse inverted index** — GA March 2026 (`text` skip index), dictionary + postings beside the
column, tokenizers incl. ngrams and sparse-grams. **BM25 top-k was only RFC'd May 2026**
(`textSearch()`, issue #105781). Before that, "ClickHouse search" often meant `ngrambf_v1` bloom
skip indexes, not BM25.

**`scrydb`** ([arXiv 2608.24060](https://arxiv.org/abs/2608.24060), 25 Aug 2026) — "SQLite is
Enough": lexical (FTS5) + semantic (sqlite-vec) + hybrid rerank/fuse in one Python library.
Papered library, not a production war story.

> **The pattern:** *"don't buy a search cluster until Postgres/CH/SQLite is actually on fire."*
> This is simultaneously the strongest validation of the "runs on your existing database" thesis
> and the strongest competition for it.

## 4. The 2026 shift — the querier got smart, so the index can stay dumb

The loudest idea of 2026, and it is not vapor:

- **Jo Kristian Bergum** (Hornet, ex-Vespa): BM25 + virtual filesystem + grep. *"GPT-5 is shockingly
  good at search, and that changes the BM25-as-a-baseline story."*
- **Doug Turnbull**, [31 Aug 2026](https://x.com/softwaredoug/status/2094513595396640890):
  GPT-4.1-mini searches `"red couch"`; GPT-5-mini searches
  `"red couch red sofa crimson couch red sectional…"`. Later models *expect* to fire many lexical
  queries.
- **Perplexity, 1 Jun 2026 — "Search as Code"**: *"single tasks invoke hundreds or even thousands of
  retrieval operations within a few minutes"*; search must become programmable primitives in the
  harness ([research.perplexity.ai](https://research.perplexity.ai/articles/rethinking-search-as-code-generation)).
- ***Is Grep All You Need?*** (May 2026): grep generally ≥ vector inside agent harnesses on
  conversation memory.
- ***Beyond Semantic Similarity / DCI*** (2026): replacing Qwen3-Embedding-8B with grep/find/shell
  on BrowseComp-Plus, Claude Sonnet 4.6: **69.0 % → 80.0 % accuracy, $1,440 → $1,016**.
- ***Boolean queries are all you need?*** (2026): LLM + Boolean engine on MS MARCO V2.1, 100
  calls/topic, **NDCG@10 = 0.6863**.

**Verdict: high, as a shift in *who formulates the query*.** The agent is the query planner; BM25 and
grep are the physical ops. This does not obsolete ranking — it changes the first-stage interface
from "one embedding" to "many exact predicates."

**Direct consequence for `index`:** a fast, embeddable, *inspectable* lexical engine with a
programmable predicate surface is worth more in 2026 than it was in 2023, not less. The engine
should be designed to be called a thousand times by a machine, not twenty times by a human — which
argues for a hard sub-millisecond warm path and a callable predicate API, not just a search box.

## 5. Late interaction is the actual "next index structure"

- **turbopuffer, 29 Jul 2026**: late interaction (ColBERT) in beta. *"tpuf uses a single-vector ANN
  index for a fast first pass, then reranks hits using exact late interaction scoring."*
  Eskildsen's compute ladder: *"BM25 (least compute) → sparse vector → regex → dense vector → late
  interaction (most compute)… we will index every byte. you simply choose how much compute you want
  to spend."* ([x.com/Sirupsen](https://x.com/Sirupsen/status/2082475482465947940))
- **OpenSearch, 4 Dec 2025**: two-phase ColBERT (bi-encoder ANN, then MaxSim).
- **Mixpeek, Sep 2026**: on AgentIR, dense drops 30 %+ on noisy agent traces; ColBERT drops 18 %;
  agent-aware ColBERT drops 4 %.

> **This is the "next index structure" if there is one. Not learned B-trees. Not bigger single
> vectors.** Eskildsen's compute ladder is also the cleanest available statement of the API
> `index` should expose: **one index, a dial for how much compute a query is worth.**

## 6. Object-storage-native search won the AI-infra layer

turbopuffer's own query mix, last 30 days as of Jun 2026:

> *"a year ago, ~98 % of tpuf queries were vector ANN. last 30d: **64 % vector ANN / 19 % full-text
> BM25 / 13 % filter-only / 3 % aggregate**"*
> ([x.com/turbopuffer](https://x.com/turbopuffer/status/2064025922429477033))

Eskildsen, Dec 2025: *"We now manage trillions of vectors and tens of petabytes."*

**The query mix is the finding.** Search engines are absorbing the filtered-scan / set-intersection
/ BM25 workload that used to live in Elastic and Postgres. The hot path is **no longer "one
cosine"** — it is predicate-heavy retrieval with optional semantic recall.

**And the silent bottleneck**, Eskildsen (2025 talk): OpenAI embedding **P50 ~300 ms** —
*"it doesn't matter that the turbopuffer latency is 8 milliseconds when it takes 300 milliseconds to
create a query vector."* Customers are *"not right now limited by the turbopuffer cost, they're
limited by the embedding costs."*

> **Consequence:** an embedded engine whose dense arm is a **static embedding computed locally in
> microseconds** (see [`relevance.md`](relevance.md) §3) does not just save money — it removes the
> dominant latency term that the best-funded player in the space says it cannot remove.

## 7. What is explicitly *not* next

Recorded so the roadmap does not drift back into it:

- **Learned indexes as a search replacement** (§1).
- **Pure vector RAG as the default** — LIMIT plus a thousand hybrid blog posts.
- **"Just grep the world" without a first-stage index** — the grep papers still BM25-narrow or
  operate on small corpora. Bergum's recipe is BM25 *then* grep, not grep-from-cold-start on 100 M
  docs.
- **Bigger embedding dimensions as a way out of LIMIT.** The bound is geometric.

## 8. Who to read, and what to discount

| Person | Right about | Discount |
|---|---|---|
| Simon Eskildsen (@Sirupsen, turbopuffer) | storage hierarchy is the cost law; postings are set intersection; cold vs warm belongs in the SLO | customer % savings, ARR tweets |
| Will Bryk (Exa) | IVF+BQ+MRL to serve billions of embeddings; agents need < 200–350 ms | "fastest in the world" |
| Jo Kristian Bergum (@jobergum) | agents changed the BM25 baseline; lexical tools are inspectable | new-startup efficiency claims |
| Doug Turnbull (@softwaredoug) | evals, metadata, updates; "RAG is 2015 search" | course plugs |
| Paul Masurel (@fulmicoton, Tantivy) | segment independence, S3 without Express, 10 M-doc segments | left Twitter |
| Jason Bosco (Typesense) | in-RAM search is a solved, cheap product below ~30 M docs | comedy tweets |
| Alex Garcia (sqlite-vec) | embedded vector search is brute force and that's fine to ~1 M | the linear-scan cliff |
| ParadeDB (Ming Ying) | BM25 belongs in Postgres via a Tantivy index-access-method | "Elastic-quality" slogan |
| Orion Weller (LIMIT) | single-vector capacity is geometrically bounded | synthetic eval oversold on X |
| Andy Pavlo | SQL isn't dying; vector DBs are a feature | **not a search-engine source in 2024–26** |

## 9. The five techniques worth stealing, per the sweep

1. **Object storage + NVMe cache** for multi-tenant / mostly-cold indexes (turbopuffer, Quickwit).
2. **BM25 first stage + RRF hybrid + cross-encoder/ColBERT rerank** as the 2026 relevance default.
3. **Posting-list top-k** (WAND / block-max / weakAnd) *before* reaching for HNSW.
4. **Search in the primary store** (pg_search, FTS5, DuckDB FTS, ClickHouse `text`) until dual-write
   pain is proven.
5. **Give agents lexical tools** (BM25, grep, Boolean) rather than one opaque `top_k(embed(q))`.
