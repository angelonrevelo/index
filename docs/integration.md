# Integration — the engine measured against applications' own contracts

> **This file is a dated evidence record, not the current state.** Every measurement below was taken
> on 2026-09-05 against evaluation worktrees, and its "their `main` is untouched" framing was true
> then. It is no longer: profstopick and presyo have both adopted the engine on their own `main`
> since 2026-09-21. For what each app runs *today*, read
> [`adoption.md`](adoption.md); this file is kept unedited so the numbers stay attached to the
> conditions that produced them.
>
> 2026-09-05. Evidence for the claim that this engine can replace hand-written code in real
> applications, rather than merely score well on their exported data.
>
> Two applications, two independent contracts, both written by someone who was not trying to make
> this engine look good:
>
> | App | Their harness | Result |
> |---|---|---|
> | **profstopick** | `test/search-name-order.test.mjs`, unmodified but for the import line | **9 / 9** |
> | **profstopick** | the whole suite (`npm test`) | **1,922 / 1,958**, and the 4 failures fail identically on their untouched `main` |
> | **onegrid** | `differential.property.test.ts` + the whole `packages/wasm` suite | **294 / 294**, driven by a real compiled module instead of their JS stand-in |
> | **presyo** | their own `searchProduct` on a disposable PostgreSQL, same rows, same queries | at **260,000 rows**: engine **100 % / 100 %** vs SQL **100 % / 99.4 %** clean; **100 % / 99.0 %** vs **99.8 % / 97.6 %** on typos |
> | **browser** | `js/browser.html` in real headless Chromium 147 | **PASS** — 37 us/query on the main thread |
>
> | **sisia-app** | its own catalog `LIKE` search, on its own registrar data | out-of-order title words **91.2 % vs 0.0 %**; exact-code lookup **tied**, and that non-win is reported |
>
> **All four apps are covered.** sisia was written off one round too early: the corpus this repo has
> benchmarked from the start declares `"source": "sisia class_section_all"` — it *is* sisia's
> registrar table, exported through profstopick's research pack. Its *hybrid* path (pgvector + RRF +
> Vertex reranker) is still out of reach and is not claimed.
>
> **Nothing is deployed.** All three live on throwaway git branches.

`Index::search` / `Searcher::search` and their typeahead twins `search_prefix` (so `idx_search`
at `prefix` 0 **and** 1) execute `query::parse`: mixed phrase + exclude, exact-key rank-1, emoji
tokens, Filipino `AliasTable` rows when the grocery table is loaded (WASM/`idx_build_new` and
`index build` load it). In typeahead only a trailing unquoted term is prefix-expanded. The ABI
builder records positions, so a quoted phrase answers on `idx_build_new` with no extra call. See
[`bench/roadmap/p98-query-axis.md`](../bench/roadmap/p98-query-axis.md).

## The setup

**Nothing on `profstopick`'s `main` branch was touched.** The evaluation lives in a git worktree on
its own branch:

```
git -C profstopick worktree add ../profstopick-index-eval -b index-engine-eval
```

Two files were added there, and **no existing file was modified**:

| File | What it is |
|---|---|
| `src/lib/search-match-index.mjs` | An adapter exposing the same `fold` / `matchFolded` signatures as `src/lib/search-match.ts`, answered by the engine through the WASM C ABI. |
| `test/search-name-order-index.test.mjs` | A copy of `test/search-name-order.test.mjs` with **one line changed** — the import. Every assertion is the application's own. |

The single changed line is marked in the file. That constraint is the whole point: a pass means the
engine satisfies a contract written by someone who was not trying to make it look good.

## What that test is

`test/search-name-order.test.mjs` is not a synthetic exercise. Its header records a production
measurement taken on **2026-08-17, while 88 people an hour were using the site**:

> `analytics_event` recorded **158 search hits against 109 misses — a 40.8 % miss rate** — and 26 of
> the 81 people who searched at all hit a dead end. **60 of those 109 misses (55 %) are multi-token,
> name-shaped queries**, and cross-checking every one against the `professor` table found the person
> ALREADY IN THE CORPUS, stored surname-first:
>
> ```
> typed                exists as
> allan pastrana  ->   Pastrana, Allan J.      (that visitor tried SEVEN times)
> Adrian Rigor    ->   Rigor, Adrian           (typed and backspaced ELEVEN times)
> ```

