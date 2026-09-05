# Changelog

All notable changes to `index`. Format follows [Keep a Changelog](https://keepachangelog.com/);
this project is pre-release and unversioned, so everything lives under `[Unreleased]`. See
[ROADMAP.md](ROADMAP.md) for the tiered plan and [docs/roadmap-rejected.md](docs/roadmap-rejected.md)
for what was deliberately ruled out.

## [Unreleased]

### Corrected - 2026-09-05 (a benchmark error worth 9.4 points)

- **`boost 0.0` does not make a field inert.** It contributes no score, but a document matching a
  term there is still a MATCH - and a match earns `typo_bucket` 0, **the primary sort key**, so it
  outranks every document that does not match at all. Verified directly:

  ```
  schema: title (boost 3.0), facet (boost 0.0)
  doc 0: "camote powder" / "bakingneeds"
  query "bakingneeds" -> 1 hit: doc 0, score 0.0000, bucket 0
  ```

- **`p15`'s headline is corrected: +25.5 points from expansion, not +34.9.** It compared an
  expansion arm carrying a boost-0.0 category field against a baseline with no such field. Measured
  on presyo with **no expansion at all**, adding the field alone moves 61.7 % -> **71.1 %**. `p15`
  now shows all three rows and the fair baseline is the middle one.

- **`p18` and `p19` are unaffected** - both of their arms carried the facet field, so those
  comparisons were fair. `p18`'s +20.4 and `p19`'s no-op stand.

- **Caught by a baseline being implausibly high**, which is the same tell as every previous
  methodology error in this repo. Seventh so far, and the most expensive: it changed a number that
  had been reported repeatedly.

### Measured - 2026-09-05 (third corpus: prediction refuted, and a new adoption rule)

- **`profstopick-dept` bench - 2,253 Ateneo course titles across 92 departments.** Chosen because
  `p18`'s vocabulary-reuse condition made a **falsifiable prediction** about it, written into the
  bin before running: course titles reuse vocabulary heavily, so held-out retention should resemble
  presyo's 68 % rather than blead's 33 %.

- **The prediction is refuted, but the corpus cannot test the condition.** Its plain baseline is
  **already 99.2 %** - saturated - so there is no gain to retain and retention is undefined.
  Reporting "0 % retention, condition refuted" would be reading a ratio with a meaningless
  denominator. **The vocabulary-reuse condition remains neither confirmed nor refuted.**

- **A new adoption rule, which the earlier corpora could not have produced: expansion can HURT.**
  99.2 % -> 88.3 % (-10.8 pt), a fair same-schema comparison. **Do not apply `learn_expansion` to a
  facet that already retrieves well** - adding twenty terms to a query that was already returning
  the right documents can only displace them. The mechanism of the harm is larger than the
  `EXPANSION_DISTANCE = 1` ordering predicts and is **recorded as undiagnosed** rather than
  explained away.


### Measured - 2026-09-05 (held out on blead: the generalization claim gets its condition)

- **`learn_expansion` generalizes where a facet's members REUSE VOCABULARY - and that condition is
  now measured, not guessed.** `p18` reported blead in-sample only; the held-out split it named as
  missing has been run.

  | corpus | in-sample gain | held-out gain | held-out retains | staleness cost |
  |---|---|---|---|---|
  | presyo - grocery products | +34.9 pt | +23.9 pt | **68 %** | ~11-17 pt |
  | **blead - business names** | +20.4 pt | **+6.7 pt** | **33 %** | **13.7 pt** |

  **The staleness cost is comparable (13.7 vs ~17 pt) but what survives is not.** Half of
  `Baking Needs` and the other half share brands, product types and sizes, so a term learned from
  one predicts the other; two logistics firms share almost nothing but legal suffixes.

- **This narrows the claim and makes it more useful.** Not "it generalizes" but "it generalizes
  where a facet's members reuse vocabulary" - and **an adopter can check that on their own data
  before adopting**, using exactly the in-sample/held-out split these benches run, which needs no
  labels beyond the facet they already have.


### Measured - 2026-09-05 (categorizer sweeps, and a no-op worth running)

- **The operating curve, not the single number, is the deliverable.** `k` and the confidence
  threshold were chosen rather than measured, so both were swept.

  | threshold | accuracy | share kept |
  |---|---|---|
  | take everything | 68.0 % | 100 % |
  | 0.6 | 81.0 % | 68.7 % |
  | 0.8 | 89.5 % | 46.8 % |
  | **unanimous** | **95.7 %** | **22.1 %** |

  **At unanimity the index is right about 95.7 % of the products it will speak up for** - 22 % of
  the catalogue, good enough to apply automatically. The application picks the point; the vote share
  comes free with the prediction.

- **`k` is a coverage dial, not an accuracy dial.** Overall accuracy peaks at k=10 and is flat
  either side (66.7 % - 68.0 %), but confident accuracy climbs monotonically 73.8 % -> 85.0 % from
  k=3 to k=40 while confident share falls 86.3 % -> 50.4 %. A larger neighbourhood makes agreement
  rarer and more meaningful.

- **`learn_expansion` on the classifier is an exact no-op (-0.0 pt), and it was run anyway.**
  Predicted from the design: expansion fires only when the whole query IS a facet value, and a
  product name never is. Run because the prediction is falsifiable in a useful direction - **a
  mechanism that had fired there would have meant the strict trigger leaks**, which is `p17`'s
  damage gate checked from the opposite side.

### Measured - 2026-09-05 (the analyzer half of the goal)

- **`presyo-categorize` bench - the index as a classifier, filling a 21 % data-quality hole.** `p15`
  recorded that **50,607 of presyo's 241,677 products (20.9 %) are `Uncategorized`** and left it
  alone. This asks the inverse of every question measured so far: not "find the products in this
  category" but **"what category is this product?"**

  The method needs nothing new - search the name against products whose category is known, take a
  **majority vote over the top-10 neighbours**. kNN classification where **the index is the model**:
  no training, no embeddings, no second system.

  | | accuracy | share of predictions |
  |---|---|---|
  | all predictions | 68.0 % | 100 % |
  | **confident only (>=60 % agree)** | **81.0 %** | 68.7 % |

  **The confidence threshold is worth 13 points and costs nothing** - the vote share is a by-product
  of the prediction, so an app takes the confident 69 % automatically and queues the rest.

- **Applied to the real hole: 100 % get a proposal, 46 % confidently, at 2,169 products/second**
  single-threaded. The *lower* confidence there is correct behaviour rather than a disappointment:
  products left uncategorised are plausibly the ones that were hard to categorise to begin with, and
  a method equally sure about both sets would be the suspicious result.

- **Validation is deliberately conservative.** Accuracy is measured only on labelled products
  **withheld from the index**, never on the unlabelled rows - their true categories are unknown, so
  no accuracy is claimed for them and none is estimated.

- **The misses are mostly taxonomy boundaries, and that is measured rather than asserted.** Of 1,284
  misses, **188 (14.6 %) name a category sharing a word with the true one** - `Spirits` for
  `Liquor`, `Fresh Meat` for `Frozen Meat`, `Fresh Seafood` for `Frozen Seafood`. Counting those as
  acceptable would read 72.7 %, and **that number is deliberately not claimed**: whether `Spirits`
  may stand in for `Liquor` is presyo's call about its own taxonomy, and a bench that grades itself
  generously on someone else's category scheme is measuring its own opinion.


### Measured - 2026-09-05 (learn_expansion generalizes)

- **`blead-industry` bench - the feature works on a corpus it was not designed for.**
  `learn_expansion` was built against presyo's grocery catalogue; a feature measured only on the
  data it was built for has proved very little. `blead`'s lead store supplies a second corpus with
  the same failure shape and a completely different vocabulary: **25,979 real Philippine business
  names** tagged with an industry, where nothing in `10K EAST CONCRETE MIX SPECIALIST, INC.` says
  *Wholesale/Retail*.

  | corpus | documents | facet values | plain | learned | delta |
  |---|---|---|---|---|---|
  | presyo - grocery products | 241,677 | 145 | 61.7 % | 96.6 % | **+34.9 pt** |
  | **blead - business names** | **25,979** | **27** | **64.8 %** | **85.2 %** | **+20.4 pt** |

  `Construction` and `Public Admin` go from **0 %** to 80 % and 100 %. **MRR reaches 1.000** - after
  expansion every one of the 27 industries has a correct result at rank 1.

- **Comparable by construction.** Label leakage is 11.8 % here against presyo's 13.8 %, so the label
  is independent of the retrieval signal to nearly the same degree and a difference in outcome
  cannot be blamed on an easier label. Same metric, same arms, same `boost 0.0` facet field.

- **The smaller gain is explained, not glossed.** blead's 27 industries average ~960 members and
  lump unrelated firms together (`"Other Services"` reaches only 60 %, `"Real Estate"` 50 %);
  business names carry less signal than product names; and the achievable ceiling for this corpus
  was not computed, so 85.2 % may already be near it. **Two corpora is enough to say "not
  presyo-shaped"; it is not enough to say "general".**


### Added - 2026-09-05 (learned query expansion, in the engine)

- **`IndexBuilder::learn_expansion(facet_field, top_k)` - the engine now bridges vocabulary
  mismatch by itself.** For each distinct value of a low-cardinality field (category, department,
  tag), it learns the terms most concentrated in the documents carrying it; a query that **is** one
  of those values is expanded with them. An application gets this by naming a field - no pipeline,
  no model, no external service.

  On presyo's 241,677 real products: **61.7 % -> 96.6 % precision@10 in-sample**, 145 facet values
  learned, **+3.4 s build**, category-query latency **316 us** vs 178 us for an exact product query.

- **In-sample vs held out, labelled rather than blurred.** The 96.6 % is measured on the same
  catalogue the table was learned from. `p17-presyo-expand.md`'s **79.6 %** held out half the
  products. Both are real and they answer different questions - deployment condition for existing
  products, versus behaviour for products added after the table was built. **The ~17-point gap is
  the cost of a stale table**, and it is what should decide re-derivation cadence.

- **Emitted as an alternative inside every query group, not as a group of its own.** `bucket_of`
  SUMS a penalty per unsatisfied group, so a dedicated expansion group would make a document that
  matches only expansion terms pay `MISSING_TERM_PENALTY` for every original token - ranking a
  genuine category member BELOW a document that merely shares one word with the category's name.
  As alternatives, one expansion hit satisfies the group it sits in.

- **Priced with the existing typo dial.** `EXPANSION_DISTANCE = 1` reuses `typo_penalty^distance`
  for weight and `bucket_of` for ordering, so an expansion match ranks below an exact match and
  above nothing - without a second ranking mechanism to keep consistent.

- **The trigger is strict**, and `p17` measured why: a loose rule fires on 19.7 % of ordinary
  product queries and costs 2.0 points of exact-product hit@1, because `Signature Select Ice Cream
  Butter Pecan` contains the category `Cream`. Asserted by
  `expansion_fires_only_on_an_exact_facet_value`.

- **Format: `IDXTEXT2` carries 11 sections** (176-byte table). A learned expansion is a ranking
  signal, so an artifact that dropped it would rank worse than the index it was built from -
  "browse got worse after we deployed", which is close to undebuggable. Fourth time this rule has
  applied. Asserted by `a_learned_expansion_survives_a_round_trip`.

### Fixed - 2026-09-05

- **A fixed learning threshold silently learned nothing for small facet values.** Requiring a term
  to appear in 5+ of a value's documents means a value with a handful of documents yields an **empty
  table** - the feature looks broken rather than inapplicable. The threshold now scales,
  `(n/4).clamp(2, 5)`, so a large value behaves exactly as when `p17` measured it.


### Measured - 2026-09-05 (the vocabulary gap, largely closed)

- **`presyo-expand` bench - derived aliases are worth +23.9 points, held out.** `p15` found the
  61.7 % category-retrieval gap and proposed a query-time alias table **on a hunch**; `p16` measured
  the ceiling on curated aliases; this derives them from presyo's own catalogue and asks whether the
  derivation works.

  For each category, score terms by concentration inside it and append the top *k* to the query.
  One pass over the catalogue, a lookup table, **no model and no new storage.**

  | query | precision@10 | delta |
  |---|---|---|
  | category name alone | 55.7 % | - |
  | **+ top 20 derived terms** | **79.6 %** | **+23.9 pt** |
  | + top 20 **random** terms *(control)* | 19.8 % | **-35.9 pt** |

- **The control is the point.** Adding 20 terms changes what a query matches; if random terms helped
  equally, the gain would be "longer queries retrieve more" and the derivation would be decoration.
  Random expansion instead **costs 35.9 points** - a ~60-point spread. Third bench in this repo
  designed *in advance* to separate the boring explanation from the interesting one.

- **Leak-free by construction.** Expansions come from a train half (120,839 products); the index
  holds only the disjoint test half (120,838). No product contributes both to a term's weight and to
  the score it earns. Deriving from the same assignments being scored would report a large win by
  construction - the trap `p13` avoided and `p12` fell into.

- **Caveat recorded rather than discovered later: most derived terms are brand names**, not concepts
  (`"Soup Mixes"` -> *knorr*, *campbell*; `"Baking Needs"` -> *torani*, *lakanto*). The mechanism
  largely learns brand→category association - real signal in a grocery catalogue, but it cannot
  cover a brand it has never seen, and per-category quality varies (`"Pet Accessories"` picked up
  *eyewear*, *35mm*, *sweden* and still improved on aggregate).

### Measured - 2026-09-05 (sizing the gain, and testing the brand caveat)

- **Derived expansion captures 71 % of the achievable gain.** Indexing the category name on every
  product - `p15`'s option 2, and close to cheating since the label becomes part of the document -
  is the right upper bound. It reaches **89.5 %**.

  | | precision@10 | share of achievable gain |
  |---|---|---|
  | category name alone | 55.7 % | - |
  | **derived expansion** | **79.6 %** | **71 %** |
  | derived, brands removed | 70.2 % | 43 % |
  | category indexed *(upper bound)* | 89.5 % | 100 % |

- **The residual 9.9 points are not vocabulary.** Even with the label indexed, precision stops at
  89.5 % because a product whose *name* contains the category's words outscores one that merely
  belongs to it. That is BM25 field weighting, a different problem - and it makes `p15`'s option 3
  (dense/hybrid retrieval) a weaker case than when it was listed.

- **The brand caveat was tested, not just carried.** `p17` flagged that most derived terms are brand
  names and that an expansion built from history cannot cover an unseen brand. The catalogue has a
  `brand_name` column, so striking every brand token out measures it: **brand-free expansion still
  captures 43 % of the achievable gain (+14.5 pt)**. Not pure brand memorisation; a category of
  entirely unseen brands degrades toward +14.5 rather than to zero. Brand-free is also better at
  k=5 (70.2 %) than k=20 (67.0 %) - once brands are gone, depth pulls in noise.

### Gated - 2026-09-05 (expansion damage, caught and fixed)

- **The damage gate caught a real regression before it shipped.** Query expansion is built for
  *category* queries, but a shipped expander does not know what kind of query it was handed. Fired
  loosely - whenever a query *contains* a category's words - it triggers on **19.7 %** of ordinary
  product queries and costs **2.0 points of exact-product hit@1**:

  ```
  Signature Select Ice Cream Butter Pecan 1.5Qt   matched category "Cream"
  Moringa-O2 Shampoo Herbal 200ml                 matched category "Shampoo"
  Huggies Dry Diapers Pants Double XL x 22pcs     matched category "Diapers"
  ```

  Every casualty is a product whose name legitimately contains a category word, buried under that
  category's other members. **The feature would have made browse better by making search worse.**

- **The fix is one comparison.** Requiring the query's token set to *equal* a category's rather than
  contain it fires on **zero** product queries and costs **+0.0 points**, while retaining the full
  +23.9 on genuine category queries.

  | trigger | hit@1 | fired on |
  |---|---|---|
  | plain | 99.2 % | - |
  | loose (contains) | 97.3 % | 596 of 3,021 |
  | **strict (equals)** | **99.2 %** | **0** |

  Shippable design, both sides measured: **+23.9 points on browse, 0.0 damage on lookup.**

### Housekeeping - 2026-09-05

- **Stopped a bad background sweep.** A repo-wide grep for search-shaped code ran 31 minutes and
  reached ~15 % of the tree, and its pattern (`\bsearch\(`) matched `.search()` on any JS string -
  scoring unrelated repos identically to the ones that mattered. Direct row-count inspection had
  already superseded it. **Disk size and keyword counts were both bad proxies for "has a corpus".**


### Measured - 2026-09-05 (what an alias table is worth)

- **`biasd-entity` bench - p15's proposed fix, now with evidence.** `p15` ranked a query-time
  alias/expansion table as the first fix for its 61.7 % category gap, **on a hunch and with no
  ground truth**. `biasd` - a PH news aggregator that resolves political entities in article text -
  has exactly that ground truth: **4,560 real entities and 7,217 alias queries**, with **292 (4.0 %)
  sharing no token at all with their canonical label**.

  | | hit@1 | hit@10 |
  |---|---|---|
  | canonical label only | 71.6 % | 94.1 % |
  | **label + aliases indexed** | **86.5 %** | **99.8 %** |
  | *on the 292 disjoint queries* | | |
  | canonical label only | 0.0 % | **0.7 %** |
  | **label + aliases indexed** | 43.5 % | **95.5 %** |

  `"Bobby"` -> Alberto Pacquiao, `"Queenie"` -> Alexandria Gonzales, `"Baham"` -> Abraham Kahlil
  Mitra. Filipino nicknames - nothing to match, and no tokenizer or typo policy reaches them. **The
  same failure class p15 found in presyo's categories.**

- **Stated as a ceiling, not a forecast.** biasd's aliases are curated; presyo's would have to be
  derived from co-occurrence, which starts from a strictly harder position. And arm B indexes
  aliases where p15's option 1 would expand the query - different mechanisms, different costs, only
  the first measured.

- **A finding for biasd itself: arm A fails an 80 % hit@1 bar at 71.6 %.** An index of canonical
  political names is not adequate for what newspapers actually print; more than a quarter of real
  surface forms miss the right entity. biasd already carries the surface lists, so this quantifies
  what skipping them costs.

### Found, not yet benchmarked - 2026-09-05

- **`blead/data/lead-store.db`** - 30,687 distinct real business names with industry, city, address
  and PSIC code, and visible near-duplicates (`10K EAST CONCRETE MIX SPECIALIST, INC.` vs
  `10K CONCRETE MIX SPECIALIST INC.`). A seventh app, entity-dedup shaped.
- **A repo sweep found nothing else.** Several repos looked large (hobbycat 1,468 MB, trin 763 MB)
  but the size was `node_modules`: the databases hold **16 and 10 rows**. Disk size was a bad proxy
  for corpus size and row counts should have been checked first.


### Measured - 2026-09-05 (241,677 real products, and a conclusion corrected)

- **`presyo-catalog` bench - the first workload in this project that is NOT at ceiling: 61.7 %
  precision@10.** Built on `presyo/data/endless-prep/catalog-active.csv`, presyo's active catalogue
  exported from production: **241,677 real products**, four times the largest real corpus this
  project had.

- **This corrects a conclusion I had stated four times.** After `p13`/`p14` I wrote that every
  measurable workload was at ceiling and that "everything this repo can learn from static corpora,
  it has learned". **Wrong** - and wrong because I had not looked hard enough for corpora. Each
  earlier bench asked a question whose answer was already in the document text; this one does not.

  | bench | query | result |
  |---|---|---|
  | `p6-real-corpus` | a school's own name | 99.8 % |
  | `p8-sisia-catalog` | a course's own code | 100 % |
  | `p13-presyo-prior` | a listing's own name | 98.4 % |
  | `p14-presyo-broad` | a word the product name repeats | 97.2 % |
  | **`p15-presyo-catalog`** | **a category the name never contains** | **61.7 %** |

  Saturation was a property of the questions, not of the engine.

- **The failure mode is vocabulary mismatch, and no existing feature can fix it.** `"Baking Needs"`
  should return *Camote Powder*, *Mung Beans*, *Sago Tapioca*, *Cream of Tartar*, *Hotcake Mix* -
  **not one shares a token with the query**. Six categories score 0 %. BM25F tuning, typo tolerance,
  prefix anchoring and static priors all operate on tokens that must match something.

- **Label independence is what makes it a real test.** Only **13.8 %** of products contain their own
  category name, against `p14`'s `gold_product_type` labels which retailers repeat in the product
  name - which is why `p14` could only measure lexical precision.

### Corrected - 2026-09-05 (p7-scale was generous)

- **Recombination understates typo p99 by ~1.9x.** `p7-scale` reaches 250 K and 1 M by recombining
  61,467 real schools and flags the caveat; p15 quantifies it. Recombination stalls at **41,069
  terms** because it cannot invent vocabulary, while a real catalogue of similar size carries
  **117,472** - nearly 3x the dictionary a fuzzy expansion must search.

  | | p7 @250 K (recombined) | p15 @241,677 (real) |
  |---|---|---|
  | distinct terms | 41,069 | 117,472 |
  | typo p99 | 3.08 ms | **5.72 ms** |

  The 5 ms bar is missed at **a quarter of a million real rows**, not at a million. An honest
  downgrade of a number this project has been quoting, from better data rather than a code change.


### Verified - 2026-09-05 (no scale regression from today's scoring changes)

- **Three additions to the hot scoring path cost nothing measurable at 1 M documents.** Static
  priors, prefix anchoring and deletion each add per-candidate work inside the retrieval loop, and
  the day's correctness benches (`real-corpus`, `sisia-catalog`, `maphy-place`) say **nothing** about
  latency at scale. `p7-scale` was re-run before calling the changes done.

  | | recorded | after |
  |---|---|---|
  | exact p50 @1M | 1.14 ms | 274 us |
  | typo p50 @1M | 1.26 ms | 598 us |
  | typo p99 @1M | 9.15 ms | 6.01 ms |
  | bytes/doc @1M | 157.2 | **161.2** |

  Latency is same-or-better and is **not** claimed as a speedup - the two runs are from different
  sessions and are not controlled against each other. The supported claim is the negative one.

- **The index grew exactly 4.0 B/doc, and that is the whole story.** `first_term` is one `u32` per
  document. Priors were uniform and nothing was deleted, so **both wrote zero-length sections** -
  the two features that were not used cost nothing at all, which is what lets them ship without
  taxing an adopter who never calls them.


### Measured - 2026-09-05 (the broad-query workload, and the end of static priors)

- **`presyo-broad` bench - the labelled workload `p13` said did not exist.** `p13` claimed building
  one required somebody to decide what the right answer to "milk" is. **That was half wrong**:
  presyo's gold fixture already carries a curated taxonomy (`gold_brand`, `gold_product_type`), so
  the labels were in the file. **79 broad queries** (32 brand, 47 category), mean **53 relevant
  listings each**. The index sees only `raw_name`; labels come from gold metadata that is not
  indexed.

  Metric is **precision@10**, not recall - recall@10 is meaningless when 53 listings are relevant,
  and reporting it would repeat the `"CITY "` mistake from `p12`.

  | prior | precision@10 | delta |
  |---|---|---|
  | none | **97.2 %** | - |
  | source quality | 95.8 % | **-1.4 pt** |
  | popularity | 94.7 % | **-2.5 pt** |

- **Static priors are settled: they ship OFF.** On the workload they were supposed to be for, they
  do not merely fail to help - they **actively degrade precision by up to 2.5 points**, pulling
  well-stocked or clean-named products above ones that match the query. `p13` showed a prior buying
  nothing; `p14` shows it costing something. Two independent measurements, one answer.

- **The tail was checked before being called a target, and it was the labels.** `"chocolate"` scores
  40 % precision@10, which looks like the obvious next ranking target. **65 listings say "chocolate"
  in the retailer's name; only 14 carry it in `gold_product_type`.** "Nestle Chuckie Chocolate Milk
  Drink" is typed "Milk Drink", so the label scores it irrelevant while a shopper plainly wants it.
  The engine was right and the label was wrong - **true precision is HIGHER than 97.2 %**, and the
  tail is not work to be done.

  Sixth methodology catch in this repo, second caught *before* publishing. The habit is now
  explicit: **check whether a bad number is the system failing or the measurement lying, before
  assigning work to it.**

- **Every measurable consumer workload is at ceiling** - `p6` 99.8 %, `p8` 100 %, `p13` 98.4 %,
  `p14` 97.2 %. Further ranking work would be tuning against noise. The roadmap's next row is no
  longer a feature: it is an adopter running this against production traffic.


### Measured - 2026-09-05 (static priors: the answer is no, here)

- **`presyo-prior` bench - the last unproven feature, settled.** Static priors were measured on
  presyo's **500 gold cross-store clusters, 2,440 real listings, 13 retailers**, with the prior
  fitted on a train half and scored on a **held-out** half. Fitting and scoring on the same clusters
  would have made the prior a compressed copy of the answer key and reported a win by construction.

  | prior | recall@10 | hit@1 | rankings |
  |---|---|---|---|
  | none | 98.4 % | 99.4 % | - |
  | source quality (any spread) | 98.4 % | 99.4 % | **1,227 moved** |

  **+0.00 points.** The retailer quality spread is real (0.80 to 0.99 across sources) and the prior
  demonstrably applies - it reordered 1,227 held-out queries - it simply buys nothing.

- **The `rankings` column is the point.** "No effect" and "no benefit" produce identical metric
  columns, and the wrong one of those is the one that silently ships. Measuring whether the feature
  changed anything is what makes the negative result trustworthy rather than a possible bug report.
  This is the first bench in the repo designed *in advance* to distinguish them, which is the habit
  the previous five methodology failures were arguing for.

- **The cause is headroom, not the feature.** 98.4 % recall@10 leaves 1.6 points in the entire
  workload, and a prior cannot add a token a listing does not have. So the corpus **cannot answer
  the question** rather than answering it negatively: a prior earns its place on broad or browse
  queries where candidates are near-tied, and presyo's gold set has ground truth only for the
  specific-product task.

### Changed - 2026-09-05

- **Static priors are now labelled narrow rather than unproven**, on evidence. They stay shipped -
  correct, cheap, serialized, safe by construction - and `ROADMAP.md` now tells an adopter to leave
  them off until their own numbers say otherwise.
- **The next roadmap row is no longer engine work.** Every consumer benchmark in this project
  (`p6` 99.8 %, `p8` 100 %, `p13` 98.4 %) is at or near ceiling on the task it measures, so further
  ranking work would be tuning against noise. The blocker is a **labelled broad-query set**, which
  means someone deciding what the right answer to "milk" is.


### Added - 2026-09-05 (deletion)

- **`Searcher::delete` / `undelete` - the index can finally forget.** One bit per document, empty
  when nothing is deleted, routed to the owning segment by global ordinal. `live_count()` and
  `deleted_count()` report the split; `doc_count()` still counts tombstones.

  This was the last correctness gap against presyo, which soft-merges products via `superseded_by`
  (36,260 merged rows) and filters `WHERE superseded_by IS NULL` on public reads. An index that
  could not forget kept serving merged duplicates until the next rebuild.

- **Deletion does not rewrite postings**, and the cost is stated rather than hidden. A deleted
  document stops appearing at once but still counts toward document frequency and average field
  length until a rebuild - Lucene's behaviour between merges, and right here for the same reason:
  rewriting postings per delete turns an O(1) operation into an O(index) one. After many deletions
  IDF reflects a collection larger than the live one, so near-ties can reorder; the result SET is
  always correct.

- **`needs_compaction()` counts deletions, not just additions.** A single segment that has forgotten
  a third of its documents has skew 0.0, so a signal watching only skew would report it healthy
  forever while it drifted arbitrarily far from a clean rebuild. For presyo, deletions are the
  likelier driver of the two.

- **Filtering happens at heap INSERTION, not at candidate selection.** The candidate step is
  followed by cursor advances the MaxScore loop depends on; skipping it early would either
  desynchronize them or duplicate the advance. Scoring a deleted document and discarding it is a
  little wasted work in exchange for one obviously correct place to filter.

- **Format: `IDXTEXT2` carries 10 sections** (160-byte table). Deletions MUST persist - an artifact
  that dropped them would resurrect every deleted document on reload, which for presyo means the
  merged duplicates reappearing. Asserted by `deletions_survive_a_round_trip`.


### Added - 2026-09-05 (anchored prefix matching)

- **Typeahead now knows the user is typing from the START of a name.** `p12-maphy-place.md` found
  every typeahead miss to be a prefix ending part-way through a second token: `"DEL C"` plans as
  exact-`del` plus prefix-`c*`, and since `del` is common and `c*` matches nearly everything,
  `DEL CARMEN` never ranked. A tokenized query had lost a string-level fact.

  Storing each document's `first_term` (term id of the first token of field 0, **four bytes per
  document**) restores it. In prefix mode a document that does not anchor keeps `UNANCHORED_KEEP`
  of its score. **Typeahead hit@10 94.6 % -> 96.4 %**; `real-corpus` and `sisia-catalog` unchanged,
  which is the check that matters for a global scoring change.

  Applied as a **demotion of the non-anchored**, never a boost of the anchored - the same rule
  static priors follow: a scoring factor above 1 would let a true score exceed the block maxima the
  pruner trusts and silently drop valid results, only on corpora large enough for pruning to
  engage. Fires only for multi-token prefix queries: `"carmen"` must still find `DEL CARMEN`
  unpenalized, and for a single token the prefix term IS the whole query.

