# Research ledger — count ABI delta — 2026-09-20

Validated claims the main loop kept. Decision-bearing rows were re-opened against the cited page.

## Claims

[C1] PlanetScale TIN (GA 2026-09-16) treats COUNT(*) as a first-class query: `SELECT COUNT(*) FROM photos WHERE tags ==> '"san francisco"'`, and on Wikipedia (8.0 GB, in-memory) reports 10,260 QPS at 2 ms p99 for disjunction COUNT vs ParadeDB 291 QPS / 95 ms and Postgres GIN 1.4 QPS / 30,292 ms. — https://planetscale.com/blog/introducing-tin — high — competitor

[C2] TIN stores each term's exact posting counts and, for COUNT(*) disjunctions whose page-level bitmaps share no bits, returns the sum of those stored counts without reading postings. — https://planetscale.com/blog/introducing-tin — high — competitor

[C3] Tantivy 0.26.2 ships `collector::Count` whose fruit is `usize`: `searcher.search(&query, &Count)` returns matching documents and `requires_scoring` is false. — https://docs.rs/tantivy/latest/tantivy/collector/struct.Count.html — high — competitor

[C4] Meilisearch default search returns `estimatedTotalHits`; `page` / `hitsPerPage` switch to exhaustive `totalHits` / `totalPages`; `hitsPerPage=0` yields exhaustive `totalHits` with no documents. — https://www.meilisearch.com/docs/guides/front_end/pagination — high — competitor

[C5] Meilisearch documents that exhaustive `totalHits` is resource-intensive and that raising `pagination.maxTotalHits` above ~20,000 can make queries take seconds. — https://www.meilisearch.com/docs/guides/front_end/pagination — high — competitor

[C6] Algolia Grow (pay as you go, retrieved 2026-09-20) includes 10,000 search requests/month then USD 0.50 per additional 1,000, and 100,000 records then USD 0.40 per additional 1,000 records. Grow Plus is USD 1.75 per additional 1,000 search requests. Free: 10,000 requests and 50,000 records. — https://www.algolia.com/pricing — high — money

[C7] Algolia InstantSearch's stats widget displays matching hits via `nbHits` ("20,337 results found in 1ms"). — https://www.algolia.com/doc/api-reference/widgets/stats/js — high — product_form

[C8] Algolia support states hit and facet counts may be approximations, `exhaustive.nbHits` can be false, and there is currently no way to bypass that. — https://support.algolia.com/hc/en-us/articles/4406975248145-Why-are-my-facet-and-hit-counts-number-of-results-not-accurate — high — competitor

[C9] Algolia meters a search-as-you-type keystroke, an empty landing-page query, each facet/sort/filter refinement, and each `searchForFacetValues` call as a billed search request. InstantSearch's `refinementList` makes one search for counts and one for results. — https://support.algolia.com/hc/en-us/articles/17245378392977-How-does-Algolia-count-search-requests-operations-and-records — high — money

[C10] Elasticsearch `_search` `track_total_hits` defaults to 10,000 (accurate up to that threshold, then `hits.total.relation` is `gte`); `true` forces an exact count and Elastic documents that this can disable Max WAND. — https://www.elastic.co/docs/solutions/search/the-search-api — high — competitor

[C11] Elasticsearch `GET /{index}/_count` returns a count of documents matching a query and broadcasts across shards. — https://www.elastic.co/docs/api/doc/elasticsearch/operation/operation-count.md — high — competitor

[C12] Typesense search responses include integer `found` ("The number of documents found") and `out_of` (collection size). Typesense v29.0 changelog: for `group_by` queries `found` is no longer exact and is guaranteed only within 2%. — https://typesense.org/docs/29.0/api/README.md — high — competitor

[C13] ParadeDB Community has been AGPL-3.0 from day one; it monetizes via support contracts and commercial non-AGPL licenses. — https://www.paradedb.com/blog/agpl — high — money

[C14] Typesense Server is GPL v3 because it expects to run as a separate daemon, not as a library inside application code; client libraries remain Apache. — https://github.com/typesense/typesense — high — product_form

