//! The index: postings, BM25F scoring, and MaxScore top-k retrieval.
//!
//! # Why the scorer is written here rather than depended on
//!
//! Everything else in this engine is bought (`docs/research/build-or-buy.md`). The scorer is the
//! one justified exception, and the reason is narrow and measurable
//! (`docs/research/relevance.md` §1):
//!
//! - **Tantivy hardcodes `K1 = 1.2; B = 0.75`** as module constants with no public API (issue
//!   #2924, open).
//! - **Lucene and Tantivy quantize the field norm into a single byte.** On 3–5-token product
//!   titles, distinct lengths collapse to the same norm and the length signal dies *before* `b`
//!   can act. presyo's corpus is short product titles — exactly the failure case.
//! - **BM25F and per-field `b` are mutually exclusive** in Elasticsearch's `combined_fields`.
//!
//! So: exact field lengths as `u16`, real BM25F with blended IDF, and `k1`/`b` exposed.
//!
//! # Why MaxScore and not Block-Max WAND
//!
//! BMW is the famous name and is excellent on sparse queries (Ding & Suel: 8.1× over exhaustive
//! OR). But it **inverts on dense queries** — on SPLADE in PISA, BMW 681 ms vs exhaustive OR
//! 553 ms vs **MaxScore 220 ms** (`docs/research/speed.md` §1). MaxScore needs no per-document heap
//! sorting, degrades better at large k, and Lucene and PISA converged on it independently.

use crate::analyze::{apply_alias, tokenize, AliasTable, Token};
use crate::dict::TermDict;
use std::collections::BTreeMap;

/// Maximum number of fields. Four covers `{brand, title, category, description}`, the shape
/// `docs/research/relevance.md` recommends, and keeps a posting one cache-friendly struct.
pub const MAX_FIELD: usize = 4;

/// Per-field retrieval parameters.
#[derive(Clone, Debug)]
pub struct Field {
    pub name: String,
    /// BM25F field boost. Acts as a term-frequency multiplier, per Robertson's formulation and
    /// Elasticsearch's `combined_fields` docs: boost 2 = "as if each term appeared twice".
    pub boost: f32,
    /// Per-field length normalization. This is the parameter Tantivy will not let you set.
    pub b: f32,
}

impl Field {
    pub fn new(name: &str, boost: f32, b: f32) -> Self {
        Field { name: name.to_string(), boost, b }
    }
}

/// The index schema plus global scoring parameters.
#[derive(Clone, Debug)]
pub struct Schema {
    pub field: Vec<Field>,
    /// BM25 term-frequency saturation. Anserini's default is 0.9; Lucene's is 1.2.
    pub k1: f32,
    /// Penalty multiplier applied per edit of distance, so a typo'd match scores below a clean one
    /// *within* the retrieval pass. The strict typo *bucket* is applied afterwards — see
    /// [`Index::search`].
    pub typo_penalty: f32,
}

impl Schema {
    pub fn new(field: Vec<Field>) -> Self {
        assert!(!field.is_empty() && field.len() <= MAX_FIELD, "1..={MAX_FIELD} fields");
        Schema { field, k1: 0.9, typo_penalty: 0.6 }
    }
    pub fn field_count(&self) -> usize {
        self.field.len()
    }
}

/// Typo-bucket penalty for a query token a document matched **not at all**.
///
/// Strictly greater than the maximum edit distance (2), because *not matching a word* is worse
/// than *matching it with two typos*. Getting this wrong inverts the ranking: documents that
/// silently ignore a query term outrank documents that actually found it.
///
/// This bug shipped in the first version of this file and was caught only by
/// `bench/roadmap/p6-real-corpus.md` running against presyo's real cross-store corpus, where it
/// cost **28.8 % vs 96.4 % typo recall@10**. No unit test on a small corpus can find it, because
/// on a small corpus every document matches every token.
pub const MISSING_TERM_PENALTY: u32 = 3;

/// What a document keeps when a **typeahead** query does not start where it starts.
///
/// Applied only in prefix mode, and applied as a **demotion of the non-anchored** rather than a
/// boost of the anchored, for the same reason static priors are normalized into `(0, 1]`:
/// retrieval prunes against upper bounds, and a factor above 1 would let a true score exceed the
/// block maxima the pruner trusts. A factor `<= 1` keeps every existing bound valid, so no bound
/// needed patching and the pruning code is untouched.
///
/// Relative order among non-anchored documents is unchanged — they are all scaled identically — so
/// this only ever *promotes* documents the user is plausibly typing from the start of.
pub const UNANCHORED_KEEP: f32 = 0.5;

/// Edit-distance marker carried by a *learned expansion* term.
///
/// Not a real edit distance — it is how an expansion term is priced. `emit` already discounts a
/// term's weight by `typo_penalty^distance` and `bucket_of` already sorts on distance, so borrowing
/// the same dial makes an expansion match rank **below an exact match and above nothing**, which is
/// exactly the intended semantics, without a second ranking mechanism to keep consistent.
pub const EXPANSION_DISTANCE: u8 = 1;

/// Typo-bucket penalty for a query **quantity** the document does not carry.
///
/// A stated size is not just another word. `Purefoods Honeycured Bacon Roll Pack 500g` must never
/// return the 250 g listing because the 500 g listing happened to omit two adjectives — that is a
/// price-comparison bug that presents as a relevance bug. Set far above
/// [`MISSING_TERM_PENALTY`] so no amount of word overlap can outbid a size mismatch.
///
/// This is safe when the corpus carries no listing of the requested size: every candidate is
/// penalized identically, so the relative order is unchanged and the engine still answers.
///
/// Found by `bench/roadmap/p6-real-corpus.md` against presyo's real cross-store corpus — it was
/// the last of six reported size violations to survive, and the only one that was a real defect.
pub const MISSING_QUANTITY_PENALTY: u32 = 16;

/// Postings per block for the block-max skip metadata.
///
/// 128 is the classic choice (Ding & Suel's BMW uses it) and the one variable-block work measures
/// against. Smaller blocks prune better but the side table grows — and that table is **15–42 % of
/// a compressed index at b=32** (`docs/research/speed.md` §1), which is the cost nobody quotes
/// alongside the speedup. Ours stores one `u32` and one `f32` per block, so at 128 it is ~0.5 bytes
/// per posting.
const BLOCK: usize = 128;

/// Posting lists longer than this get a champion list. Short lists are already cheap to scan, and a
/// champion list for every term would cost more memory than it saves.
const CHAMPION_MIN_DF: usize = 512;
/// How many top-scoring documents a champion list holds.
const CHAMPION_SIZE: usize = 64;

/// Maximum dictionary terms one query token may expand to.
///
/// Elasticsearch's `max_expansions` default, and its docs warn precisely about this: *"High values
/// can cause poor performance due to the high number of variations examined."* Without a cap, a
/// short common token in a large vocabulary expands to hundreds of terms, each contributing a
/// posting list to walk — which shows up as tail latency, not median.
///
/// Expansions are kept in order of **edit distance first, then document frequency ascending**, so
/// the cap discards the vaguest and least discriminative matches rather than an arbitrary slice.
const MAX_EXPANSION: usize = 16;

/// One posting on the **hot path**: a document and its precomputed saturated contribution.
///
/// `sat = pseudo_tf / (k1 + pseudo_tf)` depends only on the document and its field lengths — it is
/// entirely query-independent. Computing it inside the scoring loop meant every posting visited
/// paid a loop over fields with a float division each; precomputed, scoring a posting is one
/// multiply.
///
/// **Exactly 8 bytes, and the per-field term frequencies deliberately live elsewhere**
/// ([`Index::posting_tf`]). They are needed only to serialize, never to score, and retrieval at a
/// million documents is memory-bound — carrying them inline made every cache line hold half as
/// many postings as it could.
#[derive(Clone, Copy, Debug)]
struct Posting {
    doc: u32,
    sat: f32,
}

/// A scored search hit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    /// Dense internal document ordinal.
    pub doc: u32,
    /// BM25F score, typo-penalized.
    pub score: f32,
    /// Sum over query tokens of the best edit distance that token matched at. 0 = every token
    /// matched exactly. **This is the primary sort key** — see [`Index::search`].
    pub typo_bucket: u32,
}

/// A document being fed to [`IndexBuilder`].
#[derive(Clone, Debug, Default)]
pub struct Doc {
    /// Field text, positionally aligned with [`Schema::field`]. Missing entries are treated empty.
    pub field_text: Vec<String>,
}

impl Doc {
    pub fn new(field_text: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Doc { field_text: field_text.into_iter().map(Into::into).collect() }
    }
}

/// Accumulates documents, then produces an immutable [`Index`].
pub struct IndexBuilder {
    schema: Schema,
    alias: AliasTable,
    /// term text -> doc -> per-field tf
    term_post: BTreeMap<String, BTreeMap<u32, [u16; MAX_FIELD]>>,
    /// per-doc, per-field exact token count. `u16` on purpose — see the module docs.
    doc_len: Vec<[u16; MAX_FIELD]>,
    /// Per-document static prior, as supplied. Normalized at build time; see [`Index::prior_of`].
    raw_prior: Vec<f32>,
    /// First analyzed token of field 0 per document, resolved to a term id at build time.
    /// See [`Index::first_term`].
    first_text: Vec<String>,
    /// `(facet field, how many terms to learn per value)`, when expansion learning is enabled.
    expansion_cfg: Option<(usize, usize)>,
    /// Normalized text of the facet field per document. Empty unless learning is enabled.
    facet_text: Vec<String>,
}

impl IndexBuilder {
    pub fn new(schema: Schema) -> Self {
        IndexBuilder {
            schema,
            alias: AliasTable::new(),
            term_post: BTreeMap::new(),
            doc_len: Vec::new(),
            raw_prior: Vec::new(),
            first_text: Vec::new(),
            expansion_cfg: None,
            facet_text: Vec::new(),
        }
    }