- **Format: `IDXTEXT2` grew to 9 sections** (later 10; see the deletion entry above). `first_term` is serialized because
  anchoring is a *ranking* signal - an artifact that dropped it would rank worse than the index it
  was built from, and the symptom ("search got slightly worse after we deployed") is close to
  undebuggable. Asserted by `prefix_anchoring_survives_a_round_trip`.


### Added - 2026-09-05 (static priors)

- **`IndexBuilder::add_with_prior` - query-independent per-document importance.** Normalized into
  `(0, 1]` at build time, which removes a correctness trap **by construction**: retrieval prunes
  against upper bounds (block maxima, and the non-essential bail comparing `score + optimistic
  remainder` to the threshold), so a multiplier above 1 would make true scores exceed those bounds
  and the pruner would silently discard documents that belong in the result - only on corpora large
  enough for pruning to engage. Scaling by the largest prior keeps every applied factor `<= 1`, so
  no bound needed patching and the pruning code is untouched. Only prior *ratios* affect ranking, so
  nothing is lost.

  A prior scales the score and deliberately **does not touch `typo_bucket`**, which stays the
  primary sort key: an important document matched through a typo must still lose to an exact match
  on an unimportant one. Asserted in `a_prior_cannot_outrank_an_exact_match`.

- **Format `IDXTEXT1` -> `IDXTEXT2`**, section table 7 -> 10 spans (112 -> 160 bytes) across the
  day: prior, then `first_term`, then the deleted bitmap. One unreleased version absorbed all
  three. The magic was
  bumped rather than the span appended silently, because a reader expecting seven spans would
  mis-parse the eighth as posting data and fail as garbage results rather than as an error. A
  uniform prior writes a zero-length span - the absence of a prior and a uniform prior are the same
  thing, and neither pays `doc_count` floats to say nothing.

