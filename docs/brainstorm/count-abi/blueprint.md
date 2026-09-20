# Blueprint — index count ABI (extend)

Decided 2026-09-20. Claim ids refer to `research.md`. Rejected alternatives are named in each section.

## 1. Idea + thesis

**index count ABI** — export the exact `COUNT(*)` that p93 already ships in Rust so every host can answer how many documents match without ranking.

North star: a count query on the public surface returns the same integer `Index::count_any` / `count_all` already pin against a brute-force set.

Non-goals: consumer-app PRs, the 8 GB Wikipedia TIN corpus, replacing the engine, page-level bitmaps in v1, sidecar / AGPL / learned-sparse.

Why now: CHANGELOG p93 says the methods are Rust-only; TIN made COUNT a published competitor row on 2026-09-16 [C1].

Rejected: a greenfield re-plan of the product. This is a delta against what already ships.

## 2. Customer

One primary persona: the engineer wiring search into an in-house PH catalog or grocery app (presyo, profstopick, onegrid, sisia) who already has rows in a database.

JTBD: when showing a result list, print the exact number of matching documents ("412 snacks") without ranking 412 hits or calling a hosted search API.

Today: `hit.length` after top-k (wrong once `k` < matches), SQL `COUNT` on `pg_trgm`, or skip the number.

Secondary: a DBA on the CLI; a Python ctypes host.

Accessibility: low-end Android / OPFS, Taglish titles already folded, offline.

## 3. Market + competition

| name | kind | gap |
|---|---|---|
| presyo pg_trgm UNION | local | exact SQL COUNT, 17 s → 400 ms, not in a tab |
| PlanetScale TIN | global | COUNT(*) as SQL, page-level bitmaps, Postgres-only [C1][C2] |
| Tantivy Count | global | exact, no-score, Rust-only, no one C ABI [C3] |
| Meilisearch | global | default is an estimate; exhaustive is opt-in and capped [C4][C5] |
| Algolia | substitute | `nbHits` billed as a search request; counts may be approximate [C6][C7][C8][C9] |
| Elasticsearch | global | `track_total_hits` defaults to a 10,000 cap [C10][C11] |
| MiniSearch | substitute | no query-match count at all [C20] |

Local rival: no PH-headquartered hosted search SaaS was found [C28]; the incumbent is the app's own Postgres.

Moat: one `.idx` + one C ABI already serving browser/Node/FFI/CLI; exact counts that skip ranking and honour deletions.

Rejected as moat: "first mover."

## 4. Product form

**sdk** — the customer already links `include/index.h` or imports `js/index.mjs`.

Rejected: **saas** (Algolia's metered bill is the thing this library exists to avoid [C6]); **service** (sidecar already in `docs/roadmap-rejected.md`; Typesense is GPL because it expects a daemon [C14]).

## 5. v1 cut line

- One JTBD: exact any/all COUNT on C ABI, JS host, and CLI.
- One channel: the existing repo and in-house engineers.
- One pricing experiment: stay free (MIT OR Apache-2.0).

Parked: phrase COUNT; TIN-style stored per-term counts; `idx_searcher_count_*`; consumer PRs.

## 6. Frontend plan

Page map (SDK): README, `docs/integration.md`, `js/browser.html` playground, CLI `--help`.

Onboarding: time-to-first-successful-count is `index search --count any toothpaste` after the existing pipe build, or `SearchIndex.countAny`.

Headline: exact `COUNT(*)` on the same ABI that already searches.

Brand brief: positioning = in-process search that can now say how many matched; audience = PH catalog/grocery engineers; tone = measured, terse.

## 7. Backend + API plan

Runtime: the existing Rust workspace. Framework: hand-written C ABI. Rejected: wasm-bindgen [C24] and `docs/roadmap-rejected.md`.

Boundaries: `idx_count_any` / `idx_count_all`; `SearchIndex.countAny` / `countAll`; `index search --count any|all QUERY`.

Store: existing `.idx` postings. Auth: none. Rate limit: none. Observability: smoke tests. Compute: customer perimeter.

## 8. AI architecture

`is_used`: false. COUNT is a posting-list walk. Rejected cloud locality: a model would add a bill without changing the integer.

## 9. Data strategy

Not an AI product. The brute-force oracle in `index-text` is the labelling plan for the integer.

## 10. GTM

Primary: in-house app engineers on this machine — ship COUNT and ask them to print the integer.

Deferred: Filipino Web Development Peers Discord (7,777 members [C23]).

CAC hypothesis: ₱0 per paying customer, assumed — there is no paid motion in v1.

## 11. Money

Library is USD 0 per query (assumed; the licence is MIT OR Apache-2.0). Build and monthly run: ₱0 assumed. Break-even is adoption inside an app that would otherwise pay Algolia Grow USD 0.50 / 1,000 requests [C6].

## 12. Constraints, edge cases, complaints

Constraints: permissive licence; ABI 14 additive; nothing traps; `index-accel` stays wasm32-only.

Edge cases: empty query → 0; deletions not counted; absent token → any ignores / all is 0; null handle → `UINT32_MAX`; multi-segment sums.

Complaints: "why wasn't this on the ABI"; CLI still ranks; count ≠ `hit.length` of a top-k search (correct, must be documented).

Bottleneck: COUNT still walks postings [C2]; consumer PRs; phrase COUNT.

## 13. Risk register

1. Additive ABI without a host test — smoke must list the symbols. Kill: smoke green while the export is missing.
2. CLI counts only the first segment — Searcher must sum. Kill: two-segment fixture disagrees with a rebuild.
3. ABI version bump that breaks hosts — keep 14. Kill: `js/index.mjs` vs module mismatch.
4. Hosts treat `UINT32_MAX` as a huge count — coerce and throw. Kill: null-handle test reads 4294967295 as success.
5. Scope creeps into TIN bitmaps — v1 is the export. Kill: CHANGELOG mentions a new posting codec.

## 14. Timeline + team

Two weeks, founder-engineer, AI multiplier ×3 assumed (wiring, not adoption).

- Week 1 export: symbols, JS, Searcher, CLI, smokes.
- Week 2 prove: CHANGELOG, ROADMAP section, gate green.

## 15. Endgame

**open_core**. Realm's 100k developers / 350 payers / USD 39M exit [C16] is the failure mode. Stay permissive so a managed cloud can ship it — the TigerData / ParadeDB AGPL lesson [C13].

## 16. Open question

- Bump ABI_VERSION or keep 14 (range-facet precedent)?
- Searcher-level C ABI in this cut?
- Is Typesense `found` exact for ordinary keyword search [C12]?
- Will any consumer print the integer?

## 17. Handoff

`setup_preset`: `node-cli` (existing CLI; `setup` is not run). ROADMAP P0: export COUNT through the ABI and JS host; Searcher + CLI `--count`; gate with `js/smoke.mjs` and `scripts/cli-smoke.sh`.