    /// Learn a query-expansion table from a **facet field** — a low-cardinality field like a
    /// category, department or tag.
    ///
    /// For each distinct value of that field, the terms most *concentrated* in the documents
    /// carrying it are recorded. A later query that **is** one of those values is expanded with
    /// them.
    ///
    /// # Why this exists
    ///
    /// `bench/roadmap/p15-presyo-catalog.md` measured the failure this fixes: querying
    /// `"Baking Needs"` over 241,677 real products returns nothing relevant, because the products in
    /// that category are named *Camote Powder* and *Sago Tapioca* and **share no token with the
    /// query**. Lexical retrieval cannot bridge it; there is nothing to match.
    ///
    /// `bench/roadmap/p17-presyo-expand.md` measured this mechanism, held out: **+23.9 points of
    /// precision@10, which is 71 % of the gain available even from indexing the label directly**,
    /// against a random-expansion control that *costs* 35.9 points.
    ///
    /// # The trigger rule, and why it is strict
    ///
    /// Expansion fires only when the whole query matches a learned value. A *loose* rule — fire when
    /// the query merely contains the value's words — fires on 19.7 % of ordinary product queries and
    /// costs **2.0 points of exact-product hit@1**, because `Signature Select Ice Cream Butter
    /// Pecan` contains the category `Cream`. Strict matching fires on **zero** product queries and
    /// costs nothing.
    ///
    /// **Most learned terms are brand names**, and that is measured rather than assumed: striking
    /// every brand token out still retains 43 % of the gain, so this is not pure brand memorisation
    /// — but a value whose brands are all unseen will degrade toward that figure.
    pub fn learn_expansion(mut self, facet_field: usize, top_k: usize) -> Self {
        assert!(facet_field < MAX_FIELD, "facet field out of range");
        self.expansion_cfg = Some((facet_field, top_k));
        self
    }

    /// Install an alias table, applied at index time **and** query time.
    pub fn with_alias(mut self, alias: AliasTable) -> Self {
        self.alias = alias;
        self
    }

    /// Add a document with a **static prior** — query-independent importance.
    ///
    /// This is the knob a hand-rolled ranker always has and a general one usually does not.
    /// `bench/roadmap/p12-maphy-place.md` measured the cost of not having it: maphy's place search
    /// beat this engine on typeahead (90.4 % vs 85.2 % hit@10) purely because its tie-break ranks
    /// by administrative level, and BM25F had no way to express "a region outranks a barangay".
    /// Every consumer has such a prior — store trust, rating count, catalog level, PSGC level.
    ///
    /// `prior` must be finite and **> 0**; larger means more important. The scale is arbitrary
    /// because priors are normalized against the largest at build time (see [`Self::build`]), so
    /// `1..=4` and `100..=400` rank identically.
    pub fn add_with_prior(&mut self, doc: &Doc, prior: f32) -> u32 {
        let id = self.add(doc);
        let p = if prior.is_finite() && prior > 0.0 { prior } else { 1.0 };
        self.raw_prior[id as usize] = p;
        id
    }

    /// Add a document with the default prior of 1.0. Returns its dense ordinal.
    pub fn add(&mut self, doc: &Doc) -> u32 {
        let id = self.doc_len.len() as u32;
        let mut len = [0u16; MAX_FIELD];
        let mut first = String::new();
        let mut facet = String::new();
        for (fi, text) in doc.field_text.iter().enumerate().take(self.schema.field_count()) {
            let mut tok: Vec<Token> = tokenize(text);
            apply_alias(&mut tok, &self.alias);
            len[fi] = tok.len().min(u16::MAX as usize) as u16;
            if fi == 0 {
                first.clone_from(&tok.first().map(|t| t.text.clone()).unwrap_or_default());
            }
            if self.expansion_cfg.is_some_and(|(f, _)| f == fi) {
                facet = tok.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join(" ");
            }
            for t in tok {
                let e = self.term_post.entry(t.text).or_default().entry(id).or_insert([0; MAX_FIELD]);
                e[fi] = e[fi].saturating_add(1);
            }
        }
        self.doc_len.push(len);
        self.raw_prior.push(1.0);
        self.first_text.push(first);
        self.facet_text.push(facet);
        id
    }