### Added - 2026-09-05 (maphy place search: the fifth app)

- **`maphy-place` bench.** `place-index.ts` scores every entry and alias on every keystroke with
  four exact-substring predicates, so **the score of a misspelling is exactly zero**.

  | query class | engine@10 | maphy@10 |
  |---|---|---|
  | exact place name | 100.0 % | 100.0 % |
  | typeahead (answerable) | 94.6 % | **100.0 %** |
  | one-character typo | **99.7 %** | 9.3 % |

  **maphy returns an empty list for 90.7 % of one-character-typo queries.** Latency is a tie at this
  pool size (27.5 vs 31.3 us p50); claiming a win there would be dishonest.

### Fixed - 2026-09-05

- **A benchmark reported a spurious engine loss and it moved the roadmap.** The typeahead row used
  each name's first five characters as the query, so **all 40 municipalities named `CITY OF ...`
  issued the identical query `"CITY "`** and all 17 regions issued `"REGIO"`. No ranker can put 40
  documents in 10 slots, so 137 of 1,067 queries measured which arbitrary ten a tie-break picked.
  Excluding them moved the engine 85.2 % -> 94.6 % and maphy 90.4 % -> 100 %.

  From the spurious loss, *static priors* had been named the next roadmap row. Sweeping the prior's
  strength from 0 to 1.0 then moved typeahead hit@10 **not at all** - identical at every setting,
  including off - which is what sent the investigation to the real cause. **A fix that does not move
  the number was never addressing the number.** The real gap is anchored prefix matching, now the
  next row; the priors are kept but relabelled as unproven.