The cause is structural: `flatten()` concatenates a label to `rigoradrian`, so a first-name-first
query can *never* prefix- or substring-match a surname-first index, in any order.

Six of its nine assertions are explicitly **survival checks** — single-token matching, the accent
fold, course-code punctuation and professor-before-course ordering "must not move to buy the other
three."

## Result

```
$ node --test test/search-name-order-index.test.mjs
✔ a full name typed first-name-first finds the professor
✔ a partly-typed second token still finds them
✔ the surname-first spelling keeps working
✔ each query token must match a DISTINCT name token
✔ a name that is not in the corpus still returns nothing
✔ the accent fold still reaches a name nobody can type
✔ a course code with punctuation still matches however it is typed
✔ professors still outrank courses
✔ an empty or whitespace query still returns nothing

ℹ tests 9   ℹ pass 9   ℹ fail 0
```

**9 of 9, including all three production failures and all six survival checks.**

## It failed first, and what that bought

The first run passed **7 of 9**. Both failures were real and both produced engine changes that
benefit every consumer, not just this one:

**1. `math30.23` did not reach `MATH 30.23`.** The engine had no notion of a token the user typed
without a space. Fixed by **compound splitting**: a token absent from the dictionary that splits
cleanly into two dictionary terms is replaced by those two, preferring the split into the *rarest*
pair (a split into two common terms is far more likely to be an accident — `therapist` →
`the` + `rapist`). This also closes an entire category of presyo's frozen fixture — `cocacola`,
`bearbrand`, `luckyme`, `pancitcanton`.

Splitting is tried **only after exact and fuzzy have both failed**. Trying it earlier measured
**7.12 ms p99 against 6.37 ms** at a million documents, because a typo'd token often happens to
split into two real terms and taking that split discards the typo correction. Meilisearch likewise
prices concatenation at one typo rather than ahead of one.

`MATH  30-23` needed a second change: **a separator between digits is normalized to `.`**, so
`30.23` and `30-23` become the same token. The cost is stated in the code — a hyphenated numeric
range folds to a decimal — and accepted, since index and query normalize identically.

**2. `"jacob c"` matched `Jacob, Precious`.** A genuine bug with a sharp cause: in prefix mode a
one-character token fell through to the distance-1 automaton, and *every* single letter is one
substitution from `c`, so `"c"` matched essentially the whole dictionary. **The length gate has to
hold in prefix mode too.** Fixed by building a distance-0 prefix automaton for tokens below the
1-typo threshold, with a regression test asserting `"c"` reaches `cruz` and `colgate` but not
`jacob` or `precious`.

## What is the app's, and what is the engine's

The adapter deliberately keeps three rules **out** of the engine, because they are product
decisions rather than retrieval ones:

- **Professor before course, always**, ahead of any relevance signal.
- **Partial matches are suppressed when any complete match exists.** The engine ranks a document
  that dropped a query token below one that matched it, but still returns it — right for a product
  catalogue, wrong for a name lookup. The cut belongs in the application layer.
- **Deterministic tiebreak** by comment count, then label.

## What this does and does not prove

**Does:** the engine satisfies a real application's real, adversarially-written search contract,
through the WASM ABI, from JavaScript, with the app's own assertions unmodified — and doing so
surfaced two engine defects that its own test suite had not.

**Does not:** it is not deployed. Nothing on `main` imports the adapter; the branch exists to
measure, not to ship. The app's remaining ~150 tests were not run against it, its build pipeline
(`build:search-index`) still emits the JSON shard, and no browser has executed this. Adoption is a
decision for the repo's owner, and the honest prerequisites are in `ROADMAP.md`.

---

# onegrid — filling a socket that was cut and left empty

## What was there