    /// Learn, per facet value, the terms most concentrated in the documents carrying it.
    ///
    /// Concentration is `in-value rate / overall rate`: a term appearing in most of a value's
    /// documents and few others scores high. A minimum document count keeps a term that appears
    /// twice in one small value from outranking one that appears four hundred times in it.
    ///
    /// Returns `(normalized value, term ids)` sorted by value so lookup is a binary search.
    fn derive_expansion(
        cfg: Option<(usize, usize)>,
        facet: &[String],
        term: &[String],
        posting: &[Vec<Posting>],
    ) -> Vec<(String, Vec<u32>)> {
        use std::collections::HashMap;
        let Some((_, top_k)) = cfg else { return Vec::new() };
        if top_k == 0 || facet.is_empty() {
            return Vec::new();
        }
        // How many of a value's documents a term must appear in before it can be learned.
        //
        // Scaled rather than fixed: an absolute 5 silently learns NOTHING for a facet value with
        // only a handful of documents, because no term repeats that often in it — the table comes
        // back empty and the feature looks broken rather than inapplicable. Capped at 5 so a large
        // value behaves exactly as it did when p17 measured it.
        let min_df = |n_value: usize| (n_value / 4).clamp(2, 5);

        let mut doc_count_of: HashMap<&str, usize> = HashMap::new();
        for f in facet.iter().filter(|f| !f.is_empty()) {
            *doc_count_of.entry(f.as_str()).or_default() += 1;
        }
        let total = facet.len() as f64;

        // term id -> value -> how many of that value's documents carry the term.
        let mut per_value: HashMap<&str, HashMap<u32, usize>> = HashMap::new();
        for (tid, list) in posting.iter().enumerate() {
            for p in list {
                let f = facet.get(p.doc as usize).map(String::as_str).unwrap_or("");
                if f.is_empty() {
                    continue;
                }
                *per_value.entry(f).or_default().entry(tid as u32).or_default() += 1;
            }
        }

        let mut out: Vec<(String, Vec<u32>)> = Vec::with_capacity(per_value.len());
        for (value, counts) in per_value {
            let n_value = doc_count_of.get(value).copied().unwrap_or(0) as f64;
            if n_value <= 0.0 {
                continue;
            }
            let mut scored: Vec<(f64, u32)> = counts
                .into_iter()
                .filter(|&(_, n)| n >= min_df(n_value as usize))
                .map(|(tid, n)| {
                    let inside = n as f64 / n_value;
                    let overall = posting[tid as usize].len() as f64 / total;
                    (inside / (overall + 1e-9), tid)
                })
                .collect();
            scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
            scored.truncate(top_k);
            if scored.is_empty() {
                continue;
            }
            // Do not expand a value with its own words: they are already in the query.
            let own: Vec<&str> = value.split(' ').collect();
            let keep: Vec<u32> = scored
                .into_iter()
                .map(|(_, t)| t)
                .filter(|&t| !own.contains(&term[t as usize].as_str()))
                .collect();
            if !keep.is_empty() {
                out.push((value.to_string(), keep));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Scale priors into `(0, 1]` by dividing by the largest, and drop them entirely when they
    /// carry no information. See [`Index::prior`] for why the range matters to correctness.
    fn normalize_prior(raw: Vec<f32>) -> Vec<f32> {
        let max = raw.iter().copied().fold(0.0f32, f32::max);
        if max <= 0.0 || raw.iter().all(|&p| p == max) {
            return Vec::new(); // uniform: nothing to store and nothing to apply
        }
        raw.into_iter().map(|p| (p / max).clamp(f32::MIN_POSITIVE, 1.0)).collect()
    }

    pub fn build(self) -> Result<Index, String> {
        let term: Vec<String> = self.term_post.keys().cloned().collect();
        let dict = TermDict::build(&term)?;

        let doc_count = self.doc_len.len();

        let mut posting: Vec<Vec<Posting>> = Vec::with_capacity(self.term_post.len());
        let mut posting_tf: Vec<Vec<[u16; MAX_FIELD]>> = Vec::with_capacity(self.term_post.len());
        for per_doc in self.term_post.into_values() {
            let mut pl = Vec::with_capacity(per_doc.len());
            let mut tl = Vec::with_capacity(per_doc.len());
            for (doc, tf) in per_doc {
                pl.push(Posting { doc, sat: 0.0 });
                tl.push(tf);
            }
            posting.push(pl);
            posting_tf.push(tl);
        }

        let mut avg_len = [1.0f32; MAX_FIELD];
        for fi in 0..self.schema.field_count() {
            let total: u64 = self.doc_len.iter().map(|l| l[fi] as u64).sum();
            // Guard the empty-field case so the normalizer never divides by zero.
            avg_len[fi] = (total as f32 / doc_count.max(1) as f32).max(1.0);
        }

        // Computed before the struct literal takes ownership of `posting`.
        let expansion =
            Self::derive_expansion(self.expansion_cfg, &self.facet_text, &term, &posting);

        let mut ix = Index {
            schema: self.schema,
            alias: self.alias,
            dict,
            posting,
            posting_tf,
            block_last: Vec::new(),
            block_max: Vec::new(),
            champion: Vec::new(),
            max_sat: Vec::new(),
            expansion,
            prior: Self::normalize_prior(self.raw_prior),
            deleted: Vec::new(),
            deleted_count: 0,
            first_term: {
                // `term` is the sorted list the dictionary was built from, so a binary search over
                // it gives the same ids the postings use.
                self.first_text
                    .iter()
                    .map(|t| match t.is_empty() {
                        true => u32::MAX,
                        false => term.binary_search(t).map(|i| i as u32).unwrap_or(u32::MAX),
                    })
                    .collect()
            },
            doc_len: self.doc_len,
            avg_len,
            doc_count,
        };
        ix.rebuild_meta();
        Ok(ix)
    }
}

/// An immutable, queryable index.
pub struct Index {
    schema: Schema,
    alias: AliasTable,
    dict: TermDict,
    /// Indexed by `term_id`; each list is sorted ascending by `doc`.
    posting: Vec<Vec<Posting>>,
    // NOT PRESENT, DELIBERATELY: an internal->external document id map.
    //
    // `docs/research/speed.md` ranks docID reordering as the best remaining multiplier, and the
    // diagnostic in `bench/roadmap/p7-scale.md` pointed straight at it: block maxima are
    // uninformative because score is uncorrelated with document id. It was implemented (documents
    // sorted by total length, so short high-scoring documents cluster) and MEASURED WORSE:
    // typo p99 at 1 M went 6.40 ms -> 7.02 ms, build 14.1 s -> 19.0 s, index 157 MB -> 161 MB.
    //
    // The reason is corpus-specific and worth keeping: the DepEd corpus has nearly uniform document
    // lengths (name + locality + region), so length-sorting separates scores hardly at all, while
    // the remap, the extra u32 per document and the indirection all cost something. Reordering
    // stays the right idea for a corpus with real length variance -- but it has to be measured on
    // that corpus rather than assumed from the literature.
    /// Per-field term frequencies, parallel to `posting`. **Serialization only** — never touched by
    /// a query, so it is kept out of the hot path's cache lines.
    posting_tf: Vec<Vec<[u16; MAX_FIELD]>>,
    /// Per term, per block of [`BLOCK`] postings: the last document id in the block, and the
    /// largest pseudo term-frequency inside it.
    ///
    /// This is the block-max metadata that makes retrieval sublinear in corpus size. Without it,
    /// document-at-a-time retrieval walks every posting of every query term, so latency tracks
    /// document count no matter how good the top-k bound is — measured at
    /// `bench/roadmap/p7-scale.md` as 11.8 ms typo p99 at 1 M documents.
    block_last: Vec<Vec<u32>>,
    block_max: Vec<Vec<f32>>,
    /// Per term, positions into that term's posting list of its highest-scoring documents. Empty
    /// for terms below [`CHAMPION_MIN_DF`].
    ///
    /// **Champion lists exist to solve the exact failure `bench/roadmap/p7-scale.md` diagnosed.**
    /// The tail at a million documents was low-selectivity queries: the worst case, `"School ame"`,
    /// has two terms, one carrying a 428,654-posting list and the other matching nothing. With no
    /// discriminative term, the top-k threshold starts at zero and climbs slowly, and block-max
    /// skipping never engages -- a block of 128 arbitrary documents almost always contains one
    /// short, high-scoring document, so every block maximum looks promising.
    ///
    /// Seeding the heap from precomputed champions makes the threshold high *before* the scan
    /// begins, which is what turns the existing skip machinery on. Classic static pruning. It
    /// changes no answers: a champion is a real document, scored exactly as the main loop scores
    /// it, and the main loop declines to insert it twice.
    champion: Vec<Vec<u32>>,
    /// Per term, the largest saturated contribution any of its documents attains.
    ///
    /// **Precomputed, because it is a property of the index and not of the query.** Computing it
    /// inside `plan()` meant every query scanned every posting of every expanded term *before*
    /// retrieval began — an O(total postings) prologue that made latency grow linearly with corpus
    /// size and turned MaxScore's pruning into theatre. Measured by `bench/roadmap/p7-scale.md`:
    /// typo p99 went 614 us at 5 K documents to **15.9 ms at 1 M**, tracking document count almost
    /// exactly. That is what a hidden full scan looks like from the outside.
    max_sat: Vec<f32>,
    /// Learned query expansion: `(normalized facet value, term ids)`, sorted by value.
    ///
    /// Empty unless [`IndexBuilder::learn_expansion`] was called. See that method for the
    /// measurements this exists to deliver and for why the trigger is strict.
    expansion: Vec<(String, Vec<u32>)>,
    /// Per-document static prior in `(0, 1]`, or **empty** when every document has the default.
    ///
    /// # Why the range is (0, 1] and not "any boost"
    ///
    /// Retrieval prunes against *upper bounds*: block maxima bound a block's contribution, and the
    /// non-essential bail compares `score + optimistic remainder` against the top-k threshold.
    /// A multiplier above 1 would make the true score exceed those bounds and the pruner would
    /// discard documents that belong in the result — silently, and only on large corpora where
    /// pruning actually engages.
    ///
    /// Normalizing the supplied priors by the largest one removes the failure mode by
    /// construction rather than by patching every bound: the applied factor is always `<= 1`, so
    /// every existing bound stays a valid upper bound and the pruning code is untouched. Ranking is
    /// unaffected because only the *ratios* between priors matter.
    prior: Vec<f32>,
    /// Term id of the **first token of field 0** per document; `u32::MAX` when the field is empty.
    ///
    /// # What this is for
    ///
    /// `bench/roadmap/p12-maphy-place.md` measured the engine losing typeahead to a hand-rolled
    /// `norm.startsWith(q)` scan, and every miss had the same shape: a prefix ending part-way
    /// through a *second* token. `"DEL C"` plans as exact-`del` plus prefix-`c*`; `del` is a common
    /// token and `c*` matches nearly everything, so `DEL CARMEN` never ranks. A tokenized query has
    /// lost the string-level fact that the user is typing from the START of a name.
    ///
    /// Storing the first term id restores it for four bytes per document, without positions.
    first_term: Vec<u32>,
    /// One bit per document; set means deleted. **Empty when nothing is deleted**, which is the
    /// common case and costs nothing to carry.
    ///
    /// # What deletion does and does not do
    ///
    /// A deleted document stops appearing in results immediately. It is **not** removed from the
    /// postings, and collection statistics — document frequency, average field length — still count
    /// it until the index is rebuilt. That is exactly how Lucene behaves between merges, and it is
    /// the right trade here for the same reason: rewriting postings on every delete would turn an
    /// O(1) operation into an O(index) one, and this project's compaction story is already
    /// "rebuild from the application's database", which is the source of truth.
    ///
    /// The consequence is small and worth stating rather than discovering: after many deletions,
    /// scores drift slightly because IDF reflects a collection larger than the live one. The result
    /// SET is always correct; only the ordering of near-ties can move. `Searcher::needs_compaction`
    /// is what tells an application when that has gone far enough to matter.
    deleted: Vec<u64>,
    deleted_count: usize,
    doc_len: Vec<[u16; MAX_FIELD]>,
    avg_len: [f32; MAX_FIELD],
    doc_count: usize,
}

impl Index {
    /// `1.0` when anchored or when anchoring does not apply, [`UNANCHORED_KEEP`] otherwise.
    #[inline]
    fn anchor_factor(&self, doc: u32, anchor: &[u32]) -> f32 {
        match self.is_anchored(doc, anchor) {
            true => 1.0,
            false => UNANCHORED_KEEP,
        }
    }

    /// Does this document's field-0 text begin with the query's first token?
    ///
    /// `anchor` is the set of term ids the query's first group expanded to (including typo
    /// variants), so a misspelled first word still anchors.
    #[inline]
    fn is_anchored(&self, doc: u32, anchor: &[u32]) -> bool {
        if anchor.is_empty() {
            return true; // nothing to anchor against: do not demote anything
        }
        match self.first_term.get(doc as usize) {
            Some(&t) if t != u32::MAX => anchor.contains(&t),
            _ => false,
        }
    }

    /// Mark a document deleted. Returns `true` if this call changed anything.
    ///
    /// Idempotent, and cheap: one bit. The document stops appearing in results at once.
    pub fn delete(&mut self, doc: u32) -> bool {
        if doc as usize >= self.doc_count || self.is_deleted(doc) {
            return false;
        }
        if self.deleted.is_empty() {
            self.deleted = vec![0u64; self.doc_count.div_ceil(64)];
        }
        self.deleted[doc as usize / 64] |= 1u64 << (doc as usize % 64);
        self.deleted_count += 1;
        true
    }

    /// Restore a deleted document. Returns `true` if this call changed anything.
    pub fn undelete(&mut self, doc: u32) -> bool {
        if !self.is_deleted(doc) {
            return false;
        }
        self.deleted[doc as usize / 64] &= !(1u64 << (doc as usize % 64));
        self.deleted_count -= 1;
        true
    }

    #[inline]
    pub fn is_deleted(&self, doc: u32) -> bool {
        match self.deleted.is_empty() {
            true => false,
            false => self
                .deleted
                .get(doc as usize / 64)
                .is_some_and(|w| w >> (doc as usize % 64) & 1 == 1),
        }
    }

    /// Documents marked deleted.
    pub fn deleted_count(&self) -> usize {
        self.deleted_count
    }

    /// Documents that can still be returned. `doc_count() - deleted_count()`.
    pub fn live_count(&self) -> usize {
        self.doc_count - self.deleted_count
    }

    /// The normalized static prior of a document: `1.0` when none was supplied.
    #[inline]
    pub fn prior_of(&self, doc: u32) -> f32 {
        match self.prior.is_empty() {
            true => 1.0,
            false => self.prior.get(doc as usize).copied().unwrap_or(1.0),
        }
    }

    /// Whether this index carries any non-uniform prior.
    pub fn has_prior(&self) -> bool {
        !self.prior.is_empty()
    }

    pub(crate) fn set_prior(&mut self, prior: Vec<f32>) {
        self.prior = prior;
    }

    pub(crate) fn set_first_term(&mut self, first_term: Vec<u32>) {
        self.first_term = first_term;
    }

    pub(crate) fn set_expansion(&mut self, expansion: Vec<(String, Vec<u32>)>) {
        self.expansion = expansion;
    }

    pub(crate) fn set_deleted(&mut self, deleted: Vec<u64>) {
        self.deleted_count = deleted.iter().map(|w| w.count_ones() as usize).sum();
        self.deleted = deleted;
    }
}

impl std::fmt::Debug for Index {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Index")
            .field("doc_count", &self.doc_count)
            .field("term_count", &self.dict.term_count())
            .field("dict_bytes", &self.dict.byte_len())
            .finish()
    }
}

/// Relative tolerance below which two scores are treated as tied.
///
/// **MaxScore accumulates a document's term contributions in a different order than exhaustive
/// scoring does, and `f32` addition is not associative.** Two documents whose true scores are
/// equal can therefore compare differently depending on the retrieval path, which makes the result
/// order depend on the optimizer rather than on relevance. Anything inside this tolerance falls
/// through to the deterministic document-id tiebreak instead — the property presyo pins in its own
/// contract test.
///
/// Found by `block_max_pruning_agrees_with_exhaustive_or_at_scale`, which returned identical
/// document *sets* with two adjacent entries transposed.
const SCORE_TIE_REL: f32 = 1e-6;

/// The single ranking comparator, used by both the pruned and the exhaustive path.
#[inline]
pub(crate) fn rank_cmp(a: &Hit, b: &Hit) -> std::cmp::Ordering {
    a.typo_bucket.cmp(&b.typo_bucket).then_with(|| {
        let scale = a.score.abs().max(b.score.abs()).max(1.0);
        if (a.score - b.score).abs() <= SCORE_TIE_REL * scale {
            std::cmp::Ordering::Equal
        } else {
            b.score.total_cmp(&a.score)
        }
    }).then(a.doc.cmp(&b.doc))
}

/// A candidate in the top-k min-heap, ordered by score so the *worst* is always at the root.
///
/// `f32` is not `Ord`, so `total_cmp` is used; NaN cannot arise here (scores are finite sums of
/// finite terms) but `total_cmp` is total regardless, which keeps the heap's invariant sound.
///
/// **This replaced a `Vec` scanned linearly for its minimum on every replacement.** At 1 M
/// documents that was ~200 comparisons per accepted candidate against a 100-slot pool, and it was
/// the second of two hidden linear factors `bench/roadmap/p7-scale.md` exposed.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Candidate {
    score: f32,
    doc: u32,
    bucket: u32,
}

impl Eq for Candidate {}

impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // `BinaryHeap` is a max-heap and its root is what gets evicted, so "greater" must mean
        // "worse": lower score first, and **among equal scores, the HIGHER document id**.
        //
        // Getting that second clause backwards silently evicted the lower doc id from the pool
        // while the final sort preferred it, so equal-scoring documents disappeared from results
        // entirely. Caught by `block_max_pruning_agrees_with_exhaustive_or_at_scale`, which showed
        // a *missing* document rather than a reordered one — and the block-skip logic was suspected
        // first and was innocent.
        other.score.total_cmp(&self.score).then(self.doc.cmp(&other.doc))
    }
}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// A borrowed view of an index's internals, for serialization only.
pub(crate) struct Snapshot<'a> {
    pub doc_count: usize,
    pub field: &'a [Field],
    pub k1: f32,
    pub typo_penalty: f32,
    pub avg_len: [f32; MAX_FIELD],
    pub alias: Vec<(&'a str, &'a str)>,
    pub dict_bytes: Vec<u8>,
    pub posting: Vec<Vec<(u32, [u16; MAX_FIELD])>>,
    pub doc_len: &'a [[u16; MAX_FIELD]],
    /// Normalized static priors, empty when uniform.
    pub prior: &'a [f32],
    /// First-token term id per document; see [`Index::first_term`].
    pub first_term: &'a [u32],
    /// Deleted-document bitmap; empty when nothing is deleted.
    pub deleted: &'a [u64],
    /// Learned query expansion; empty unless it was asked for.
    pub expansion: &'a [(String, Vec<u32>)],
}