- **An exclusion filter silently did nothing.** It keyed on `normalize("CITY ")`, which trims the
  trailing space, against a map built from 5 chars of the normalized full name (`"city "`). The
  filter excluded 45 queries instead of 137 and the numbers barely moved, which looked like the
  hypothesis being weak rather than the filter being broken.

### Honest open-items - 2026-09-05

- **Static priors have a rationale and no evidence.** Every consumer has a query-independent
  importance signal, but no benchmark yet shows the feature improving ranking on one.
- **The barangay tier (~42,000 entries) is not on disk**, so the latency claim is untested at
  maphy's shipped worst case; the measured pool is ~40x smaller.
- **Anchored prefix is unbuilt.** It is the actual fix for the remaining typeahead gap.


### Added - 2026-09-05 (maphy place search: the fifth app)

- **`maphy-place` bench - maphy's place search reproduced and measured, and the result is mixed.**
  `apps/web/src/components/map/lib/place-index.ts` scores every entry and every alias on every
  keystroke with four exact-substring predicates, so **the score of a misspelling is exactly zero**.

  | query class | engine@10 | maphy@10 |
  |---|---|---|
  | exact place name | 100.0 % | 100.0 % |
  | typeahead (first 5 chars) | 85.2 % | **90.4 %** |
  | one-character typo | **99.7 %** | 9.3 % |

  **maphy returns an empty list for 90.7 % of one-character-typo queries.** That is the win, and it
  is not close.

  **The engine LOSES typeahead**, and the reason is worth more than the win: maphy's tie-break ranks
  by administrative level, a domain prior BM25F cannot express. A hand-rolled ranker that encodes
  domain knowledge beats a general one that does not, and no BM25 tuning fixes it - the engine needs
  a **static prior** hook. This is the cleanest evidence for that roadmap row the project has, and
  it came from losing.

  **Latency is a tie** at this pool size (27.8 vs 34.0 us p50; maphy is faster at p99). A linear
  scan over 1,067 short strings is not slow, and saying otherwise would be dishonest.

### Honest open-items - 2026-09-05 (place search)

- **The barangay tier is not on disk.** ~42,000 more entries is maphy's shipped worst case and the
  only place the latency claim can be tested; `apps/web/public/data/place/` is empty in this
  checkout. The measured pool is ~40x smaller, so the latency row is a floor on the gap, not the
  gap.
- **A metric, not the engine, produced a spurious FAIL.** Exact-name rank 1 first read 91.8 % and
  failed a 95 % gate. **137 of 1,067 entries (12.8 %) are duplicate names**, so demanding one
  specific row at rank 1 demands the impossible and would score a perfect ranker at ~87 %. Measured
  against the set of same-named places, both systems get 100 %. Fourth time in this repo a
  methodology was wrong before the code was - and every one was caught by a number sitting
  suspiciously close to a structural constant of the data.
- **Adopting the engine here is a trade, not an upgrade**: a typeahead regression for a typo win.
  That is maphy's product decision, not this bench's.


### Added - 2026-09-05 (the browser point-in-polygon number)

- **`index-geo` + `index-geo-wasm` + `js/geo.mjs` - point location in a browser at 3.61 M points/s,
  ~27x a JavaScript scan.** `docs/research/geometry-sota.md` found the geometry stack largely solved
  and named one hole with **no published figure in any language**: point-in-polygon at scale in a
  browser. `bench/roadmap/p10-geo-join.md` filled the native half; `p11-geo-wasm.md` fills this one.

  The finding is that **WASM is at parity with native** - 4.43 M points/s in Node and 3.61 M in real
  headless Chrome, against 4.50 M for the same index in native Rust, and against DuckDB's 2.02 M
  native *multicore* reference. A browser costs about 20 %, not a factor. The reason is structural
  rather than a compiler win: most queries touch no geometry at all, so what crosses into WASM is a
  binary search over a `u64` array.

  **22,334 bytes gzipped**, against DuckDB-WASM's 149.4 MB npm package. Hand-written raw-pointer C
  ABI, no wasm-bindgen, same convention as `index-wasm` - so one artifact serves Node, the browser,
  an edge worker and a native FFI.

- **The gate now runs it.** `geo-bench` is wired into CI as a **correctness** gate, not a
  performance one: it throws unless the WASM index and a pure-JS scan return the identical polygon
  for every one of 53,715 real points. CI runners are too noisy to gate on timings; the agreement is
  what has caught every bug in this line of work.

### Fixed - 2026-09-05

- **A MultiPolygon is not a polygon with holes.** The JS fixture loader treated the first ring of
  each polygon as its outer boundary and every later ring as a hole - correct for a simple polygon,
  **wrong for an archipelago**, where each island is a second *outer* ring. It silently turned most
  of the Philippines into holes and reported 34,726 POIs inside a province where the truth is
  51,732.

  The per-point assert did not catch it, because both JS arms shared the loader and were wrong
  together. The **cross-tier** comparison against the independent Rust implementation did.
  **Two agreeing implementations that share an input parser agree about the parser, not about the
  answer.** `js/geo.mjs` now takes `{ pt, outer }` rings explicitly and its doc comment names the
  trap.

### Honest open-items - 2026-09-05 (browser tier)

- **Chrome only.** No Safari, no Firefox. The ABI is deliberately fixed-width - `i32` offsets, no
  memory64, no relaxed SIMD - because relaxed SIMD is Safari-flag-gated and memory64 is
  Safari-unsupported. "Designed for it" is not "measured on it".
- **No SIMD, no workers.** The crossing test is a natural fit for vectorization and is unexercised;
  everything measured runs on one thread.
- **The favourable regime is many small polygons.** 88 large provinces give only 4.8x, because a
  bbox-prefiltered scan over 88 candidates is already cheap. Stated as a rule with a condition
  rather than as a single number.


### Added - 2026-09-05 (geometry: point-in-polygon as an index)

- **`geo-join` bench - point-in-polygon over real maphy geometry, 28.2x over a naive scan and 9.1x
  over a bbox-prefiltered one.** `docs/research/geometry-sota.md` surveyed the field and found the
  cloud-native geometry stack largely solved - FlatGeobuf's bbox-over-HTTP, PMTiles tile addressing,
  COPC octree range reads, meshopt as the vertex codec - and one hole with **no published numbers in
  any language**: point-in-polygon at scale in a browser. This is the first half of filling it.

  Both sides of the join are real files: **53,715 POIs** and **88 unclipped PSGC-coded provinces**
  (991 rings, 37,507 vertices) or **2,454 municipal polygons** decoded from maphy's own PMTiles.
  Provenance and rebuild commands are in `bench/fixture/README.md`.

  Two index shapes were built, and they give opposite answers, which is the finding:
  a **cell index** (rasterize to a Hilbert-ordered grid; interior cells answer with zero geometry)
  wins on **many small polygons** - 11.93 ms, 126 KB - while a **vertex index** (each cell stores
  the polygons containing its centre plus the boundary segments crossing it, answered by a local
  parity walk) wins on **few large ones** - 11.65 ms against the cell index's 17.72 ms. Measuring
  only one dataset would have produced a confident and wrong general claim in either direction.

- **`sfc-2d` now runs on real coordinates**, closing the gap `p9-sfc-2d.md` recorded against itself.
  Real and synthetic points on an identical grid give **identical range counts** and over-fetch
  within 7 %, so the synthetic stand-in was honest and the earlier conclusion is unchanged.

### Honest open-items - 2026-09-05 (geometry)

- **The browser number is still unmeasured.** `geo-join` is native Rust. The vacuum the research
  identified is specifically a WASM/browser figure, and this table does not get to claim it. The
  native throughput is 4.50 M points/s single-threaded against DuckDB's 2.02 M points/s native
  multicore reference - different hardware, different data, and DuckDB solves the harder general
  case.
