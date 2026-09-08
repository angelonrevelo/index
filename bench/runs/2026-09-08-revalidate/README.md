# Revalidation sweep — 2026-09-08

Every bench binary in the workspace, re-run against the numbers its published document carries,
on the current `master` tree (`0d0694b` plus the p82 hand-off). **Benches ran one at a time** —
the sweep re-learned the repo's own lesson mid-flight: the first `alec-surface` run read body-query
p99 40.7 ms (FAIL) while a concurrent lane was still running its own benches; re-run quiet, it
read 24.1 ms and PASSed. Interleave, or do not claim a delta.

Verdicts use three words: **reproduces** (fresh matches published within stated tolerance),
**documented red** (fresh matches the published failing state — the red is the claim, and it
holds), **drift** (fresh and published disagree beyond noise; explained or investigated).

| bench | published | fresh (this machine, quiet) | verdict |
|---|---|---|---|
| `real-corpus` | OVERALL PASS; typo hit@10 ≥ 90 % | PASS 2/2 — profstopick typo hit@10 99.9 % (their matcher 14.9 %); presyo gold recall@10 100 %, 0 size violations | reproduces |
| `profstopick-dept` | in-sample −0.8 pt; held-out −34.7 pt | in-sample **+0.8 pt**; held-out **−34.7 pt** | held-out reproduces exactly; in-sample sign flipped — near-zero either way, the corpus still refutes expansion there |
| `sisia-catalog` | title hit@10 91.2 % vs 0.0 % | **91.2 % vs 0.0 %**, OVERALL PASS, p99 10.5 µs | reproduces (identical) |
| `maphy-place` | typeahead hit@10 96.4 % | **96.4 %**; typo@10 99.7 %; p99 0.28 ms | reproduces (identical) |
| `geo-join` (municipal) | 4.50 M pts/s, 28.2× | **≈5.31 M pts/s, 28.8×**; pip counts byte-identical | reproduces, slightly faster |
| `blead-industry` | +18.1 pt in-sample (77.4→95.6) | **+18.1 pt (77.4→95.6)** | reproduces (identical) |
| `alec-surface` | body-query p99 18.5 ms < 25 ms | **24.1 ms < 25 ms**, 0 diffs vs brute force | reproduces under the bar; p99 elevated ~30 % over published — long-tail machine state, worth watching |
| `booted-schema` | name 100 %, comment 100 % top-10 | PASS; typo p99 0.58 ms | reproduces |
| `accel-kernel` | 2,400 trials 0 wrong; agg 387 M/s, group 305, top-k 291, filter 192 | **0 wrong**; agg 360, group 288, top-k 271, filter 186 M/s | correctness reproduces exactly; throughput −5..−7 %, within machine drift |
| `cdc-equivalence` | membership exact over 8,000 ops | OVERALL PASS | reproduces |
| `fuzzy-term` | exact p99 1,380 ns vs 1,000 ns bar (red); typo recall 14.8× | exact p99 **1,170–1,230 ns** (red); recall **12.3× / 11.8×** at 860 K terms | documented red holds; slightly better p99 than published |
| `pool-audit` | 0.00 % in all six cells | **0.00 % in all six cells** | reproduces (identical) |
| `prune-consistency` | ROADMAP text says "stays red at 512 decoys" | **PASS at every decoy count incl. 512, 2048** | **drift — the ROADMAP text is stale**; the bench's own verdict line already said all three pruning sites gate correctly. Roadmap corrected this session |
| `beat-btreemap` | PLA wins space/p50/p99, 4 distributions | OVERALL PASS, all rungs | reproduces |
| `crack-converge` | naive fails sequential, stochastic passes | MIXED as designed | reproduces |
| `phrase-cost` | 980 hits 0 wrong | OVERALL PASS, 0 wrong | reproduces |
| `biasd-entity` | arm A fails 80 % bar at 71.6 % | arm A **71.7 %** (FAIL); arm B 85.1 % PASS | reproduces (documented red) |
| `facet-shop` | tallies exact; p99 88 µs | OVERALL PASS; tally p99 84 µs; two-arm sort choosing scan at the median | reproduces |
| `presyo-catalog` | typo p99 4.54 ms < 5 ms; ~51 B/doc; expansion +21.8 in-sample | typo p99 **5.05–6.07 ms** (over); **57.3 B/doc**; expansion **97.0 %**, +21.0 pt | typo tail sits ON the documented ~250 K crossing — the red `p7`'s bar names; bytes/doc drifted +12 % (config-dependent, see below) |
| `presyo-expand` | +21.5 pt held out | **+21.4 pt**; strict trigger fired 0; strict damage +0.0 | reproduces |
| `presyo-categorize` | 46 % confidently, 2,169/s | **42.1 % confidently, 2,401/s** | reproduces with drift −3.9 pp coverage / +10 % throughput |
| `presyo-prior` | cannot answer (no broad-query labels) | same conclusion, same reason | reproduces |
| `presyo-broad` | 97.2 % with documented label limit | same | reproduces |
| `scale` | OVERALL FAIL by design; 1 M typo p99 13.6 ms | OVERALL FAIL; **14.03 ms** | documented red holds |
| `segment-scale` | rank-1 agreement 96–98 % at 50 segs | OVERALL PASS; selective rank-1 99.0–99.4 % | reproduces, slightly better |
| `image-corpus --limit 2000` | 0 panics; cheap tier <10 ms; 112 B/img | **0 panics**; 2.18 ms; 112.0 B/img; 0 extension disagreements; p58/p59 correctly WITHHELD (synthetic) | reproduces |
| `video-shot` | 27 videos, shot sampling ≥ uniform | OVERALL PASS; 14.88× fewer frames; p63 recall verdict still WITHHELD (no labels) | reproduces |
| `sfc-2d`, `fuzzy-decision`, `beat-btreemap`, `crack-converge` | see their docs | all ran; fuzzy-over-FM now viable through k=3 at σ=26 (published: k≤2) | reproduces; the k=3 note is machine/model drift on a decision bench |