`docs/research/demand.md` Finding 4 recorded that onegrid had **ratified an acceleration ABI and
shipped no module**: `packages/wasm/src/abi.ts` pins a hand-written raw-pointer C ABI with
`ACCEL_ABI_VERSION = 1`, `js-backend.ts` is the reference implementation, `differential.ts` is a
harness that proves an accelerated backend identical to it, `bundle-budget.json` budgets the
artifact — and `packages/wasm/crate/` **does not exist**. Every test ran against
`createFakeAccelModule()`, a JavaScript stand-in.

Their own rule, from `packages/wasm/src/index.ts`:

> *"The JavaScript implementation is the SPECIFICATION, it delegates to `@onegrid/data` rather than
> forking it, and the accelerated implementation is only allowed to exist as long as it can be shown
> identical to it."*

## What was built

`crates/index-accel` — all seven kernels (`og_sort_pass`, `og_filter_mask`, `og_group_code`,
`og_group_combine`, `og_aggregate`, `og_bitmap_op`, `og_top_k`) plus `og_abi_version` and
`og_heap_base`, as a **`no_std`, zero-allocation** cdylib. **6,342 bytes of WebAssembly.**

`no_std` is structural, not stylistic: **the JavaScript host owns the heap**, bump-allocating
everything above `og_heap_base()`. A Rust allocator running in the same linear memory would hand out
addresses the host believes it owns, so the crate is built such that it *cannot* allocate. `top_k`
marks consumed survivors with `i32::MIN` in the caller's scratch buffer rather than in a side array
for exactly this reason.

## Result

```
$ cd onegrid-index-eval/packages/wasm && npx vitest run
 ✓ src/__tests__/memory.test.ts (15 tests)
 ✓ src/__tests__/js-backend.test.ts (38 tests)
 ✓ src/__tests__/detect.test.ts (18 tests)
 ✓ src/__tests__/bench.test.ts (7 tests)
 ✓ src/__tests__/wasm-binding.test.ts (192 tests)
 ✓ src/__tests__/differential.real-module.test.ts (12 tests)   <- real module
 ✓ src/__tests__/differential.property.test.ts (12 tests)
 Test Files  7 passed (7)
      Tests  294 passed (294)
```

`differential.real-module.test.ts` is a copy of their property test with **one edit**: the backend
under test is `createWasmBackend(<real compiled module>)` instead of
`createWasmBackend(createFakeAccelModule())`. Every generator, property and assertion is theirs.
Those generators are deliberately hostile — the value arbitrary is weighted toward **NaN, `-0`,
`±Infinity`, duplicates and ties**, and column length starts at 0, because ties are where a stable
sort and a group-by are actually interesting.

## Why there are no host unit tests for the kernels

**Every pointer in this ABI is a `u32`**, because on wasm32 a pointer *is* an offset into linear
memory. On a 64-bit host an address does not fit in 32 bits, so a test passing
`vec.as_mut_ptr() as u32` silently truncates and the kernel reads a garbage address — which is
exactly what the first version did, faulting with `STATUS_ACCESS_VIOLATION` instead of failing an
assertion. Simulating linear memory with an arena would require giving every kernel a base parameter
the real ABI does not have, i.e. testing a different function from the one that ships.

So correctness is proven where the module actually runs, by the consumer's own harness. Only the
pointer-free parts (the version constant, bitmap length rounding, `-0` normalization) are asserted
natively.


---

# presyo — the shipped implementation, head to head

## The setup

A git worktree on `index-engine-eval`; `presyo`'s `main` untouched. Two new files, no existing file
modified:

| File | What it is |
|---|---|
| `scripts/compare_index_engine.sh` | Starts a disposable `postgres:16-alpine`, mirroring their own `test_product_contract_postgres.sh` (including the `-v` on `docker rm`, which is load-bearing because the image declares a VOLUME). |
| `scripts/compare_index_engine.ts` | Loads identical rows into PostgreSQL and into the engine, asks both the same questions, prints both answers. |

Their **own `searchProduct`** is imported and called — the real 7-lane `UNION ALL` over `pg_trgm`
with the inline `ts_rank` rescore, running against the trigram indexes migrations 004 and 012
create. Nothing is reimplemented on their side.