- **Vertex dedup does not pay on this data: 1.09x.** Shared-vertex topology is the premise of
  TopoJSON's arc sharing. On maphy's real province geometry it collapses 37,507 vertices to 34,442,
  flat from a 340 m quantization down, because adjacent provinces were simplified independently and
  do not share vertex coordinates. Most duplicate instances are ring closures. Recorded as a
  refuted hypothesis rather than built on.
- **41,966 barangays is an extrapolation.** The trend from 88 to 2,454 polygons (1.9x to 28.2x) is
  strongly favourable but unrun at maphy's real target size.
- **Still no head-to-head against an R-tree.** `p9`'s caveat stands: sufficient is not better.
- **Three correctness bugs, all caught by asserting equality with the scan over every point.**
  Boundary and interior cells are not mutually exclusive when neighbouring polygons have slivers
  between them (28 wrong answers); a cell centre can be inside several overlapping polygons at once
  (1,496); and a parity walk is undecidable when the ray passes exactly through a vertex (1 in
  53,715, now detected and deferred to a full test). Sampled agreement would have missed the third.


### Added - 2026-09-05 (incremental update)

- **`index-text::searcher` - multi-segment search, so documents can be added without a rebuild.**
  `docs/adoption.md` named index immutability as the real gate on presyo, whose daily scrape
  processes 2.08 M raw observations; a rebuild is ~15 s at a million documents. A `Searcher` holds
  ordered immutable segments, new documents go into a small cheap one, and queries run across all
  segments and merge with the same comparator the single-segment path uses.

  The design fits this project specifically: **the application's database is the source of truth and
  the index is derived**, so compaction is not a merge of segments - it is a rebuild from rows the
  app already has, on whatever schedule it likes, while new rows become searchable immediately.
  `skew()` reports how much of the collection lives outside the largest segment and
  `needs_compaction()` is advisory, because this crate does not know when an application can afford
  a rebuild and silently blocking a write to compact would be worse than saying so.

  Global document ordinals never move when a segment is appended, so an application may store them.

### Honest open-items - 2026-09-05

- **Segmented scoring perturbs the order of near-equal documents, and the test says so.** BM25 needs
  collection-wide statistics; a segment only knows its own. Measured on a 195+5 split: a
  **selective** query identifies the same document as a full rebuild, while a **broad** query
  matching most of the collection can reorder its near-ties. The result SET is preserved either way.
  This is the standard cost of segmented search - Lucene's IDF is per-shard for the same reason -
  and it is asserted per query class rather than averaged into a single flattering number.
- **Deletion is not implemented.** `Searcher` closes additions only. presyo soft-merges products via
  `superseded_by` (36,260 merged rows), so an index that cannot forget will serve merged duplicates
  until compaction. A per-segment deleted bitmap is the next row.
- **A test fixture, not the engine, was wrong.** The first version of the skew corpus ended every
  document with the word `pack`, and `pack` vanished as a term - because the analyzer correctly
  merged `3 pack` into the quantity `3pc`. The engine was right; the fixture was misleading, and the
  comment now says so.


### Added - 2026-09-05 (sisia-app, on sisia-app's own data)

- **`index-bench` bin `sisia-catalog`** + `bench/roadmap/p8-sisia-catalog.md`. **sisia was written
  off one round too early.** The corpus this repo has benchmarked from the start declares
  `"source": "sisia class_section_all"` in its own metadata — it *is* sisia's registrar table,
  exported through profstopick's research pack: **2,253 distinct course-code x title pairs** of real
  sisia data, on disk the whole time.

  Its catalog search (`Course.ts`) is `course_code LIKE ?` plus `LOWER(title) LIKE ?`, ordered by
  code, with no relevance ranking. Reproduced as the baseline rather than strawmanned.

  | | engine | sisia's shipped LIKE |
  |---|---|---|
  | exact code hit@1 | 100.0 % | 100.0 % |
  | **out-of-order title words hit@10** | **91.2 %** | **0.0 %** |

  A substring `LIKE` matches one contiguous run, so a two-word query in the wrong order matches
  nothing across 2,038 real course titles — and no parameter changes that, because the limit is the
  operator. It is the same failure class sisia documented at `driveHybridSearch.ts:88-93`, where AND
  semantics "matched almost nothing" and switching to OR then over-matched.

### Honest open-items - 2026-09-05

- **A non-win, kept rather than deleted.** On the prefix-bleed set the engine is **not** better than
  sisia's `LIKE`, and two attempts to construct a metric where it was are recorded in the bench doc.
  The first measured rank: both reach 100 %, because `ORDER BY course_code` sorts `CHEM 399.1` above
  `CHEM 399.11` by lexicographic luck, so sisia's documented bug does not manifest as a ranking
  failure on this corpus. The second measured extra rows returned and the engine came out **worse**
  (1,391 vs 295) — because it *ranks* where `LIKE` *filters*, so that metric was measuring recall
  and calling it imprecision. **For exact course-code lookup, sisia's `LIKE` is adequate here.**
- **sisia's hybrid path is untouched.** The `ts_rank_cd` sparse arm fused with pgvector by RRF k=60
  and reranked by Vertex needs a database, a corpus and an API key this machine does not have. The
  engine's sparse arm is shaped to drop into it (`query -> (id, rank)[]`), but that is asserted, not
  measured.


### Added - 2026-09-05 (champion lists)

- **Static pruning via champion lists.** Per term with `df >= 512`, the positions of its 64
  highest-scoring documents are precomputed; a query seeds its top-k heap from **one** term's
  champions - the most discriminative that has a list - so the threshold is already high when the
  scan begins. Champions are real documents scored exactly as the main loop scores them, so no
  answer changes; the main loop declines to insert a seeded document twice.

  | | exact p50 @1M | typo p50 @1M | typo p99 @1M |
  |---|---|---|---|
  | before | 475-580 us | 809-875 us | 6.02-6.40 ms |
  | after | **279-314 us** | **643-687 us** | 6.36-7.24 ms |

  **Median latency improved ~1.7x; the tail did not move.** Kept because throughput follows the
  median, and reported honestly because the tail did not.

### Honest open-items - 2026-09-05

- **The 1 M tail cannot be resolved at the precision the bar demands.** Three identical runs of the
  same binary measured **6.36, 6.72 and 7.24 ms** - a 0.9 ms spread on a 5 ms bar. Any p99 claim at
  this scale that omits that spread is overclaiming, so the row stays FAIL rather than being
  declared met by picking the best run.
- **Champion lists raise the threshold; they do not make block maxima informative.** Those are two
  different problems and only the first is solved. On a query scoring against one common term, a
  document's score tracks its length and a block of 128 arbitrary documents almost always contains a
  short one, so a legitimately-high block maximum cannot be skipped no matter how high the threshold
  is. Recursive graph bisection remains the technique that attacks the real cause.
- **Two seeding designs were measured, not guessed.** Seeding from every query term measured
  **7.44 ms p99** - worse than no seeding - because it scored `terms x 64` candidates with a binary
  search each and deduplicated with a linear `contains` over a growing vector.


### Added - 2026-09-05 (the browser tier, and presyo at catalogue scale)

- **`js/browser.html` + `js/browser-check.mjs` - the engine running in a real browser.** Node proves
  it works outside Rust; a browser is a different claim: no `fs`, the index arrives over HTTP, the
  query runs on the main thread beside a render loop, and it is the only place
  `instantiateStreaming` and its hard `application/wasm` MIME requirement are exercised. It is also
  the tier profstopick actually ships to.

  The page drives the **raw C ABI**, not `js/index.mjs`, so a failure cannot be hidden by the Node
  wrapper. Measured in headless Chromium 147: 244,200-byte module, 380,564-byte index, 1,322
  documents, **52.7 ms compile+instantiate, 16.8 ms open+parse, and 37 microseconds per query on the
  main thread** with typo tolerance intact. Playwright is deliberately not a dependency - the check
  borrows it from a sibling checkout and exits 2 with instructions if it cannot, rather than
  pretending it ran.
- **presyo compared at 260,000 rows**, their production order of magnitude. The 1,940 real gold rows
  stay exactly as exported; the rest are distractors **recombined from real presyo tokens**, so
  every token is real and every row distinct. Whole-document replication was deliberately avoided -
  it produces near-identical copies, the corpus shape that most distorts top-k pruning, a mistake
  this project already made once and recorded in `bench/roadmap/p7-scale.md`.

  | 260,000 rows | recall@10 | hit@1 | mean latency |
  |---|---|---|---|
  | clean, presyo shipped SQL | 100.0 % | 99.4 % | 61.40 ms |
  | clean, index engine | 100.0 % | **100.0 %** | **0.34 ms** |
  | one typo, presyo shipped SQL | 99.8 % | 97.6 % | 54.01 ms |
  | one typo, index engine | **100.0 %** | **99.0 %** | **0.42 ms** |

  **Recall held at 134x the haystack and hit@1 went up**, with queries staying sub-millisecond. A
  distractor can only ever hurt the score - ground truth is still `gold_product_id`, so a recombined
  row can never be counted as a hit.

### Honest open-items - 2026-09-05

- **The browser tier has no persistence.** The index is re-fetched on every load; OPFS caching
  (ROADMAP P8) is unbuilt, and Safari evicts script-written storage after 7 days regardless, so any
  cache needs a rebuild path.
- **The 260 K presyo run is padded.** Only 1,940 rows are real exported production data; it does not
  populate their `search_text` column or their ~296 K aliases, and says nothing about their real
  catalogue's term distribution.
- **Nothing is deployed.** Three worktree branches, no PR against any app.


### Added - 2026-09-05 (presyo, head to head with the shipped implementation)