**Missing inputs (recorded, not failed):**

- `real-million` was MISSING at sweep start and was then **recovered the same day**: the export is
  one SSH pipe to the house Postgres (the recipe printed in the bench header), and 1,199,988 real
  rows came back in under a minute. Fresh, on the committed tree: **typo p99 5.32 ms at 1 M real**
  (bar 5 ms: FAIL by 6 %) and **5.34 ms with the p83 tier prototype** — versus 8.26 ms published in
  `p55`, an improvement owed to `p69`'s varint postings and `p82`'s hand-off, not to the (reverted)
  tiers. The cap table on REAL vocabulary: cap 8 → 4.73 ms at 98.80 % top-10 agreement; cap 2 →
  3.35 ms at 92.95 %.

**Drift worth carrying forward:**

1. `prune-consistency` passes everywhere; the ROADMAP's "stays red at 512" sentence described the
   pre-`p25`/`p26` engine and was corrected this session.
2. `presyo-catalog`'s typo p99 straddles the 5 ms bar across runs (4.54 published → 5.05–6.07
   fresh). That is exactly the decision `p56` framed and the ROADMAP's next-list row 5 exists to
   settle: the bar is met to ~250 K real documents and not beyond. Not silently relaxed.
3. Bytes/doc on `presyo-catalog` read 57.3 vs ~51 published — the earlier figure was measured with
   `p69`'s varint postings on a different field configuration; this run's schema includes the
   facet + numeric columns the expansion path needs. Recorded rather than reconciled silently.

Fresh console output for every run above is in this directory's `*.log` files where a run was
captured end-to-end; summaries were transcribed at run time.


## Estate coverage — every repo checked (2026-09-08)

All 74 git repositories under `C:/Users/maran/Code/` were surveyed for a search-shaped workload
(the pattern classes: SQL `LIKE`/`ILIKE`, `toLowerCase().includes()` filtering, `tsvector`/GIN
columns, dedicated search routes). **19 have one.** Cross-referenced against this project's bench
coverage:

**Measured against `index` this sweep or before (10):** `presyo` (241 K catalogue + gold clusters),
`profstopick` (registrar snapshot), `blead` (25,979 business names), `sisia-app` (catalog),
`maphy` (places + geometry), `onegrid` (kernels), `booted` (2,600 tables), `alec` (119 K long
documents + the image corpus), `biasd` (entity aliases), and the video/geo tiers on their data.

**Declined, recorded, and re-checked (2):** `polkadoc` — declined Tantivy in writing with a
measured 0.10 s linear scan and "measured need is absent"; its search is now a Rust `search` module
over the user's own corpus, still a linear scan, still local. `advo` — declined a vector index; its
`tsvector` corpus search is unchanged. The declines stand on their own measurements, not on
politeness.

**Search-shaped, never measured, real data on disk — the p84 candidate list, 2 of 7 now measured
(same day):**

| repo | workload | data on disk | their matcher today | p84 verdict |
|---|---|---|---|---|
| `yclap` | species gallery filter | ~7.4 MB iNat pipeline JSONs | `toLowerCase().includes()` | **MEASURED** — [`p84`](../../bench/roadmap/p84-yclap-species.md): 93 % of typo'd species queries return nothing today; engine 99.4 % hit@10 at 24 KB index. Clean typeahead belongs to their uncapped filter (recorded straight) |
| `hobbycat` | listing search | 233 KB SQLite / **8 active listings** | SQL `LIKE %…%` | **MEASURED** — [`p85`](../../bench/roadmap/p85-hobbycat-listings.md): engine wins corrupted terms 65.6 % vs 15.6 %, but 8 rows = measured need absent (polkadoc class); bench re-runs against growth |
| `orsem-website` | course search | courses.json 369 KB + curriculum 306 KB | dedicated Search page | candidate — data on disk |
| `nookr2` | category + member combobox | Supabase seed 40 KB | `toLowerCase().includes()` | candidate — data on disk |
| `wheresthefx` | event search | 1.5 MB OSM venue fixture | drizzle `ilike` | candidate — data on disk |
| `openbid` | bid search | GIN `to_tsvector`, no static seed | Postgres FTS | candidate — needs an export |
| `trin` | candidate filter | 109 KB seed catalog | `toLowerCase().includes()` | candidate — data on disk |

**Search-shaped, no static corpus to measure against (6):** `medicapp` (Supabase `ilike`, prod data
in DB), `mesro`, `paracelis-civic-door`, `aisis+` (live-scraped schedules), `polkadoc`'s user
corpus, `advo`'s DB corpus. These need an export from their owners before a bench can be honest —
a bench against fabricated rows would be the sampling error `p60` documented.

**No search-shaped workload (the rest, ~40):** tooling, harnesses, agents, games, scaffolds —
including the "grouped NO" list where every match was argv/route `.includes()`. `life` remains
solved by a Postgres partial index at 2.9 ms, as the demand survey recorded.

The governing rule stands: a repo joins this table's top section only with a measurement it
already takes. The seven p84 candidates all have one (their own matcher is the baseline), and five
of the seven have the corpus on disk today.