## The task

presyo's hardest one: **cross-store product identity**. `tests/fixtures/recall-gold-cases.json` is a
frozen production export (2026-06-13) of 500 gold clusters — listings from different retailers that
are the same physical product. **The first listing of each cluster is the query and is not loaded**,
so a hit can only come from a different store's wording. Ground truth is `gold_product_id`.

Typo queries corrupt exactly one alphabetic character, never at position 0 and **never on a digit** —
corrupting a size would test the opposite of what a price-comparison app needs.

## Result

At the gold set alone (1,940 rows):

```
                        recall@10     hit@1      mean latency
  clean queries
    presyo shipped SQL    100.0%     99.4%      46.72 ms
    index engine          100.0%     99.8%       0.09 ms
  ONE MISTYPED CHARACTER
    presyo shipped SQL     99.8%     97.6%      43.39 ms
    index engine          100.0%     99.0%       0.14 ms
```

And at **presyo's production order of magnitude** (`INDEX_SCALE=260000`) — the 1,940 real gold rows
kept exactly as exported, padded to 260,000 with distractors recombined from real presyo tokens:

```
corpus  260,000 rows (1,940 real gold listings + recombined distractors), 500 gold clusters
  load PostgreSQL 23.9 s   build index 11.2 s

                        recall@10     hit@1      mean latency
  clean queries
    presyo shipped SQL    100.0%     99.4%      61.40 ms
    index engine          100.0%    100.0%       0.34 ms
  ONE MISTYPED CHARACTER
    presyo shipped SQL     99.8%     97.6%      54.01 ms
    index engine          100.0%     99.0%       0.42 ms
```

**Recall held at 134x the haystack, and hit@1 went up.** A distractor can only ever hurt the score —
ground truth is still `gold_product_id`, so a recombined row can never be counted as a hit. Query
latency stayed **sub-millisecond at presyo's catalogue scale**.

Whole-document replication was deliberately not used to reach 260 K: it produces near-identical
copies, which is the corpus shape that most distorts top-k pruning — a mistake this project already
made once and recorded in `bench/roadmap/p7-scale.md`.

## Reading this honestly

**presyo's SQL is good.** At this corpus size `pg_trgm` handles a single mistyped character almost
perfectly — 99.8 % recall — and any earlier impression that their search was weak on recall was
wrong. The engine's margin is real but narrow: **+0.2 points of typo recall and +1.4 points of typo
hit@1**.

**The latency gap is large but not purely algorithmic.** The SQL side includes a loopback round trip
to a Docker container and connection overhead; the engine runs in-process. Treat ~47 ms vs ~0.1 ms as
"in-process beats a database round trip", which is a real architectural property but not a claim
about query planners.

**The 260 K run is padded, not real.** Only the 1,940 ground-truth rows are exported production
data; the rest are recombinations of real presyo tokens. That is enough to show recall and latency
survive the haystack growing 134x, and not enough to claim anything about their real catalogue's
term distribution. Their production corpus also carries ~296 K aliases and a `search_text` column
this comparison does not populate.

**One million documents still misses the interactive bar** — `bench/roadmap/p7-scale.md` measures
6.02 ms p99 against a 5 ms target, with the cause diagnosed (low-selectivity queries over
400 K-posting terms).

**What this does prove:** the engine can be driven by presyo's own data through the WASM ABI, it
matches or beats their shipped implementation on their own hardest task and their own ground truth,
and it does so without a schema migration, an extension, or a second datastore — the index is built
from rows the database already has.

## Not done

presyo's `tests/product_search_contract.test.ts` also pins an **input** contract —
`normalizeProductSearchQuery('Coca-Cola 1.5L') === 'coca cola 1 5l'` and
`productSearchToken(...) === ['coca','cola','1.5l']`. The engine's analyzer deliberately produces
`1500ml` instead, because canonicalizing the unit is what makes `1.5L`, `1500ml` and `1.5 liters`
one token — a property presyo itself measured as worth **+4.6 pp recall**. Satisfying that contract
verbatim would be a regression, so it is left unsatisfied and stated rather than quietly bent.