- **`scripts/compare_index_engine.{sh,ts}` in a presyo worktree** - a disposable `postgres:16-alpine`
  loaded with 1,940 real cross-store listings from their frozen production gold-cluster export
  (2026-06-13), with **their own `searchProduct` imported and called**: the real 7-lane `pg_trgm`
  `UNION ALL` with the inline `ts_rank` rescore, over the trigram indexes migrations 004 and 012
  create. Same rows, same queries, both answers printed.

  The task is presyo's hardest: cross-store product identity. The query listing is held out and not
  loaded, so a hit can only come from a different retailer's wording. Ground truth is
  `gold_product_id`.

  | | recall@10 | hit@1 | mean latency |
  |---|---|---|---|
  | clean, presyo shipped SQL | 100.0 % | 99.4 % | 46.72 ms |
  | clean, index engine | 100.0 % | **99.8 %** | 0.09 ms |
  | one typo, presyo shipped SQL | 99.8 % | 97.6 % | 43.39 ms |
  | one typo, index engine | **100.0 %** | **99.0 %** | 0.14 ms |

  **Read honestly: presyo's SQL is good.** `pg_trgm` handles a single mistyped character almost
  perfectly at this corpus size; the engine's margin is narrow (+0.2 points of typo recall, +1.4 of
  typo hit@1). The latency gap includes a loopback round trip to a container, so it demonstrates
  "in-process beats a database round trip", not a better query planner. And 1,940 rows is not their
  260 K-product catalogue.

### Honest open-items - 2026-09-05

- **sisia-app could not be reached at all.** Its catalog is a gitignored `sisia.db` that lives on the
  VPS, and its hybrid retrieval needs Postgres + pgvector + a Gemini key. A genuine gap, recorded
  rather than papered over with a proxy measurement.
- **presyo's input contract is deliberately left unsatisfied.** It pins
  `productSearchToken('Coca-Cola 1.5L') === ['coca','cola','1.5l']`; the analyzer produces `1500ml`
  instead, because canonicalizing the unit is what makes `1.5L`, `1500ml` and `1.5 liters` a single
  token - a property presyo itself measured as worth **+4.6 pp recall**. Satisfying it verbatim
  would be a regression, so it is stated rather than quietly bent.
- **Nothing is deployed.** Three worktree branches, no PR, no browser execution.


### Added - 2026-09-05 (onegrid's kernels, and a second app contract satisfied)

- **`index-accel` - onegrid's ratified `AccelModule` ABI, implemented.** `demand.md` Finding 4
  recorded that onegrid had ratified an acceleration ABI, written the JavaScript reference backend,
  written the property-based differential harness that proves an accelerated backend identical to
  it, budgeted the artifact - and shipped **no module**. `packages/wasm/crate/` did not exist and
  every test ran against `createFakeAccelModule()`. **The socket was cut and empty.**

  All seven kernels (`og_sort_pass`, `og_filter_mask`, `og_group_code`, `og_group_combine`,
  `og_aggregate`, `og_bitmap_op`, `og_top_k`) plus `og_abi_version` and `og_heap_base`, in
  **6,342 bytes** of `no_std`, zero-allocation WebAssembly. `no_std` is structural rather than
  stylistic: **the JavaScript host owns the heap**, bump-allocating above `og_heap_base()`, so a
  Rust allocator in the same linear memory would hand out addresses the host believes it owns.
  `top_k` marks consumed survivors with `i32::MIN` in the caller's scratch buffer for the same
  reason.

  **Result: onegrid's `packages/wasm` suite passes 294 / 294** driven by the real module, including
  a copy of their `differential.property.test.ts` whose only edit is the backend under test. Their
  generators are deliberately hostile - weighted toward NaN, `-0`, infinities, duplicates, ties and
  zero-length columns, because ties are where a stable sort and a group-by are actually interesting.
- **profstopick's full suite run against the search adapter: 1,922 / 1,958.** The 4 failures fail
  identically on their untouched `main` (verified by running them there), so the adapter breaks
  nothing.
- CI now builds both wasm artifacts.

### Changed - 2026-09-05

- `docs/integration.md` now covers both applications, with what each proves and what it does not.

### Honest open-items - 2026-09-05

- **There are deliberately no host unit tests for the accel kernels, and the reason is structural.**
  Every pointer in that ABI is a `u32`, because on wasm32 a pointer *is* an offset into linear
  memory; on a 64-bit host an address does not fit in 32 bits, so a test passing
  `vec.as_mut_ptr() as u32` silently truncates and the kernel reads garbage - which is exactly what
  the first version did, faulting with `STATUS_ACCESS_VIOLATION` rather than failing an assertion.
  Simulating linear memory with an arena would mean giving every kernel a base parameter the real
  ABI does not have, i.e. testing a different function from the one that ships. Correctness is
  proven where the module runs, by the consumer's own harness.
- **presyo and sisia-app still have data-level evidence only.** presyo's contract test and sisia's
  sparse arm both require a live Postgres this machine does not have; what has been measured is
  their exported corpora (`real-corpus`, 100 % cross-store recall@10, zero size violations).
- **Nothing is deployed.** Both integrations are throwaway branches. No browser has executed either.


### Added - 2026-09-05 (measured against an application's own contract)

- **`docs/integration.md`** - the engine run against **profstopick's `test/search-name-order.test.mjs`,
  unmodified, with only the import line redirected**, on a git worktree branch that leaves its `main`
  untouched. That test encodes a production measurement from 2026-08-17 (158 hits vs 109 misses, a
  40.8 % miss rate) and six explicit survival assertions. **Result: 9 of 9 pass**, including all
  three production failures. It passed 7 of 9 first; both failures produced engine fixes.
- **In-process index building in the WASM ABI** (`idx_build_new` / `idx_build_add` /
  `idx_build_finish` / `idx_build_free` / `idx_serialize`, ABI v2). Without it a host could only
  open a prebuilt blob, which means adopting the engine required a Rust build step - a far larger
  ask than `npm install`. A Node or browser host can now index its own data directly, and serialize
  the result to cache it.
- **Compound splitting.** A token absent from the dictionary that splits cleanly into two dictionary
  terms is replaced by those two, preferring the **rarest** pair (a split into two common terms is
  usually an accident: `therapist` -> `the` + `rapist`). Closes `math30.23` for profstopick and an
  entire category of presyo's frozen fixture - `cocacola`, `bearbrand`, `luckyme`, `pancitcanton`.
  Tried **only after exact and fuzzy have both failed**: attempting it earlier measured **7.12 ms
  p99 against 6.37 ms** at a million documents, because a typo'd token often happens to split into
  two real terms and taking that split discards the correction.
- **Separator normalization between digits.** `30.23`, `30,23` and `30-23` become one token, so a
  code matches however it is punctuated. The cost is stated in the code and accepted: a hyphenated
  numeric range folds to a decimal.

### Fixed - 2026-09-05

- **The typo length gate did not hold in prefix mode - a real bug.** A one-character token fell
  through to the distance-1 automaton, and every single letter is one substitution away from any
  other, so `"c"` matched essentially the entire dictionary. `"jacob c"` therefore matched
  `Jacob, Precious` by spending `jacob` twice, which is the exact failure profstopick's contract test
  names as "how a name matcher turns into a fuzzy one". Fixed with a distance-0 prefix automaton
  below the 1-typo threshold, plus a regression test asserting `"c"` reaches `cruz` and `colgate`
  but not `jacob` or `precious`.
- **Query planning expanded twice per unmatched token.** Probing "is this reachable by fuzzy?" and
  then expanding again paid for two automaton traversals on exactly the tokens most expensive to
  traverse. Restructured to expand once and reuse the result: **7.40 ms -> 6.40 ms p99** at 1 M.
- `is_none_or` (stable 1.82) used against a declared MSRV of 1.74.

### Honest open-items - 2026-09-05

- **Nothing is deployed.** The profstopick branch exists to measure, not to ship: no PR, the app's
  other ~150 tests were not run against the adapter, its build pipeline still emits the JSON shard,
  and no browser has executed it. presyo, sisia-app and onegrid have data-level evidence only.
- **1 M still misses the 5 ms p99 bar (6.40 ms), and the cause is now measured rather than guessed.**
  The tail is low-selectivity queries - the worst case has two terms, one carrying a 428,654-posting
  list, with nothing discriminative to prune against. Block maxima are uninformative because the
  score is uncorrelated with document id, which is precisely what docID reordering fixes. Three
  optimization attempts were made on hypotheses before this was measured; the diagnostic should have
  come first.


### Added - 2026-09-05 (the engine leaves Rust)

- **`index-wasm` - the binding that makes the engine usable outside Rust.** A `cdylib` with a
  **hand-written C ABI and no wasm-bindgen**, so one artifact serves the browser, Node, edge workers
  *and* native FFI for Go/Python/PHP/Ruby. Deliberately the ABI shape onegrid already ratified on
  this machine (versioned, JS owns the heap via explicit alloc/free), so `index` drops into a socket
  that already exists rather than inventing a second one. **173,700-byte `.wasm`.** Three tests
  exercise the ABI exactly as a host does, and assert that every failure mode - null handle, bad
  magic, truncation, non-UTF-8 query - returns a sentinel rather than trapping, because a trap in
  WASM kills the whole module instance.
- **`js/index.mjs`** - the JavaScript host. Re-derives its typed-array views after every call,
  because `memory.grow()` detaches them **even at `grow(0)`**.
- **`js/demo.mjs`** - end-to-end proof against real data: a 174 KB module plus a 380 KB index
  answering typo'd Filipino name queries at **~20 us per query from JavaScript**, with exact,
  surname-only, lowercase, one-deletion, transposition and typeahead cases all resolving to the
  right professor.
- **`js/smoke.mjs`** + CI steps - the WASM artifact is built and loaded on every CI run, so "works
  in any language" cannot silently stop being true.
- **`index-bench` bin `emit-artifact`** - builds a shippable `.idx` from profstopick's real
  registrar snapshot and prints an explicitly **not** like-for-like size comparison against the
  2,505,813-byte JSON shard that application ships today.

### Fixed - 2026-09-05

- **`u32` sentinels were unrecognisable in JavaScript.** WASM has no unsigned 32-bit return type, so
  `u32::MAX` arrives as `-1` and the error check `n === 0xffffffff` never fired. Caught by the CI
  smoke test on its first run. Every boundary now coerces with `>>> 0`.
- **Per-posting scoring recomputed a query-independent value.** `pseudo_tf` ran a loop over fields
  with a float division for **every posting visited**; the saturated contribution depends only on
  the document and its field lengths. Precomputed at build and load time: exact p50 at 1 M documents
  **1.02 ms -> 590 us**.

