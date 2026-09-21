# Adoption — what shipping this would actually take, per app

> The measurements are in [`integration.md`](integration.md). This file is the other half: for each
> app, exactly what a pull request would contain, what gate it must clear, what it buys, what it
> costs, and how to undo it.
>
> **No PR has been opened.** Four shipping repositories are not mine to change unilaterally. This
> exists so that "which app?" is the only remaining question, and the answer to it is one command
> rather than a week.

## The state to build from

| Repo | Branch | Worktree | What is on it |
|---|---|---|---|
| profstopick | `main` (`d59f8c3`, deployed 2026-09-21) | — | **Serving.** Typeahead is literal-first `matchFolded` + this engine (`public/search-engine.wasm`, ABI 14, `a3b4911`) with `prefix: true`, which now runs the p98 plan (`-word`, `"phrase"`); the URL is versioned by the module's sha256 (profstopick D191, D198). The `feat/index-engine-p98` adapter branch is an evaluation, superseded. |
| onegrid | `index-accel-eval` | `../onegrid-index-eval` | `packages/wasm/src/__tests__/differential.real-module.test.ts` (their property test, one line changed) |
| presyo | `main` (`be6b3414`, deployed 2026-09-21) | — | **Built, flag OFF.** `PRODUCT_SEARCH_ENGINE=index` serves `/api/product/search` from the artifact in-process via WASM + a 20-id keyed hydrate, SQL fallback on any failure. Judged 88 queries: p@10 83.4% → 93.1%, wrong-size 29.5% → 0.7%, p50 14.6 ms vs 466 ms. Not flipped: the pilot view fell to 331 rows and the nightly build refuses below 20,000. See presyo `docs/INDEX_EVAL.md`. |

The evaluation branches added files only. profstopick and presyo have since adopted the engine on `main` (rows above); onegrid has not.

---

## profstopick — the strongest case, and the smallest change

**Why.** Its own production measurement, 2026-08-17: **158 hits against 109 misses, a 40.8 % miss
rate**, 60 of them name-shaped queries whose professor was already in the corpus. That is not a
tuning gap; `flatten()` concatenates a label to `rigoradrian`, so a first-name-first query can never
match a surname-first index, in any order. The engine passes their nine-assertion contract test
**9/9**, including all three production failures and all six survival checks, and their full suite
runs **1,922/1,958** with the same four failures their untouched `main` has.

**On `feat/index-engine-p98` today:** items 1 and 4. `/search` still uses `search-match.ts`.

**Still to land before `/search` flips**
1. `src/lib/search-match-index.mjs` — ABI 14 adapter (**on the branch**).
2. A build step emitting `public/search-<school>-<hash>.idx` beside the existing JSON shard, from
   `script/build-search-index.ts`.
3. `use-search-index.ts` fetching the `.idx` instead of the JSON, behind an env flag.
4. `test/search-name-order-index.test.mjs` kept as a permanent second harness (**on the branch**, 9/9).

**Gate.** `npm test` at 1,922/1,958 or better, and their three search tests green.

**Buys.** Typo tolerance where there is none today, and a smaller browser payload — a 380 KB index
against a 2.5 MB JSON shard that occupies **95.6 % of the 5 MB localStorage quota**.

**Costs.** A WASM artifact in the bundle (244 KB, gzips smaller). The index becomes opaque — no
longer inspectable in devtools as JSON.

**Rollback.** The env flag. The JSON shard keeps being emitted until the flag is removed.

**Honest caveat.** Their `search-index-hash.test.mjs` and `search-index-cache.test.mjs` pin the
current shard's shape and would need updating — that is real work this branch has not done.

---

## onegrid — the socket already exists

**Why.** onegrid ratified the ABI, wrote the reference backend, wrote the differential harness, and
budgeted the artifact. It shipped no module; `packages/wasm/crate/` does not exist and every test
runs against `createFakeAccelModule()`. `index-accel` is that module — 6,342 bytes of `no_std` WASM
— and their whole `packages/wasm` suite passes **294/294** against it.

**The PR would contain**
1. The `index-accel` crate vendored into `packages/wasm/crate/` (it is dependency-free and self-
   contained), or consumed as a prebuilt `.wasm`.
2. A build step producing `packages/wasm/accel.wasm`.
3. `differential.real-module.test.ts` alongside the existing fake-module test.

**Gate.** `packages/wasm` at 294/294, plus `bundle-budget.json` — the artifact must fit the budget
they already wrote.

**Buys.** The acceleration path they designed for, on the two hot spots they measured: the
`contains`/`startsWith` scan in `data/src/filter.ts:125-140`, and `enumerateDistinct` at ~500 ms for
10 M rows.

**Costs.** A Rust toolchain in their build, or a checked-in binary.

**Rollback.** `detect.ts` already probes capability and falls back to the JS backend. Ship it
disabled and flip it.

**Honest caveat.** The kernels are proven *correct* by their harness. They are **not yet proven
faster** — `bench.test.ts` exists but no head-to-head JS-vs-WASM timing at 1 M rows has been run.
**Do not merge this on a speed claim that has not been measured.**

---

## presyo — the largest opportunity, and the one that needs a decision first

**Why.** At 260,000 rows the engine matched their shipped SQL on recall (100 %) and beat it on hit@1
(100 % vs 99.4 % clean; 99.0 % vs 97.6 % on typos), at sub-millisecond query latency against ~55 ms.

