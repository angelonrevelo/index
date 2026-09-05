# Relevance — ranking, fuzzy matching, and entity resolution SOTA

> Web research sweep, 2026-09-05. Every number carries a source. `UNVERIFIED` means no primary
> source could be reached. Read alongside [`demand.md`](demand.md) — this file is the *supply*
> side (what is known to work); that file is the *demand* side (what the apps need).

---

## 1. Lexical ranking — and why we cannot just use Tantivy's BM25

**Tantivy hardcodes `K1 = 1.2; B = 0.75`** as module constants in `src/query/bm25.rs` with no
public API to change them; tuning `b` requires a fork or a custom scorer
([source](https://github.com/quickwit-oss/tantivy/blob/main/src/query/bm25.rs), read 2026-09-05;
[issue #2924](https://github.com/quickwit-oss/tantivy/issues/2924), open).

**Why the defaults fail on short product titles.** On a title, term frequency is almost always 1,
so `k1` is inert; all the signal lives in `b`'s `dl/avgdl` term, which then rewards brevity rather
than relevance. Worse, Lucene *and* Tantivy quantize the field norm into **a single byte**
(`SmallFloat.floatToByte315`, 256-entry decode table) — at 3–5 tokens, distinct lengths collapse to
the same norm, destroying the signal before `b` can matter
([Lucene 9.9.1 BM25Similarity](https://lucene.apache.org/core/9_9_1/core/org/apache/lucene/search/similarities/BM25Similarity.html);
Anserini exact-length variant, [arXiv 2509.02558](https://arxiv.org/pdf/2509.02558), 2025-09).

Elastic's own guidance gives literature-typical ranges **b ≈ 0.3–0.9, k1 ≈ 0.5–2.0** and says fix
query construction before touching them
([Practical BM25 pt.3](https://www.elastic.co/blog/practical-bm25-part-3-considerations-for-picking-b-and-k1-in-elasticsearch),
2018-04-19). The widely repeated "b = 0.3–0.5 for titles" has **no primary study behind it —
UNVERIFIED**. Also note `avgdl` is per-shard, not per-index, in Elasticsearch.

**Multi-field: BM25F, never summed per-field scores.** `most_fields` fails twice — IDF is computed
*per field*, so a term rare in `title` but common in `description` gets absurd weight; and TF
traverses two saturation curves independently
([softwaredoug, 2025-09-18](https://softwaredoug.com/blog/2025/09/18/bm25f-from-scratch)).
Elastic's docs warn `cross_fields` "score combination can even be incorrect" and recommend
**`combined_fields`**, which implements BM25F with blended IDF and field boosts acting as
term-frequency multipliers (boost 2 = "as if each term appeared twice")
([docs](https://www.elastic.co/docs/reference/query-languages/query-dsl/query-dsl-combined-fields-query)).

> **Consequence for `index`:** you cannot have both per-field `b` and BM25F. Pick BM25F, store
> **exact field lengths as `u16`** rather than a quantized byte, and expose `k1`/`b`. This is a
> concrete, defensible reason to write our own scorer rather than depend on Tantivy's — and it is
> the *first* such reason found in three research sweeps.

## 2. Learned sparse — real gains, wrong shape for v1

BEIR avg nDCG@10 / MS MARCO MRR@10 / params
([sbert sparse models](https://sbert.net/docs/sparse_encoder/pretrained_models.html), read 2026-09-05):

| Model | BEIR | MRR@10 | Params |
|---|---|---|---|
| splade-v3 | 51.7 | 40.2 | 109 M |
| opensearch-neural-sparse-v2-distill | 52.8 | — | 67 M |
| granite-embedding-30m-sparse | 50.8 | — | 30 M |
| **opensearch-doc-v3-gte** | **54.6** | — | 137 M, **inference-free query side** |

Index blowup ≈ **4.7×** (BM25-T5 1.2 GB vs SPLADE++ 5.6 GB on MS MARCO passage — secondary source,
[zeroentropy](https://www.zeroentropy.dev/concepts/sparse-retrieval/), undated).
**DF-FLOPS** (SIGIR'25, [arXiv 2505.15070](https://arxiv.org/abs/2505.15070)) penalizes high-DF
terms: **~10× faster retrieval, latency on par with BM25**, −2.2 MRR@10, better on 12/13 BEIR tasks.

**Seismic is the Rust story**: blocked inverted lists + geometric clustering, `cargo add seismic`,
**185 µs/query at MRR@10 40.27** on splade-v3 MS MARCO (vs 745 µs baseline), 7.9 GB
([repo](https://github.com/TusKANNy/seismic); SIGIR'24 [arXiv 2404.18812](https://arxiv.org/abs/2404.18812)).

On **Amazon ESCI**, fine-tuned SPLADE scores **0.389 nDCG@10 vs BM25 0.305 (+27.5 %)**; on WANDS
+7.9 % — but the same tuning *lost* 17.9 % on general web
([Qdrant, 2026-03-09](https://qdrant.tech/articles/sparse-embeddings-ecommerce-part-4/)).

Notable: **ELSER is stagnant at v2** — no v3, and Elastic's `semantic_text` default moved in 9.4+
to a *dense* model. uniCOIL / DeepImpact / TILDE are legacy.

**Verdict: not in v1.** It needs a ~100 M-param query encoder in-process plus ~5× index. Revisit
only via an **inference-free-query variant** (opensearch-doc-v3, 54.6 BEIR, zero query-side model)
— that specific combination is the one thing that could displace the lexical arm.

## 3. Dense + hybrid — static embeddings are the only CPU-viable option

**Best sub-500M dense:** EmbeddingGemma-300m — **69.67 MTEB(eng,v2)**, 61.15 multilingual, MRL
768→128, **< 200 MB quantized** ([arXiv 2509.20354](https://arxiv.org/pdf/2509.20354);
[Google, 2025-09-04](https://developers.googleblog.com/en/introducing-embeddinggemma/)).

**But the CPU answer is static embeddings.** Model2Vec: **~500× faster, ~50× smaller** than the
teacher; ~50 sent/s → **~25,000 sent/s on one core** ([MinishLab](https://github.com/MinishLab/model2vec)).

| Model | Score | vs baseline |
|---|---|---|
| all-MiniLM-L6-v2 | 56.09 MTEB | baseline |
| potion-base-32M | 52.13 MTEB | 93 % |
| potion-retrieval-32M | **35.06** | **81.7 %** of MiniLM's 42.92 |
| static-retrieval-mrl-en-v1 | NanoBEIR 0.5032 | 87.4 % of all-mpnet, **397× faster on CPU** |
| potion-multilingual-128M | 47.31 MMTEB | 90.9 % of LaBSE, **101 languages incl. Filipino** |

**An official Rust crate exists**: [model2vec-rs](https://github.com/MinishLab/model2vec-rs),
**8,000 vs 4,650 samples/s** (1.7× over Python). Query encoding is a table lookup + mean + L2 —
**microseconds, not milliseconds**. This is the only dense arm that fits a 10 ms embedded budget.

**Fusion — the number that matters.** Elastic BEIR avg nDCG@10
([2023-07-20](https://www.elastic.co/search-labs/blog/improving-information-retrieval-elastic-stack-hybrid)):

| Method | BEIR nDCG@10 |
|---|---|
| BM25 | 0.425 |
| ELSER | 0.543 |
| **RRF (k=20)** | **0.551** |
| **Tuned linear combination** | **0.576** |

Bruch et al. ([arXiv 2210.11934](https://arxiv.org/abs/2210.11934), TOIS) find **convex combination
beats RRF in- and out-of-domain**, is normalization-agnostic, and tunes α from a *small* labeled
set. RRF original: Cormack SIGIR 2009, **k=60**
([PDF](https://cormack.uwaterloo.ca/cormacksigir09-rrf.pdf)). OpenSearch L2 + arithmetic-mean
normalization: **nDCG +7.4 % / +10.3 %** ([blog](https://opensearch.org/blog/hybrid-search-optimization/)).

> **Reconciles with [`demand.md`](demand.md) Finding 1:** the house converged on RRF k=60 three
> times independently, and the literature says RRF is the right *zero-config default* but leaves
> ~0.025 nDCG on the table versus a tuned convex combination. Ship RRF k=60 as the default; expose
> α; switch once ~40 judgments exist.

**On code-mixed queries** — the finding that matters most for Taglish — BM25 + rerank scored
**0.30 vs multilingual dense 0.16**, despite dense winning clean English 0.61.
**UNVERIFIED** (secondary aggregator), but directionally the strongest available argument for
lexical-first on Taglish.

## 4. Reranking — the latency wall, and the one real CPU measurement

Cross-encoder family, nDCG@10 TREC-DL19 / docs-per-sec on V100
([model card](https://huggingface.co/cross-encoder/ms-marco-MiniLM-L6-v2)):

| Model | nDCG@10 | docs/s |
|---|---|---|
| TinyBERT-L2 | 69.84 | 9000 |
| **MiniLM-L6** | **74.30** | **1800** |
| MiniLM-L12 | 74.31 | 960 |

**L12 buys +0.01 nDCG for 2× the cost — never use it.**

The one real CPU measurement found: **Ettin-reranker-17M does 267.4 pairs/sec on an Intel
i7-13700K** (vs 7,517/s on an H100), while beating ms-marco-MiniLM-L12 by **+0.051 nDCG@10
(0.5576 vs 0.5066)** ([HF blog, 2026-05-19](https://huggingface.co/blog/ettin-reranker)).

> **267 pairs/s = 3.7 ms/pair on passages. Reranking even 10 documents blows a 10 ms budget.**
> Product titles are ~15 tokens vs ~256, so expect ~10× faster, putting k=30 near ~10 ms — but
> **that is extrapolation, UNVERIFIED. It must be measured before anything depends on it.**

Late interaction on CPU is more promising than expected:
**mxbai-edge-colbert-v0-17m** BEIR **0.490** (beats ColBERTv2's 0.488 at 1/8 the params), the 32m
variant 0.521 ([mixedbread, 2025-10-16](https://www.mixedbread.com/blog/edge-v0));
answerai-colbert-small-v1 (33 M) 0.534, "hundreds of thousands of docs in milliseconds on CPU"
([Answer.AI, 2024-08-13](https://www.answer.ai/posts/2024-08-13-small-but-mighty-colbert.html)).

Big rerankers are GPU-only: mxbai-rerank-base-v2 (0.5 B) **0.67 s/query on an A100**.
jina-reranker-v3.5 (0.6 B) leads BEIR at **63.20** but is non-commercial.
FlashRank ships **ce-esci-MiniLM-L12-v2** (ESCI-tuned e-commerce CE) and TinyBERT-L2 at **4 MB**
ONNX ([repo](https://github.com/PrithivirajDamodaran/FlashRank)) but **publishes no latency table
— UNVERIFIED**. **No published CPU-INT8 reranker benchmark exists anywhere.** That is a genuine
measurement gap and a cheap, citable contribution if we fill it.

## 5. E-commerce specifics — the transferable finding

**Instacart's Intent Engine is the single most transferable result for presyo.** They replaced an
ML query-understanding stack with LLM rewrites: legacy click-mined rewrites covered **~50 % of
traffic**; LLM rewrites reached **95 %+ coverage at 90 %+ precision**, with **−6 % scroll depth on
tail queries and −50 % complaints**, at **sub-300 ms — achieved by precomputing offline and
caching, not by calling an LLM in the hot path**
([tech.instacart.com, 2025-11](https://tech.instacart.com/building-the-intent-engine-how-instacart-is-revamping-query-understanding-with-llms-3ac8051ae7ac)).

**Business signals: bound them.** Elastic's `rank_feature` offers `saturation(S) = S/(S+pivot)` as
the default precisely because unnormalized BM25 cannot safely absorb raw popularity.
**Algolia never blends at all** — eight strictly ordered tie-break criteria (Typo → Geo → Words →
Filters → Proximity → Attribute → Exact → **Custom**), so business signals only break ties; the
engine is C++ as an NGINX module at **1–20 ms server-side**, with the work pushed to index time
([support](https://support.algolia.com/hc/en-us/articles/4406975267089-How-fast-is-Algolia)).
Shopify makes **availability a hard gate, not a signal**. Vespa publishes the cleanest phased cost
model: first-phase on all matches → second-phase top-N per node → global-phase on the merged set.

**LTR: trees still win.** Elastic's native LTR (8.12.0) is **XGBoost LambdaMART** as a second-stage
reranker; Shopify's 2025 stack names **CatBoost/LightGBM alongside neural rankers**, with 80+
text-similarity features and a **48 % speedup** from their TurboDSL engine
([shopify.engineering, 2025-11-12](https://shopify.engineering/world-class-product-search)).

**Counterfactual-LTR reality check:** "Unbiased Learning to Rank Meets Reality" (SIGIR'24
reproducibility, Baidu-ULTR) found ULTR methods **robustly improve click prediction but do not
consistently improve ranking**, and results are dominated by **choice of ranking loss**, not the
debiasing method ([arXiv 2404.02543](https://arxiv.org/abs/2404.02543)).
**Do not build a propensity model before there is a working loss.**

## 6. Evaluation — and the circularity trap

BM25 BEIR baseline ≈ **0.425** (Elastic's measurement above; Thakur 2021's 42–43 figure could not
be re-verified — UNVERIFIED). Reranked pipelines now reach 63.20.

**UMBRELA** (GPT-4o) is the TREC 2024/25 RAG assessor
([arXiv 2406.06519](https://arxiv.org/abs/2406.06519)); system rankings correlate with humans at
**Kendall τ = 0.84** — but **τ collapses to 0.63 (top-60) and 0.44 (top-20) when the same LLM both
reranks and judges.** E-commerce specifically: ChatGPT relevance judgments hit **~82 % agreement
with human annotators** (ECIR 2024). Counterweights: LLM judges **systematically over-rate** and
are sensitive to passage length and lexical surface (SIGIR 2026,
[arXiv 2602.17170](https://arxiv.org/abs/2602.17170)).

> **Rule: bootstrap judgment lists with LLMs, validate on a human-labelled slice, and never let the
> reranker judge itself.**

Datasets: **Amazon ESCI** — 130,652 queries / 2,621,738 judgments, E/S/C/I labels, en/es/ja
([repo](https://github.com/amazon-science/esci-data)). **WANDS** — 42,994 products, 480 queries,
233,448 judgments (Wayfair). **Airbnb: interleaving + counterfactual eval raised experiment
sensitivity up to 100× vs A/B** ([arXiv 2508.00751](https://arxiv.org/abs/2508.00751)) — the best
argument for interleaving in a low-traffic app, which describes every app in `demand.md`.

**Filipino/Taglish:** **Batayan** is the reference benchmark — 8 tasks over Tagalog *and*
code-switched Taglish, ACL 2025 ([arXiv 2502.14911](https://arxiv.org/abs/2502.14911)).
**No Filipino-specific retrieval benchmark exists.** That is a gap presyo's 60-query fixture and
profstopick's name corpus could credibly fill.

---

## 7. Typo tolerance — the production consensus is unanimous

**SymSpell** (symmetric delete): **1,870× faster than a BK-tree**, **1,000,000× faster than
Norvig's corrector**; lookup **0.033 ms/word at ED=2, 0.180 ms at ED=3**
([repo](https://github.com/wolfgarbe/SymSpell)). The honest metric is candidate-set size: BK-tree
computes Levenshtein on **17–61 % of the vocabulary**, SymSpell on **0.0042–0.016 %**
([seekstorm, 2017-07-24](https://seekstorm.com/blog/symspell-vs-bk-tree/)). Cost is memory — the
delete index blows up superlinearly with ED, which is why ED=2 is the practical ceiling.

**Levenshtein automata over FST** (Schulz–Mihov). BurntSushi's `fst` crate
([burntsushi.net/transducers/](https://burntsushi.net/transducers/), 2015-11-11):

| Corpus | Keys | FST size | Bytes/key | Build | **Peak RAM** |
|---|---|---|---|---|---|
| Dictionary | 119,095 | 324 KB | **2.7** | 0.12 s | 9.4 MB |
| Gutenberg | 3.54 M | 22 MB | 6.2 | 2.04 s | 21.8 MB |
| Wikipedia titles | 15.8 M | 157 MB | 9.9 | 18.3 s | 34.1 MB |
| Common Crawl URL | **1.649 B** | 27 GB | 16.4 | 82 min | **56 MB** |

ED-2 automaton intersection over the 15.8 M-key / 157 MB index: **0.094 s** unanchored. **Peak RAM
is flat because the FST is mmap'd** — the single most important property for an embedded engine.

**The production engines have all converged on the same shape:**

| Engine | 1-typo gate | 2-typo gate | Cap | Notes |
|---|---|---|---|---|
| **Typesense** | `min_len_1typo=4` | `min_len_2typo=7` | `num_typos=2` | `typo_tokens_threshold=1` — expansion is **lazy**, only when exact underdelivers |
| **Meilisearch** | 5–8 chars | 9+ chars | 2 | first-char typo counts **double**; FST + automaton in a **single streaming pass**; typo is a bucket-sort rule |
| **Algolia** | `minWordSizefor1Typo=4` | `minWordSizefor2Typos=8` | 3 if first letter | `typoTolerance:min/strict` forces Typo first in the ranking formula |
| **Lucene/ES** | `AUTO:3,6` → 3–5 chars | > 5 chars | 2 | `max_expansions=50`, `prefix_length=0`; docs warn about pathological expansion |

**≤ 2 edits hard-capped · length gates at ~4 and ~8 · first character protected · typo count is a
ranking bucket, not a filter · fired lazily.** Copy this exactly.

**The non-negotiable guard rail:** set the equivalent of Algolia's `allowTyposOnNumericTokens` to
**false**. `300g` fuzzy-matching `800g` is a correctness bug in a price-comparison app that will
present as a relevance bug.

**Phonetics still earn a place — for brand names only.** Filipino orthography is phonemic, so users
spell English brands by ear: `kolgate`, `nesle`, `kokakola`. These are phonetic substitutions, not
typing slips, and edit distance ≥ 2 rejects them. Apply Double Metaphone to the **brand dictionary
only** (a few thousand tokens, < 50 KB), never to the full catalogue.

**Tagalog is a resource desert, and that is liberating.** The best anchor is **calamanCy**
(EMNLP NLP-OSS 2023, [arXiv 2311.07171](https://arxiv.org/abs/2311.07171)) — per-task F1 numbers
UNVERIFIED (only the abstract was retrievable). Tagalog morphology is hostile to naive stemming:
infixation (`sulat`→`sumulat`, inserted *inside* the root), circumfixion (`ka-…-an`), CV-
reduplication (`bili`→`bibili`). Edit distance handles none of these. **There is no
production-grade Filipino stemmer.** Treat it as a synonym-table problem — a few hundred curated
`gatas↔milk`, `sabon↔soap`, `mantika↔oil` rows will beat anything statistical trainable on the
available corpora. presyo already has the seed: an LLM-generated `search_terms_fil` column and a
fixed 73-value Filipino taxonomy.

**Vector-based fuzzy loses.** Dense retrievers suffer "a significant drop in retrieval and ranking
effectiveness" under typos (Zhuang & Zuccon, EMNLP 2021,
[aclanthology 2021.emnlp-main.225](https://aclanthology.org/2021.emnlp-main.225/)); an entire line
of follow-ups exists purely to patch it (typos-aware bottlenecked pre-training
[2304.08138](https://arxiv.org/abs/2304.08138), typo-robust representation learning
[2306.10348](https://arxiv.org/abs/2306.10348)). Exact MRR@10 deltas UNVERIFIED, but four
independent papers agree qualitatively. **Use edit distance for typos; reserve embeddings for
semantic recall (`panlaba` → detergent).**

## 8. Entity resolution — the unseen-entity cliff decides the architecture

**WDC Products** (11,715 offers / 2,162 products; every test set 4,500 pairs), pair-wise F1
([arXiv 2301.09521](https://arxiv.org/html/2301.09521)):

| Method | Seen | Half-seen | **Unseen** |
|---|---|---|---|
| Word co-occurrence | 58.07 | 46.04 | **29.70** |
| Magellan | 35.83 | 37.45 | 36.61 |
| RoBERTa | 78.58 | 75.91 | **71.14** |
| Ditto | 79.16 | 75.22 | 70.24 |
| R-SupCon | **81.88** | 68.69 | **57.23** |

**LLMs dominate the unseen case.** Zero-shot GPT-4 on WDC Products: **89.61 F1** vs fine-tuned
Ditto 84.90 and RoBERTa 77.53. Under *cross-dataset transfer* **Ditto collapses to 48.74 and
RoBERTa to 55.52 while GPT-4 holds 89.61** ([arXiv 2310.11244v4](https://arxiv.org/html/2310.11244v4),
2024-10-18). Cost from the same paper: zero-shot GPT-4o-mini **0.006 ¢/prompt**; latency
0.46–1.54 s.

> **At 0.006 ¢/pair, adjudicating 100,000 candidate pairs costs about $6.** For a grocery catalogue
> this is a solved economic problem. **Do not train a matcher.** The blocking stage is the
> engineering work.

**Blocking: SC-Block** (supervised contrastive) — **99.5 % recall @ k=5 on Abt-Buy vs BM25's
92.2 %**; on WDC-Blarge (200 B pairs) 56.7 % vs BM25 41.7 %; candidate sets ~half the size;
end-to-end **1.5–2× faster (4× on WDC-Blarge: 30 h → 8 h)**
([arXiv 2303.03132](https://arxiv.org/html/2303.03132)). Note DeepBlocker is *slower than
rule-based blockers even with 4 GPUs*, and an SBERT baseline overfits badly (74.3 % → 24.4 %).

**GTIN/EAN is hidden, missing, or wrong on roughly a third of scraped retailer pages**
([groupbwt](https://groupbwt.com/blog/product-matching/)). presyo measures its own version of this:
**barcode is populated on 4.2 % of products.** Treat GTIN as a high-precision shortcut covering a
minority of rows.

> **This validates presyo's existing design and explains its honest correction.** Its
> Fellegi–Sunter matcher measured in-sample 100 % precision but **real out-of-sample ≈ 82 %** — the
> exact seen→unseen cliff the WDC table predicts. A new SKU arrives weekly, so a Philippine grocery
> catalogue is *permanently* in the unseen regime. Realistic ceiling: **~85–90 % F1 with LLM
> adjudication, ~70 % with a fine-tuned small model.**

---

## Recommended pipeline — CPU-only, embedded, < 10 ms @ 1 M docs

Budget: **1 ms query understanding · 3 ms retrieval · 1.5 ms fusion + features · 3 ms optional
rerank · 1.5 ms slack.**

**Stage 0 — index time (unbounded; do everything here).** Algolia's principle. Normalize units
(`1.5L` → a synthetic `1500_ml` token), extract brand/size/pack into typed fields, canonicalize
Taglish variants through an alias table, precompute popularity/availability/price-percentile as
quantized `u8` features, build the term FST. **Cluster products into match-groups offline** —
cross-retailer matching must never happen at query time.

**Stage 1 — query understanding (~1 ms).** Pure lookup: alias table → normalized tokens; FST for
size + unit; brand gazetteer. **No model in the hot path.** Precompute LLM rewrites offline for the
head and torso of the query log (Instacart: 95 % coverage, cached). Spell correction via FST +
Levenshtein automaton, length-gated.

**Stage 2 — retrieval (~3 ms), two arms in parallel.**
- **Lexical BM25F** over `{brand:3, title:2, category:1, description:1}`. The primary arm —
  code-mixed and SKU-like queries are where lexical wins. **Implement the scorer ourselves**
  (§1: Tantivy's constants are compile-time and its 1-byte fieldnorm destroys length signal on
  3–5-token titles). Store exact field lengths as `u16`.
- **Static-embedding dense** via `model2vec-rs` (`potion-multilingual-128M`, 256-d, int8). 1 M
  vectors ≈ 256 MB, ~1 ms. Query encoding is microseconds.

Take top-200 from each arm.

**Stage 3 — fusion (~0.5 ms).** **RRF k=60 as the zero-config default**; expose a convex-combination
α and switch once ~40 judgments exist (Elastic: 0.576 vs 0.551).

**Stage 4 — business features (~1 ms).** Blend, but **bound every signal** with
`saturation(S) = S/(S+pivot)`. Availability is a **hard filter**. Cut to top-50.

**Stage 5 — rerank (~3 ms, optional, flagged).** The biggest quality jump available (0.425 → 0.55+)
and the thing most likely to break the SLA. Ship INT8-ONNX **Ettin-17M** or the ESCI-tuned
`ce-esci-MiniLM-L12-v2` over **top-20 titles only**, behind a feature flag with a latency
circuit-breaker that falls back to Stage-4 output. If measurement says no, substitute a
**LambdaMART/LightGBM model over ~30 cheap features** — trees are what Elastic and Shopify actually
deploy and they cost microseconds.

### Tradeoffs, stated plainly

- **Static embeddings cost ~19 % of retrieval quality** (35.06 vs 42.92) to gain ~500× CPU
  throughput. At a 10 ms budget with no GPU there is no alternative — a transformer bi-encoder at
  ~50 sent/s is 20 ms for the *query alone*.
- **No learned sparse in v1** (§2).
- **Cross-encoder reranking is the crux** — design it optional from day one.
- **Cross-retailer matching is offline, always.**
- **Skip counterfactual LTR** until there is real traffic and a working loss.