### Changed - 2026-09-05

- **`MAX_EXPANSION` 50 -> 16.** Expansions are already ordered by edit distance then document
  frequency ascending, so the cap keeps the closest and rarest - the most discriminative and the
  cheapest to traverse. Measured: recall unchanged at 100 % on both production corpora, typo p99 at
  1 M **7.88 ms -> 6.39 ms**.
- `BLOCK` stays at 128; 64 was measured and made no difference (6.39 vs 6.40 ms), so the extra
  metadata is not worth carrying.

Cumulative at 1 M documents across this session: **exact p50 4.66 ms -> 590 us; typo p99 15.9 ms ->
6.39 ms.**

### Honest open-items - 2026-09-05

- **1 M documents still misses the 5 ms p99 bar** (6.39 ms). Remaining levers named and unbuilt:
  PEF/Slicing postings compression, recursive graph bisection docID reordering, SIMD block decode.
- **The WASM artifact is proven in Node, not in a browser, and not inside any application's own
  codebase.** No PR has been opened against presyo, profstopick, sisia-app or onegrid. Demonstrated
  on their data and through their ABI is not the same as adopted.
- **No incremental updates.** The index is immutable; adding a document means a rebuild.


### Added - 2026-09-05 (persistence, scale, CI)

- **`index-text::format` - the portable on-disk index.** Range-readable by construction: a fixed
  section table at the head, **every offset a `u64`**, and a cumulative posting-offset array so a
  reader can fetch **one posting list** with a single ranged request. `docs/research/portability.md`
  is why: wasi-libc's `mmap` is a fake that silently reads the whole file into linear memory, so an
  mmap-first format closes WASM permanently. Six tests, including a demonstration that a single-term
  read touches a small fraction of the file, byte-for-byte deterministic serialization (so an index
  can be content-hashed and cached immutably), and truncation/corruption handling that errors rather
  than panics.
- **`index-bench` bin `scale`** - 5 K to 1 M documents over **61,467 real Philippine schools** (DepEd
  masterlist). Real corpus at 61,467: **194 us exact p50, 1.83 ms typo p99, 10.2 MB**. At 1 M:
  **1.14 ms exact p50, 1.26 ms typo p50, 9.15 ms typo p99** - p50 is milliseconds, **p99 misses the
  5 ms bar and that row is recorded as FAIL**. Spec: `bench/roadmap/p7-scale.md`.
- **CI, finally** (`.github/workflows/gate.yml`) - open since 2026-06-19. Runs tests, doc tests and
  clippy with `-D warnings` on Linux, so "green" stops meaning "green on one Windows box". The
  `bench/roadmap/` items stay excluded, and the two benches needing sibling checkouts are built but
  not run rather than faked.

### Fixed - 2026-09-05 (four retrieval defects, found by scaling)

- **An O(total postings) prologue on every query.** `plan()` recomputed each term's maximum-score
  bound by scanning all its postings - a bound that is a property of the index, not the query. Query
  latency tracked document count almost exactly. Precomputed at build and load time.
- **A linear scan for the heap minimum on every replacement** (~200 comparisons per accepted
  candidate). Replaced with a `BinaryHeap`.
- **Unbounded fuzzy expansion.** One token could expand to hundreds of dictionary terms, each adding
  a posting list to walk - tail latency, not median. Capped at 50, Elasticsearch's `max_expansions`,
  ordered by edit distance then document frequency so the cap drops the vaguest and least
  discriminative matches rather than an arbitrary slice.
- **An inverted tie-break in the candidate heap - a soundness bug.** The heap evicted the *lower*
  document id among equal scores while the final sort *preferred* it, so equal-scoring documents
  disappeared from results entirely. The block-skip logic was suspected first and was innocent;
  disabling it to isolate the cause is what found the real one.
- **Non-associative float accumulation made result order depend on the optimizer.** MaxScore
  accumulates a document's terms in a different order than exhaustive scoring, and `f32` addition is
  not associative, so genuinely-tied documents could be transposed. Accumulation moved to `f64` and
  a single ranking comparator with a relative tie tolerance now falls through to the deterministic
  document-id tiebreak.

Cumulative effect at 1 M documents: **exact p50 4.66 ms -> 1.14 ms; typo p99 15.9 ms -> 9.15 ms.**

### Changed - 2026-09-05

- **Block-max MaxScore.** Per-block last-doc and max-score metadata (`BLOCK = 128`) with a
  block-skipping `seek`, so retrieval can skip a whole range of documents when even the optimistic
  bound cannot beat the threshold.
- **Candidate pool `max(k*5, 100)` -> `max(k*3, 32)`.** The pruning threshold is the *pool's* worst
  score, so every extra slot weakens skipping. Measured: identical recall on both production corpora
  (`real-corpus` stays at 100 %) with materially lower tail latency.
- **`search_exhaustive` now mirrors the pool semantics**, so the oracle tests the claim it should -
  *pruning does not change the answer* - and not a different claim, *the pool is large enough*,
  which is a recall question answered by production data rather than by an assertion.

### Honest open-items - 2026-09-05

- **A benchmark methodology error, recorded rather than quietly fixed.** `scale` originally
  replicated whole documents, producing ~16 near-identical copies of every school - the corpus shape
  that most defeats top-k pruning. It measured 19.5 ms p99 at 1 M and sent three optimization
  attempts chasing a problem the benchmark had invented. **The corpus is part of the claim.**
- **1 M documents still misses the interactive p99 bar.** The remaining levers are named and
  unbuilt: PEF/Slicing postings compression (postings are currently 12 uncompressed bytes each),
  recursive graph bisection docID reordering, SIMD block decode.
- **Above 61,467 documents the corpus is recombined, not observed** - real tokens, synthetic
  combinations. It measures posting-list and top-k scaling, not vocabulary growth on new text.
- **The engine is still not wired into any application.** No napi-rs or WASM artifact exists.


### Added - 2026-09-05 (the engine, and its first real-data proof)

**`index-text` — the retrieval engine itself.** The spine `ROADMAP.md` Part II describes, built and
measured against production data rather than specced. `index-core` remains dependency-free; the
engine depends only on `tantivy-fst` and `levenshtein_automata`, the exact stack Meilisearch and
Tantivy ship (4.2 M and 4.4 M recent downloads, audited in `docs/research/build-or-buy.md`).

- `analyze` - the single Unicode fold, tokenization, **unit canonicalization as integer arithmetic**
  (`1.5L` = `1500ml` = `1.5 liters` = `1,5L`, exact, no floating point), split-quantity merging
  (`850 g` -> `850g`, which real retailer feeds require), and curated alias tables including a
  Philippine grocery + Filipino/English starter set.
- `dict` - FST term dictionary plus **the typo policy**, which is the part no crate provides:
  <= 2 edits hard-capped, length gates at 0-3/4-7/8+, first character protected, **numeric tokens
  exempt from fuzzy matching**, and lazy firing that short-circuits on an exact hit.
- `index` - **BM25F with exact `u16` field lengths** (Lucene and Tantivy quantize the fieldnorm into
  one byte, which destroys the length signal on short product titles) and per-field `k1`/`b`
  (Tantivy's are compile-time constants, issue #2924). Retrieval is **block-max MaxScore, not BMW** -
  BMW inverts on dense queries (SPLADE in PISA: BMW 681 ms vs MaxScore 220 ms).
- `fuse` - **RRF at k=60 as a built-in**, the constant three of this machine's codebases converged on
  independently, plus tunable convex combination.

**`index-bench` bin `real-corpus`** - the engine against **two corpora exported from production
databases**, with the consumer's own shipped matcher reproduced as the baseline:

- **profstopick, 1,322 real Ateneo professors.** Typo hit@10 **99.9 % vs the shipped matcher's
  14.9 %**; zero-result rate **0.1 % vs 85.1 %**; MRR@10 0.998 vs 0.149; exact hit@1 99.8 %.
  29,881-byte dictionary (6.55 B/term), 5 ms build, p50 42.6 us / p99 202 us.
- **presyo, 500 gold cross-store clusters / 1,940 real retailer listings.** Given one store's raw
  name, find the same product in a *different* store: clean recall@10 **100.0 %** (hit@1 99.8 %),
  typo recall@10 **100.0 %** (hit@1 99.0 %). **Zero size violations** across the 478 queries stating
  a mass or volume. 6,780-byte dictionary, 2 ms build, p50 33.3 us / p99 86.5 us.
- `INDEX_DIAG=1` asserts MaxScore's pruned results are identical to exhaustive scoring (100.0 % vs
  100.0 %) - the only thing that makes the pruning worth having.

Spec, thresholds and provenance: `bench/roadmap/p6-real-corpus.md`.

### Fixed - 2026-09-05 (three defects only real data could find)

- **Unmatched query terms were scored as if they matched perfectly.** `typo_bucket` mapped a query
  group the document matched *not at all* to `0` - identical to a perfect match - so documents that
  silently **ignored** a query word outranked documents that **found it with one typo**. Cost:
  presyo typo recall@10 of **28.8 % where it should have been 100 %**, and profstopick typo hit@10
  of 46.4 % instead of 99.9 %. Exhaustive scoring measured 28.6 %, proving it was ranking rather
  than pruning. Fixed by `MISSING_TERM_PENALTY = 3`, strictly above the maximum edit distance.
  **No unit test on a small corpus can find this** - on a small corpus every document matches every
  token - which is the argument for the real-corpus bench existing at all.
- **A stated size lost to word overlap.** `Purefoods Honeycured Bacon Roll Pack 500g` returned the
  **250 g** listing, because the 500 g listing omitted two adjectives and three word-misses
  outweighed one size-miss. A price-comparison bug presenting as a relevance bug. Fixed by
  `MISSING_QUANTITY_PENALTY = 16` - a stated size is not just another word - and proven safe when no
  listing carries the requested size, since the penalty then cancels across all candidates.
- **`1.5 L` with a space did not canonicalize.** Real retailer feeds write `850g` and `850 g` for the
  same product across stores; without merging, the size stopped being one comparable token and the
  numeric guard had nothing to guard.