---

# The browser tier

Node proves the engine works outside Rust. A browser is a different claim: no `fs`, the index
arrives over HTTP, the query runs on the main thread beside a render loop, and it is the only place
`instantiateStreaming` and its hard `application/wasm` MIME requirement are exercised. It is also
the tier profstopick actually ships to.

`js/browser.html` deliberately does **not** import `js/index.mjs` — it drives the raw C ABI, so a
failure cannot be hidden by the Node wrapper. `js/browser-check.mjs` serves it and drives real
headless Chromium.

```
$ node js/browser-check.mjs
browser: HeadlessChrome/147.0.7727.15

  wasm module 244,200 bytes | index file 380,564 bytes | documents 1,322
  compile+instantiate 52.7 ms | open + parse 16.8 ms

  PASS ABI version is 2
  PASS a real index opens from a fetched ArrayBuffer
  PASS exact name    ABACAN, RAPHAEL  -> ABACAN, RAPHAEL (bucket 0, 3.50 ms)
  PASS surname only  ABACAN           -> ABACAN, RAPHAEL (bucket 0, 0.50 ms)
  PASS one deletion  ABAAN, RAPHAEL   -> ABACAN, RAPHAEL (bucket 1, 0.70 ms)
  PASS transposition ABCAAN, RAPHAEL  -> ABACAN, RAPHAEL (bucket 1, 0.20 ms)
  PASS typeahead from a 5-char prefix
  PASS a nonsense query returns nothing
  PASS 300 real name queries: 37 us each, on the main thread

OVERALL: PASS
```

**37 microseconds per query on the browser's main thread**, with typo tolerance, from a 380 KB index.
Playwright is deliberately not a dependency of this repo; the check borrows it from a sibling
checkout and exits 2 with instructions if it cannot, rather than pretending the check ran.

**Still not proven in a browser:** persistence. The index is fetched every load; OPFS caching
(ROADMAP P8) is unbuilt, and Safari's 7-day eviction rule means any cache needs a rebuild path.

---

# sisia-app — the corpus was here all along

`bench/roadmap/p8-sisia-catalog.md` has the full account. In short:

- The export declares **`"source": "sisia class_section_all"`** — 2,253 distinct course-code x title
  pairs of real sisia data, on disk the whole time. Writing sisia off as unreachable was an
  oversight, corrected.
- Its catalog search (`Course.ts`) is `course_code LIKE ?` plus `LOWER(title) LIKE ?`, ordered by
  code, **no relevance ranking**. Reproduced as the baseline rather than strawmanned.
- **Out-of-order title words: 91.2 % vs 0.0 %.** A substring `LIKE` matches one contiguous run, so a
  two-word query in the wrong order matches nothing — and no parameter fixes that, because the limit
  is the operator. This is the same failure class sisia already documented at
  `driveHybridSearch.ts:88-93`, where AND semantics "matched almost nothing" and the switch to OR
  then over-matched.
- **Exact course-code lookup: tied at 100 %, and reported as a non-win.** Two attempts to build a
  metric where the engine won on the prefix-bleed set are recorded rather than deleted — the first
  measured rank (both 100 %, because `ORDER BY course_code` sorts the shorter code first by
  lexicographic luck), the second measured "extra rows" and the engine came out *worse*, because it
  ranks where `LIKE` filters. That metric was measuring recall and calling it imprecision.
  **For exact code lookup, sisia's `LIKE` is adequate on this corpus.**

## Cleaning up

The worktree is disposable:

```
git -C profstopick worktree remove ../profstopick-index-eval
git -C profstopick branch -D index-engine-eval

git -C onegrid worktree remove ../onegrid-index-eval
git -C onegrid branch -D index-accel-eval

git -C presyo worktree remove ../presyo-index-eval
git -C presyo branch -D index-engine-eval
```

Both worktrees have a `node_modules` directory junction into their parent checkout; `worktree
remove` handles it, but delete the junction first if it complains.