[C15] Meilisearch Community Edition is MIT; Enterprise sharding is commercial / BUSL 1.1. — https://github.com/meilisearch/meilisearch — high — competitor

[C16] MongoDB acquired Realm (Tightdb, Inc.) on 2019-05-07 for USD 39.0 million cash; TechCrunch reported >100,000 developers and 350 paying companies after ~USD 40 million raised. — https://www.sec.gov/Archives/edgar/data/1441816/000144181620000067/R13.htm — high — endgame

[C17] turbopuffer published floors: USD 16/month (Launch), USD 256/month (Scale), at least USD 4,096/month plus 35% usage premium (Enterprise). — https://turbopuffer.com/pricing — high — money

[C18] Pinecone Standard bills a USD 50/month usage minimum; Enterprise USD 500/month. — https://www.pinecone.io/pricing/ — high — money

[C19] Algolia closed a USD 150 million Series D at a USD 2.25 billion post-money valuation on 2021-07-28. — https://techcrunch.com/2021/07/28/search-api-startup-algolia-raises-150-million-at-2-25-billion-valuation/ — high — endgame

[C20] MiniSearch.search returns scored hits and documents no query-match count field; `documentCount` is corpus size, not match cardinality. — https://lucaong.github.io/minisearch/classes/MiniSearch.MiniSearch.html — high — competitor

[C21] Orama search() returns `{ elapsed, hits, count }` where count is the number of matching documents in the documented example (Apache-2.0, in-process). — https://github.com/oramasearch/orama — medium — product_form

[C22] Meilisearch Cloud starts at USD 20/month (retrieved 2026-09-20). — https://www.meilisearch.com/pricing — high — money

[C23] Filipino Web Development Peers Discord listed 7,777 members on 2026-09-20. — https://discord.com/servers/filipino-web-development-peers-996276138588524624 — high — customer

[C24] A wasm32-unknown-unknown cdylib export that JS can call without wasm-bindgen is a `#[no_mangle] pub extern "C"` function. — https://blog.rust-lang.org/2025/04/04/c-abi-changes-for-wasm32-unknown-unknown/ — high — product_form

[C25] Microsoft docfind runs full-text search in the browser as one WASM module (11.48 MB uncompressed / 5.20 MB Brotli for 50,000 AG News articles). — https://github.com/microsoft/docfind — high — ai.locality

[C26] Lucene 10.3 `TotalHitCountCollector` is the collector behind `IndexSearcher.count(Query)` and skips collecting a segment when `Weight.count` is implemented. — https://lucene.apache.org/core/10_3_0/core/org/apache/lucene/search/TotalHitCountCollector.html — high — competitor

[C27] PlanetScale Postgres non-HA PS-5 is USD 5/month in AWS us-east-1 (pricing page generated 2026-09-20). — https://planetscale.com/pricing — high — money

[C28] No Philippine-headquartered hosted search SaaS was found in this sweep as of 2026-09-20; the local incumbent for the in-house grocery app is Postgres `pg_trgm` COUNT/UNION, not a search vendor. — unverified — competitor

## Gaps

- Is Typesense `found` always exact for ordinary (non-group_by) keyword search?
- Do production C ABIs bump ABI_VERSION for additive COUNT exports, or keep the version (this repo's range-facet kept ABI 14)?
- Paid CAC for an embedded Algolia alternative sold to indie PH grocery/campus apps is unpublished.
- Can TIN-style page-bitmap POPCNT COUNT run at similar speed on wasm32 (no AVX2)?
- Phrase COUNT is still unbuilt here (p93 named it as not done).

## Counts (self-report)

- agent_count: 4 (market+competitor, technology, customer+channel, business+endgame)
- claim_count: 28 kept after merge
- downgraded_count: 1 (Orama count exactness → medium; C21)
- dedupe_count: 9 (TIN QPS, Meilisearch estimatedTotalHits, Algolia Grow prices, ES track_total_hits, Typesense found, ParadeDB AGPL, Tantivy Count, InstantSearch double-billing, Meilisearch Cloud floor each appeared in ≥2 ledgers)