- **The size guard reported three violations that were not violations** - a *benchmark* bug. `6S` in
  `MILKMAN YOGURT DRINK STRAWBERRY 6S 100ML` parses as 6 pieces; comparing a pack count against the
  query's `100 ml` produced spurious failures. A benchmark that reports the wrong defect is worse
  than no benchmark, so it is recorded rather than quietly patched.

### Honest open-items - 2026-09-05

- **The engine has not been run inside any consumer application.** It is measured *against* their
  data, not wired into their code. No napi-rs or WASM artifact exists (ROADMAP P8/P9).
- **Nothing is persisted.** The index is rebuilt in memory every run; the portable on-disk format
  (ROADMAP P7) is not written, so none of the browser/edge/four-transport story is real yet.
- **`real-corpus` depends on sibling checkouts**, so it stays gate-excluded until the corpora or a
  fixture subset are vendored here. A gate that silently skips is not a gate.
- **Both corpora are small** - 1,322 and 1,940 documents. The 100 % figures are real but are not
  evidence about 260 K products or 10 M rows.
- No CI, still. `cargo test --workspace` is 55 tests green on one Windows box.


### Changed - 2026-09-05 (demand-led re-baseline)

**The project's thesis changed.** A full research programme - eleven applications on this machine
surveyed by source inspection, six web-research lanes, one cross-model X/practitioner sweep, and a
crates.io maturity audit - established that **the learned-index core solves a problem none of the
consumer applications has**. Their hot paths are text -> ranked documents, predicate -> row set, and
name -> canonical entity; a faster `u64 -> position` map appears in none of them. The literature
agrees independently (MountDB, arXiv 2605.23815: PGM is used as an SST fence pointer, not a
retrieval algorithm).

Nothing was deleted. The existing work is **re-scoped from thesis to component** - fence pointers,
succinct structures, an adaptive filter column, and a p99 measurement harness better than the ones
in the literature. `ROADMAP.md` was rewritten around what the applications measurably need:
normalization, a typo-tolerant term dictionary, BM25F, and a portable index format.

- `ROADMAP.md` - rewritten. Old P3 (spatial/viewport) and P4 (federated multi-modal) **deferred**,
  not cancelled: no surveyed repo has a workload their current stack fails at. New spine P3-P13,
  ordered by consumer pain closed per week of work. Adds a Positioning section.
- `README.md` - Status section now leads with the reframe rather than the learned-index claim.
- Governing rule adopted: **no roadmap row without a named consumer and a measurement that consumer
  already takes.**

### Added - 2026-09-05

- `docs/research/` - the evidence base, eight files, every performance claim carrying a source URL
  and a date, with UNVERIFIED marked explicitly:
  - `demand.md` - the eleven-app survey. What each corpus is, how it searches today, what it
    measured about its own pain.
  - `relevance.md` - ranking and fuzzy-matching SOTA; the BM25F/fieldnorm argument that justifies
    writing our own scorer; the production typo-tolerance consensus.
  - `speed.md` - dynamic pruning, posting-list compression, and a ranked list of the ten
    highest-leverage techniques. Includes corrections to its own first draft.
  - `landscape.md` - the embedded/drop-in engine field, the sync problem, why Postgres FTS fails
    structurally, and five unfilled market gaps.
  - `portability.md` - WASM, browser storage, native bindings, edge runtimes, in-database
    extensions, and a ranked ten-item distribution strategy.
  - `business.md` - who makes money selling search, verified pricing, and five ranked positions.
  - `claim.md` - what practitioners say publicly, with a credibility verdict on each.
  - `build-or-buy.md` - crates.io maturity audit; the rule and the verdicts.
- `bench/roadmap/p5-fuzzy-term-feasibility.md` + `index-bench` bin **`fuzzy-term`** - a measured
  feasibility result for the typo-tolerant term dictionary, built on `tantivy-fst` +
  `levenshtein_automata`. At profstopick's corpus size (11,949 labels / 12,029 distinct tokens):
  **96,547-byte term dictionary (8.03 B/key), exact p99 760 ns, fuzzy p99 663 microseconds, and
  typo recall 135 -> 1,999 of 2,000 (14.8x).** Holds to 861 K distinct terms. Verdict: **FEASIBLE.**
- `index-bench` now depends on `tantivy-fst` and `levenshtein_automata` for that spike only.
  **`index-core` remains dependency-free.**

### Fixed - 2026-09-05

- **Two open Triage rows closed with data instead of opinion.** `pgm-extra` has **134 downloads in
  90 days** (498 all-time) and `pgm_index` has 80 - there is no production usage to inherit, so the
  build-vs-buy answer is *neither*. And the `seismic` crate that the relevance research recommended
  for learned-sparse retrieval has **16 downloads in 90 days and no publish since 2025-03-05**; the
  algorithm is real, the dependency is not.
- **A rejected-ideas entry was producing a wrong conclusion.** Rejecting *building* an FST (June
  2026) was correct; concluding that *typo tolerance* was therefore out of scope did not follow, and
  it cost the project its largest available win. `docs/roadmap-rejected.md` now carries a standing
  question for every rejection: does this scope out the PROBLEM, or only one SOLUTION to it?
- **The first version of the `fuzzy-term` bench measured a 115-token vocabulary** and reported a
  flattering 0.03 bytes/key. Caught only because the number was implausibly good; the corpus now
  carries a realistic long tail. A benchmark's corpus is part of its claim.
- **One hypothesis refuted and recorded rather than quietly dropped:** prefix-anchoring is *not* a
  speed lever for fuzzy lookup (-22.7% to +9.6% effect on p50, no consistent sign). First-character
  protection is a precision and typeahead rule, not a performance one, and the roadmap must not
  claim otherwise.

### Honest open-items carried forward

- **`fuzzy-term` exits non-zero on purpose** - two documented reds: exact p99 1,380 ns against a
  1,000 ns bar at 848 K terms, and 8.03 B/key against an 8.0 bar on the 12 K name corpus. Neither is
  to be silently relaxed.
- **The `fuzzy-term` corpus is synthetic.** profstopick's real shard is not committed; generating it
  needs a database. The headline number is representative, not actual.
- **Still no CI** (open since 2026-06-19), so "green gate" means green on one Windows box.
- **No customer has asked for this.** Positioning is derived from other people's published pain.
  Three of the eleven surveyed repos declined the product outright, in writing.
- Real SOSD 200M datasets still not downloaded, blocking the P13 research row.
- Minor test gaps carried over: no `FmIndex::locate` test for an absent pattern; no `PgmIndex` =
  `PlaIndex` result-equivalence test.


### Added — 2026-06-19 (initial build: P0, P1, P2)

P0 — learned ordered index
- `index-core`: `PlaIndex` — piecewise-linear learned index over sorted `u64` keys with bounded
  `±ε` prediction and bounded last-mile search. Default build uses the **optimal convex-hull PLA**
  (O'Rourke / PGM) with exact `i128` geometry; `build_greedy` kept as the comparison baseline.
- `PgmIndex` — recursive multi-level variant (PLA over segment-start keys).
- `index-core::data` — deterministic, dependency-free generators (`sequential`, `uniform`,
  `lognormal`, PLA-hostile `hard`, byte-`gen_text`) + SOSD binary loader (`load_sosd_u64`).
- `index-bench` bin `beat-btreemap` — bytes/key + p50/**p99** vs `std::BTreeMap`, `rdtsc` clock,
  median-of-5. Result: PLA wins space/p50/p99 on all four distributions at n=1M and n=10M (ε=16).

P1 — adaptive database cracking
- `CrackerColumn` — naive + stochastic cracking; `query(lo,hi)` partitions toward the workload.
- `index-bench` bin `crack-converge` — convergence under random/sequential/ends workloads.
  Result: naive **fails** sequential (cum/scan 2.48×), stochastic **passes** (0.01×); random
  converges 599×.

P2 — compressed full-text + fuzzy
- `FmIndex` — BWT + backward search: `count`, `locate` (sampled SA), and `fuzzy_kmismatch`.
- `wavelet` — `BitRank` (popcount-prefix bit-rank) + balanced `WaveletTree` (rank/access in
  O(log σ)), making the FM-index succinct: ~5.9 / 9.7 / 14.6 bits/char at σ=4/26/256.
- `index-bench` bin `fuzzy-decision` — fuzzy-over-FM viability vs k and σ. Decided: viable to
  k≤2 at σ=26, only k≤1 at σ=256 (exponential-in-k blowup confirmed).

Tooling / docs
- Rust GNU toolchain adopted (no MSVC linker on the build box); documented in README.
- `ROADMAP.md`, `docs/roadmap-rejected.md`, `bench/README.md`, and five `bench/roadmap/*` specs.

### Changed — 2026-06-19
- FM-index rank backend swapped from a 256-wide checkpoint Occ table (168 bits/char, *larger than
  the text*) to a wavelet tree (≈ entropy + overhead).
- `beat-btreemap` default ε set to 16 after a sweep showed ε=64 loses p99 on irregular data at 10M.
- Timer upgraded from `std::Instant` (~100 ns granularity) to `rdtsc` with overhead calibration.

### Fixed — 2026-06-19
- Data generators (`gen_uniform`, `gen_lognormal`) had an O(n²) generate-sort-dedup retry loop that
  hung at n=10M on skewed data; replaced with O(n log n) increment/bump generation.
- `WaveletTree::rank` returned garbage (and caused OOB in fuzzy search) for symbols outside the
  alphabet; added an out-of-range guard.

### Honest open-items (not done this batch)
- **No CI** — there is no `.github/workflows`; tests/benches run only locally. A CI gate (with the
  `bench/roadmap/` exclusion intact) is unscheduled.
- **q-gram filter+verify fuzzy fallback (P2)** — specced, not implemented; needed for k≥2 at large σ.
- **Real SOSD 200M datasets** not bundled; benches run on synthetic stand-ins (loader works on real
  files via `beat-btreemap <file>`).
- **FM-index rank `cum` overhead** — u32-per-word (~50%); a two-level rank would trim it.
- **Minor test gaps**: no `FmIndex::locate` test for an absent pattern; no `PgmIndex` ≡ `PlaIndex`
  result-equivalence test (PGM is covered for correctness, not cross-checked against PLA).
- **P3 (spatial/viewport) and P4 (federated)** not started.