/// One expanded query term: a dictionary entry, the query token it came from, and its penalty.
#[derive(Clone, Copy, Debug)]
struct QueryTerm {
    term_id: u32,
    group: u16,
    distance: u8,
    /// `idf * typo_penalty^distance`, folded once.
    weight: f32,
    /// Upper bound on this term's contribution to any document's score.
    max_score: f32,
}

impl Index {
    pub fn doc_count(&self) -> usize {
        self.doc_count
    }
    pub fn term_count(&self) -> usize {
        self.dict.term_count()
    }
    /// Serialized term-dictionary size — the browser byte budget's denominator.
    pub fn dict_byte_len(&self) -> usize {
        self.dict.byte_len()
    }
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// The dictionary ordinal of an exact term, if present. Used by `format` tests and by any
    /// caller that wants to address a posting list directly.
    pub fn term_id_of(&self, term: &str) -> Option<u32> {
        self.dict.exact(term)
    }

    /// A borrowed view of everything `format` needs to serialize, so the fields can stay private.
    #[allow(clippy::type_complexity)]
    pub(crate) fn snapshot(&self) -> Snapshot<'_> {
        Snapshot {
            doc_count: self.doc_count,
            field: &self.schema.field,
            k1: self.schema.k1,
            typo_penalty: self.schema.typo_penalty,
            avg_len: self.avg_len,
            alias: self.alias.iter().collect(),
            dict_bytes: self.dict.to_bytes(),
            posting: self
                .posting
                .iter()
                .zip(self.posting_tf.iter())
                .map(|(l, tfs)| l.iter().zip(tfs.iter()).map(|(p, tf)| (p.doc, *tf)).collect())
                .collect(),
            doc_len: &self.doc_len,
            prior: &self.prior,
            first_term: &self.first_term,
            deleted: &self.deleted,
            expansion: &self.expansion,
        }
    }

    /// Rebuild from deserialized parts. Kept crate-private: the only supported way to construct an
    /// index from bytes is [`Index::from_bytes`], which validates every span first.
    pub(crate) fn from_parts(
        schema: Schema,
        alias: AliasTable,
        dict_bytes: &[u8],
        posting_raw: Vec<Vec<(u32, [u16; MAX_FIELD])>>,
        doc_len: Vec<[u16; MAX_FIELD]>,
        avg_len: [f32; MAX_FIELD],
        doc_count: usize,
    ) -> Result<Index, String> {
        let dict = crate::dict::TermDict::from_bytes(dict_bytes.to_vec())?;
        if dict.term_count() != posting_raw.len() {
            return Err(format!(
                "dictionary has {} terms but {} posting lists were read",
                dict.term_count(),
                posting_raw.len()
            ));
        }
        let mut posting: Vec<Vec<Posting>> = Vec::with_capacity(posting_raw.len());
        let mut posting_tf: Vec<Vec<[u16; MAX_FIELD]>> = Vec::with_capacity(posting_raw.len());
        for l in posting_raw {
            let mut pl = Vec::with_capacity(l.len());
            let mut tl = Vec::with_capacity(l.len());
            for (doc, tf) in l {
                pl.push(Posting { doc, sat: 0.0 });
                tl.push(tf);
            }
            posting.push(pl);
            posting_tf.push(tl);
        }
        let mut ix = Index {
            schema,
            alias,
            dict,
            posting,
            posting_tf,
            block_last: Vec::new(),
            block_max: Vec::new(),
            champion: Vec::new(),
            max_sat: Vec::new(),
            // Set by the caller after construction via `set_prior`, because the prior section is
            // optional: an index written before priors existed simply has none.
            prior: Vec::new(),
            deleted: Vec::new(),
            deleted_count: 0,
            expansion: Vec::new(),
            first_term: Vec::new(),
            doc_len,
            avg_len,
            doc_count,
        };
        ix.rebuild_meta();
        Ok(ix)
    }

    /// One pass over the whole index, at build or load time, to fill the retrieval metadata.
    fn rebuild_meta(&mut self) {
        // Fill each posting's saturated contribution first; everything else derives from it.
        let sat: Vec<Vec<f32>> = self
            .posting
            .iter()
            .zip(self.posting_tf.iter())
            .map(|(list, tfs)| {
                list.iter()
                    .zip(tfs.iter())
                    .map(|(p, tf)| self.saturate(self.pseudo_tf(tf, p.doc)))
                    .collect()
            })
            .collect();
        for (list, s) in self.posting.iter_mut().zip(sat.iter()) {
            for (p, v) in list.iter_mut().zip(s.iter()) {
                p.sat = *v;
            }
        }

        let mut max_sat = Vec::with_capacity(self.posting.len());
        let mut block_last = Vec::with_capacity(self.posting.len());
        let mut block_max = Vec::with_capacity(self.posting.len());
        for list in &self.posting {
            let mut term_max = 0.0f32;
            let mut last = Vec::with_capacity(list.len().div_ceil(BLOCK));
            let mut bmax = Vec::with_capacity(list.len().div_ceil(BLOCK));
            for chunk in list.chunks(BLOCK) {
                let mut m = 0.0f32;
                for p in chunk {
                    m = m.max(p.sat);
                }
                term_max = term_max.max(m);
                last.push(chunk[chunk.len() - 1].doc);
                bmax.push(m);
            }
            max_sat.push(term_max);
            block_last.push(last);
            block_max.push(bmax);
        }
        self.max_sat = max_sat;
        self.block_last = block_last;
        self.block_max = block_max;

        self.champion = self
            .posting
            .iter()
            .map(|list| {
                if list.len() < CHAMPION_MIN_DF {
                    return Vec::new();
                }
                let mut idx: Vec<u32> = (0..list.len() as u32).collect();
                // Highest saturated contribution first; document id breaks ties so the build is
                // deterministic.
                idx.sort_unstable_by(|&a, &b| {
                    list[b as usize]
                        .sat
                        .total_cmp(&list[a as usize].sat)
                        .then(list[a as usize].doc.cmp(&list[b as usize].doc))
                });
                idx.truncate(CHAMPION_SIZE);
                idx
            })
            .collect();
    }

    /// Score one document against every query term by binary-searching each posting list.
    ///
    /// Used only for champion seeding, where the document is known but no cursor is positioned on
    /// it. Returns `None` when the document matches no term.
    fn score_doc(
        &self,
        doc: u32,
        term: &[QueryTerm],
        group_is_quantity: &[bool],
        group_dist: &mut [u8],
        anchor: &[u32],
    ) -> Option<Hit> {
        if self.is_deleted(doc) {
            return None;
        }
        group_dist.iter_mut().for_each(|d| *d = u8::MAX);
        let mut score = 0.0f64;
        let mut any = false;
        for t in term {
            let list = &self.posting[t.term_id as usize];
            if let Ok(at) = list.binary_search_by_key(&doc, |p| p.doc) {
                score += (t.weight * list[at].sat) as f64;
                let g = t.group as usize;
                group_dist[g] = group_dist[g].min(t.distance);
                any = true;
            }
        }
        if !any {
            return None;
        }
        Some(Hit {
            doc,
            // The prior scales the relevance score; it deliberately does NOT touch `typo_bucket`,
            // which stays the primary sort key. An important document with a misspelled match must
            // not outrank an exact match on an unimportant one — a prior expresses importance, not
            // correctness.
            score: score as f32 * self.prior_of(doc) * self.anchor_factor(doc, anchor),
            typo_bucket: Self::bucket_of(group_dist, group_is_quantity),
        })
    }

    /// Advance `cursor` to the first posting of `term_id` with `doc >= target`, skipping whole
    /// blocks via their last-document id. O(blocks skipped + BLOCK) rather than O(postings).
    #[inline]
    fn seek(&self, term_id: u32, cursor: &mut usize, target: u32) {
        let list = &self.posting[term_id as usize];
        let last = &self.block_last[term_id as usize];
        let mut b = *cursor / BLOCK;
        while b < last.len() && last[b] < target {
            b += 1;
        }
        let start = (b * BLOCK).max(*cursor);
        let mut c = start;
        while c < list.len() && list[c].doc < target {
            c += 1;
        }
        *cursor = c;
    }

    /// Robertson-Sparck-Jones IDF with the standard +0.5 smoothing, as Lucene uses.
    #[inline]
    fn idf(&self, df: usize) -> f32 {
        let n = self.doc_count as f32;
        let df = df as f32;
        (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
    }

    /// BM25F pseudo term frequency: field contributions summed **before** saturation, each
    /// normalized by that field's own length and `b`. This is the whole point of BM25F — summing
    /// per-field *scores* instead traverses the saturation curve once per field and computes IDF
    /// per field, which is the documented `most_fields` failure.
    #[inline]
    fn pseudo_tf(&self, tf: &[u16; MAX_FIELD], doc: u32) -> f32 {
        let len = &self.doc_len[doc as usize];
        let mut ptf = 0.0f32;
        for (fi, f) in self.schema.field.iter().enumerate() {
            let t = tf[fi] as f32;
            if t == 0.0 {
                continue;
            }
            let dl = len[fi] as f32;
            let norm = 1.0 - f.b + f.b * (dl / self.avg_len[fi]);
            ptf += f.boost * t / norm.max(f32::MIN_POSITIVE);
        }
        ptf
    }

    #[inline]
    fn saturate(&self, ptf: f32) -> f32 {
        ptf / (self.schema.k1 + ptf)
    }

    /// Analyze a query string into expanded, weighted query terms.
    ///
    /// `prefix_last` applies typeahead semantics to the final token only — Meilisearch's rule, and
    /// the right default for a search-as-you-type box.
    fn plan(&self, query: &str, prefix_last: bool) -> (Vec<QueryTerm>, Vec<bool>, bool) {
        let mut tok = tokenize(query);
        apply_alias(&mut tok, &self.alias);
        // A group is a "quantity" group when its token parses as a real physical size. A bare
        // number that is not a size (a year, a model number) is an ordinary term.

        // One pass. Each token contributes one query group, EXCEPT a token that matches nothing at
        // all and splits cleanly into two dictionary terms, which contributes two.
        //
        // **Compound splitting is a last resort, tried only after exact and fuzzy have both come
        // back empty.** `math30.23` for `MATH 30.23`, `cocacola` for `Coca Cola`, `bearbrand`,
        // `luckyme` — an entire category of presyo's frozen fixture (`joined`) and one of
        // profstopick's contract assertions. Trying it *before* fuzzy is a measurable regression:
        // a typo'd token often happens to split into two real terms, and taking that split throws
        // away the typo correction and replaces one narrow match with two broad common ones.
        // Meilisearch likewise prices concatenation at one typo rather than ahead of one.
        //
        // The expansion is computed **once** per token and reused — an earlier version probed
        // "is this reachable by fuzzy?" and then expanded again, paying for two automaton
        // traversals on exactly the tokens that are most expensive to traverse.
        let n = tok.len();
        let mut group_is_quantity: Vec<bool> = Vec::with_capacity(n);
        let mut out: Vec<QueryTerm> = Vec::new();

        for (ti, t) in tok.iter().enumerate() {
            let is_last = ti + 1 == n;
            let matches = self.dict.expand_lazy(&t.text, t.is_numeric, prefix_last && is_last);

            if matches.is_empty() && t.text.chars().count() >= 4 {
                if let Some((a, b)) = self.split_compound(&t.text) {
                    for part in [a, b] {
                        let numeric = part.chars().any(|c| c.is_ascii_digit());
                        let gi = group_is_quantity.len() as u16;
                        group_is_quantity
                            .push(numeric && crate::analyze::parse_quantity(&part).is_some());
                        let m = self.dict.expand_lazy(&part, numeric, false);
                        self.emit(&mut out, m, gi);
                    }
                    continue;
                }
            }

            let gi = group_is_quantity.len() as u16;
            group_is_quantity
                .push(t.is_numeric && crate::analyze::parse_quantity(&t.text).is_some());
            self.emit(&mut out, matches, gi);
        }

        // --- learned expansion, fired STRICTLY.
        //
        // Only when the whole query IS a learned facet value. A loose rule that fired whenever a
        // query merely *contained* one triggered on 19.7 % of ordinary product queries and cost
        // 2.0 points of exact-product hit@1 — `Signature Select Ice Cream Butter Pecan` contains the
        // category `Cream`. Strict matching fires on none of them. Measured in
        // `bench/roadmap/p17-presyo-expand.md`.
        let mut expanded = false;
        if !self.expansion.is_empty() && !tok.is_empty() {
            let key = tok.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join(" ");
            if let Ok(at) = self.expansion.binary_search_by(|e| e.0.as_str().cmp(key.as_str())) {
                // Emitted as an ALTERNATIVE inside every group, not as a group of its own.
                //
                // `bucket_of` SUMS a penalty per unsatisfied group, so a group of expansion terms
                // would leave a document that matches only them paying `MISSING_TERM_PENALTY` for
                // every original token — ranking a genuine category member BELOW a document that
                // merely shares one word with the category's name. As alternatives, one expansion
                // hit satisfies the group it sits in, which is the intended meaning: *this document
                // belongs to the thing you asked for.*
                let extra: Vec<crate::dict::TermMatch> = self.expansion[at]
                    .1
                    .iter()
                    .map(|&term_id| crate::dict::TermMatch { term_id, distance: EXPANSION_DISTANCE })
                    .collect();
                for gi in 0..group_is_quantity.len() as u16 {
                    self.emit(&mut out, extra.clone(), gi);
                }
                expanded = true;
            }
        }

        (out, group_is_quantity, expanded)
    }

    /// Number of learned facet values, or 0 when expansion was not learned.
    pub fn expansion_count(&self) -> usize {
        self.expansion.len()
    }

    /// Term ids learned for a facet value, for inspection and tests.
    ///
    /// Ids rather than text because the dictionary is an FST: it maps term -> id, and the reverse
    /// direction would need a second structure that nothing else wants.
    pub fn expansion_of(&self, value: &str) -> &[u32] {
        let tok = tokenize(value);
        let key = tok.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join(" ");
        match self.expansion.binary_search_by(|e| e.0.as_str().cmp(key.as_str())) {
            Ok(at) => &self.expansion[at].1,
            Err(_) => &[],
        }
    }

    /// The bucket cost of a document, given the best edit distance it achieved per query group.
    #[inline]
    fn bucket_of(group_dist: &[u8], group_is_quantity: &[bool]) -> u32 {
        group_dist
            .iter()
            .enumerate()
            .map(|(g, &d)| {
                if d != u8::MAX {
                    d as u32
                } else if group_is_quantity.get(g).copied().unwrap_or(false) {
                    MISSING_QUANTITY_PENALTY
                } else {
                    MISSING_TERM_PENALTY
                }
            })
            .sum()
    }

    /// Turn a token's dictionary matches into weighted query terms for group `gi`, applying the
    /// expansion cap.
    fn emit(&self, out: &mut Vec<QueryTerm>, mut matches: Vec<crate::dict::TermMatch>, gi: u16) {
        if matches.len() > MAX_EXPANSION {
            // Closest first; among equals, the rarer term is the more discriminative one.
            matches.sort_by_key(|m| (m.distance, self.posting[m.term_id as usize].len()));
            matches.truncate(MAX_EXPANSION);
        }
        for m in matches {
            let df = self.posting[m.term_id as usize].len();
            if df == 0 {
                continue;
            }
            let w = self.idf(df) * self.schema.typo_penalty.powi(m.distance as i32);
            out.push(QueryTerm {
                term_id: m.term_id,
                group: gi,
                distance: m.distance,
                weight: w,
                // Upper bound, read not computed — see `Index::max_sat`.
                max_score: w * self.max_sat[m.term_id as usize],
            });
        }
    }

    /// Split an unknown token into two known dictionary terms, if exactly that is possible.
    ///
    /// Prefers the split whose two halves are the *rarest* pair, because a split into two common
    /// terms is far more likely to be an accident (`therapist` -> `the` + `rapist`) than a genuine
    /// compound.
    fn split_compound(&self, token: &str) -> Option<(String, String)> {
        let ch: Vec<char> = token.chars().collect();
        let mut best: Option<(usize, String, String)> = None;
        for cut in 2..ch.len().saturating_sub(1) {
            let a: String = ch[..cut].iter().collect();
            let b: String = ch[cut..].iter().collect();
            let (Some(ia), Some(ib)) = (self.dict.exact(&a), self.dict.exact(&b)) else {
                continue;
            };
            let cost = self.posting[ia as usize].len() + self.posting[ib as usize].len();
            if best.as_ref().map_or(true, |(c, _, _)| cost < *c) {
                best = Some((cost, a, b));
            }
        }
        best.map(|(_, a, b)| (a, b))
    }

    /// Search, returning at most `k` hits.
    ///
    /// **Ranking is two-stage, and the split is deliberate.** Retrieval runs MaxScore over a
    /// typo-penalized BM25F score, which is what makes pruning sound — a bound has to be a bound.
    /// The strict *typo bucket* ("a document matching with 0 typos always ranks above one matching
    /// with 1 typo", Meilisearch) is then applied to the candidate set. Folding the bucket into the
    /// score instead would break MaxScore's upper bounds; filtering on it would discard recall.
    /// So the candidate pool is deliberately over-fetched before the bucket sort is applied.
    pub fn search(&self, query: &str, k: usize) -> Vec<Hit> {
        self.search_opt(query, k, false)
    }

    /// [`Index::search`] with typeahead semantics on the last token.
    pub fn search_prefix(&self, query: &str, k: usize) -> Vec<Hit> {
        self.search_opt(query, k, true)
    }

    fn search_opt(&self, query: &str, k: usize, prefix_last: bool) -> Vec<Hit> {
        if k == 0 {
            return Vec::new();
        }
        let (mut term, group_is_quantity, expanded) = self.plan(query, prefix_last);
        if term.is_empty() {
            return Vec::new();
        }

        // The anchor set: every term id the query's FIRST group expanded to, typo variants
        // included, so a misspelled first word still anchors.
        //
        // Only for a typeahead. A full query is not necessarily typed from the start of a name --
        // "carmen" should find "DEL CARMEN" without penalty -- whereas a per-keystroke prefix
        // almost always is. Empty when the query is a single group, because then the prefix term
        // IS the whole query and anchoring adds nothing that scoring does not already say.
        let anchor: Vec<u32> = match prefix_last && group_is_quantity.len() > 1 {
            true => term.iter().filter(|t| t.group == 0).map(|t| t.term_id).collect(),
            false => Vec::new(),
        };
        // Over-fetch so the typo-bucket sort has material to reorder.
        //
        // This number is a direct latency/recall lever and is deliberately modest. The pruning
        // threshold is the *pool's* worst score, not the top-k's, so every extra pool slot lowers
        // the threshold and weakens block skipping. presyo's own SQL uses
        // `candidate_limit = max(100, limit*5)`; measured here, 3x/32 retains identical recall on
        // both production corpora (`real-corpus` stays at 100 %) while cutting tail latency at a
        // million documents. Raise it only with a measurement.
        // When a learned expansion fires, the pool must grow with it.
        //
        // The pool is ordered by SCORE; the final ranking is ordered by `typo_bucket` FIRST. A
        // document with a perfect bucket but a modest score can therefore be evicted before the
        // bucket sort ever sees it — and expansion makes that far more likely, because it adds many
        // terms whose matches are scoring competitors that were not there before.
        //
        // Measured on profstopick's 2,253 course titles (`bench/roadmap/p20-profstopick-dept.md`):
        // with expansion firing, precision@10 read 88.3 % at the default pool and climbed
        // monotonically as the pool grew — 92.2 %, 95.3 %, 97.2 %, then **100.0 %**. The documents
        // were always ranked correctly; they were being thrown away before the sort.
        //
        // Tied to expansion firing rather than raised globally: `real-corpus` and `sisia-catalog`
        // never expand, and a wider pool would cost them tail latency for nothing.
        let pool = match expanded {
            true => (k * 24).max(256),
            false => (k * 3).max(32),
        };
        let group_count = group_is_quantity.len().max(1);

        // MaxScore requires terms ordered by ascending maximum contribution.
        term.sort_by(|a, b| a.max_score.partial_cmp(&b.max_score).unwrap_or(std::cmp::Ordering::Equal));
        let mut prefix_sum = vec![0.0f32; term.len() + 1];
        for i in 0..term.len() {
            prefix_sum[i + 1] = prefix_sum[i] + term[i].max_score;
        }

        let mut cursor: Vec<usize> = vec![0; term.len()];
        let mut heap: std::collections::BinaryHeap<Candidate> =
            std::collections::BinaryHeap::with_capacity(pool + 1);
        let mut threshold = f32::NEG_INFINITY;
        // Per-group best distance for the document currently being scored.
        let mut group_dist = vec![u8::MAX; group_count];

        // ---- Champion seeding ---------------------------------------------------------------
        // Prime the heap from precomputed high-scoring documents so the threshold is already high
        // when the scan begins, which is what lets block-max skipping engage on a query with no
        // discriminative term. Purely an optimization: every seeded document is real and scored
        // exactly as the main loop would score it, and the main loop declines to insert it twice.
        //
        // **Seed from ONE term — the most discriminative that has a champion list.** Seeding from
        // every term was measured at 7.44 ms p99 against 6.02 ms without seeding at all: it scored
        // up to `terms x CHAMPION_SIZE` candidates, each with a binary search per term, and
        // deduplicated them with a linear `contains` over a growing vector. The work to raise the
        // threshold has to stay small relative to the scan it saves, and one term's champions are
        // enough to raise it — for the query this exists for (`"School ame"`, one term carrying
        // 428,654 postings) that term *is* the query.
        let mut seeded: Vec<u32> = Vec::new();
        if let Some(best) = term
            .iter()
            .filter(|t| !self.champion[t.term_id as usize].is_empty())
            .max_by(|a, b| a.max_score.total_cmp(&b.max_score))
        {
            let list = &self.posting[best.term_id as usize];
            for &at in self.champion[best.term_id as usize].iter().take(pool) {
                let doc = list[at as usize].doc;
                let Some(hit) =
                    self.score_doc(doc, &term, &group_is_quantity, &mut group_dist, &anchor)
                else {
                    continue;
                };
                // Champions of a single term are distinct documents, so no dedup is needed.
                seeded.push(doc);
                if heap.len() < pool {
                    heap.push(Candidate { score: hit.score, doc, bucket: hit.typo_bucket });
                    if heap.len() == pool {
                        threshold = heap.peek().map_or(f32::NEG_INFINITY, |c| c.score);
                    }
                } else if hit.score > threshold {
                    heap.pop();
                    heap.push(Candidate { score: hit.score, doc, bucket: hit.typo_bucket });
                    threshold = heap.peek().map_or(f32::NEG_INFINITY, |c| c.score);
                }
            }
        }
        seeded.sort_unstable();

        loop {
            // The non-essential prefix is the longest run of terms whose combined maximum score
            // cannot, on its own, reach the current threshold.
            let mut first_essential = 0usize;
            if heap.len() >= pool {
                while first_essential < term.len() && prefix_sum[first_essential + 1] <= threshold {
                    first_essential += 1;
                }
                if first_essential == term.len() {
                    break; // nothing left can qualify
                }
            }

            // Next candidate = smallest doc id across essential lists.
            let mut candidate = u32::MAX;
            for i in first_essential..term.len() {
                let list = &self.posting[term[i].term_id as usize];
                if let Some(p) = list.get(cursor[i]) {
                    if p.doc < candidate {
                        candidate = p.doc;
                    }
                }
            }
            if candidate == u32::MAX {
                break;
            }

            // ---- Block-max pruning -------------------------------------------------------
            // Bound the best score ANY document in `[candidate, next)` could achieve, where
            // `next` is the earliest block boundary among the essential lists. Inside that range
            // every essential list stays within its current block, so its contribution cannot
            // exceed that block's maximum; the non-essential lists are bounded by `prefix_sum`.
            // If even that optimistic total cannot beat the threshold, the entire range is
            // skipped in one move instead of one posting at a time. This is the difference
            // between latency that tracks corpus size and latency that does not.
            if heap.len() >= pool {
                let mut range_end = u32::MAX;
                let mut range_bound = prefix_sum[first_essential];
                for i in first_essential..term.len() {
                    let tid = term[i].term_id as usize;
                    if cursor[i] >= self.posting[tid].len() {
                        continue;
                    }
                    let blk = cursor[i] / BLOCK;
                    range_bound += term[i].weight * self.block_max[tid][blk];
                    range_end = range_end.min(self.block_last[tid][blk].saturating_add(1));
                }
                if range_bound <= threshold {
                    debug_assert!(range_end > candidate, "block skip must make progress");
                    for i in first_essential..term.len() {
                        let tid = term[i].term_id;
                        self.seek(tid, &mut cursor[i], range_end);
                    }
                    continue;
                }
            }

            // Score the candidate over the essential lists, advancing their cursors.
            let mut score = 0.0f64;
            group_dist.iter_mut().for_each(|d| *d = u8::MAX);
            for i in first_essential..term.len() {
                let list = &self.posting[term[i].term_id as usize];
                if let Some(p) = list.get(cursor[i]) {
                    if p.doc == candidate {
                        score += (term[i].weight * p.sat) as f64;
                        let g = term[i].group as usize;
                        group_dist[g] = group_dist[g].min(term[i].distance);
                        cursor[i] += 1;
                    }
                }
            }

            // Walk the non-essential lists in reverse, bailing as soon as even the optimistic
            // remainder cannot lift this document over the threshold.
            for i in (0..first_essential).rev() {
                if score as f32 + prefix_sum[i + 1] <= threshold {
                    break;
                }
                // Cursors on non-essential lists lag; seek forward to the candidate, skipping
                // whole blocks rather than stepping posting by posting.
                self.seek(term[i].term_id, &mut cursor[i], candidate);
                let list = &self.posting[term[i].term_id as usize];
                let c = cursor[i];
                if let Some(p) = list.get(c) {
                    if p.doc == candidate {
                        score += (term[i].weight * p.sat) as f64;
                        let g = term[i].group as usize;
                        group_dist[g] = group_dist[g].min(term[i].distance);
                    }
                }
            }
            // Applied once, at the end. The mid-loop bail above compares the UNPRIMED running
            // score against the threshold, which is conservative in the right direction: the prior
            // can only shrink the score, so bailing on the unprimed value bails no earlier than it
            // should and never discards a document it should have kept.
            let score = score as f32
                * self.prior_of(candidate)
                * self.anchor_factor(candidate, &anchor);

            let bucket = Self::bucket_of(&group_dist, &group_is_quantity);

            // A champion-seeded document is already in the heap; re-scoring it is harmless but
            // re-inserting it would duplicate a result.
            if !seeded.is_empty() && seeded.binary_search(&candidate).is_ok() {
                continue;
            }

            // Deleted documents are dropped at INSERTION, not at candidate selection: the candidate
            // step is followed by cursor advances that the loop depends on, and skipping it early
            // would either desynchronize them or require duplicating the advance. Scoring a deleted
            // document and discarding it is a little wasted work in exchange for one obviously
            // correct place to filter.
            if self.is_deleted(candidate) {
                continue;
            }
            if heap.len() < pool {
                heap.push(Candidate { score, doc: candidate, bucket });
                if heap.len() == pool {
                    threshold = heap.peek().map_or(f32::NEG_INFINITY, |c| c.score);
                }
            } else if score > threshold {
                // O(log pool): pop the worst, push the new one, read the new worst.
                heap.pop();
                heap.push(Candidate { score, doc: candidate, bucket });
                threshold = heap.peek().map_or(f32::NEG_INFINITY, |c| c.score);
            }
        }

        let mut heap: Vec<Hit> = heap
            .into_iter()
            .map(|c| Hit { doc: c.doc, score: c.score, typo_bucket: c.bucket })
            .collect();

        // Stage two: strict typo bucket first, then score, then doc id for a deterministic
        // tiebreak (presyo pins exactly this property in its contract test).
        heap.sort_by(rank_cmp);
        heap.truncate(k);
        heap
    }

    /// Diagnostic: run a query and report how much work retrieval actually did.
    ///
    /// Returns `(hits, query_terms, postings_scored)`. Exists because three separate optimization
    /// attempts were made on hypotheses about where time went, and only measurement settled it.
    pub fn search_stat(&self, query: &str, k: usize) -> (Vec<Hit>, usize, u64) {
        let (term, _, _) = self.plan(query, false);
        let total: u64 = term.iter().map(|t| self.posting[t.term_id as usize].len() as u64).sum();
        (self.search(query, k), term.len(), total)
    }

    /// Exhaustive OR scoring — **the same algorithm with the pruning removed.**
    ///
    /// This is the oracle for [`Index::search`], and it mirrors its candidate-pool semantics
    /// deliberately: score every document, keep the best `pool` by score, then apply the typo-bucket
    /// sort. That isolates the claim an oracle should test — *block-max pruning does not change the
    /// answer* — from a different claim that pruning has nothing to do with: *the pool is large
    /// enough that the bucket sort is not truncated*. The second is a recall question, and it is
    /// answered by `bench/roadmap/p6-real-corpus.md` against production data, not by an assertion.
    ///
    /// Conflating the two hides a real tradeoff: shrinking the pool tightens the pruning threshold
    /// and cuts tail latency, but a document with a better typo bucket and a lower score can fall
    /// out of the pool before the bucket sort ever sees it.
    pub fn search_exhaustive(&self, query: &str, k: usize) -> Vec<Hit> {
        let (term, group_is_quantity, _) = self.plan(query, false);
        let group_count = group_is_quantity.len().max(1);
        let mut acc: BTreeMap<u32, (f64, Vec<u8>)> = BTreeMap::new();
        for t in &term {
            for p in &self.posting[t.term_id as usize] {
                let e = acc.entry(p.doc).or_insert_with(|| (0.0, vec![u8::MAX; group_count]));
                e.0 += (t.weight * p.sat) as f64;
                let g = t.group as usize;
                e.1[g] = e.1[g].min(t.distance);
            }
        }
        let mut hit: Vec<Hit> = acc
            .into_iter()
            .map(|(doc, (score, gd))| Hit {
                doc,
                score: score as f32,
                typo_bucket: Self::bucket_of(&gd, &group_is_quantity),
            })
            .collect();
        // Mirror `search`'s pool semantics exactly: best `pool` by score, then bucket sort.
        let pool = (k * 3).max(32);
        hit.sort_by(|a, b| {
            let scale = a.score.abs().max(b.score.abs()).max(1.0);
            if (a.score - b.score).abs() <= SCORE_TIE_REL * scale {
                a.doc.cmp(&b.doc)
            } else {
                b.score.total_cmp(&a.score)
            }
        });
        hit.truncate(pool);
        hit.sort_by(rank_cmp);
        hit.truncate(k);
        hit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grocery() -> Index {
        let schema = Schema::new(vec![
            Field::new("brand", 3.0, 0.4),
            Field::new("title", 2.0, 0.4),
            Field::new("category", 1.0, 0.75),
        ]);
        let mut b = IndexBuilder::new(schema).with_alias(AliasTable::philippine_grocery());
        for (brand, title, cat) in [
            ("Coca Cola", "Coca Cola Regular 1.5L", "Inumin"),
            ("Coca Cola", "Coca Cola Zero Sugar 500ml", "Inumin"),
            ("Pepsi", "Pepsi Regular 1.5L", "Inumin"),
            ("Bear Brand", "Bear Brand Powdered Milk 300g", "Gatas"),
            ("Bear Brand", "Bear Brand Powdered Milk 900g", "Gatas"),
            ("Colgate", "Colgate Total Charcoal Deep Clean 80g", "Toothpaste"),
            ("Nescafe", "Nescafe Classic Reseal 200g", "Kape"),
            ("Lucky Me", "Lucky Me Pancit Canton Chilimansi 60g", "Instant Noodles"),
        ] {
            b.add(&Doc::new([brand, title, cat]));
        }
        b.build().unwrap()
    }

    #[test]
    fn exact_query_ranks_its_document_first() {
        let ix = grocery();
        let h = ix.search("Nescafe Classic Reseal 200g", 5);
        assert!(!h.is_empty());
        assert_eq!(h[0].doc, 6, "the exact SKU must rank first, got {h:?}");
        assert_eq!(h[0].typo_bucket, 0);
    }

    /// The property the whole engine exists for, on the query shape presyo's fixture calls `typo`.
    #[test]
    fn typo_query_still_finds_the_document() {
        let ix = grocery();
        for (q, want) in [("colgaye", 5), ("nescaffe", 6), ("coka cola", 0)] {
            let h = ix.search(q, 5);
            assert!(!h.is_empty(), "query {q:?} returned nothing");
            assert_eq!(h[0].doc, want, "query {q:?} -> {h:?}");
        }
    }

    /// Meilisearch's rule, asserted: a clean match outranks a typo'd one.
    #[test]
    fn zero_typo_documents_outrank_typo_documents() {
        let ix = grocery();
        let h = ix.search("pepsi", 8);
        assert_eq!(h[0].typo_bucket, 0);
        assert!(
            h.windows(2).all(|w| w[0].typo_bucket <= w[1].typo_bucket),
            "typo bucket must be non-decreasing down the result list: {h:?}"
        );
    }

    /// The correctness guard, end to end at the query layer.
    #[test]
    fn a_size_query_never_returns_a_different_size() {
        let ix = grocery();
        let h = ix.search("Bear Brand Powdered Milk 300g", 2);
        assert_eq!(h[0].doc, 3, "300g SKU must win over the 900g SKU: {h:?}");
        // And the 900g doc must score strictly lower, not merely tie.
        assert!(h.len() < 2 || h[0].score > h[1].score);
    }

    /// The only thing that makes dynamic pruning trustworthy.
    #[test]
    fn maxscore_agrees_with_exhaustive_or() {
        let ix = grocery();
        for q in [
            "coca cola",
            "bear brand milk",
            "colgate total charcoal",
            "1500ml",
            "gatas",
            "pancit canton chilimansi 60g",
            "nescaffe classic",
        ] {
            let fast = ix.search(q, 5);
            let slow = ix.search_exhaustive(q, 5);
            assert_eq!(
                fast.iter().map(|h| h.doc).collect::<Vec<_>>(),
                slow.iter().map(|h| h.doc).collect::<Vec<_>>(),
                "MaxScore and exhaustive OR disagree on {q:?}\n fast={fast:?}\n slow={slow:?}"
            );
            for (a, b) in fast.iter().zip(slow.iter()) {
                assert!(
                    (a.score - b.score).abs() < 1e-4,
                    "score mismatch on {q:?}: {a:?} vs {b:?}"
                );
            }
        }
    }

    /// Filipino query terms resolve through the alias table without an LLM in the hot path.
    #[test]
    fn filipino_category_queries_resolve() {
        let ix = grocery();
        let h = ix.search("gatas", 5);
        assert!(!h.is_empty(), "`gatas` must reach the milk documents");
        assert!(h.iter().any(|x| x.doc == 3 || x.doc == 4), "{h:?}");

        let h = ix.search("kape", 5);
        assert!(h.iter().any(|x| x.doc == 6), "`kape` must reach Nescafe: {h:?}");
    }

    /// The `joined` category in presyo's fixture: users type brand names with no space.
    #[test]
    fn prefix_search_supports_typeahead() {
        let ix = grocery();
        let h = ix.search_prefix("nesc", 5);
        assert!(h.iter().any(|x| x.doc == 6), "typeahead on a partial brand: {h:?}");
    }

    #[test]
    fn field_boost_changes_ranking() {
        // Same corpus, but with the brand field un-boosted, the ranking must be allowed to differ.
        let mut b = IndexBuilder::new(Schema::new(vec![
            Field::new("brand", 10.0, 0.4),
            Field::new("title", 1.0, 0.4),
        ]));
        b.add(&Doc::new(["Pepsi", "Coca Cola competitor drink"]));
        b.add(&Doc::new(["Generic", "Pepsi mentioned in the title only"]));
        let ix = b.build().unwrap();
        let h = ix.search("pepsi", 2);
        assert_eq!(h[0].doc, 0, "a boosted brand-field match must outrank a title-only one: {h:?}");
    }

    /// Exact field lengths are the reason this scorer exists — prove they are not quantized away.
    #[test]
    fn document_length_affects_score() {
        let mut b = IndexBuilder::new(Schema::new(vec![Field::new("title", 1.0, 0.9)]));
        b.add(&Doc::new(["colgate"]));
        b.add(&Doc::new(["colgate with many many many other additional trailing words here"]));
        let ix = b.build().unwrap();
        let h = ix.search("colgate", 2);
        assert_eq!(h.len(), 2);
        assert!(
            h[0].doc == 0 && h[0].score > h[1].score,
            "the shorter document must score higher — this is what a 1-byte fieldnorm destroys: {h:?}"
        );
    }

    #[test]
    fn empty_and_nonsense_queries_are_safe() {
        let ix = grocery();
        assert!(ix.search("", 5).is_empty());
        assert!(ix.search("qzxwv unmatched", 5).is_empty());
        assert!(ix.search("nonexistentbrand987654", 5).is_empty());
        assert!(ix.search("coca", 0).is_empty());
    }

    /// presyo's fixture carries adversarial rows; they must not panic or match everything.
    #[test]
    fn adversarial_queries_do_not_panic() {
        let ix = grocery();
        let _ = ix.search("% OR 1=1 --", 5);
        let _ = ix.search("coca cola coca cola coca cola coca cola coca cola", 5);
        let _ = ix.search(&"a".repeat(500), 5);
        let _ = ix.search("🛒🛒🛒", 5);
    }

    /// The defect real data caught: a document that matches a query token **with a typo** must
    /// outrank a document that does not match that token at all.
    #[test]
    fn matching_a_word_with_a_typo_beats_not_matching_it() {
        let mut b = IndexBuilder::new(Schema::new(vec![Field::new("title", 1.0, 0.4)]));
        // 0: has every query token, one of them only reachable via a typo.
        b.add(&Doc::new(["jergens lotion soothing aloe"]));
        // 1..: share every *other* token but have no `lotion` at all.
        b.add(&Doc::new(["jergens soothing aloe body wash"]));
        b.add(&Doc::new(["jergens soothing aloe shower gel"]));
        b.add(&Doc::new(["jergens soothing aloe hand cream"]));
        let ix = b.build().unwrap();

        // `loion` is one deletion from `lotion`.
        let h = ix.search("jergens loion soothing aloe", 4);
        assert_eq!(
            h[0].doc, 0,
            "the document that actually contains the typo'd word must win: {h:?}"
        );
        assert!(
            h[0].typo_bucket < h[1].typo_bucket,
            "an unmatched token must cost more than a 1-edit match: {h:?}"
        );
    }

    /// The defect real data caught: a stated size must outrank word overlap.
    #[test]
    fn a_stated_size_beats_extra_word_overlap() {
        let mut b = IndexBuilder::new(Schema::new(vec![Field::new("title", 1.0, 0.4)]));
        // 0: the right size, but missing two of the query's words.
        b.add(&Doc::new(["purefoods honeycured bacon 500g"]));
        // 1: every word, wrong size.
        b.add(&Doc::new(["purefoods honeycured bacon roll pack 250g"]));
        let ix = b.build().unwrap();
        let h = ix.search("purefoods honeycured bacon roll pack 500g", 2);
        assert_eq!(
            h[0].doc, 0,
            "the correct size must win even with less word overlap: {h:?}"
        );
    }

    /// ...but when no document carries the requested size, the engine must still answer, and the
    /// ordering among the remaining candidates must be unaffected by the uniform penalty.
    #[test]
    fn an_unavailable_size_does_not_suppress_results() {
        let mut b = IndexBuilder::new(Schema::new(vec![Field::new("title", 1.0, 0.4)]));
        b.add(&Doc::new(["purefoods honeycured bacon roll pack 250g"]));
        b.add(&Doc::new(["purefoods honeycured bacon 1000g"]));
        let ix = b.build().unwrap();
        let h = ix.search("purefoods honeycured bacon roll pack 750g", 2);
        assert_eq!(h.len(), 2, "no size matches, but the query must still be answered: {h:?}");
        // Both documents pay the same quantity penalty, so it cancels and word overlap decides.
        // Document 0 carries all five query words; document 1 is missing `roll` and `pack`.
        assert_eq!(h[0].doc, 0, "with size unavailable, word overlap decides: {h:?}");
        assert_eq!(
            h[1].typo_bucket - h[0].typo_bucket,
            2 * super::MISSING_TERM_PENALTY,
            "the difference must be exactly the two missing words, not the size: {h:?}"
        );
    }

    /// **The correctness oracle for block-max pruning.**
    ///
    /// `maxscore_agrees_with_exhaustive_or` runs on eight documents, where the threshold never
    /// rises enough for a block to be skipped — so it cannot catch a pruning bug. This builds a
    /// corpus large enough to span many blocks, with a deliberately Zipf-ish vocabulary so common
    /// terms produce long posting lists and rare ones produce short, then asserts that pruned and
    /// exhaustive retrieval return **identical** results on hundreds of generated queries.
    #[test]
    fn block_max_pruning_agrees_with_exhaustive_or_at_scale() {
        fn mix(state: &mut u64) -> u64 {
            *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = *state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        let mut st = 0x5EED_1234u64;
        // Zipf-ish: low ids are drawn far more often, so their posting lists span many blocks.
        let word = |st: &mut u64| -> String {
            let r = (mix(st) % 1000) as f64 / 1000.0;
            let id = (r * r * r * 400.0) as usize;
            format!("w{id}")
        };

        let mut b = IndexBuilder::new(Schema::new(vec![
            Field::new("title", 2.0, 0.4),
            Field::new("body", 1.0, 0.6),
        ]));
        // 5,000 documents = ~40 blocks on the most common term at BLOCK = 128.
        for _ in 0..5_000 {
            let title: Vec<String> = (0..4).map(|_| word(&mut st)).collect();
            let body: Vec<String> = (0..12).map(|_| word(&mut st)).collect();
            b.add(&Doc::new([title.join(" "), body.join(" ")]));
        }
        let ix = b.build().unwrap();
        assert!(ix.doc_count() == 5_000);

        let mut checked_with_pruning = 0usize;
        for _ in 0..300 {
            let n = 1 + (mix(&mut st) % 4) as usize;
            let q: Vec<String> = (0..n).map(|_| word(&mut st)).collect();
            let q = q.join(" ");
            let fast = ix.search(&q, 10);
            let slow = ix.search_exhaustive(&q, 10);
            assert_eq!(
                fast.iter().map(|h| (h.doc, h.typo_bucket)).collect::<Vec<_>>(),
                slow.iter().map(|h| (h.doc, h.typo_bucket)).collect::<Vec<_>>(),
                "pruned and exhaustive disagree on {q:?}"
            );
            for (a, c) in fast.iter().zip(slow.iter()) {
                assert!((a.score - c.score).abs() < 1e-3, "score mismatch on {q:?}: {a:?} {c:?}");
            }
            if !fast.is_empty() {
                checked_with_pruning += 1;
            }
        }
        assert!(
            checked_with_pruning > 200,
            "expected most queries to return hits; only {checked_with_pruning} did"
        );
    }

    /// The `joined` category: users type brand and course names with no space at all.
    /// presyo's frozen fixture has seven of these; profstopick's contract test has one.
    #[test]
    fn a_token_typed_without_spaces_still_matches() {
        let ix = grocery();
        for (q, want) in [("cocacola", 0), ("bearbrand", 3), ("luckyme", 7)] {
            let h = ix.search(q, 3);
            assert!(!h.is_empty(), "joined query {q:?} returned nothing");
            assert_eq!(h[0].doc, want, "joined query {q:?} -> {h:?}");
        }
    }

    /// A separator between digits is normalized, so a code reaches its document however it is
    /// punctuated. profstopick requires `MATH 30.23`, `math30.23` and `MATH  30-23` to agree.
    #[test]
    fn a_numeric_code_matches_however_it_is_punctuated() {
        let mut b = IndexBuilder::new(Schema::new(vec![Field::new("code", 1.0, 0.4)]));
        b.add(&Doc::new(["MATH 30.23 Probability"]));
        b.add(&Doc::new(["MATH 10 Algebra"]));
        let ix = b.build().unwrap();
        for q in ["math 30.23", "math30.23", "MATH  30-23", "math 30-23"] {
            let h = ix.search(q, 2);
            assert!(!h.is_empty(), "{q:?} returned nothing");
            assert_eq!(h[0].doc, 0, "{q:?} -> {h:?}");
        }
    }

    /// Compound splitting must not invent matches out of two common words.
    #[test]
    fn compound_splitting_does_not_fire_on_unknown_tokens() {
        let ix = grocery();
        assert!(ix.search("zzzzqqqq", 3).is_empty(), "an unsplittable unknown must stay unknown");
    }

    #[test]
    fn results_are_deterministic() {
        let ix = grocery();
        let a = ix.search("coca cola", 5);
        let b = ix.search("coca cola", 5);
        assert_eq!(a, b, "same input must give the same verdict every run");
    }

    #[test]
    fn a_prior_cannot_outrank_an_exact_match() {
        // The safety property. A prior expresses IMPORTANCE, not correctness: an important
        // document matched via a typo must still lose to an exact match on an unimportant one,
        // because `typo_bucket` is the primary sort key and the prior only scales the score.
        // Without this, a big static boost would let the engine confidently return the wrong word.
        let schema = Schema::new(vec![Field::new("name", 1.0, 0.5)]);
        let mut b = IndexBuilder::new(schema);
        b.add_with_prior(&Doc::new(["quezon"]), 1000.0); // very important, will match with a typo
        b.add_with_prior(&Doc::new(["quezan"]), 1.0); // unimportant, matches exactly
        let ix = b.build().unwrap();

        let h = ix.search("quezan", 5);
        assert_eq!(h[0].doc, 1, "the exact match must win regardless of the prior");
        assert_eq!(h[0].typo_bucket, 0);
        // Only the exact match may come back at all: typo expansion is LAZY, so with an exact hit
        // present the fuzzy alternative is never fired. That is the engine being right, and the
        // first version of this test asserted `h[1]` existed and failed for that reason -- the
        // property under test is "the prior did not promote the typo", not "a typo hit exists".
        for x in h.iter().skip(1) {
            assert!(x.typo_bucket > 0, "an exact match cannot rank below a typo match");
        }
    }

    #[test]
    fn a_uniform_prior_is_the_same_as_no_prior() {
        let mk = |p: Option<f32>| {
            let schema = Schema::new(vec![Field::new("name", 1.0, 0.5)]);
            let mut b = IndexBuilder::new(schema);
            for t in ["cebu city", "cebu", "mandaue cebu"] {
                match p {
                    Some(v) => b.add_with_prior(&Doc::new([t]), v),
                    None => b.add(&Doc::new([t])),
                };
            }
            b.build().unwrap()
        };
        let a = mk(None);
        let c = mk(Some(7.0));
        assert!(!c.has_prior(), "a uniform prior carries no information");
        let ra: Vec<u32> = a.search("cebu", 3).iter().map(|h| h.doc).collect();
        let rc: Vec<u32> = c.search("cebu", 3).iter().map(|h| h.doc).collect();
        assert_eq!(ra, rc);
    }


    /// A miniature of the presyo failure: a category whose members share no word with its name.
    fn baking_corpus() -> IndexBuilder {
        let schema = Schema::new(vec![
            Field::new("name", 3.0, 0.5),
            Field::new("category", 1.0, 0.75),
        ]);
        let mut b = IndexBuilder::new(schema).learn_expansion(1, 8);
        // Members of "baking needs" -- not one contains the words "baking" or "needs".
        for n in [
            "camote powder 500g",
            "mung beans small dice 100g",
            "sago tapioca 200g",
            "cream of tartar 50g",
            "hotcake mix fluffy 350g",
            "tapioca pearl large 500g",
            "powder sugar fine 1kg",
            "tapioca starch 250g",
        ] {
            b.add(&Doc::new([n, "baking needs"]));
        }
        // A distractor category that DOES contain the word, to make the query ambiguous.
        for n in ["baking tray steel", "baking sheet paper", "needs assessment binder"] {
            b.add(&Doc::new([n, "kitchen tools"]));
        }
        b
    }

    #[test]
    fn learned_expansion_finds_members_that_share_no_word_with_the_query() {
        let plain = {
            let schema = Schema::new(vec![
                Field::new("name", 3.0, 0.5),
                Field::new("category", 1.0, 0.75),
            ]);
            let mut b = IndexBuilder::new(schema);
            for n in ["camote powder 500g", "sago tapioca 200g", "baking tray steel"] {
                b.add(&Doc::new([n, "x"]));
            }
            b.build().unwrap()
        };
        // Without expansion, "baking needs" cannot reach a product named "camote powder".
        let h = plain.search("baking needs", 10);
        assert!(
            h.iter().all(|x| x.doc != 0),
            "control: a lexical index must NOT reach the camote powder"
        );

        let ix = baking_corpus().build().unwrap();
        assert!(ix.expansion_count() > 0, "expansion should have been learned");
        assert!(!ix.expansion_of("baking needs").is_empty());

        let hit = ix.search("baking needs", 5);
        assert!(!hit.is_empty());
        // At least one true member -- documents 0..8 -- must surface.
        assert!(
            hit.iter().any(|h| (h.doc as usize) < 8),
            "expansion must surface a category member, got {:?}",
            hit.iter().map(|h| h.doc).collect::<Vec<_>>()
        );
    }

    #[test]
    fn expansion_fires_only_on_an_exact_facet_value() {
        // The strict trigger. "baking tray" CONTAINS a facet word but is not the facet, so it must
        // behave exactly as if no expansion existed -- this is the rule that kept the loose variant
        // from costing 2.0 points of exact-product hit@1 in p17.
        let ix = baking_corpus().build().unwrap();
        assert!(ix.expansion_of("baking tray").is_empty());
        assert!(ix.expansion_of("baking").is_empty());
        assert!(ix.expansion_of("needs").is_empty());

        // An exact product query still finds its exact product first.
        let h = ix.search("baking tray steel", 5);
        assert_eq!(h[0].doc, 8, "an ordinary product query must be unaffected");
    }

    #[test]
    fn expansion_is_absent_unless_it_is_asked_for() {
        let schema = Schema::new(vec![Field::new("name", 1.0, 0.5), Field::new("cat", 1.0, 0.75)]);
        let mut b = IndexBuilder::new(schema);
        for n in ["camote powder", "sago tapioca"] {
            b.add(&Doc::new([n, "baking needs"]));
        }
        let ix = b.build().unwrap();
        assert_eq!(ix.expansion_count(), 0, "opt-in only: no learn_expansion, no table");
    }

    #[test]
    fn a_learned_value_does_not_expand_with_its_own_words() {
        // "baking" and "needs" are already in the query; re-adding them would be noise and would
        // double-count the very tokens the strict trigger matched on.
        let ix = baking_corpus().build().unwrap();
        let ids = ix.expansion_of("baking needs");
        let own: Vec<u32> = ["baking", "needs"]
            .iter()
            .filter_map(|t| ix.dict.exact(t))
            .collect();
        for o in own {
            assert!(!ids.contains(&o), "expansion must not contain the value's own words");
        }
    }

}