**DECIDED and BUILT (2026-09-12).** The architecture question below was settled — **artifact,
mmap'd** — and the integration now exists on presyo's side: `scripts/index-build.sh` (nightly cron,
06:45, after their materialized-view refresh) and `scripts/index-eval.py`. See
`presyo/docs/INDEX_EVAL.md`. The engine is built from source at `/opt/presyo/bin/index`.

**Measured on presyo's REAL corpus**, 35,637 rows from `mv_pilot_ready_product` — not recombined
distractors this time:

| | build | artifact | terms |
|---|---|---|---|
| | 0.19 s | 3.47 MB | 50,492 |

precision@10 over 15 queries with a checkable ground truth: **99 % vs their shipped SQL's 93 %**.
And the entire margin is one thing this repo had not previously identified —

**FILIPINO-LANGUAGE QUERIES.** `sabon` (Filipino: soap) scores **20 % on their SQL path and 90 %
here**; `tsokolate` 80 % vs 100 %. All 13 English queries tie at 100 %. Their `pg_trgm` similarity
returns Breeze detergent, Dove bodywash and — literally — "El Sabor Nacho Chips" for `sabon`
(*sabon* ≈ *sabor*). Trigram similarity has no way to cross the language boundary; the engine
reaches it through their `search_text` column. For a Philippine price-comparison product that is a
core use case, and it is a stronger argument for adoption than the latency numbers above.

**A metric warning worth carrying.** The obvious comparison — how many ids do the two engines agree
on — is misleading: overlap@20 averaged 47 % and `sabon` overlapped on **0 of 20**, which reads as a
broken engine. Both return 20 relevant products; they return different ones. Overlap measures
agreement, not quality.

**The decision that was made.** presyo's architecture question was never "is the engine good" but
**where the index lives**:

- **In the API process** — build at boot from Postgres, rebuild on a schedule. Simplest, no new
  infrastructure, and matches the "no second datastore" thesis. Costs process memory (~160 MB at
  1 M docs) and a rebuild window.
- **As an artifact — CHOSEN.** Build in cron, write a `.idx`, API mmaps it. Cheap queries, bounded
  staleness, one more file to ship. At 3.47 MB for their whole storefront corpus, "one more file"
  is not much of a cost.
- **In Postgres via pgrx** — closest to their current shape, and **not portable**: RDS no, Supabase
  no, Neon deprecated. Viable only because presyo self-hosts — a bet the artifact route avoids.

**The blocking issue is CLOSED.** This section previously read: *"The engine has no incremental
update: adding a document means a rebuild. … This is the real gate on presyo adoption, and it is
unbuilt."* `index apply` shipped on 2026-09-08 with `cdc-equivalence` at **0 keys wrong,
14,288 = 14,288** over 8,000 inserts/updates/deletes. The claim outlived its truth by four days and
was still being used to justify not adopting; that is the failure this correction records.

**Two build traps, found the hard way, now documented in presyo's script.** CSV `HEADER` is
REQUIRED — the schema maps by column name, and without it the build reports
`built 35636 rows, 0 terms`, warns that every row has a blank key, and **exits 0**. A caller that
trusts the exit code ships an empty index. And `--schema` caps at 4 fields
(`index: 5 fields, at most 4 are supported`), which is not in the CLI's own usage text.

**Since 2026-09-21:** presyo's API can SERVE from the artifact behind `PRODUCT_SEARCH_ENGINE=index` (default off), and a judged typo/size/Filipino benchmark was run on the real corpus (presyo `docs/INDEX_EVAL.md`). **Still not done:** `apply` has never run against their change stream — the nightly full rebuild is the update path, and a multi-segment artifact is refused.

**Honest caveat.** The 260 K comparison is 1,940 real rows plus recombined distractors. It does not
populate their `search_text` column or their ~296 K aliases, and their input contract
(`productSearchToken('Coca-Cola 1.5L') === ['coca','cola','1.5l']`) is deliberately unsatisfied —
the analyzer produces `1500ml`, which is worth **+4.6 pp recall** by presyo's own measurement. *(Measured 2026-09-21 on the real corpus: the engine folds `1.5L`/`1.5 l`/`1500ml` identically on both sides, so the gap closes without a presyo-side rewrite — `coca cola 1500ml` 0% → 90% p@10.)*

---

## sisia-app — the smallest, most contained change

**Why.** Out-of-order title words: **91.2 % vs 0.0 %**. Their catalog `LIKE` matches one contiguous
substring, so a two-word query in the wrong order matches nothing across 2,038 real course titles.

**The PR would contain** an adapter behind `Course.ts`'s existing query functions, built from the
same SQLite rows at boot.

**Gate.** Their existing course/instructor tests, plus the `sisia-catalog` bench.

**Explicitly NOT proposed.** Replacing the hybrid `driveHybridSearch` path. That is `ts_rank_cd` +
pgvector + RRF k=60 + a Vertex reranker, and none of it has been measured here — it needs a
database, a corpus and an API key this machine does not have. **Do not let a catalog-search win
imply a hybrid-retrieval win.**

---

## What no PR can settle

- **Billion-query scale.** The largest honest corpus available here is 61,467 real rows; everything
  above is recombination. The 1 M p99 varies **±0.9 ms across identical runs**, so a 5 ms bar is
  below this harness's resolution. Real scale evidence requires production traffic, and production
  traffic requires a deployment — which is why this file exists rather than another benchmark.
- **Whether apps stop optimizing for their data.** That is a claim about a year of operation, not a
  test result. What can be said today is narrower and true: on four apps' own contracts, on their
  own data, the engine matched or beat what they ship, and the two defects it did *not* beat are
  written down.
