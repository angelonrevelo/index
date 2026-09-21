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

use crate::analyze::{apply_alias, tokenize, tokenize_into, AliasTable, Token};
use crate::dict::TermDict;
use crate::reorder;
use std::collections::BTreeMap;

/// Smallest amount of work — postings, documents or terms, whichever the caller is iterating —
/// that justifies starting threads at all.
///
/// **Measured on this machine, not guessed.** `std::thread::scope` costs ~87 us per thread here
/// (2 threads 175 us, 16 threads 922 us, best of twenty), and `build()` has six parallel regions,
/// so a build that spawns unconditionally pays ~5 ms before it computes anything. The cheapest
/// unit of work in those regions — one posting in the saturation pass — costs ~35 ns serially.
///
/// A region only reaches two threads at `2 * PARALLEL_MIN_WORK` units, where it can save at best
/// half of `2 * 25_000 * 35 ns` = 1.75 ms against 175 us of startup: a 5x margin at the *trigger*,
/// on the *cheapest* region, which is where a threshold has to be safe. Below it the serial loop
/// runs, so the 60-document indexes the unit tests build - three orders of magnitude under the
/// trigger on both documents and postings — never leave the calling thread.
const PARALLEL_MIN_WORK: usize = 25_000;

/// Chunks handed out per thread. More than one because the work per index is wildly uneven — one
/// term can carry 400 000 postings while its neighbours carry three — so a static split by index
/// leaves one thread finishing long after the rest. Chunks are claimed from a queue, which costs
/// one uncontended lock per chunk and removes that tail.
const PARALLEL_CHUNK_PER_THREAD: usize = 8;

/// How many threads a region of `work` units should use: one below the threshold, and above it one
/// thread per [`PARALLEL_MIN_WORK`] units, up to the machine's parallelism.
///
/// Returning 1 is the whole safety story for `wasm32`, where `available_parallelism` reports
/// `Unsupported` and `thread::spawn` cannot run: a single-threaded plan never reaches a spawn.
fn parallel_thread(work: usize) -> usize {
    // Test-only: lets one test build the same corpus both ways and compare the bytes. Thread-local
    // rather than global, so it cannot leak into another test running concurrently — the worker
    // threads never read it, only the thread planning the region does.
    #[cfg(test)]
    if FORCE_SERIAL.with(std::cell::Cell::get) {
        return 1;
    }
    if work < 2 * PARALLEL_MIN_WORK {
        return 1;
    }
    let cpu = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    cpu.min(work / PARALLEL_MIN_WORK).max(1)
}

#[cfg(test)]
thread_local! {
    /// See [`parallel_thread`].
    static FORCE_SERIAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

thread_local! {
    /// Scratch bitmap for dense COUNT OR, reused across queries on this thread so a disjunction
    /// does not allocate `doc_count / 8` bytes every time.
    static COUNT_ACC: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Evaluate `f` at every index in `0..n`, spreading the indices over threads when `work` says it
/// is worth it, and return the results **in index order**.
///
/// The determinism the whole build depends on comes from the shape rather than from discipline:
/// every output slot is written by exactly one `f(i)`, `f` reads only shared immutable state, and
/// nothing is accumulated across slots. A parallel run therefore produces the identical `Vec` a
/// serial one does, bit for bit, whatever order the chunks are claimed in.
fn par_index_map<R, F>(n: usize, work: usize, f: F) -> Vec<R>
where
    R: Send + Default,
    F: Fn(usize) -> R + Sync,
{
    let mut out: Vec<R> = Vec::new();
    out.resize_with(n, R::default);
    par_slice_mut(&mut out, work, |i, slot| *slot = f(i));
    out
}

/// Apply `f` to every element of `out` **in place**, spreading the elements over threads when
/// `work` says it is worth it. `f` receives the element's index and a mutable reference to it.
///
/// Same determinism argument as [`par_index_map`], and the same threshold: each element is visited
/// exactly once by exactly one thread, `f` sees only that element and shared immutable state, so
/// the result does not depend on how the chunks were claimed.
fn par_slice_mut<T, F>(out: &mut [T], work: usize, f: F)
where
    T: Send,
    F: Fn(usize, &mut T) + Sync,
{
    let n = out.len();
    let thread = parallel_thread(work).min(n);
    if thread <= 1 {
        for (i, slot) in out.iter_mut().enumerate() {
            f(i, slot);
        }
        return;
    }
    let chunk = n.div_ceil(thread * PARALLEL_CHUNK_PER_THREAD).max(1);
    let queue: std::sync::Mutex<Vec<(usize, &mut [T])>> =
        std::sync::Mutex::new(out.chunks_mut(chunk).enumerate().collect());
    std::thread::scope(|scope| {
        for _ in 0..thread {
            let queue = &queue;
            let f = &f;
            scope.spawn(move || loop {
                // `into_inner` on poison: `f` runs with the lock released, so a poisoned queue
                // means another worker panicked and the chunks left in it are still sound to
                // claim. The panic itself propagates out of `scope`.
                let next = queue.lock().unwrap_or_else(|e| e.into_inner()).pop();
                let Some((at, part)) = next else { return };
                let base = at * chunk;
                for (i, slot) in part.iter_mut().enumerate() {
                    f(base + i, slot);
                }
            });
        }
    });
}

/// Everything `rebuild_meta` derives for one term, computed from that term's postings alone.
///
/// Grouped into one struct so the whole derivation is a single parallel pass per term rather than
/// four, and so each pass reads the term's postings once instead of once per output.
#[derive(Default)]
struct TermMeta {
    max_sat: f32,
    block_last: Vec<u32>,
    block_max: Vec<f32>,
    champion: Vec<u32>,
}

/// One term's postings as the builder accumulates them: document ids and per-field frequencies in
/// two parallel columns, in ascending document order.
///
/// # Why this is not a map
///
/// It was `BTreeMap<u32, [u16; MAX_FIELD]>` per term, and that inner map was **the single most
/// expensive thing in the engine**: at a million documents it cost 13.2 s of a 24.3 s build — more
/// than tokenizing, and more than everything `build()` does put together. Every new
/// (term, document) pair allocated a node and walked a tree, fifteen million times over.
///
/// Documents are added in ascending id order and a term's postings are needed in exactly that
/// order, so the map was sorting data that arrived sorted. Appending to a `Vec` — and bumping the
/// last entry when the same document mentions the term again, in another field or at another
/// position — produces the identical sequence with no per-posting allocation and no comparisons.
#[derive(Default)]
struct TermPost {
    doc: Vec<Posting>,
    tf: Vec<[u16; MAX_FIELD]>,
}

impl TermPost {
    /// Record one occurrence of the term in `field` of `doc`. `doc` must be >= the last recorded.
    #[inline]
    fn hit(&mut self, doc: u32, field: usize) {
        match self.doc.last() {
            Some(p) if p.doc == doc => {
                let tf = &mut self.tf[self.doc.len() - 1][field];
                *tf = tf.saturating_add(1);
            }
            _ => {
                self.doc.push(Posting { doc, sat: 0.0 });
                let mut tf = [0u16; MAX_FIELD];
                tf[field] = 1;
                self.tf.push(tf);
            }
        }
    }
}

/// Maximum number of **scored** fields. Four covers `{brand, title, category, description}`, the
/// shape `docs/research/relevance.md` recommends, and keeps a posting one cache-friendly struct.
///
/// This is deliberately not the limit on how many columns a document may carry — see
/// [`MAX_COLUMN`] and [`Schema::with_column`]. Raising *this* constant would widen
/// `[u16; MAX_FIELD]` on every posting of every existing index, which
/// `bench/roadmap/p65-field-budget.md` rejects: it taxes four shipping consumers to serve one new
/// corpus.
pub const MAX_FIELD: usize = 4;

/// Maximum number of **columns** a schema may address: scored fields plus unscored columns.
///
/// A column index below [`Schema::field_count`] names a scored field; one at or above it names an
/// unscored column, which is stored, faceted, ranged or keyed on but never tokenized and never
/// scored. An unscored column costs **zero bytes per posting** — it is read straight out of
/// [`Doc::field_text`] at build time and never enters `term_post` or `doc_len`.
///
/// Sixteen, not four, because the thing being bounded here is a `Vec<String>` per schema and a
/// `usize` per declared column, not a per-posting array. `p51`'s image document wants seven.
pub const MAX_COLUMN: usize = 16;

/// Bits a packed position reserves for the token index inside its field.
///
/// A position is `field << POSITION_FIELD_SHIFT | token_index`. Packing the field IN rather than
/// storing it alongside is what makes phrase matching field-aware for free: consecutive positions
/// can only be adjacent if they share a field, so `"Colgate Total"` cannot match a document whose
/// name ends in *Colgate* and whose brand begins with *Total*. `doc_len` is `u16`, so a token index
/// never needs more than sixteen bits.
const POSITION_FIELD_SHIFT: u32 = 16;
/// Mask for the token index of a packed position.
const POSITION_INDEX_MASK: u32 = (1 << POSITION_FIELD_SHIFT) - 1;

/// Pack a field index and a token index into one sortable `u32`.
#[inline]
fn pack_position(field: usize, at: u16) -> u32 {
    ((field as u32) << POSITION_FIELD_SHIFT) | at as u32
}

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
        Field {
            name: name.to_string(),
            boost,
            b,
        }
    }
}

/// The index schema plus global scoring parameters.
#[derive(Clone, Debug)]
pub struct Schema {
    pub field: Vec<Field>,
    /// **Unscored columns**, in declaration order, addressed at column index
    /// `field.len() + i`. See [`Schema::with_column`].
    ///
    /// A name only: an unscored column has no boost and no `b` because nothing ever scores it.
    /// That is the whole point — it is a place to put `width`, `format` or `orientation` without
    /// spending one of the four [`MAX_FIELD`] slots and a `u16` on every posting.
    pub unscored: Vec<String>,
    /// BM25 term-frequency saturation. Anserini's default is 0.9; Lucene's is 1.2.
    pub k1: f32,
    /// Penalty multiplier applied per edit of distance, so a typo'd match scores below a clean one
    /// *within* the retrieval pass. The strict typo *bucket* is applied afterwards — see
    /// [`Index::search`].
    pub typo_penalty: f32,
}

impl Schema {
    pub fn new(field: Vec<Field>) -> Self {
        assert!(
            !field.is_empty() && field.len() <= MAX_FIELD,
            "1..={MAX_FIELD} fields"
        );
        Schema {
            field,
            unscored: Vec::new(),
            k1: 0.9,
            typo_penalty: 0.6,
        }
    }

    /// Declare an **unscored column**: a stored value that can be faceted, ranged or keyed on
    /// without occupying one of the four scored-field slots.
    ///
    /// # Why this exists
    ///
    /// Before this, a facet or a numeric column was declared *by scored-field index*
    /// (`with_facet(field)`), so `Field::new("width", 0.0, 0.75)` — boost 0.0, meaning "never
    /// scored" — still consumed a [`MAX_FIELD`] slot **and** a `u16` field length on every posting
    /// that nobody would ever read. `bench/roadmap/p65-field-budget.md` measured the alternative
    /// and rejected it: widening the per-posting array to eight taxes presyo's 241,677 products,
    /// sisia and profstopick to serve one image corpus.
    ///
    /// An unscored column costs a `String` per schema and whatever the facet/numeric/key store
    /// already costs per document. It costs **nothing per posting**, because it is never tokenized:
    /// [`IndexBuilder::add`] analyzes only the first [`Schema::field_count`] entries of
    /// [`Doc::field_text`], and reads a column straight out of the remainder.
    ///
    /// Positionally, the column is the next entry of [`Doc::field_text`] after the scored fields
    /// and any column declared before it. Returns `self` so declarations chain.
    pub fn with_column(mut self, name: &str) -> Self {
        assert!(self.add_column(name), "at most {MAX_COLUMN} columns");
        self
    }

    /// Non-consuming [`Schema::with_column`], for the C ABI. Returns `false` rather than panicking
    /// when the column budget is spent — a trap kills the whole WASM instance.
    pub fn add_column(&mut self, name: &str) -> bool {
        if self.column_count() >= MAX_COLUMN {
            return false;
        }
        self.unscored.push(name.to_string());
        true
    }

    /// How many **scored** fields. Only these are tokenized, scored and length-normalized.
    pub fn field_count(&self) -> usize {
        self.field.len()
    }

    /// How many columns in total: scored fields plus unscored columns. This is the addressing
    /// space [`IndexBuilder::with_facet`], [`IndexBuilder::with_numeric`] and
    /// [`IndexBuilder::with_key`] take an index into.
    pub fn column_count(&self) -> usize {
        self.field.len() + self.unscored.len()
    }

    /// The name of a column by index: a scored field's name below [`Schema::field_count`], an
    /// unscored column's name above it.
    pub fn column_name(&self, column: usize) -> Option<&str> {
        match self.field.get(column) {
            Some(f) => Some(f.name.as_str()),
            None => self
                .unscored
                .get(column - self.field.len())
                .map(String::as_str),
        }
    }

    /// Resolve a column by name, scored fields first. `None` when no column carries that name.
    pub fn column_of(&self, name: &str) -> Option<usize> {
        (0..self.column_count()).find(|&i| self.column_name(i) == Some(name))
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

/// What a document keeps when field 0 is **not** exactly the query.
///
/// A document whose first field has the same token count as the query and matches every group is
/// *the thing that was asked for*, not merely a good match for it. BM25 cannot see that: it sums
/// term frequency across fields, so a longer relative whose comment repeats the term outscores the
/// exact row. `bench/roadmap/p40-booted-schema.md` found this on real schema names -- `session`
/// losing to `session_resource`, `user` to `user_agent` -- and the same shape exists for product
/// names.
///
/// **A demotion, not a boost**, exactly like [`UNANCHORED_KEEP`] and static priors: `max_score`
/// bounds assume a factor of 1.0, so a factor in `(0, 1]` keeps every pruning bound valid by
/// construction. A boost above 1.0 would quietly break MaxScore.
///
/// Deliberately mild. It is a tiebreak between comparable matches, not a filter -- a demoted
/// document with a much better BM25 score still wins, which is what keeps `"Colgate Toothpaste"`
/// from burying a better product just because its name is not word-for-word the query.
pub const INEXACT_FIELD_KEEP: f32 = 0.9;

/// How much wider the candidate pool gets when a learned expansion fires.
///
/// # This is a workaround, and the measurement says so
///
/// The pool is ordered by SCORE; the final ranking is ordered by `typo_bucket` FIRST, so a
/// perfect-bucket document with a modest score can be evicted before the sort sees it. Expansion
/// makes that acute by adding scoring competitors. Widening the pool hides it.
///
/// **No single multiplier is correct**, which is how you know it is not the real fix. Measured
/// in-sample precision@10 against the multiplier, `bench/roadmap/p21-pool-eviction.md`:
///
/// | mult | presyo | profstopick | blead |
/// |---|---|---|---|
/// | 3 | 96.5 % | 87.5 % | 84.8 % |
/// | 6 | **97.0 %** | 92.2 % | 89.6 % |
/// | 24 | 97.0 % | 96.7 % | **95.6 %** |
/// | 48 | 97.0 % | **99.4 %** | 95.6 % |
///
/// presyo saturates at 6, blead at 24, and profstopick is still climbing at 48. 24 is chosen as
/// the point where two of three corpora have saturated and the third has most of its gain; on
/// presyo it buys nothing over 6 and costs ~200 µs.
///
/// **The real fix is to make pruning consistent with ranking.** The engine prunes by score and
/// ranks by bucket-then-score, and those two disagree — that is the actual defect, and it is
/// recorded in `p21` rather than papered over here.
const EXPANDED_POOL_MULT: usize = 24;
/// Floor for the widened pool, so a small `k` still clears the eviction.
const EXPANDED_POOL_MIN: usize = 256;

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
pub const MAX_EXPANSION: usize = 16;

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
        Doc {
            field_text: field_text.into_iter().map(Into::into).collect(),
        }
    }
}

/// Accumulates documents, then produces an immutable [`Index`].
pub struct IndexBuilder {
    schema: Schema,
    alias: AliasTable,
    /// term text -> that term's postings. A hash map, not a `BTreeMap`: the sorted term order the
    /// dictionary needs is established once in [`IndexBuilder::build`], not maintained across the
    /// fifteen million insertions a million-document corpus performs.
    term_post: std::collections::HashMap<String, TermPost>,
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
    /// Fields designated as stored facets by [`IndexBuilder::with_facet`], in call order.
    /// Their positions here are the **slot** indices the query API takes.
    facet_field: Vec<usize>,
    /// Raw (untokenized, trimmed) facet value per document, per slot: `facet_store[doc][slot]`.
    facet_store: Vec<Vec<String>>,
    /// Fields designated as numeric columns by [`IndexBuilder::with_numeric`], in call order.
    numeric_field: Vec<usize>,
    /// Parsed numeric value per document, per numeric slot. `NaN` marks absent or unparseable.
    numeric_store: Vec<Vec<f64>>,
    /// Field holding the application's primary key, if one was designated.
    key_field: Option<usize>,
    /// Raw (untokenized, trimmed) key per document. Empty unless `key_field` is set.
    key_store: Vec<String>,
    /// Whether to record token positions. See [`IndexBuilder::with_position`].
    position_on: bool,
    /// Recursive graph bisection over the term-document graph at build time. **Off by default
    /// on measurement** — `bench/roadmap/p90-docid-reorder.md`: −6.2 % of the file on the real
    /// presyo schema, ~0 % on a short-title schema, latency at parity, and a real cost: every
    /// consumer that maps a result's doc ordinal back to its own rows breaks unless it addresses
    /// rows by key (`p48`/`p53` primitives). Same shape as static priors: correct, cheap, and
    /// ships off until a host wants it.
    reorder_on: bool,
    /// term text -> per document, that document's packed positions for the term, in ascending
    /// document order — the same order and the same reasoning as `term_post`. Empty unless
    /// `position_on`.
    term_pos: std::collections::HashMap<String, Vec<(u32, Vec<u32>)>>,
    /// The token buffer [`IndexBuilder::add`] hands to [`tokenize_into`] and reuses for every
    /// field of every document. Owning it here is what keeps a million-document build from
    /// allocating three million `Vec<Token>`s and fifteen million `String`s of text that is
    /// consulted once and thrown away.
    tok_buf: Vec<Token>,
}

impl IndexBuilder {
    pub fn new(schema: Schema) -> Self {
        IndexBuilder {
            schema,
            alias: AliasTable::new(),
            term_post: std::collections::HashMap::new(),
            doc_len: Vec::new(),
            raw_prior: Vec::new(),
            first_text: Vec::new(),
            expansion_cfg: None,
            facet_text: Vec::new(),
            facet_field: Vec::new(),
            facet_store: Vec::new(),
            numeric_field: Vec::new(),
            numeric_store: Vec::new(),
            key_field: None,
            reorder_on: false,
            key_store: Vec::new(),
            position_on: false,
            term_pos: std::collections::HashMap::new(),
            tok_buf: Vec::new(),
        }
    }

    /// Record **token positions**, which is what a phrase query needs and nothing else does.
    ///
    /// Opt-in, and it is the only build option that costs a posting-sized structure rather than a
    /// document-sized one: one `u32` per token OCCURRENCE, against eight bytes per (term, document)
    /// pair for the postings themselves. A corpus of short product names roughly doubles; a corpus
    /// of prose does much worse. Off by default, so no existing index pays for a feature it does
    /// not query.
    ///
    /// Must be set before the first document is added -- positions cannot be recovered afterwards,
    /// because the analyzed tokens are not kept.
    pub fn with_position(mut self) -> Self {
        assert!(
            self.set_position(),
            "positions must be enabled before the first document"
        );
        self
    }
    /// Toggle recursive graph bisection of the term-document graph at build time (default
    /// off; see the field doc for the measurement that decided it).
    /// Reordering clusters documents that share terms into adjacent doc ids, which is what a
    /// posting list's delta widths and a block-max's hit density both feed on. It changes NO
    /// answer: scores are per-document quantities that move with their document, so ranking is
    /// identical and only tie-breaks between equal scores may flip. See
    /// `bench/roadmap/p90-docid-reorder.md` for the measurement and for the length-sort attempt
    /// this is not.
    pub fn with_doc_reorder(mut self, on: bool) -> Self {
        self.reorder_on = on;
        self
    }

    /// Non-consuming [`IndexBuilder::with_position`], for the C ABI. Returns `false` once a
    /// document has been added, rather than panicking -- a trap kills the whole WASM instance.
    pub fn set_position(&mut self) -> bool {
        if !self.doc_len.is_empty() {
            return false;
        }
        self.position_on = true;
        true
    }

    /// One past the highest column index a facet, numeric or key declaration may name.
    ///
    /// [`MAX_FIELD`] is the floor rather than the bound, so a schema that declares fewer than four
    /// scored fields and no unscored column keeps accepting exactly the indices it accepted before
    /// `p65` — a two-field schema could always `with_facet(3)` and store the empty string, and
    /// tightening that would be a silent behaviour change dressed as a refactor.
    fn column_limit(&self) -> usize {
        self.schema.column_count().max(MAX_FIELD)
    }

    /// Declare a facet on a column resolved **by name** — the ergonomic form of
    /// [`IndexBuilder::with_facet`] once a schema carries [`Schema::with_column`] declarations,
    /// since an unscored column's index is otherwise a number the caller has to count out.
    pub fn with_facet_of(self, name: &str) -> Self {
        let column = self.schema.column_of(name).expect("no such column");
        self.with_facet(column)
    }

    /// Declare a numeric column resolved **by name**. See [`IndexBuilder::with_facet_of`].
    pub fn with_numeric_of(self, name: &str) -> Self {
        let column = self.schema.column_of(name).expect("no such column");
        self.with_numeric(column)
    }

    /// Declare the primary key column resolved **by name**. See [`IndexBuilder::with_facet_of`].
    pub fn with_key_of(self, name: &str) -> Self {
        let column = self.schema.column_of(name).expect("no such column");
        self.with_key(column)
    }

    /// Store field `field` as a **facet**: a value kept verbatim per document, for filtering and
    /// counting rather than for scoring.
    ///
    /// Facet values are deliberately **not tokenized**. `"Lucky Me"` is one value, not two terms;
    /// a shopper filtering by brand wants the brand, and tokenizing would make `"Lucky Me"` and
    /// `"Me Lucky"` the same facet. This is the one place the engine stores field text rather than
    /// an analysis of it, and the cost is bounded by the field's cardinality, not by `doc_count` --
    /// values are interned, so a million products over 145 categories store a million `u32` ids and
    /// 145 strings.
    ///
    /// Independent of [`IndexBuilder::learn_expansion`], which also takes a "facet field" but uses
    /// it to derive query expansions and keeps a *tokenized* copy. They may name the same field.
    ///
    /// `field` is a **column** index, so it may name an unscored [`Schema::with_column`] as well as
    /// a scored field. Faceting on an unscored column is the cheaper of the two and the reason that
    /// declaration exists: it stores the same interned labels and costs no per-posting `u16`.
    pub fn with_facet(mut self, field: usize) -> Self {
        assert!(self.set_facet_field(field), "facet field out of range");
        self
    }

    /// The designated facet fields, in slot order.
    pub fn facet_field(&self) -> &[usize] {
        &self.facet_field
    }

    /// Store field `field` as a **numeric column**: parsed once at build time, for range filtering
    /// and histograms.
    ///
    /// A price slider is a range filter plus a histogram, and neither is expressible with the
    /// interned string labels [`IndexBuilder::with_facet`] stores -- "9.99" and "10.00" sort and
    /// bucket as text, not as numbers.
    ///
    /// Values are `f64`, not `f32`: `f32` carries 24 bits of mantissa, so a price in minor units
    /// stops being exact above about 16.7 million. Eight bytes per document per column is the
    /// price of not having to reason about where that boundary falls.
    ///
    /// Text that does not parse as a number becomes `NaN`, which no range contains -- an absent
    /// value is excluded from every filter rather than silently treated as zero.
    pub fn with_numeric(mut self, field: usize) -> Self {
        assert!(self.set_numeric_field(field), "numeric field out of range");
        self
    }

    /// Store field `field` as the document's **primary key**: the application's own identifier for
    /// the row, kept verbatim so the row can be found again without knowing its dense ordinal.
    ///
    /// # Why this exists
    ///
    /// Every incremental operation an application actually performs is keyed on ITS id, not on
    /// ours: *"the row with id 4172 changed"*. [`Searcher::delete`] takes a dense global ordinal,
    /// which is assigned at insertion and is not something a database row carries — so without a
    /// key there is no way to express an update or a delete arriving from a change stream. This is
    /// the prerequisite for [`Searcher::upsert_segment`] and for the `index apply` CLI.
    ///
    /// Keys are **not tokenized and not interned**: unlike a facet, a key is expected to be unique
    /// per document, so interning would store one label per row and buy nothing. They are stored in
    /// document order and a sorted lookup is derived at load, exactly like `numeric_order`.
    ///
    /// **Not to be confused with the internal->external id map deliberately absent from `Index`.**
    /// That one is a docID *reordering* permutation for ranking performance, measured worse in
    /// `p7`; this is an application identifier and touches no ranking.
    ///
    /// A document whose key field is empty has **no key**: it can be searched and deleted by
    /// ordinal, but a change stream can never address it. That is a real hazard for a table with a
    /// nullable id, so it is reported by `IndexBuilder::build` rather than silently tolerated.
    pub fn with_key(mut self, field: usize) -> Self {
        assert!(self.set_key_field(field), "key field out of range");
        self
    }

    /// Non-consuming [`IndexBuilder::with_key`], for the C ABI. Returns `false` for an
    /// out-of-range field or once a document has been added, rather than panicking.
    pub fn set_key_field(&mut self, field: usize) -> bool {
        if field >= self.column_limit() || !self.key_store.is_empty() || !self.doc_len.is_empty() {
            return false;
        }
        self.key_field = Some(field);
        true
    }

    /// The designated key field, if any.
    pub fn key_field(&self) -> Option<usize> {
        self.key_field
    }

    /// Non-consuming [`IndexBuilder::with_numeric`], for the C ABI. See
    /// [`IndexBuilder::set_facet_field`] for why the ABI cannot use the consuming form.
    pub fn set_numeric_field(&mut self, field: usize) -> bool {
        if field >= self.column_limit() || !self.numeric_store.is_empty() {
            return false;
        }
        if !self.numeric_field.contains(&field) {
            self.numeric_field.push(field);
        }
        true
    }

    /// The designated numeric fields, in slot order.
    pub fn numeric_field(&self) -> &[usize] {
        &self.numeric_field
    }

    /// Non-consuming [`IndexBuilder::with_facet`]. Returns `false` for an out-of-range field
    /// instead of panicking, because the C ABI must be able to reject bad input from a host
    /// without trapping — a trap kills the whole WASM instance.
    /// Call it once per field to facet on. Order is the slot order the query API uses, and a
    /// repeated field is ignored rather than duplicated -- two slots over one field would double
    /// the storage to answer identical questions.
    pub fn set_facet_field(&mut self, field: usize) -> bool {
        if field >= self.column_limit() || !self.facet_store.is_empty() {
            return false;
        }
        if !self.facet_field.contains(&field) {
            self.facet_field.push(field);
        }
        true
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
        let p = if prior.is_finite() && prior > 0.0 {
            prior
        } else {
            1.0
        };
        self.raw_prior[id as usize] = p;
        id
    }

    /// Add a document with the default prior of 1.0. Returns its dense ordinal.
    pub fn add(&mut self, doc: &Doc) -> u32 {
        let id = self.doc_len.len() as u32;
        let mut len = [0u16; MAX_FIELD];
        let mut first = String::new();
        let mut facet = String::new();
        // Taken out of `self` for the duration so the token buffer, `term_post` and `term_pos` are
        // three plainly disjoint borrows; put back below, so the next document reuses it.
        let mut tok = std::mem::take(&mut self.tok_buf);
        for (fi, text) in doc
            .field_text
            .iter()
            .enumerate()
            .take(self.schema.field_count())
        {
            // The tokens are `tok[..n]`. `tok.len()` is the high-water mark of every field seen so
            // far, retained as scratch on purpose — see [`tokenize_into`].
            let n = tokenize_into(text, &mut tok);
            apply_alias(&mut tok[..n], &self.alias);
            len[fi] = n.min(u16::MAX as usize) as u16;
            if fi == 0 {
                first.clear();
                if n > 0 {
                    first.push_str(&tok[0].text);
                }
            }
            if self.expansion_cfg.is_some_and(|(f, _)| f == fi) {
                facet.clear();
                for (at, t) in tok[..n].iter().enumerate() {
                    if at > 0 {
                        facet.push(' ');
                    }
                    facet.push_str(&t.text);
                }
            }
            for (at, t) in tok[..n].iter().enumerate() {
                if self.position_on {
                    // Positions above `u16::MAX` are dropped rather than wrapped: `doc_len` is
                    // already `u16`, so a field that long is truncated everywhere else too, and a
                    // wrapped position would place a token at a phrase offset it does not occupy.
                    if let Ok(at) = u16::try_from(at) {
                        let pos = pack_position(fi, at);
                        // Looked up by `&str` first: a term already in the map — which is all but
                        // a few hundred thousand of the fifteen million hits a large build makes —
                        // then costs no allocation at all. Only a genuinely new term pays for one.
                        if let Some(per_doc) = self.term_pos.get_mut(t.text.as_str()) {
                            match per_doc.last_mut() {
                                Some((d, p)) if *d == id => p.push(pos),
                                _ => per_doc.push((id, vec![pos])),
                            }
                        } else {
                            self.term_pos.insert(t.text.clone(), vec![(id, vec![pos])]);
                        }
                    }
                }
                if let Some(post) = self.term_post.get_mut(t.text.as_str()) {
                    post.hit(id, fi);
                } else {
                    self.term_post
                        .entry(t.text.clone())
                        .or_default()
                        .hit(id, fi);
                }
            }
        }
        self.tok_buf = tok;
        self.doc_len.push(len);
        self.raw_prior.push(1.0);
        self.first_text.push(first);
        self.facet_text.push(facet);
        if let Some(f) = self.key_field {
            self.key_store.push(
                doc.field_text
                    .get(f)
                    .map(|t| t.trim().to_string())
                    .unwrap_or_default(),
            );
        }
        self.facet_store.push(
            self.facet_field
                .iter()
                .map(|&f| {
                    doc.field_text
                        .get(f)
                        .map(|t| t.trim().to_string())
                        .unwrap_or_default()
                })
                .collect(),
        );
        self.numeric_store.push(
            self.numeric_field
                .iter()
                .map(|&f| {
                    doc.field_text
                        .get(f)
                        .and_then(|t| t.trim().parse::<f64>().ok())
                        .filter(|v| v.is_finite())
                        .unwrap_or(f64::NAN)
                })
                .collect(),
        );
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
        let Some((_, top_k)) = cfg else {
            return Vec::new();
        };
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
                *per_value
                    .entry(f)
                    .or_default()
                    .entry(tid as u32)
                    .or_default() += 1;
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
        raw.into_iter()
            .map(|p| (p / max).clamp(f32::MIN_POSITIVE, 1.0))
            .collect()
    }

    pub fn build(mut self) -> Result<Index, String> {
        // The term order every term id in the index refers to. Sorting once here replaces the
        // ordering a `BTreeMap` used to maintain on every insertion, and produces the identical
        // sequence: the keys are distinct, so `sort_unstable_by` over them is a total order.
        let mut entry: Vec<(String, TermPost)> = self.term_post.into_iter().collect();
        entry.sort_unstable_by(|a, b| a.0.cmp(&b.0));

        // Moves only — the two posting columns were accumulated in their final layout, so what was
        // a full re-materialization of every posting list is now a pointer per term.
        let mut term: Vec<String> = Vec::with_capacity(entry.len());
        let mut posting: Vec<Vec<Posting>> = Vec::with_capacity(entry.len());
        let mut posting_tf: Vec<Vec<[u16; MAX_FIELD]>> = Vec::with_capacity(entry.len());
        for (text, post) in entry {
            term.push(text);
            posting.push(post.doc);
            posting_tf.push(post.tf);
        }
        // DocID reordering, applied before anything consumes an id: every posting list is
        // remapped and re-sorted so ids ascend again (the saturated score bound rides along —
        // it is a per-(term, document) quantity and moves with its posting), `term_pos` is
        // remapped with it so position runs stay attached and aligned, and every per-document
        // store is permuted by the same map. Scores, ranking, keys and facet values are all
        // per-document quantities; only the doc-id ORDER moves.
        if self.reorder_on {
            let n = self.doc_len.len();
            let ids: Vec<Vec<u32>> = posting
                .iter()
                .map(|l| l.iter().map(|p| p.doc).collect())
                .collect();
            let refs: Vec<&[u32]> = ids.iter().map(|l| &l[..]).collect();
            let perm = reorder::graph_bisect_permutation(n, &refs);
            let identity = (0..n as u32).collect::<Vec<_>>();
            if perm != identity {
                for (docs, tfs) in posting.iter_mut().zip(posting_tf.iter_mut()) {
                    let mut joint: Vec<(u32, [u16; MAX_FIELD])> = docs
                        .iter()
                        .zip(tfs.iter())
                        .map(|(p, t)| (perm[p.doc as usize], *t))
                        .collect();
                    joint.sort_unstable_by_key(|(d, _)| *d);
                    for ((p, tf), (d, t)) in docs.iter_mut().zip(tfs.iter_mut()).zip(joint) {
                        p.doc = d;
                        *tf = t;
                    }
                }
                for per_doc in self.term_pos.values_mut() {
                    for (d, _) in per_doc.iter_mut() {
                        *d = perm[*d as usize];
                    }
                    per_doc.sort_unstable_by_key(|(d, _)| *d);
                }
                // Per-document stores that are PRESENT are permuted; the optional ones
                // (priors, facets, numerics) are legitimately empty when unused, and an
                // empty store has nothing to move.
                reorder::apply(&mut self.doc_len, &perm);
                if self.raw_prior.len() == n {
                    reorder::apply(&mut self.raw_prior, &perm);
                }
                if self.first_text.len() == n {
                    reorder::apply(&mut self.first_text, &perm);
                }
                if self.facet_text.len() == n {
                    reorder::apply(&mut self.facet_text, &perm);
                }
                if self.facet_store.len() == n {
                    reorder::apply(&mut self.facet_store, &perm);
                }
                if self.numeric_store.len() == n {
                    reorder::apply(&mut self.numeric_store, &perm);
                }
                if self.key_store.len() == n {
                    reorder::apply(&mut self.key_store, &perm);
                }
            }
        }

        let dict = TermDict::build(&term)?;

        let doc_count = self.doc_len.len();

        // Positions are flattened into ONE array indexed by global posting slot, rather than a
        // `Vec<Vec<u32>>` per term: at a million documents the per-posting `Vec` headers cost more
        // than the positions they point at, and the flat form serializes as two spans.
        //
        // Serial on purpose: the array is defined by the order it is appended in, so a threaded
        // version would have to concatenate its parts in that same order anyway.
        let mut term_pos = self.term_pos;
        let mut position: Vec<u32> = Vec::new();
        let mut position_at: Vec<u64> = match self.position_on {
            true => vec![0],
            false => Vec::new(),
        };
        if self.position_on {
            for (text, list) in term.iter().zip(posting.iter()) {
                let per_doc = term_pos.remove(text).unwrap_or_default();
                // Both columns are in ascending document order, so one cursor aligns them. A
                // document can be missing from `per_doc` while present in the postings: a token
                // beyond `u16::MAX` is counted but has no recordable position.
                let mut at = 0usize;
                for p in list {
                    let start = position.len();
                    if per_doc.get(at).is_some_and(|(d, _)| *d == p.doc) {
                        position.extend_from_slice(&per_doc[at].1);
                        at += 1;
                    }
                    // Ascending, so the phrase verifier can binary-search for `start + i`.
                    position[start..].sort_unstable();
                    position_at.push(position.len() as u64);
                }
            }
        }
        drop(term_pos);

        let mut avg_len = [1.0f32; MAX_FIELD];
        for fi in 0..self.schema.field_count() {
            let total: u64 = self.doc_len.iter().map(|l| l[fi] as u64).sum();
            // Guard the empty-field case so the normalizer never divides by zero.
            avg_len[fi] = (total as f32 / doc_count.max(1) as f32).max(1.0);
        }

        // Computed before the struct literal takes ownership of `posting`.
        let expansion =
            Self::derive_expansion(self.expansion_cfg, &self.facet_text, &term, &posting);

        // Intern facet values. Sorted labels so lookup is a binary search and serialization is
        // deterministic; `u32::MAX` marks a document with no facet, which is also what every
        // document gets when no facet field was designated.
        // One label set and one id column per slot. Sorted labels so lookup is a binary search
        // and serialization is deterministic; `u32::MAX` marks a document with no value there.
        let mut facet_label: Vec<Vec<String>> = Vec::new();
        let mut facet_id: Vec<Vec<u32>> = Vec::new();
        for slot in 0..self.facet_field.len() {
            let mut label: Vec<String> = self
                .facet_store
                .iter()
                .filter_map(|v| v.get(slot))
                .filter(|v| !v.is_empty())
                .cloned()
                .collect();
            label.sort_unstable();
            label.dedup();
            let store = &self.facet_store;
            let id = par_index_map(store.len(), store.len(), |d| match store[d].get(slot) {
                Some(x) if !x.is_empty() => {
                    label.binary_search(x).map(|i| i as u32).unwrap_or(u32::MAX)
                }
                _ => u32::MAX,
            });
            facet_label.push(label);
            facet_id.push(id);
        }
        let facet_field = self.facet_field;

        // Numeric columns, transposed to slot-major so a range scan walks one contiguous column.
        let mut numeric_value: Vec<Vec<f64>> = Vec::new();
        for slot in 0..self.numeric_field.len() {
            let store = &self.numeric_store;
            numeric_value.push(par_index_map(store.len(), store.len(), |d| {
                store[d].get(slot).copied().unwrap_or(f64::NAN)
            }));
        }
        let numeric_field = self.numeric_field;

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
                // it gives the same ids the postings use. One independent search per document, so
                // it parallelizes exactly.
                let first_text = &self.first_text;
                par_index_map(first_text.len(), first_text.len(), |d| {
                    match first_text[d].is_empty() {
                        true => u32::MAX,
                        false => term
                            .binary_search(&first_text[d])
                            .map(|i| i as u32)
                            .unwrap_or(u32::MAX),
                    }
                })
            },
            facet_field,
            facet_label,
            facet_id,
            numeric_field,
            numeric_value,
            doc_len: self.doc_len,
            avg_len,
            doc_count,
            position,
            position_at,
            posting_base: Vec::new(),
            numeric_order: Vec::new(),
            doc_key: self.key_store,
            key_field: self.key_field.unwrap_or(usize::MAX),
            key_order: Vec::new(),
            collection_size: None,
            term_df: Vec::new(),
            term_page_off: Vec::new(),
            term_page_id: Vec::new(),
            term_page_bit: Vec::new(),
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
    /// Schema field behind each facet slot, in slot order.
    facet_field: Vec<usize>,
    /// Sorted, deduplicated facet values per slot. Empty when the index has no facet field.
    facet_label: Vec<Vec<String>>,
    /// Per-slot, per-document index into `facet_label[slot]`, or `u32::MAX` for "no value".
    facet_id: Vec<Vec<u32>>,
    /// Schema field behind each numeric slot, in slot order.
    numeric_field: Vec<usize>,
    /// Per-slot, per-document numeric value. `NaN` marks absent.
    numeric_value: Vec<Vec<f64>>,
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
    /// Packed token positions for every posting, concatenated in posting order. **Empty unless the
    /// index was built with [`IndexBuilder::with_position`]** -- an index with no phrase queries
    /// pays nothing.
    position: Vec<u32>,
    /// `total posting count + 1` cumulative offsets into [`Index::position`]. Global posting slot
    /// `s` owns `position[position_at[s]..position_at[s + 1]]`. Empty when positions are off.
    position_at: Vec<u64>,
    /// The application's primary key per document, in document order. **Empty unless the index
    /// was built with [`IndexBuilder::with_key`]**; an entry may be empty for a row whose key field
    /// was blank, and such a row has no key rather than a key of `""`.
    doc_key: Vec<String>,
    /// Which schema field `doc_key` came from. `usize::MAX` when there is no key.
    key_field: usize,
    /// Document ordinals sorted by key, so a key resolves by binary search.
    ///
    /// **Derived, never serialized**, like `numeric_order` and `posting_base`: it is a sort of data
    /// the index already holds, and a stored copy could disagree with it.
    key_order: Vec<u32>,
    /// Documents in the whole COLLECTION this segment belongs to, when it belongs to one.
    ///
    /// # Why a segment must not score against its own size
    ///
    /// IDF answers "how rare is this term", and rarity is a property of the corpus, not of the
    /// shard that happens to hold the row. A three-row delta computing IDF over three documents
    /// scores a term at `ln(1 + 2.5/1.5) = 0.98`; the same term in a 4,001-row base scores `7.89`.
    /// Same word, same corpus, an eight-fold difference decided purely by which segment the row
    /// landed in -- which is why a row updated through `apply` can outrank or underrank its own
    /// former self.
    ///
    /// `p50` measured that as the ceiling on incremental updating: rank-1 agreement with a full
    /// rebuild stuck near 93 % however compaction was tuned, because the FIRST delta already cost
    /// that much. This field is what removes it.
    ///
    /// **Derived, never serialized**, and set by [`Searcher`] whenever the collection changes.
    /// `None` for a standalone index, where the segment IS the collection.
    ///
    /// Only IDF's numerator is corrected here. Document frequency stays per segment, because
    /// summing it across segments needs each term's TEXT and the dictionary stores an FST rather
    /// than the strings. That is the remaining gap, and `p52` measures what it leaves behind.
    collection_size: Option<usize>,
    /// Stored document frequency per term (`posting[t].len()` at build). COUNT of a live,
    /// undeleted term is this integer — it does not walk the list.
    term_df: Vec<u32>,
    /// Sparse 64-doc page presence for COUNT. Term `t` owns
    /// `term_page_id[term_page_off[t]..term_page_off[t + 1]]` (sorted page indexes) and the
    /// matching 64-bit presence words in `term_page_bit`. A COUNT OR/AND is a merge of those
    /// words plus an AND-NOT of `deleted`; it does not walk postings. Derived, never serialized.
    term_page_off: Vec<u32>,
    term_page_id: Vec<u32>,
    term_page_bit: Vec<u64>,
    /// Per numeric slot, every document that HAS a finite value there, ascending by value.
    ///
    /// **Derived, never serialized**, exactly like `block_max`: it is a sort of a column the index
    /// already stores, and a stored copy could disagree with that column. Four bytes per document
    /// per numeric slot, and only for documents with a value.
    ///
    /// This is what lets [`Index::search_sorted`] stop early. See `bench/roadmap/p46-sort-tail.md`.
    numeric_order: Vec<Vec<u32>>,
    /// `term_count + 1` cumulative posting counts, so `(term, index within its list)` maps to the
    /// global posting slot `position_at` is indexed by. **Derived, never serialized** -- it is a
    /// prefix sum of lengths the posting lists already carry, and storing it would be a second
    /// copy of the same fact that could disagree with the first.
    posting_base: Vec<u64>,
}

impl Index {
    /// `1.0` when field 0 is exactly the query, [`INEXACT_FIELD_KEEP`] otherwise.
    ///
    /// "Exactly" means the same number of tokens **and** every query group matched, which is what
    /// `bucket == 0` already says. Order is not checked, so `"Toothpaste Colgate"` counts as exact
    /// for `"Colgate Toothpaste"` -- the tokenizer is bag-of-words everywhere else and pretending
    /// otherwise here would be inconsistent.
    #[inline]
    fn exact_field_factor(&self, doc: u32, bucket: u32, group_count: usize) -> f32 {
        let exact = bucket == 0
            && self
                .doc_len
                .get(doc as usize)
                .is_some_and(|l| l[0] as usize == group_count);
        if exact {
            1.0
        } else {
            INEXACT_FIELD_KEEP
        }
    }

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

    pub(crate) fn set_numeric(&mut self, field: Vec<usize>, value: Vec<Vec<f64>>) {
        self.numeric_field = field;
        self.numeric_value = value;
    }

    pub(crate) fn set_facet(
        &mut self,
        field: Vec<usize>,
        label: Vec<Vec<String>>,
        id: Vec<Vec<u32>>,
    ) {
        self.facet_field = field;
        self.facet_label = label;
        self.facet_id = id;
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
/// Kept although nothing compares against it any more: it records the tolerance the quantized grid
/// replaced, and `the_score_grid_matches_the_documented_tolerance` asserts the grid still sits near
/// it. Deleting the constant would delete the only statement of how coarse the grid is meant to be.
#[allow(dead_code)]
const SCORE_TIE_REL: f32 = 1e-6;

/// Low mantissa bits dropped by [`canon_score`], chosen so the grid is about [`SCORE_TIE_REL`].
///
/// `f32` has 23 mantissa bits, so dropping `k` leaves a relative step of `2^-(23-k)`. `k = 3` gives
/// `2^-20`, about `9.5e-7` — the same order as the `1e-6` tolerance this replaces.
const SCORE_TIE_BITS: u32 = 3;

/// Round a score onto a coarse grid, so that comparing two scores is TRANSITIVE.
///
/// # Why this exists rather than a tolerance
///
/// The comparator used to ask `(a - b).abs() <= SCORE_TIE_REL * scale` and call that equal. That is
/// not an equivalence relation: `a ~ b` and `b ~ c` does not imply `a ~ c`, so the derived ordering
/// is not a total order. Rust's sort detects the violation and **panics** — which it did, on the
/// first long-document corpus the engine ever saw (`bench/roadmap/p42-alec-surface.md`). Short
/// fields never produced enough near-ties in one result set to expose it; 1,000-character bodies
/// did immediately.
///
/// Quantizing instead makes the comparison a pure function of each value, so transitivity holds by
/// construction. Rounding is to nearest rather than truncating, which halves how often two scores
/// that were meant to tie land either side of a grid boundary.
///
/// The original intent is preserved: two paths that compute the same document's score with
/// different float accumulation orders still agree, because the difference is far below the grid.
#[inline]
pub(crate) fn canon_score(x: f32) -> f32 {
    // Only positive, finite scores reach the grid. Anything else is passed through: `total_cmp`
    // orders it consistently, and BM25 sums of positive terms cannot produce it anyway.
    if !x.is_finite() || x < 0.0 {
        return x;
    }
    let half = 1u32 << (SCORE_TIE_BITS - 1);
    let mask = !((1u32 << SCORE_TIE_BITS) - 1);
    // The bit pattern of a non-negative float increases monotonically with its value, so adding a
    // constant and masking is monotone non-decreasing — the grid never reorders two values.
    f32::from_bits(x.to_bits().wrapping_add(half) & mask)
}

/// Collection-wide statistics, so every segment of a [`Searcher`] scores against the same corpus.
///
/// # The problem it removes
///
/// IDF answers "how rare is this term", and rarity is a property of the CORPUS, not of the shard
/// that happens to hold the row. Left per segment, a three-row delta scores a term at
/// `ln(1 + 2.5/1.5) = 0.98` while the same term in a 4,001-row base scores `7.89` — an eight-fold
/// difference decided by which segment a row landed in. `p38` measured the consequence as a ranking
/// ceiling and `p50` measured it again on a change stream: rank-1 agreement with a full rebuild
/// stuck near 93 %, because the FIRST delta already cost that much.
///
/// # Why BOTH numbers, and never only one
///
/// The first attempt corrected only `doc_count`, and it made ranking **worse** — caught immediately
/// by `ranking_skew_is_bounded_for_a_small_delta`. A small segment has a small `df` as well as a
/// small `n`, and the two partially cancel; raising `n` alone leaves `df = 1` against a corpus of
/// 200 and inflates that segment's terms instead of correcting them. **A partial correction of a
/// ratio is not a partial improvement.** So `df` is summed across segments too, and the two travel
/// together.
///
/// # Why it is keyed on text
///
/// A term id is an index into one segment's dictionary and means something different in the next,
/// so document frequency can only be summed by the term's TEXT. The dictionary stores an FST rather
/// than the strings, but the stream yields the key bytes during traversal, so
/// [`TermDict::expand_lazy_text`] recovers them for the handful of planned terms at no extra
/// traversal. This is the same design Elasticsearch calls `dfs_query_then_fetch`.
#[derive(Debug, Default, Clone)]
pub struct CollectionStat {
    /// Documents across every segment, deleted ones included — matching the single-index rule that
    /// a tombstone does not rewrite statistics until a rebuild.
    pub doc_count: usize,
    /// Term text -> summed document frequency across every segment.
    pub df: std::collections::HashMap<String, usize>,
}

impl CollectionStat {
    /// IDF for `text`, against the whole collection. Falls back to `local_df` for a term the stats
    /// pass never saw, which cannot normally happen and must not be a panic if it does.
    #[inline]
    fn idf(&self, text: &str, local_df: usize) -> f32 {
        let df = self.df.get(text).copied().unwrap_or(local_df) as f32;
        let n = self.doc_count as f32;
        (1.0 + (n - df + 0.5).max(0.5) / (df + 0.5)).ln()
    }
}

/// One clause of a filter bar: a slot, the values that satisfy it, and whether to invert.
///
/// Values inside a clause are **OR**-ed ("Colgate or Oral-B"); clauses are **AND**-ed. That is how
/// every storefront filter bar behaves — ticking two brands widens the result, ticking a brand and
/// a category narrows it.
///
/// An unknown value is ignored rather than fatal, because a filter bar built from one segment's
/// labels can name a value another segment has never seen. The two ends of that rule are
/// deliberately opposite:
///
/// - **include** with *every* value unknown matches **nothing** — nobody can satisfy it;
/// - **exclude** with *every* value unknown excludes **nothing** — there is nothing to remove.
///
/// Getting those the wrong way round is how a filter silently returns the entire corpus.
///
/// A document with no value in the slot is **kept by an exclude** and dropped by an include, which
/// is what "not Colgate" should do to an unbranded row.
#[derive(Clone, Copy, Debug)]
pub struct FacetClause<'a> {
    /// Facet slot, in [`IndexBuilder::with_facet`] call order.
    pub slot: usize,
    /// Values that satisfy this clause, OR-ed together.
    pub value: &'a [&'a str],
    /// Invert: keep documents whose value is **not** among `value`.
    pub exclude: bool,
}

impl<'a> FacetClause<'a> {
    /// A clause satisfied by any of `value`.
    pub fn any(slot: usize, value: &'a [&'a str]) -> Self {
        FacetClause {
            slot,
            value,
            exclude: false,
        }
    }

    /// A clause satisfied by documents matching none of `value`.
    pub fn none(slot: usize, value: &'a [&'a str]) -> Self {
        FacetClause {
            slot,
            value,
            exclude: true,
        }
    }
}

/// Everything one scan needs beyond the index itself.
///
/// `search_opt` reached eight positional parameters, most of them empty slices whose meaning was
/// invisible at the call site. A struct with a `Default` makes each entry point say only what it
/// changes — `Scan { query, k, offset, ..Scan::default() }` reads as "a page", and nothing else
/// has to be repeated.
/// One `emit` call's worth of a planned query: the matches that will feed [`Index::emit`] for one
/// group, and whether the call came from the learned-expansion branch.
///
/// `learned` matters only to [`Index::term_stat`], which must keep excluding those terms from the
/// collection-wide `df` sum exactly as it always has — they score per segment by documented
/// decision (`p52`).
#[derive(Debug, Clone)]
pub(crate) struct ExpansionEmit {
    pub group: u16,
    pub matches: Vec<(crate::dict::TermMatch, String)>,
    pub learned: bool,
}

/// A fully expanded query, before the cap and the IDF weights are applied.
///
/// **This is the phase split `p52` named and left undone.** Both collection-stat passes traverse
/// the same fuzzy automaton over the same dictionary: the stat pass to learn `(text, df)`, the
/// search pass to plan postings. [`Index::expand_query`] runs that traversal ONCE per segment and
/// its result is handed to the weigh phase, so a segmented query pays one expansion, not two. The
/// per-segment granularity is deliberate — each dictionary is its own, so segment `i`'s expansion
/// is handed back to segment `i` and the answer stays bit-identical to a re-derivation.
#[derive(Debug, Clone)]
pub(crate) struct QueryExpansion {
    /// Emit calls in the order `plan_stat` would have made them: one per token (two for a compound
    /// split), then the learned-expansion extras. The order is load-bearing — `emit` mutates no
    /// shared state, but the cap sorts each call's list independently, so regrouping them could
    /// change which terms survive.
    pub emits: Vec<ExpansionEmit>,
    /// One entry per query group: whether the group is a physical quantity.
    pub group_is_quantity: Vec<bool>,
    /// Whether the learned-expansion branch fired, which widens the scoring pool.
    pub expanded: bool,
}

struct Scan<'a> {
    query: &'a str,
    k: usize,
    /// Rows to skip before the page. Cost grows with this; see [`Index::search_page`].
    offset: usize,
    /// Typeahead semantics on the last token.
    prefix_last: bool,
    /// Dictionary expansions permitted per query token.
    cap: usize,
    /// Resolved facet clauses: `(slot, sorted label ids, exclude)`.
    facet: &'a [(usize, Vec<u32>, bool)],
    /// Half-open numeric ranges: `(slot, lo, hi)`.
    range: &'a [(usize, f64, f64)],
    /// Term ids that must appear as consecutive tokens of one field. Empty means no constraint.
    phrase: &'a [u32],
    /// Collection-wide statistics, when this segment is one of several. See [`CollectionStat`].
    stat: Option<&'a CollectionStat>,
    /// A pre-computed expansion from [`Index::expand_query`], handed over by [`crate::Searcher`]
    /// so the weigh phase below need not walk the fuzzy automaton a second time.
    pre: Option<&'a QueryExpansion>,
}

impl<'a> Scan<'a> {
    fn new(query: &'a str, k: usize) -> Self {
        Scan {
            query,
            k,
            offset: 0,
            prefix_last: false,
            cap: MAX_EXPANSION,
            facet: &[],
            range: &[],
            phrase: &[],
            stat: None,
            pre: None,
        }
    }
}

/// The single ranking comparator, used by both the pruned and the exhaustive path.
#[inline]
pub(crate) fn rank_cmp(a: &Hit, b: &Hit) -> std::cmp::Ordering {
    // Lexicographic on (bucket, quantized score descending, doc). Every component is a total
    // order and they are chained, so the whole is one — which `sort_by` requires and which the
    // previous tolerance-based form did not provide.
    a.typo_bucket
        .cmp(&b.typo_bucket)
        .then_with(|| canon_score(b.score).total_cmp(&canon_score(a.score)))
        .then_with(|| a.doc.cmp(&b.doc))
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
    /// `score - bucket * scale`, with `scale` larger than any achievable score — so it reproduces
    /// the final ranking's `(bucket asc, score desc)` order exactly rather than approximating it.
    ///
    /// Used by the **ranking pool**, not the scoring pool. See `RankCandidate`.
    eff: f32,
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
        other
            .score
            .total_cmp(&self.score)
            .then(self.doc.cmp(&other.doc))
    }
}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Is score-based pruning currently SOUND?
///
/// It is exactly when the ranking pool is full **and its worst member has bucket 0**. Then no
/// unseen document can have a better bucket, so entering the answer requires beating a *score* —
/// which is what block-max skipping and the non-essential bail actually test.
///
/// The moment the ranking pool's worst member has a bucket above 0, a document matching more of the
/// query enters regardless of how it scores, and skipping it on a score bound discards a document
/// the ranking would have kept. That is the defect `bench/roadmap/p22-prune-consistency.md`
/// reproduces from 512 decoys and `p23-pool-audit.md` measures at 9.33 % of real presyo queries.
#[inline]
fn prune_is_sound(
    rank_pool: &std::collections::BinaryHeap<RankCandidate>,
    rank_cap: usize,
) -> bool {
    rank_pool.len() >= rank_cap && rank_pool.peek().is_some_and(|w| w.0.bucket == 0)
}

/// Offer a scored candidate to both pools.
///
/// The scoring pool keeps the best by score and owns the pruning threshold; the ranking pool keeps
/// the best by final rank. Each evicts independently, so a document can be in one, both, or neither.
#[inline]
fn admit(
    score_pool: &mut std::collections::BinaryHeap<Candidate>,
    rank_pool: &mut std::collections::BinaryHeap<RankCandidate>,
    threshold: &mut f32,
    pool: usize,
    rank_cap: usize,
    c: Candidate,
) {
    if score_pool.len() < pool {
        score_pool.push(c);
        if score_pool.len() == pool {
            *threshold = score_pool.peek().map_or(f32::NEG_INFINITY, |x| x.score);
        }
    } else if c.score > *threshold {
        // O(log pool): pop the worst, push the new one, read the new worst.
        score_pool.pop();
        score_pool.push(c);
        *threshold = score_pool.peek().map_or(f32::NEG_INFINITY, |x| x.score);
    }

    if rank_pool.len() < rank_cap {
        rank_pool.push(RankCandidate(c));
    } else if rank_pool.peek().is_some_and(|w| c.eff > w.0.eff) {
        rank_pool.pop();
        rank_pool.push(RankCandidate(c));
    }
}

/// A candidate in the **ranking pool**, ordered so the root is the worst *by final rank*.
///
/// # Why there are two pools
///
/// `bench/roadmap/p22-prune-consistency.md` reproduced a real defect: the scoring pool evicts by
/// **score**, the final answer is ordered by **bucket then score**, and a document matching more of
/// the query could be thrown away before the ranking ran.
///
/// `bench/roadmap/p23-pool-audit.md` then measured it on production data and found it is not
/// hypothetical: on **28.2 % of real presyo product queries** the returned top-10 contained a
/// document that matched *less* of the query than one the pool had discarded.
///
/// Ordering a single pool by `eff` fixes it completely and costs 10–30× (measured: typo p99 at 1 M
/// went 6.0 ms to 57.5 ms), because the pruning threshold is the pool's worst member and an `eff`
/// threshold sits below every score, so block skipping never engages. So there are two pools over
/// the same scored candidates:
///
///   - the **scoring pool** keeps the best by score and supplies the pruning threshold, exactly as
///     before, so skipping and latency are largely unchanged;
///   - the **ranking pool**, sized `k`, keeps the best by `eff`, so a document the final ordering
///     would rank highly cannot be lost merely for scoring poorly.
///
/// They are merged and deduplicated at the end. A document can still be missed if pruning skipped
/// it *before scoring*, which is a strictly smaller hole and is what `p22` still records as red.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RankCandidate(Candidate);

impl Eq for RankCandidate {}

impl Ord for RankCandidate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Root is evicted, so "greater" means "worse by final rank": lower `eff` first, and among
        // equals the HIGHER document id — matching `Candidate` so the two pools agree on ties.
        other
            .0
            .eff
            .total_cmp(&self.0.eff)
            .then(self.0.doc.cmp(&other.0.doc))
    }
}

impl PartialOrd for RankCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// A borrowed view of an index's internals, for serialization only.
pub(crate) struct Snapshot<'a> {
    pub doc_count: usize,
    pub field: &'a [Field],
    /// Unscored column names, in declaration order. See [`Schema::with_column`].
    pub unscored: &'a [String],
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
    /// Schema field behind each facet slot.
    pub facet_field: &'a [usize],
    /// Interned facet labels per slot, sorted; empty unless a facet field was designated.
    pub facet_label: &'a [Vec<String>],
    /// Per-slot, per-document label id; empty exactly when `facet_label` is.
    pub facet_id: &'a [Vec<u32>],
    /// The application's primary key per document; empty unless a key field was designated.
    pub doc_key: &'a [String],
    /// Schema field `doc_key` came from; meaningless when `doc_key` is empty.
    pub key_field: usize,
    /// Packed token positions; empty unless the index was built with positions.
    pub position: &'a [u32],
    /// Cumulative offsets into `position`, one per posting plus a terminator; empty when off.
    pub position_at: &'a [u64],
    /// Schema field behind each numeric slot.
    pub numeric_field: &'a [usize],
    /// Per-slot, per-document numeric value; empty unless a numeric column was designated.
    pub numeric_value: &'a [Vec<f64>],
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
            unscored: &self.schema.unscored,
            k1: self.schema.k1,
            typo_penalty: self.schema.typo_penalty,
            avg_len: self.avg_len,
            alias: self.alias.iter().collect(),
            dict_bytes: self.dict.to_bytes(),
            posting: self
                .posting
                .iter()
                .zip(self.posting_tf.iter())
                .map(|(l, tfs)| {
                    l.iter()
                        .zip(tfs.iter())
                        .map(|(p, tf)| (p.doc, *tf))
                        .collect()
                })
                .collect(),
            doc_len: &self.doc_len,
            prior: &self.prior,
            first_term: &self.first_term,
            deleted: &self.deleted,
            expansion: &self.expansion,
            facet_field: &self.facet_field,
            facet_label: &self.facet_label,
            facet_id: &self.facet_id,
            numeric_field: &self.numeric_field,
            numeric_value: &self.numeric_value,
            position: &self.position,
            position_at: &self.position_at,
            doc_key: &self.doc_key,
            key_field: self.key_field,
        }
    }

    /// Sort document ordinals by key, so a key resolves by binary search.
    ///
    /// Documents with an EMPTY key are excluded: a blank key field means the row has no key, not
    /// that its key is the empty string, and admitting them would let a change stream address an
    /// arbitrary one of them.
    ///
    /// Sorted by key then by ordinal, so a key duplicated INSIDE one index resolves to the last
    /// document carrying it — the same newest-wins rule [`Searcher`] applies across segments, so
    /// one rule covers both cases.
    fn rebuild_key_order(&mut self) {
        self.key_order = (0..self.doc_key.len() as u32)
            .filter(|&d| !self.doc_key[d as usize].is_empty())
            .collect();
        self.key_order.sort_by(|&a, &b| {
            self.doc_key[a as usize]
                .cmp(&self.doc_key[b as usize])
                .then(a.cmp(&b))
        });
    }

    /// Install deserialized keys. Crate-private, called only by [`Index::from_bytes`].
    pub(crate) fn set_key(&mut self, field: usize, doc_key: Vec<String>) {
        self.key_field = field;
        self.doc_key = doc_key;
        self.rebuild_key_order();
    }

    /// Install deserialized positions. Crate-private, called only by [`Index::from_bytes`].
    ///
    /// Validated there rather than here: `position_at` must have one entry per posting plus a
    /// terminator, and must be non-decreasing and in bounds, or `position_of` would hand out a
    /// slice of another posting's positions and the phrase verifier would confidently agree with
    /// it.
    pub(crate) fn set_position(&mut self, position: Vec<u32>, position_at: Vec<u64>) {
        self.position = position;
        self.position_at = position_at;
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
            // Positions are optional in exactly the same way, and are installed by
            // `set_position` when the two spans are non-empty.
            position: Vec::new(),
            position_at: Vec::new(),
            posting_base: Vec::new(),
            numeric_order: Vec::new(),
            // Installed by the caller via `set_key`, like every other optional section.
            doc_key: Vec::new(),
            key_field: usize::MAX,
            key_order: Vec::new(),
            collection_size: None,
            // Set by the caller after construction via `set_facet`; the facet sections are
            // optional, exactly like the prior and expansion sections above.
            facet_field: Vec::new(),
            facet_label: Vec::new(),
            facet_id: Vec::new(),
            numeric_field: Vec::new(),
            numeric_value: Vec::new(),
            doc_len,
            avg_len,
            doc_count,
            term_df: Vec::new(),
            term_page_off: Vec::new(),
            term_page_id: Vec::new(),
            term_page_bit: Vec::new(),
        };
        ix.rebuild_meta();
        Ok(ix)
    }

    /// One pass over the whole index, at build or load time, to fill the retrieval metadata.
    fn rebuild_meta(&mut self) {
        // The prefix sum that turns `(term, index in its list)` into the global posting slot
        // `position_at` is keyed by. Derived here rather than serialized: it is a restatement of
        // the posting lengths, and a stored copy could disagree with them after a format change.
        self.posting_base = Vec::with_capacity(self.posting.len() + 1);
        let mut acc = 0u64;
        self.posting_base.push(0);
        for list in &self.posting {
            acc += list.len() as u64;
            self.posting_base.push(acc);
        }

        // Stored per-term COUNT + sparse 64-doc presence so a live COUNT does not walk postings,
        // including overlapping disjunction/conjunction and deletions (AND-NOT `deleted`).
        self.term_df = self.posting.iter().map(|l| l.len() as u32).collect();
        let occupied: usize = self.posting.iter().map(|l| l.len()).sum();
        self.term_page_off = Vec::with_capacity(self.posting.len() + 1);
        self.term_page_id = Vec::with_capacity(occupied);
        self.term_page_bit = Vec::with_capacity(occupied);
        self.term_page_off.push(0);
        for list in &self.posting {
            let mut cur = u32::MAX;
            let mut bits = 0u64;
            for p in list {
                let pg = p.doc / 64;
                let bit = 1u64 << (p.doc % 64);
                if pg != cur {
                    if cur != u32::MAX {
                        self.term_page_id.push(cur);
                        self.term_page_bit.push(bits);
                    }
                    cur = pg;
                    bits = bit;
                } else {
                    bits |= bit;
                }
            }
            if cur != u32::MAX {
                self.term_page_id.push(cur);
                self.term_page_bit.push(bits);
            }
            self.term_page_off.push(self.term_page_id.len() as u32);
        }

        self.rebuild_key_order();

        // Value order per numeric column, so a sort can walk documents best-first instead of
        // visiting every match. Absent values are excluded here rather than filtered later: a
        // document with no price has no position in a price order, which is the same rule the
        // range filter and the histogram already follow.
        //
        // `total_cmp` because `sort_by` requires a total order and `f64` does not provide one;
        // NaN cannot reach here (it is filtered above) but `total_cmp` is total regardless.
        // One column per thread: the columns are independent, and a corpus rarely declares enough
        // of them for a finer split to be worth the machinery.
        let value = &self.numeric_value;
        let numeric_work = value.iter().map(Vec::len).sum::<usize>();
        let numeric_order = par_index_map(value.len(), numeric_work, |slot| {
            let column = &value[slot];
            let mut order: Vec<u32> = (0..column.len() as u32)
                .filter(|&d| column[d as usize].is_finite())
                .collect();
            order.sort_by(|&a, &b| column[a as usize].total_cmp(&column[b as usize]));
            order
        });
        self.numeric_order = numeric_order;

        // Fill each posting's saturated contribution first; everything else derives from it.
        //
        // `acc` — the total posting count — is the work estimate for every parallel region below:
        // term count alone is a bad proxy, because a corpus can have few terms carrying enormous
        // lists or a million terms carrying three postings each.
        //
        // The lists are moved out of `self` for the duration so the closure can hold `&self` for
        // `pseudo_tf`, which needs `doc_len`, `avg_len`, `schema` and `posting_tf`.
        let mut posting = std::mem::take(&mut self.posting);
        {
            let this = &*self;
            par_slice_mut(&mut posting, acc as usize, |t, list| {
                for (p, tf) in list.iter_mut().zip(this.posting_tf[t].iter()) {
                    p.sat = this.saturate(this.pseudo_tf(tf, p.doc));
                }
            });
        }
        self.posting = posting;

        // Block maxima, per-term maxima and champion lists in one pass: each depends only on its
        // own term's postings, which is what makes the whole derivation embarrassingly parallel.
        let meta: Vec<TermMeta> = {
            let posting = &self.posting;
            par_index_map(posting.len(), acc as usize, |t| {
                let list = &posting[t];
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
                let champion = match list.len() < CHAMPION_MIN_DF {
                    true => Vec::new(),
                    false => {
                        let mut idx: Vec<u32> = (0..list.len() as u32).collect();
                        // Highest saturated contribution first; document id breaks ties so the
                        // build is deterministic.
                        idx.sort_unstable_by(|&a, &b| {
                            list[b as usize]
                                .sat
                                .total_cmp(&list[a as usize].sat)
                                .then(list[a as usize].doc.cmp(&list[b as usize].doc))
                        });
                        idx.truncate(CHAMPION_SIZE);
                        idx
                    }
                };
                TermMeta {
                    max_sat: term_max,
                    block_last: last,
                    block_max: bmax,
                    champion,
                }
            })
        };

        // Moves only.
        self.max_sat = Vec::with_capacity(meta.len());
        self.block_last = Vec::with_capacity(meta.len());
        self.block_max = Vec::with_capacity(meta.len());
        self.champion = Vec::with_capacity(meta.len());
        for m in meta {
            self.max_sat.push(m.max_sat);
            self.block_last.push(m.block_last);
            self.block_max.push(m.block_max);
            self.champion.push(m.champion);
        }
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
        let bucket = Self::bucket_of(group_dist, group_is_quantity);
        Some(Hit {
            doc,
            // The prior scales the relevance score; it deliberately does NOT touch `typo_bucket`,
            // which stays the primary sort key. An important document with a misspelled match must
            // not outrank an exact match on an unimportant one — a prior expresses importance, not
            // correctness. The same holds for the exact-field factor below.
            score: score as f32
                * self.prior_of(doc)
                * self.anchor_factor(doc, anchor)
                * self.exact_field_factor(doc, bucket, group_is_quantity.len().max(1)),
            typo_bucket: bucket,
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
    ///
    /// `n` is the **collection** size when this index is part of one, not this segment's own
    /// document count. See [`Index::set_collection_size`] for why that difference is not small.
    #[inline]
    fn idf(&self, df: usize) -> f32 {
        let n = self.collection_size.unwrap_or(self.doc_count) as f32;
        let df = df as f32;
        // `df` cannot exceed a correctly set collection size, but clamping keeps the logarithm's
        // argument positive rather than producing NaN if one is ever set inconsistently.
        (1.0 + (n - df + 0.5).max(0.5) / (df + 0.5)).ln()
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
    fn plan(
        &self,
        query: &str,
        prefix_last: bool,
        cap: usize,
    ) -> (Vec<QueryTerm>, Vec<bool>, bool) {
        self.plan_stat(query, prefix_last, cap, None)
    }

    /// Expand a dictionary token, carrying the matched TEXT only when a collection stat needs it.
    ///
    /// An empty `String` does not allocate, so the single-index path pays nothing for the pair.
    #[inline]
    fn expand_pair(
        &self,
        token: &str,
        numeric: bool,
        prefix: bool,
        want_text: bool,
    ) -> Vec<(crate::dict::TermMatch, String)> {
        if want_text {
            self.dict.expand_lazy_text(token, numeric, prefix)
        } else {
            self.dict
                .expand_lazy(token, numeric, prefix)
                .into_iter()
                .map(|m| (m, String::new()))
                .collect()
        }
    }

    /// [`Index::plan`], scoring IDF against `stat` when this segment belongs to a collection.
    ///
    /// Two phases, in the shape `p52` named: [`Index::expand_query`] walks the dictionary once and
    /// [`Index::weigh`] applies the cap and the IDF. A caller that already holds an expansion —
    /// [`crate::Searcher`], which produces one per segment to build a [`CollectionStat`] — skips the
    /// walk entirely via `Scan::pre`.
    fn plan_stat(
        &self,
        query: &str,
        prefix_last: bool,
        cap: usize,
        stat: Option<&CollectionStat>,
    ) -> (Vec<QueryTerm>, Vec<bool>, bool) {
        let ex = self.expand_query(query, prefix_last, stat.is_some());
        self.weigh(&ex, cap, stat)
    }

    /// The weigh phase of planning: cap, IDF and weights over an expansion someone already built.
    ///
    /// Pure: it reads the segment's postings and `stat` and nothing else, so the same expansion
    /// weighs identically whether it arrived fresh from [`Index::expand_query`] or was handed over —
    /// which is the property that makes the hand-off unobservable in the results.
    fn weigh(
        &self,
        ex: &QueryExpansion,
        cap: usize,
        stat: Option<&CollectionStat>,
    ) -> (Vec<QueryTerm>, Vec<bool>, bool) {
        let mut out: Vec<QueryTerm> = Vec::new();
        for e in &ex.emits {
            self.emit(&mut out, e.matches.clone(), e.group, cap, stat);
        }
        (out, ex.group_is_quantity.clone(), ex.expanded)
    }

    /// The expand phase of planning: everything up to per-group match lists, with **no cap and no
    /// weights**.
    ///
    /// `want_text` decides whether matches carry their term text — the stat pass needs it to sum
    /// document frequency across segments by string; the single-index path passes `false` and pays
    /// nothing for the pair. The learned-expansion branch is part of this phase and is flagged
    /// `learned` on its emit entries, because [`Index::term_stat`] has always excluded those terms
    /// from the collection-wide sum while [`Index::weigh`] treats them like any other.
    pub(crate) fn expand_query(
        &self,
        query: &str,
        prefix_last: bool,
        want_text: bool,
    ) -> QueryExpansion {
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
        let mut emits: Vec<ExpansionEmit> = Vec::new();

        for (ti, t) in tok.iter().enumerate() {
            let is_last = ti + 1 == n;
            let matches =
                self.expand_pair(&t.text, t.is_numeric, prefix_last && is_last, want_text);

            if matches.is_empty() && t.text.chars().count() >= 4 {
                if let Some((a, b)) = self.split_compound(&t.text) {
                    for part in [a, b] {
                        let numeric = part.chars().any(|c| c.is_ascii_digit());
                        let gi = group_is_quantity.len() as u16;
                        group_is_quantity
                            .push(numeric && crate::analyze::parse_quantity(&part).is_some());
                        let m = self.expand_pair(&part, numeric, false, want_text);
                        emits.push(ExpansionEmit {
                            group: gi,
                            matches: m,
                            learned: false,
                        });
                    }
                    continue;
                }
            }

            let gi = group_is_quantity.len() as u16;
            group_is_quantity
                .push(t.is_numeric && crate::analyze::parse_quantity(&t.text).is_some());
            emits.push(ExpansionEmit {
                group: gi,
                matches,
                learned: false,
            });
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
            let key = tok
                .iter()
                .map(|t| t.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            if let Ok(at) = self
                .expansion
                .binary_search_by(|e| e.0.as_str().cmp(key.as_str()))
            {
                // Emitted as an ALTERNATIVE inside every group, not as a group of its own.
                //
                // `bucket_of` SUMS a penalty per unsatisfied group, so a group of expansion terms
                // would leave a document that matches only them paying `MISSING_TERM_PENALTY` for
                // every original token — ranking a genuine category member BELOW a document that
                // merely shares one word with the category's name. As alternatives, one expansion
                // hit satisfies the group it sits in, which is the intended meaning: *this document
                // belongs to the thing you asked for.*
                // The expansion table stores term IDS, not text, so these carry an empty string
                // and fall back to this segment's own `df`. Documented rather than hidden: a
                // learned-expansion query is the one case still scored per segment.
                let extra: Vec<(crate::dict::TermMatch, String)> = self.expansion[at]
                    .1
                    .iter()
                    .map(|&term_id| {
                        (
                            crate::dict::TermMatch {
                                term_id,
                                distance: EXPANSION_DISTANCE,
                            },
                            String::new(),
                        )
                    })
                    .collect();
                for gi in 0..group_is_quantity.len() as u16 {
                    emits.push(ExpansionEmit {
                        group: gi,
                        matches: extra.clone(),
                        learned: true,
                    });
                }
                expanded = true;
            }
        }

        QueryExpansion {
            emits,
            group_is_quantity,
            expanded,
        }
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
        let key = tok
            .iter()
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        match self
            .expansion
            .binary_search_by(|e| e.0.as_str().cmp(key.as_str()))
        {
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
    ///
    /// `matches` pairs each dictionary match with its TEXT, which is empty on the single-index path
    /// -- an empty `String` does not allocate, so that path pays nothing for carrying it. Pairing
    /// rather than keeping two parallel vectors matters because the expansion cap SORTS this list,
    /// and a parallel vector would silently desynchronise.
    fn emit(
        &self,
        out: &mut Vec<QueryTerm>,
        mut matches: Vec<(crate::dict::TermMatch, String)>,
        gi: u16,
        cap: usize,
        stat: Option<&CollectionStat>,
    ) {
        if matches.len() > cap {
            // Closest first; among equals, the rarer term is the more discriminative one.
            matches.sort_by_key(|(m, _)| (m.distance, self.posting[m.term_id as usize].len()));
            matches.truncate(cap);
        }
        for (m, text) in matches {
            let df = self.posting[m.term_id as usize].len();
            if df == 0 {
                continue;
            }
            // Collection-wide IDF when this segment is part of one, so a term is rare or common
            // according to the corpus rather than to the shard it landed in. See `CollectionStat`.
            let idf = match stat {
                Some(s) => s.idf(&text, df),
                None => self.idf(df),
            };
            let w = idf * self.schema.typo_penalty.powi(m.distance as i32);
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
        self.search_opt(Scan::new(query, k))
    }

    /// **Opt-in bounded-error search.** Identical to [`Index::search`] but caps how many dictionary
    /// terms each query token may expand to, at `cap` instead of the default 16.
    ///
    /// This is the ONE knob that trades correctness for tail latency, and it exists because
    /// `bench/roadmap/p27-group-seeding.md` closed the alternative: both pruning bounds are already
    /// exact and have no slack, so the typo tail is not an optimization problem. It is the cost of
    /// ranking every expansion of every token. The only remaining lever is to rank fewer of them.
    ///
    /// **The error is one-sided and predictable.** Expansions are ordered by edit distance first,
    /// then document frequency ascending, so a lower cap discards the vaguest and least
    /// discriminative matches first. It can only ever *lose* a hit that a rarer, more distant typo
    /// correction would have found; it never invents one and never reorders what it keeps.
    ///
    /// `cap == 0` is treated as 1 — a token always contributes at least its closest match, so a
    /// capped search still answers exact queries exactly.
    ///
    /// Measured tradeoff: `bench/roadmap/p29-expansion-cap.md`.
    pub fn search_capped(&self, query: &str, k: usize, cap: usize) -> Vec<Hit> {
        self.search_opt(Scan {
            cap: cap.max(1),
            ..Scan::new(query, k)
        })
    }

    /// **Which parts of `text` matched `query`** — byte ranges into `text`, ascending, non-overlapping.
    ///
    /// The index does not store field text, so the caller passes back the row it already has. That
    /// is the right shape for an embedded engine: the application owns the rows, and keeping a
    /// second copy purely to highlight them is the duplication this engine exists to avoid.
    ///
    /// Spans are computed through **the same analysis the index used**, so a typo-corrected hit
    /// highlights correctly — querying `"Colgte"` marks `Colgate` — and an aliased token marks the
    /// text that was actually written. A token matches when its term id is one the query planned,
    /// which includes every typo expansion the search itself considered.
    ///
    /// Returns an empty vector when nothing matched; never panics on text it did not index.
    pub fn highlight(&self, query: &str, text: &str) -> Vec<(usize, usize)> {
        let (term, _, _) = self.plan(query, false, MAX_EXPANSION);
        if term.is_empty() {
            return Vec::new();
        }
        let mut want: Vec<u32> = term.iter().map(|t| t.term_id).collect();
        want.sort_unstable();
        want.dedup();

        let mut tok = crate::analyze::tokenize_span(text);
        // The alias table rewrites token text in place, so spans stay aligned with their tokens.
        // Applying it matters: the index stored the canonical form, and a highlight that skipped
        // this step would fail to mark exactly the words an alias exists to catch.
        let mut just: Vec<Token> = tok.iter().map(|(t, _, _)| t.clone()).collect();
        apply_alias(&mut just, &self.alias);
        for (i, t) in just.into_iter().enumerate() {
            tok[i].0 = t;
        }

        let mut span: Vec<(usize, usize)> = tok
            .iter()
            .filter(|(t, _, _)| {
                self.term_id_of(&t.text)
                    .is_some_and(|id| want.binary_search(&id).is_ok())
            })
            .map(|(_, a, b)| (*a, *b))
            .collect();

        // Merge touching or overlapping spans so a caller can wrap each one in a tag without
        // producing `<b>Col</b><b>gate</b>`.
        span.sort_unstable();
        let mut out: Vec<(usize, usize)> = Vec::with_capacity(span.len());
        for (a, b) in span {
            match out.last_mut() {
                Some(prev) if a <= prev.1 => prev.1 = prev.1.max(b),
                _ => out.push((a, b)),
            }
        }
        out
    }

    /// **Faceted search**: [`Index::search`] restricted to documents whose stored facet value is
    /// exactly `value`. Returns empty if the index has no facet field or the value is unknown.
    ///
    /// This is filter-then-rank, not rank-then-filter: the filter is applied before a document is
    /// admitted to the pool, so asking for 10 hits in a rare category returns 10 if 10 exist,
    /// rather than however many survive from a global top-10.
    pub fn search_facet(&self, query: &str, k: usize, value: &str) -> Vec<Hit> {
        self.search_facet_all(query, k, &[(0, value)])
    }

    /// [`Index::search_facet`] against a specific facet slot.
    pub fn search_facet_at(&self, query: &str, k: usize, slot: usize, value: &str) -> Vec<Hit> {
        self.search_facet_all(query, k, &[(slot, value)])
    }

    /// **Conjunctive faceted search**: every `(slot, value)` pair must hold.
    ///
    /// This is what a storefront actually asks — brand *and* category *and* whatever else — and it
    /// is one pass, not an intersection of separate result sets. An unknown value in any pair
    /// returns empty, because a filter nobody can satisfy has no results; returning everything
    /// would be the dangerous reading.
    pub fn search_facet_all(&self, query: &str, k: usize, want: &[(usize, &str)]) -> Vec<Hit> {
        self.search_filtered(query, k, want, &[])
    }

    /// [`Index::search_clause`] without ranges or paging.
    pub fn search_any(&self, query: &str, k: usize, clause: &[FacetClause]) -> Vec<Hit> {
        self.search_clause(query, k, 0, clause, &[])
    }

    /// Search restricted to a numeric range on one column: `lo <= value < hi`.
    ///
    /// Half-open on purpose. Adjacent buckets in a price filter must not both contain the boundary,
    /// or the counts beside them add up to more than the result set.
    pub fn search_range(&self, query: &str, k: usize, slot: usize, lo: f64, hi: f64) -> Vec<Hit> {
        self.search_filtered(query, k, &[], &[(slot, lo, hi)])
    }

    /// **The full filter**: every facet pair and every numeric range must hold, in one pass.
    ///
    /// This is a storefront's whole filter bar -- brand, category and a price slider -- evaluated
    /// against each candidate once, rather than as an intersection of separately ranked result sets.
    pub fn search_filtered(
        &self,
        query: &str,
        k: usize,
        want: &[(usize, &str)],
        range: &[(usize, f64, f64)],
    ) -> Vec<Hit> {
        let clause: Vec<FacetClause> = want
            .iter()
            .map(|(slot, v)| FacetClause::any(*slot, std::slice::from_ref(v)))
            .collect();
        self.search_clause(query, k, 0, &clause, range)
    }

    /// **The full filter bar**: OR within a clause, AND across clauses, plus numeric ranges, plus
    /// an offset for paging.
    ///
    /// This is the entry point every other faceted search here is sugar over.
    pub fn search_clause(
        &self,
        query: &str,
        k: usize,
        offset: usize,
        clause: &[FacetClause],
        range: &[(usize, f64, f64)],
    ) -> Vec<Hit> {
        let Some(filter) = self.resolve_clause(clause) else {
            return Vec::new();
        };
        if range
            .iter()
            .any(|&(slot, _, _)| slot >= self.numeric_value.len())
        {
            return Vec::new();
        }
        self.search_opt(Scan {
            offset,
            facet: &filter,
            range,
            ..Scan::new(query, k)
        })
    }

    /// [`Index::search`] starting at `offset` — page `n` is `offset = n * k`.
    ///
    /// **Cost grows with `offset`, not with `k`.** The engine over-fetches `offset + k` and drops
    /// the prefix, because rank order is only known once everything above the page is scored. That
    /// is true of every engine without a stored cursor; it is stated here rather than discovered.
    /// For deep paging, filter instead of paging.
    pub fn search_page(&self, query: &str, offset: usize, k: usize) -> Vec<Hit> {
        self.search_opt(Scan {
            offset,
            ..Scan::new(query, k)
        })
    }

    /// **Sort by a numeric column** instead of by relevance: "price, low to high".
    ///
    /// Returns the `k` matching documents with the smallest (or largest) value in `slot`. Documents
    /// with no value there are excluded, exactly as they are from a range filter -- there is no
    /// position in a price order for a product with no price.
    ///
    /// # This is not a ranked search, and it costs accordingly
    ///
    /// Relevance ordering is what makes pruning possible: the engine can skip a document once no
    /// unseen document can beat the pool. **A numeric order gives it nothing to prune with** -- the
    /// cheapest item in the corpus may match the query worst, so every matching document has to be
    /// visited. The cost therefore tracks the query's *match count*, like [`Index::facet_tally`],
    /// not `k`.
    ///
    /// `score` and `typo_bucket` are still populated honestly, so a host can display relevance
    /// alongside a price sort; they simply do not determine the order.
    pub fn search_sorted(&self, query: &str, k: usize, slot: usize, ascending: bool) -> Vec<Hit> {
        self.search_sorted_filtered(query, k, slot, ascending, &[], &[])
    }

    /// [`Index::search_sorted`] with the full filter bar applied first.
    pub fn search_sorted_filtered(
        &self,
        query: &str,
        k: usize,
        slot: usize,
        ascending: bool,
        want: &[(usize, &str)],
        range: &[(usize, f64, f64)],
    ) -> Vec<Hit> {
        if k == 0 {
            return Vec::new();
        }
        let Some(column) = self.numeric_value.get(slot) else {
            return Vec::new();
        };
        let clause: Vec<FacetClause> = want
            .iter()
            .map(|(slot, v)| FacetClause::any(*slot, std::slice::from_ref(v)))
            .collect();
        let Some(filter) = self.resolve_clause(&clause) else {
            return Vec::new();
        };

        let (term, group_is_quantity, _) = self.plan(query, false, MAX_EXPANSION);
        if term.is_empty() {
            return Vec::new();
        }

        // ---- Which arm? -------------------------------------------------------------------
        //
        // `p32` established that a numeric order gives the scan nothing to stop on, and measured
        // the consequence: 8x the tail of a ranked search, because every matching document must be
        // visited. That is true of the SCAN. It is not true of the problem.
        //
        // Walking the value order instead lets the sort stop after `k` matches -- but then the cost
        // is "documents examined before `k` of them match", which is ruinous for a selective query
        // and excellent for a broad one. The scan is the mirror image. So both are kept, and the
        // cheaper is chosen from numbers the index already has:
        //
        //   scan   ~ sum of the query terms' document frequencies (every posting is visited)
        //   walk   ~ k * live / matches * terms (documents tried before k of them match)
        //
        // `sum(df)` OVER-estimates the match count whenever terms overlap, which makes the walk
        // look worse than it is and biases the choice toward the scan. That is the conservative
        // direction: the scan is the arm that has always been correct here.
        //
        // The two arms must agree exactly, ties included. `sorted_arms_agree_document_for_document`
        // is the differential test that says so, and is the only reason this is safe to ship.
        let df: usize = term
            .iter()
            .map(|t| self.posting[t.term_id as usize].len())
            .sum();
        let live = self.live_count().max(1);
        let order = self
            .numeric_order
            .get(slot)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let walk_cost = match df {
            0 => usize::MAX,
            _ => order
                .len()
                .min(k.saturating_mul(live) / df + k)
                .saturating_mul(term.len().max(1)),
        };
        match !order.is_empty() && walk_cost < df {
            true => self.sorted_by_walk(
                order,
                k,
                ascending,
                &term,
                &group_is_quantity,
                column,
                &filter,
                range,
            ),
            false => self.sorted_by_scan(
                k,
                ascending,
                &term,
                &group_is_quantity,
                column,
                &filter,
                range,
            ),
        }
    }

    /// Sort-by-value by scanning every posting. The arm `p32` shipped, unchanged.
    ///
    /// Cost tracks the query's MATCH COUNT, not `k`: with a numeric order there is nothing to
    /// prune on, so every matching document is visited and scored. That is not an implementation
    /// shortcut left to be optimised away -- it is what sorting by an unrelated column means for a
    /// posting scan. `sorted_by_walk` beats it only by giving up the posting scan entirely.
    #[allow(clippy::too_many_arguments)]
    fn sorted_by_scan(
        &self,
        k: usize,
        ascending: bool,
        term: &[QueryTerm],
        group_is_quantity: &[bool],
        column: &[f64],
        filter: &[(usize, Vec<u32>, bool)],
        range: &[(usize, f64, f64)],
    ) -> Vec<Hit> {
        let group_count = group_is_quantity.len().max(1);
        let mut acc: BTreeMap<u32, (f64, Vec<u8>)> = BTreeMap::new();
        for t in term {
            for p in &self.posting[t.term_id as usize] {
                if self.is_deleted(p.doc) {
                    continue;
                }
                // Filter before accumulating: a rejected document should cost one comparison, not
                // an allocation and a scoring pass.
                if !self.facet_allows(p.doc, filter) || !self.range_allows(p.doc, range) {
                    continue;
                }
                if !column.get(p.doc as usize).is_some_and(|v| v.is_finite()) {
                    continue;
                }
                let e = acc
                    .entry(p.doc)
                    .or_insert_with(|| (0.0, vec![u8::MAX; group_count]));
                e.0 += (t.weight * p.sat) as f64;
                let g = t.group as usize;
                e.1[g] = e.1[g].min(t.distance);
            }
        }

        let mut hit: Vec<(f64, Hit)> = acc
            .into_iter()
            .map(|(doc, (score, gd))| {
                let v = column[doc as usize];
                (
                    v,
                    Hit {
                        doc,
                        score: score as f32,
                        typo_bucket: Self::bucket_of(&gd, group_is_quantity),
                    },
                )
            })
            .collect();
        // Ties broken by rank, then by doc id, so a page of equally priced items is still ordered
        // by how well it matches and is stable across runs.
        hit.sort_by(|a, b| {
            let primary = if ascending {
                a.0.total_cmp(&b.0)
            } else {
                b.0.total_cmp(&a.0)
            };
            primary.then_with(|| rank_cmp(&a.1, &b.1))
        });
        hit.truncate(k);
        hit.into_iter().map(|(_, h)| h).collect()
    }

    /// Sort-by-value that STOPS EARLY, by walking the value order instead of the postings.
    ///
    /// The scan arm visits every matching document because a numeric order gives it no bound to
    /// prune on. This arm inverts the loop: documents are visited best-value-first, and once `k`
    /// of them have matched, no unvisited document can displace one -- every remaining document
    /// has a worse value by construction.
    ///
    /// **Except at a tie.** `p32` fixed that ties break by relevance and then by document id, so
    /// stopping at exactly `k` would return an arbitrary subset of the documents sharing the `k`-th
    /// value. The walk therefore continues while the value is unchanged, sorts the whole boundary
    /// group, and only then truncates. That is the one place this arm is not O(k).
    ///
    /// Matching is tested by binary search in each query term's posting list, which is why the
    /// choice between arms weighs `terms` against the postings the scan would have walked.
    #[allow(clippy::too_many_arguments)]
    fn sorted_by_walk(
        &self,
        order: &[u32],
        k: usize,
        ascending: bool,
        term: &[QueryTerm],
        group_is_quantity: &[bool],
        column: &[f64],
        filter: &[(usize, Vec<u32>, bool)],
        range: &[(usize, f64, f64)],
    ) -> Vec<Hit> {
        let group_count = group_is_quantity.len().max(1);
        let mut hit: Vec<(f64, Hit)> = Vec::with_capacity(k + 1);
        let mut boundary: Option<f64> = None;

        // `order` is ascending by value, so a descending sort walks it backwards. One array serves
        // both directions; storing a second reversed copy would be the same fact twice.
        let step: Box<dyn Iterator<Item = &u32>> = match ascending {
            true => Box::new(order.iter()),
            false => Box::new(order.iter().rev()),
        };

        for &doc in step {
            let value = column[doc as usize];
            // Past the boundary value, the group that could tie for `k`-th place is complete.
            if let Some(edge) = boundary {
                if value != edge {
                    break;
                }
            }
            if self.is_deleted(doc)
                || !self.facet_allows(doc, filter)
                || !self.range_allows(doc, range)
            {
                continue;
            }

            // Does the query match, and with what score? Same arithmetic as the scan arm, reached
            // from the other side: there, postings are walked and documents accumulated; here, a
            // document is fixed and its entry in each term's list is looked up.
            let mut score = 0.0f64;
            let mut group_dist = vec![u8::MAX; group_count];
            let mut matched = false;
            for t in term {
                let list = &self.posting[t.term_id as usize];
                if let Ok(i) = list.binary_search_by_key(&doc, |p| p.doc) {
                    matched = true;
                    score += (t.weight * list[i].sat) as f64;
                    let g = t.group as usize;
                    group_dist[g] = group_dist[g].min(t.distance);
                }
            }
            if !matched {
                continue;
            }

            hit.push((
                value,
                Hit {
                    doc,
                    score: score as f32,
                    typo_bucket: Self::bucket_of(&group_dist, group_is_quantity),
                },
            ));
            // The `k`-th match fixes the boundary value; everything sharing it must still be seen.
            if hit.len() == k {
                boundary = Some(value);
            }
        }

        hit.sort_by(|a, b| {
            let primary = if ascending {
                a.0.total_cmp(&b.0)
            } else {
                b.0.total_cmp(&a.0)
            };
            primary.then_with(|| rank_cmp(&a.1, &b.1))
        });
        hit.truncate(k);
        hit.into_iter().map(|(_, h)| h).collect()
    }

    /// Run one sort arm explicitly. **For differential testing and benchmarking only** —
    /// production code should call [`Index::search_sorted`], which picks the cheaper arm.
    ///
    /// It is public because the claim that the two arms return the same answer is only worth
    /// anything if it can be checked at scale, on a real corpus, by `bench/roadmap/p46`'s
    /// `sort-tail` bin — and neither arm is reachable from outside otherwise, since the chooser
    /// deliberately never runs both.
    ///
    /// `walk = true` is the value-order walk that stops early; `false` is the posting scan `p32`
    /// shipped. Filters are not exposed here: the arms are compared on the query, and the filter
    /// path is asserted separately.
    pub fn search_sorted_arm(
        &self,
        query: &str,
        k: usize,
        slot: usize,
        ascending: bool,
        walk: bool,
    ) -> Vec<Hit> {
        if k == 0 {
            return Vec::new();
        }
        let Some(column) = self.numeric_value.get(slot) else {
            return Vec::new();
        };
        let (term, group_is_quantity, _) = self.plan(query, false, MAX_EXPANSION);
        if term.is_empty() {
            return Vec::new();
        }
        let order = self
            .numeric_order
            .get(slot)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        match walk {
            true => self.sorted_by_walk(
                order,
                k,
                ascending,
                &term,
                &group_is_quantity,
                column,
                &[],
                &[],
            ),
            false => self.sorted_by_scan(k, ascending, &term, &group_is_quantity, column, &[], &[]),
        }
    }

    /// Does `doc` fall inside every `(slot, lo, hi)` half-open range?
    ///
    /// A `NaN` value fails every comparison, so a document with no value in that column is excluded
    /// rather than treated as zero.
    #[inline]
    fn range_allows(&self, doc: u32, range: &[(usize, f64, f64)]) -> bool {
        range.iter().all(|&(slot, lo, hi)| {
            self.numeric_value
                .get(slot)
                .and_then(|c| c.get(doc as usize))
                .is_some_and(|&v| v >= lo && v < hi)
        })
    }

    /// Histogram: how many documents matching `query` fall in each bucket of `edge`.
    ///
    /// Returns `edge.len() - 1` counts, bucket `i` being `edge[i] <= v < edge[i+1]`. Counted over
    /// **every** matching document, like [`Index::facet_tally`], because a price slider showing
    /// "12 items under 100" has to mean 12 results exist.
    pub fn range_tally(&self, query: &str, slot: usize, edge: &[f64]) -> Vec<usize> {
        if edge.len() < 2 {
            return Vec::new();
        }
        let Some(column) = self.numeric_value.get(slot) else {
            return Vec::new();
        };
        let (term, _, _) = self.plan(query, false, MAX_EXPANSION);
        let mut seen = vec![false; self.doc_count];
        let mut count = vec![0usize; edge.len() - 1];
        for t in &term {
            for p in &self.posting[t.term_id as usize] {
                let d = p.doc as usize;
                if seen[d] || self.is_deleted(p.doc) {
                    continue;
                }
                seen[d] = true;
                let Some(&v) = column.get(d) else { continue };
                // `partition_point` over the edges: the bucket is the last edge not exceeding `v`.
                // NaN fails every comparison, lands at 0, and is then rejected by the `v >= edge[0]`
                // guard -- absent values are counted nowhere.
                if !(v >= edge[0] && v < edge[edge.len() - 1]) {
                    continue;
                }
                let b = edge.partition_point(|&e| e <= v) - 1;
                count[b] += 1;
            }
        }
        count
    }

    /// Does `doc` satisfy every clause? Values within a clause are OR-ed, clauses are AND-ed.
    #[inline]
    fn facet_allows(&self, doc: u32, filter: &[(usize, Vec<u32>, bool)]) -> bool {
        filter.iter().all(|(slot, id, exclude)| {
            let has = self
                .facet_id
                .get(*slot)
                .and_then(|c| c.get(doc as usize))
                .is_some_and(|v| id.binary_search(v).is_ok());
            has != *exclude
        })
    }

    /// Score IDF against a collection of `n` documents rather than against this segment alone.
    ///
    /// Set by [`Searcher`] on every segment whenever the collection changes; `None` restores the
    /// standalone behaviour.
    ///
    /// **Nothing has to be recomputed.** The saturated posting contributions (`sat`, `block_max`,
    /// `max_sat`) depend on term frequency and field length, never on IDF, and IDF enters at query
    /// time as part of the term weight. So this is one write per segment, not a rebuild.
    pub fn set_collection_size(&mut self, n: Option<usize>) {
        self.collection_size = n;
    }

    /// The collection this segment scores against; its own document count when standalone.
    pub fn collection_size(&self) -> usize {
        self.collection_size.unwrap_or(self.doc_count)
    }

    /// Whether this index carries application keys.
    pub fn has_key(&self) -> bool {
        !self.doc_key.is_empty()
    }

    /// The schema field the key was taken from, if any.
    pub fn key_field(&self) -> Option<usize> {
        (self.key_field != usize::MAX).then_some(self.key_field)
    }

    /// The application's key for `doc`, or `None` if it has none.
    pub fn key_of(&self, doc: u32) -> Option<&str> {
        self.doc_key
            .get(doc as usize)
            .map(String::as_str)
            .filter(|k| !k.is_empty())
    }

    /// The document carrying `key`, or `None`.
    ///
    /// **Deleted documents are still found.** Resolution answers "which ordinal is this row",
    /// which a caller needs precisely in order to delete it; filtering here would make deleting an
    /// already-deleted row indistinguishable from deleting a row that never existed.
    ///
    /// When a key appears more than once in one index the LAST document carrying it wins, matching
    /// the newest-wins rule [`Searcher`] applies across segments.
    pub fn doc_of_key(&self, key: &str) -> Option<u32> {
        if key.is_empty() {
            return None;
        }
        // `key_order` is sorted by (key, ordinal), so the last entry of an equal run is the winner.
        let at = self
            .key_order
            .partition_point(|&d| self.doc_key[d as usize].as_str() <= key)
            .checked_sub(1)?;
        let doc = *self.key_order.get(at)?;
        (self.doc_key[doc as usize] == key).then_some(doc)
    }

    /// Every key in this index, ascending, with the document that owns it. Used by compaction and
    /// by the shadow-tombstoning in [`Searcher::push`].
    pub fn key_iter(&self) -> impl Iterator<Item = (&str, u32)> + '_ {
        self.key_order
            .iter()
            .map(move |&d| (self.doc_key[d as usize].as_str(), d))
    }

    /// How many documents carry a key. Less than `doc_count` when some key fields were blank —
    /// which is worth surfacing, because those rows can never be addressed by a change stream.
    pub fn keyed_count(&self) -> usize {
        self.key_order.len()
    }

    /// Whether this index can answer a phrase query. False unless it was built with
    /// [`IndexBuilder::with_position`].
    pub fn has_position(&self) -> bool {
        !self.position_at.is_empty()
    }

    /// The packed positions at which `term` occurs in `doc`, or `None` if it does not occur.
    ///
    /// Ascending, so the phrase verifier can binary-search them.
    fn position_of(&self, term: u32, doc: u32) -> Option<&[u32]> {
        let list = self.posting.get(term as usize)?;
        let i = list.binary_search_by_key(&doc, |p| p.doc).ok()?;
        let slot = (*self.posting_base.get(term as usize)? as usize) + i;
        let lo = *self.position_at.get(slot)? as usize;
        let hi = *self.position_at.get(slot + 1)? as usize;
        self.position.get(lo..hi)
    }

    /// Does `doc` contain `phrase` as CONSECUTIVE tokens of a single field?
    ///
    /// Anchored on the phrase's **rarest** term rather than its first: the loop is
    /// `occurrences of the anchor x phrase length`, and a phrase like `"the toothpaste"` has orders
    /// of magnitude more occurrences of `the` than of `toothpaste` to walk from.
    ///
    /// An empty phrase is vacuously satisfied, which is what makes `Scan`'s default -- no phrase --
    /// mean "no phrase constraint" rather than "match nothing".
    fn phrase_allows(&self, doc: u32, phrase: &[u32]) -> bool {
        if phrase.is_empty() {
            return true;
        }
        let mut at: Vec<&[u32]> = Vec::with_capacity(phrase.len());
        for &t in phrase {
            match self.position_of(t, doc) {
                Some(p) if !p.is_empty() => at.push(p),
                // A term absent from the document ends it: every phrase term must occur.
                _ => return false,
            }
        }
        let anchor = (0..at.len()).min_by_key(|&i| at[i].len()).unwrap_or(0);
        at[anchor].iter().any(|&a| {
            // Where the phrase would start if the anchor sits at `a`.
            let index = a & POSITION_INDEX_MASK;
            if (index as usize) < anchor {
                return false;
            }
            let start = a - anchor as u32;
            at.iter().enumerate().all(|(i, p)| {
                // The field is packed into the high bits, so `start + i` crossing a field boundary
                // simply fails to be found -- but only if it cannot WRAP into the next field's low
                // positions, which is why the index is bounded explicitly.
                let want_index = (start & POSITION_INDEX_MASK) as usize + i;
                want_index <= POSITION_INDEX_MASK as usize
                    && p.binary_search(&(start + i as u32)).is_ok()
            })
        })
    }

    /// Resolve clauses to per-slot label ids, or `None` when the filter cannot be satisfied at all.
    fn resolve_clause(&self, clause: &[FacetClause]) -> Option<Vec<(usize, Vec<u32>, bool)>> {
        let mut out = Vec::with_capacity(clause.len());
        for c in clause {
            let Some(label) = self.facet_label.get(c.slot) else {
                // A slot this index does not have is the unknown-value rule one level up, and it
                // takes the same two answers rather than collapsing both to "empty". A slot with
                // no values has, by definition, no KNOWN value: an include nobody can satisfy is
                // empty, an exclude with nothing to remove is vacuous.
                //
                // Collapsing the exclude to empty is not merely inconsistent, it is wrong across
                // segments: a newer delta may carry a slot an older segment was built without, and
                // "not discontinued" would then silently delete every older row from the results.
                if c.exclude {
                    continue;
                }
                return None;
            };
            let mut id: Vec<u32> = c
                .value
                .iter()
                .filter_map(|v| label.binary_search_by(|l| l.as_str().cmp(v)).ok())
                .map(|i| i as u32)
                .collect();
            id.sort_unstable();
            id.dedup();
            if id.is_empty() {
                // See `FacetClause`: an include nobody can satisfy is empty; an exclude with
                // nothing to exclude is vacuous and simply drops out of the conjunction.
                if c.exclude {
                    continue;
                }
                return None;
            }
            out.push((c.slot, id, c.exclude));
        }
        Some(out)
    }

    /// **Facet tally**: how many documents matching `query` carry each facet value.
    ///
    /// Counted over **every** document the query touches, not over the top `k`. That is the only
    /// count a shopper can act on -- "Snacks (412)" has to mean 412 results exist, not 412 within
    /// a page. It costs a walk of the query's posting lists with no pruning, because pruning exists
    /// to avoid scoring documents that cannot rank, and every one of them still counts.
    ///
    /// Deleted documents are excluded. Documents with no facet value are omitted rather than
    /// bucketed under an empty label. Returned sorted by count descending, then value ascending.
    pub fn facet_tally(&self, query: &str) -> Vec<(&str, usize)> {
        self.facet_tally_at(query, 0)
    }

    /// [`Index::facet_tally`] for a specific slot.
    pub fn facet_tally_at(&self, query: &str, slot: usize) -> Vec<(&str, usize)> {
        let (Some(label), Some(column)) = (self.facet_label.get(slot), self.facet_id.get(slot))
        else {
            return Vec::new();
        };
        let (term, _, _) = self.plan(query, false, MAX_EXPANSION);
        let mut seen = vec![false; self.doc_count];
        let mut count = vec![0usize; label.len()];
        for t in &term {
            for p in &self.posting[t.term_id as usize] {
                let d = p.doc as usize;
                if seen[d] || self.is_deleted(p.doc) {
                    continue;
                }
                seen[d] = true;
                if let Some(&f) = column.get(d) {
                    if f != u32::MAX {
                        count[f as usize] += 1;
                    }
                }
            }
        }
        let mut out: Vec<(&str, usize)> = count
            .iter()
            .enumerate()
            .filter(|(_, &c)| c > 0)
            .map(|(i, &c)| (label[i].as_str(), c))
            .collect();
        out.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        out
    }

    /// The distinct values of facet slot 0, sorted. Empty when the index has no facet field.
    pub fn facet_label(&self) -> &[String] {
        self.facet_label_at(0)
    }

    /// The distinct values of a facet slot, sorted.
    pub fn facet_label_at(&self, slot: usize) -> &[String] {
        self.facet_label.get(slot).map_or(&[], |l| l.as_slice())
    }

    /// How many facet slots this index carries.
    pub fn facet_slot_count(&self) -> usize {
        self.facet_label.len()
    }

    /// The schema field behind each facet slot, in slot order.
    pub fn facet_field(&self) -> &[usize] {
        &self.facet_field
    }

    /// How many numeric columns this index carries.
    pub fn numeric_slot_count(&self) -> usize {
        self.numeric_value.len()
    }

    /// The schema field behind each numeric slot, in slot order.
    pub fn numeric_field(&self) -> &[usize] {
        &self.numeric_field
    }

    /// The numeric value of a document in a given column, if it has a finite one.
    pub fn numeric_of(&self, doc: u32, slot: usize) -> Option<f64> {
        self.numeric_value
            .get(slot)?
            .get(doc as usize)
            .copied()
            .filter(|v| v.is_finite())
    }

    /// The stored facet value of a document in slot 0, if it has one.
    pub fn facet_of(&self, doc: u32) -> Option<&str> {
        self.facet_of_at(doc, 0)
    }

    /// The stored facet value of a document in a given slot, if it has one.
    pub fn facet_of_at(&self, doc: u32, slot: usize) -> Option<&str> {
        match self.facet_id.get(slot)?.get(doc as usize) {
            Some(&f) if f != u32::MAX => self
                .facet_label
                .get(slot)?
                .get(f as usize)
                .map(|s| s.as_str()),
            _ => None,
        }
    }

    /// Resolve `query` to the ordered term ids a phrase must match, or `None` if it cannot match.
    ///
    /// **A phrase is exact.** Quoting is the user asserting these words in this order, so no token
    /// is typo-corrected, prefix-expanded or alias-split here -- correcting inside a phrase would
    /// silently answer a different question than the one asked.
    ///
    /// A token absent from the dictionary therefore returns `None`, and the query matches
    /// **nothing**. That is the same direction as an include clause whose values are all unknown
    /// (see [`FacetClause`]): a constraint nobody can satisfy is empty, never everything.
    fn phrase_term(&self, query: &str) -> Option<Vec<u32>> {
        let mut tok = crate::analyze::tokenize(query);
        crate::analyze::apply_alias(&mut tok, &self.alias);
        tok.iter().map(|t| self.dict.exact(&t.text)).collect()
    }

    /// **Phrase query**: the query's tokens, consecutive and in order, within one field.
    ///
    /// Requires an index built with [`IndexBuilder::with_position`]; without positions there is
    /// nothing to verify against and this returns nothing rather than silently degrading to a
    /// bag-of-words search that would look like it worked.
    ///
    /// Ranking is unchanged -- BM25F over the same terms. The phrase is a FILTER applied after
    /// scoring and before admission, exactly like a facet clause, so it costs no pruning soundness
    /// and adds no ranking signal: `"ice cream"` ranks its matches the way `ice cream` would, and
    /// merely refuses the documents where the two words are apart.
    ///
    /// A single-token phrase is a plain term query with the same answer, and is allowed rather
    /// than special-cased.
    pub fn search_phrase(&self, query: &str, k: usize) -> Vec<Hit> {
        if !self.has_position() {
            return Vec::new();
        }
        match self.phrase_term(query) {
            Some(t) if !t.is_empty() => self.search_opt(Scan {
                phrase: &t,
                ..Scan::new(query, k)
            }),
            _ => Vec::new(),
        }
    }

    /// [`Index::search_phrase`] with an offset. Cost grows with `offset`; see
    /// [`Index::search_page`].
    pub fn search_phrase_page(&self, query: &str, offset: usize, k: usize) -> Vec<Hit> {
        if !self.has_position() {
            return Vec::new();
        }
        match self.phrase_term(query) {
            Some(t) if !t.is_empty() => self.search_opt(Scan {
                phrase: &t,
                offset,
                ..Scan::new(query, k)
            }),
            _ => Vec::new(),
        }
    }

    /// The `(term text, document frequency)` pairs this segment would plan for `query`.
    ///
    /// The first half of a `dfs_query_then_fetch`: [`Searcher`] sums these across segments to build
    /// a [`CollectionStat`], then searches with it, so every segment scores a term by how rare it
    /// is in the CORPUS rather than in the shard. Costs one dictionary expansion — the same one the
    /// search would do anyway, which is why the second pass is not twice the work it looks.
    ///
    /// **The expansion cap is deliberately NOT applied here.** A term this segment would cap away
    /// may survive the cap in another one, and it still needs a collection-wide `df` there. Over-
    /// collecting costs a few map entries; under-collecting would silently fall back to local `df`
    /// for exactly the terms the cap disagrees about.
    pub fn term_stat(&self, query: &str, prefix_last: bool) -> Vec<(String, usize)> {
        let ex = self.expand_query(query, prefix_last, true);
        self.expansion_stat(&ex)
    }

    /// The `(text, df)` pairs an already-built expansion contributes to a [`CollectionStat`].
    ///
    /// This is [`Index::term_stat`]'s body over a [`QueryExpansion`], so the stat pass and the
    /// search pass read ONE traversal rather than two. Learned-expansion emits are skipped, exactly
    /// as they always have been: they carry no text and score per segment by documented decision.
    pub(crate) fn expansion_stat(&self, ex: &QueryExpansion) -> Vec<(String, usize)> {
        let mut out = Vec::new();
        for e in &ex.emits {
            if e.learned {
                continue;
            }
            for (tm, text) in &e.matches {
                out.push((text.clone(), self.posting[tm.term_id as usize].len()));
            }
        }
        out
    }

    /// [`Index::search`], scored against collection-wide statistics.
    pub fn search_with_stat(&self, query: &str, k: usize, stat: &CollectionStat) -> Vec<Hit> {
        self.search_opt(Scan {
            stat: Some(stat),
            ..Scan::new(query, k)
        })
    }

    /// [`Index::search_with_stat`], weighed from an expansion the caller already built.
    ///
    /// The hand-off half of the `dfs_query_then_fetch`: the caller (the [`crate::Searcher`]) ran
    /// [`Index::expand_query`] per segment to collect `(text, df)`, and passes segment `i`'s
    /// expansion back to segment `i` so the weigh phase re-derives nothing. Results are
    /// bit-identical to [`Index::search_with_stat`], which re-walks the automaton — a property the
    /// searcher's own differential tests assert.
    pub(crate) fn search_expanded(
        &self,
        query: &str,
        ex: &QueryExpansion,
        k: usize,
        stat: &CollectionStat,
    ) -> Vec<Hit> {
        self.search_opt(Scan {
            pre: Some(ex),
            stat: Some(stat),
            ..Scan::new(query, k)
        })
    }

    /// [`Index::search_prefix`], scored against collection-wide statistics.
    pub fn search_prefix_with_stat(
        &self,
        query: &str,
        k: usize,
        stat: &CollectionStat,
    ) -> Vec<Hit> {
        self.search_opt(Scan {
            prefix_last: true,
            stat: Some(stat),
            ..Scan::new(query, k)
        })
    }

    /// [`Index::search_prefix_with_stat`] from a handed-over expansion, for the same reason as
    /// [`Index::search_expanded`].
    pub(crate) fn search_prefix_expanded(
        &self,
        query: &str,
        ex: &QueryExpansion,
        k: usize,
        stat: &CollectionStat,
    ) -> Vec<Hit> {
        self.search_opt(Scan {
            pre: Some(ex),
            prefix_last: true,
            stat: Some(stat),
            ..Scan::new(query, k)
        })
    }

    /// [`Index::search`] with typeahead semantics on the last token.
    pub fn search_prefix(&self, query: &str, k: usize) -> Vec<Hit> {
        self.search_opt(Scan {
            prefix_last: true,
            ..Scan::new(query, k)
        })
    }

    fn search_opt(&self, scan: Scan) -> Vec<Hit> {
        let Scan {
            query,
            k,
            offset,
            prefix_last,
            cap,
            facet,
            range,
            phrase,
            stat,
            pre,
        } = scan;
        // Deep paging is served by over-fetching and dropping: rank order is only known once
        // everything above the page has been scored, so the pool must hold `offset + k`. Cost
        // therefore grows with `offset`, which is true of every engine without a stored cursor and
        // is stated in `search_page`'s docs rather than left to be discovered.
        let k = k.saturating_add(offset);
        if k == 0 {
            return Vec::new();
        }
        let (mut term, group_is_quantity, expanded) = match pre {
            Some(ex) => self.weigh(ex, cap, stat),
            None => self.plan_stat(query, prefix_last, cap, stat),
        };
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
            true => term
                .iter()
                .filter(|t| t.group == 0)
                .map(|t| t.term_id)
                .collect(),
            false => Vec::new(),
        };
        // The SCORING pool. **It no longer carries recall, and that is `p47`.**
        //
        // It used to: it was `max(3k, 32)` so that the typo-bucket sort had material to reorder,
        // and the comment here priced that width against `real-corpus` recall. Since `p47` made
        // `eff` exactly the final comparator, the RANKING pool provably holds the top `k` by that
        // comparator, so no document reaching the answer depends on this pool at all.
        //
        // Its only remaining job is to supply `threshold` — and for that job a *smaller* pool is
        // strictly better, because the threshold is the pool's worst score and every extra slot
        // lowers it and weakens block skipping. Sized to `k`, measured on `scale`: exact p50 at
        // 1 M fell 35 % (820 us -> 529 us) and typo p50 22 %, with `pool-audit` still reading
        // 0.00 % in all six cells and `real-corpus` still at 100 % recall@10.
        //
        // The EXPANDED branch is left alone. `p21` widened it because a learned expansion adds
        // many terms whose matches are scoring competitors, and that fix was measured against
        // precision@10 on profstopick; it has not been re-swept under the new reasoning.
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
            true => (k * EXPANDED_POOL_MULT).max(EXPANDED_POOL_MIN),
            false => k.max(1),
        };
        // Everything below still reads `pool` as "the scoring pool's capacity"; only its size and
        // the reason for that size have changed.
        let group_count = group_is_quantity.len().max(1);

        // MaxScore requires terms ordered by ascending maximum contribution.
        term.sort_by(|a, b| {
            a.max_score
                .partial_cmp(&b.max_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut prefix_sum = vec![0.0f32; term.len() + 1];
        for i in 0..term.len() {
            prefix_sum[i + 1] = prefix_sum[i] + term[i].max_score;
        }

        // Larger than any score this query can produce: every term contributes at most its
        // `max_score`, and prior/anchor factors are <= 1. One unit of bucket therefore outweighs
        // every possible score difference, which is what makes `eff` exactly lexicographic.
        // ---- Bucket floor of an unenumerated document (p25) --------------------------------
        // Demoting a prefix of `term` to non-essential removes those postings from the candidate
        // enumeration, so a document appearing in NO essential term is never scored. Such a
        // document misses every query group lying wholly inside the essential set, and `bucket_of`
        // charges a fixed penalty per missed group — so its bucket has a computable FLOOR.
        //
        // `bucket_floor[f]` = the least bucket any document can have if it matches none of
        // `term[f..]`. It is non-increasing in `f` (demoting more terms leaves fewer groups wholly
        // essential), which is what lets it sit in the same monotone `while` as `prefix_sum`.
        //
        // A group with no surviving term at all is charged at every `f`: every document misses it
        // equally, including the ranking pool's worst member it is compared against.
        let mut group_min = vec![usize::MAX; group_count];
        for (i, t) in term.iter().enumerate() {
            let g = t.group as usize;
            if g < group_min.len() && i < group_min[g] {
                group_min[g] = i;
            }
        }
        let mut bucket_floor = vec![0u32; term.len() + 1];
        for (g, &mi) in group_min.iter().enumerate() {
            let pen = if group_is_quantity.get(g).copied().unwrap_or(false) {
                MISSING_QUANTITY_PENALTY
            } else {
                MISSING_TERM_PENALTY
            };
            // Group `g` is wholly inside `term[f..]` exactly while `f <= min index of g`.
            let upto = if mi == usize::MAX { term.len() } else { mi };
            for slot in bucket_floor.iter_mut().take(upto + 1) {
                *slot += pen;
            }
        }

        let bucket_scale = prefix_sum[term.len()] + 1.0;
        // `eff` is the FINAL comparator expressed as one number, and it has to be exactly that
        // rather than approximately that.
        //
        // It used to be built from the raw score while `rank_cmp` compares the QUANTIZED score
        // (`canon_score`, the grid that makes two accumulation orders of the same document agree).
        // Those two disagree whenever raw scores differ but quantize equal: `eff` then prefers the
        // higher raw score and `rank_cmp` prefers the lower document id. So the ranking pool could
        // evict a document the final ordering would have kept, and the union with the scoring pool
        // was what quietly covered for it.
        //
        // Quantizing here closes that gap, and the consequence is much larger than the gap:
        // **the ranking pool now provably holds the top `k` by the final comparator**, so a range
        // that cannot enter it cannot enter the answer -- which is what lets the block skip below
        // stop waiting on the scoring threshold. See `bench/roadmap/p47-typo-tail.md`.
        let eff_of = |score: f32, bucket: u32| canon_score(score) - bucket as f32 * bucket_scale;

        let mut cursor: Vec<usize> = vec![0; term.len()];
        let mut heap: std::collections::BinaryHeap<Candidate> =
            std::collections::BinaryHeap::with_capacity(pool + 1);
        // The ranking pool. Sized `k`, not `pool`: its only job is to guarantee the final top-`k`
        // by rank survives, and holding `pool` of them is wasted heap traffic on every candidate.
        //
        // **Exactly `k`, and the tightness matters twice over.** `p23` swept it from `k` to `16k`
        // against real presyo queries and got byte-identical degradation — the pool saturates at
        // `k`, which was itself the proof that what remained was skipped BEFORE scoring (no pool
        // can retain a document it never sees). `p25` then found where, and the demotion bound it
        // added compares against **this pool's worst member**: a bigger pool has a worse worst
        // member, which makes that bound harder to satisfy and suppresses demotion for no gain.
        // Shrinking `k.max(16)` to `k.max(1)` cut typo p99 at 1 M from 30.2 ms to 16.9 ms with the
        // audit still at 0.00 %. See `bench/roadmap/p25-essential-gate.md`.
        let rank_cap = k.max(1);
        let mut rank_pool: std::collections::BinaryHeap<RankCandidate> =
            std::collections::BinaryHeap::with_capacity(rank_cap + 1);
        let mut threshold = f32::NEG_INFINITY;
        // Per-group best distance for the document currently being scored.
        let mut group_dist = vec![u8::MAX; group_count];
        // Scratch for the block-range bucket floor (p26). Hoisted: the block-skip test runs on
        // every candidate and must not allocate.
        let mut group_reachable = vec![false; group_count];

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
                // Seeded documents enter the pools directly, so the filter applies here too.
                // `seeded` still records the document: the main loop must skip it either way.
                if !self.facet_allows(doc, facet)
                    || !self.range_allows(doc, range)
                    || !self.phrase_allows(doc, phrase)
                {
                    continue;
                }
                let c = Candidate {
                    eff: eff_of(hit.score, hit.typo_bucket),
                    score: hit.score,
                    doc,
                    bucket: hit.typo_bucket,
                };
                admit(&mut heap, &mut rank_pool, &mut threshold, pool, rank_cap, c);
            }
        }
        seeded.sort_unstable();

        loop {
            // The non-essential prefix is the longest run of terms whose combined maximum score
            // cannot, on its own, reach the current threshold.
            let mut first_essential = 0usize;
            // **Gated on the BUCKET FLOOR, not on `prune_is_sound`** — see
            // `bench/roadmap/p25-essential-gate.md`.
            //
            // This site does not skip documents, it stops generating them, so the question is not
            // "can an unseen document beat the pool on score?" but "can it beat the pool on
            // BUCKET?". Demotion is safe once a document matching none of the remaining essential
            // terms is guaranteed a bucket strictly worse than the ranking pool's worst member:
            // it then cannot enter the ranking pool however it scores, and the `prefix_sum` test
            // beside it covers the scoring pool.
            //
            // Reusing `prune_is_sound` here is correct and costs 20.7x, because it is false
            // whenever the pool's worst member has a bucket above 0 — the common case — which
            // pins `first_essential` at 0 and turns MaxScore into a full OR scan. The floor is
            // *true* in exactly that case: a pool full of high-bucket documents is easy to beat.
            // The ranking pool's worst `eff`, once it is full. `eff` is exactly lexicographic
            // `(bucket, score)`, so one number decides admission and the bounds below can be
            // compared against it directly instead of against bucket and score separately.
            let rank_worst_eff = if rank_pool.len() >= rank_cap {
                rank_pool.peek().map(|w| w.0.eff)
            } else {
                None
            };
            if heap.len() >= pool {
                if let Some(worst_eff) = rank_worst_eff {
                    // A document matching none of `term[f..]` has `bucket >= bucket_floor[f]` and
                    // `score <= prefix_sum[f]`, so its `eff` is at most
                    // `prefix_sum[f] - bucket_floor[f] * bucket_scale`. Demotion is safe once that
                    // cannot beat the ranking pool's worst member. Both terms are monotone in `f`
                    // (score bound up, bucket floor down), so the bound only tightens and this
                    // stays a single forward scan.
                    while first_essential < term.len()
                        && prefix_sum[first_essential + 1] <= threshold
                        && canon_score(prefix_sum[first_essential + 1])
                            - bucket_floor[first_essential + 1] as f32 * bucket_scale
                            <= worst_eff
                    {
                        first_essential += 1;
                    }
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
                // Two independent reasons this range can be skipped, and the second is what
                // keeps typo tail latency finite (p26).
                //
                //   1. `prune_is_sound` -- the ranking pool is full of bucket-0 documents, so
                //      entering it requires beating a SCORE, which `range_bound` bounds.
                //   2. The range has a BUCKET FLOOR worse than the pool's worst member. A group is
                //      unreachable inside `[candidate, range_end)` when every one of its terms has
                //      its cursor already past `range_end`; no document in the range can match it,
                //      so all of them pay its penalty. Same argument as p25's demotion bound,
                //      applied to a document range instead of a term suffix.
                //
                // Reason 2 matters precisely where reason 1 fails: a pool whose worst member has a
                // high bucket is easy to beat on bucket. With only reason 1, a typo query whose
                // pool never saturates at bucket 0 skips nothing and scans every posting.
                let mut can_skip = range_bound <= threshold && prune_is_sound(&rank_pool, rank_cap);
                // Reason 2 is NOT gated on the scoring threshold, and that is `p47`.
                //
                // It used to be, which made it useless in exactly the case it was built for: a typo
                // query whose pool never saturates at bucket 0 also has a low score threshold, so
                // `range_bound <= threshold` was false and the bucket test was never reached. The
                // two conditions failed together.
                //
                // Dropping the gate is sound because `eff` is now exactly the final comparator, so
                // the ranking pool holds the top `k` by that comparator and nothing else can reach
                // the answer. Skipping a range that cannot enter the ranking pool can therefore
                // only cost the SCORING pool candidates -- and the scoring pool contributes no
                // document that survives the final truncation.
                if !can_skip {
                    if let Some(worst_eff) = rank_worst_eff {
                        // How many groups must be UNREACHABLE in this range for a skip to hold.
                        //
                        // The exact test is `canon(range_bound) - floor * bucket_scale <= worst_eff`,
                        // so it needs `floor >= (canon(range_bound) - worst_eff) / bucket_scale`.
                        // Computing that first turns both common outcomes into two float ops:
                        //
                        //   need <= 0           -> the range loses on SCORE alone; skip, no loop.
                        //   need > group_count  -> no floor can reach it; do not look.
                        //
                        // Only the band between them pays for the reachability loop. Without this,
                        // ungating the test ran an O(terms) loop on every candidate and cost more
                        // than it saved: 16.4 ms against a 13.3 ms baseline, which is how it was
                        // found. The bound is unchanged; only the order of evaluation is.
                        let need = (canon_score(range_bound) - worst_eff) / bucket_scale;
                        if need <= 0.0 {
                            can_skip = true;
                        } else if need <= group_count as f32 {
                            // A group is unreachable inside `[candidate, range_end)` when every one
                            // of its terms has its cursor already past `range_end`: no document in
                            // the range can match it, so all of them pay its penalty. Same argument
                            // as p25's demotion bound, applied to a document range instead of a
                            // term suffix.
                            group_reachable.iter_mut().for_each(|r| *r = false);
                            for (i, t) in term.iter().enumerate() {
                                let list = &self.posting[t.term_id as usize];
                                if cursor[i] < list.len() && list[cursor[i]].doc < range_end {
                                    let g = t.group as usize;
                                    if g < group_reachable.len() {
                                        group_reachable[g] = true;
                                    }
                                }
                            }
                            let mut range_floor = 0u32;
                            for (g, &reachable) in group_reachable.iter().enumerate() {
                                if !reachable {
                                    range_floor +=
                                        if group_is_quantity.get(g).copied().unwrap_or(false) {
                                            MISSING_QUANTITY_PENALTY
                                        } else {
                                            MISSING_TERM_PENALTY
                                        };
                                }
                            }
                            // Exact: a document in the range has `bucket >= range_floor` and
                            // `score <= range_bound`, so its `eff` is at most
                            // `canon(range_bound) - range_floor * bucket_scale`.
                            //
                            // The strict form `range_floor > worst_bucket` was tried first (p26) and
                            // fires on 0.02 % of evaluations, because floor and worst bucket are
                            // almost always EQUAL. Equality is the common case and it is decidable:
                            // on a bucket tie the comparison falls through to score, which is
                            // exactly what `eff` encodes.
                            can_skip = canon_score(range_bound) - range_floor as f32 * bucket_scale
                                <= worst_eff;
                        }
                    }
                }
                if can_skip {
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
                if score as f32 + prefix_sum[i + 1] <= threshold
                    && prune_is_sound(&rank_pool, rank_cap)
                {
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
            // The bucket is needed BEFORE the score is finalised, because the exact-field factor
            // depends on it: only a document that matched every group can be "exactly the query".
            let bucket = Self::bucket_of(&group_dist, &group_is_quantity);
            let score = score as f32
                * self.prior_of(candidate)
                * self.anchor_factor(candidate, &anchor)
                * self.exact_field_factor(candidate, bucket, group_count);

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
            // Admission and eviction are both in EFF space, so the pool keeps exactly the
            // documents the final ranking would keep. The pruning tests above compare a
            // score-space upper bound against this threshold, which is sound because the best
            // reachable `eff` for an unscored document is its score bound at bucket 0 — never
            // more. When the pool still holds poor buckets the threshold is very negative and
            // little is pruned; as good matches arrive it rises and skipping re-engages.
            // Facet filter. Applied AFTER scoring and BEFORE admission, which is what keeps
            // pruning sound: the thresholds are then derived from admitted (allowed) documents
            // only, and a block-max bound over all documents is still an upper bound over the
            // allowed subset of them. Filtering earlier -- skipping the posting entirely -- would
            // be faster and would break `first_essential`'s accounting, which assumes every
            // enumerated document is scored.
            //
            // The phrase constraint joins them here for the same reason and with the same cost
            // shape: it is a predicate on an already-scored document, so it narrows the result
            // without touching a single pruning bound.
            if !self.facet_allows(candidate, facet)
                || !self.range_allows(candidate, range)
                || !self.phrase_allows(candidate, phrase)
            {
                continue;
            }
            let c = Candidate {
                eff: eff_of(score, bucket),
                score,
                doc: candidate,
                bucket,
            };
            admit(&mut heap, &mut rank_pool, &mut threshold, pool, rank_cap, c);
        }

        // Union of the two pools, deduplicated by document. The ranking pool contributes exactly
        // the documents the scoring pool would have dropped for being cheap-but-correct.
        // Concatenate, then sort-and-dedup by document. An earlier version binary-searched a
        // vector it was simultaneously pushing to, so the tail was unsorted and the same document
        // could be admitted twice — caught immediately by
        // `block_max_pruning_agrees_with_exhaustive_or_at_scale` as duplicate hits.
        let mut merged: Vec<Candidate> = heap.into_iter().collect();
        merged.extend(rank_pool.into_iter().map(|RankCandidate(c)| c));
        merged.sort_unstable_by_key(|c| c.doc);
        merged.dedup_by_key(|c| c.doc);
        let mut heap: Vec<Hit> = merged
            .into_iter()
            .map(|c| Hit {
                doc: c.doc,
                score: c.score,
                typo_bucket: c.bucket,
            })
            .collect();

        // Stage two: strict typo bucket first, then score, then doc id for a deterministic
        // tiebreak (presyo pins exactly this property in its contract test).
        heap.sort_by(rank_cmp);
        heap.truncate(k);
        if offset > 0 {
            heap.drain(..offset.min(heap.len()));
        }
        heap
    }

    /// Diagnostic: run a query and report how much work retrieval actually did.
    ///
    /// Returns `(hits, query_terms, postings_scored)`. Exists because three separate optimization
    /// attempts were made on hypotheses about where time went, and only measurement settled it.
    pub fn search_stat(&self, query: &str, k: usize) -> (Vec<Hit>, usize, u64) {
        let (term, _, _) = self.plan(query, false, MAX_EXPANSION);
        let total: u64 = term
            .iter()
            .map(|t| self.posting[t.term_id as usize].len() as u64)
            .sum();
        (self.search(query, k), term.len(), total)
    }

    /// **`COUNT(*)` of documents containing ANY query token**, exactly as written: no typo
    /// expansion, no prefix, no ranking. Deleted documents are not counted.
    ///
    /// A count is a set question, so it skips everything [`Index::search`] exists for — scoring,
    /// pools, the typo bucket — and answers from stored df / sparse 64-doc page presence. Tokens
    /// absent from the dictionary contribute nothing. Measured against TIN's published count row
    /// in `p93`.
    pub fn count_any(&self, query: &str) -> usize {
        self.count_any_cost(query).0
    }

    /// `(count, posting_entry_visited)`. The page-presence path visits 0 postings.
    pub fn count_any_cost(&self, query: &str) -> (usize, usize) {
        let mut id: Vec<u32> = self.count_token(query).into_iter().flatten().collect();
        id.sort_unstable();
        id.dedup();
        if let Some(n) = self.count_union_page(&id) {
            return (n, 0);
        }
        let list = self.count_list(query);
        let total: usize = list.iter().map(|l| l.len()).sum();
        let n = match list.len() {
            0 => 0,
            1 => list[0].iter().filter(|p| !self.is_deleted(p.doc)).count(),
            // Dense unions go through a bitmap sized to the corpus; sparse ones are merged, so a
            // query of two rare words never allocates `doc_count / 8` bytes.
            _ if total > self.doc_count / 16 => {
                let mut bit = vec![0u64; self.doc_count.div_ceil(64)];
                for l in &list {
                    for p in l.iter() {
                        bit[p.doc as usize / 64] |= 1u64 << (p.doc % 64);
                    }
                }
                for (w, d) in bit.iter_mut().zip(self.deleted.iter()) {
                    *w &= !d;
                }
                bit.iter().map(|w| w.count_ones() as usize).sum()
            }
            _ => {
                let mut doc: Vec<u32> = list.iter().flat_map(|l| l.iter().map(|p| p.doc)).collect();
                doc.sort_unstable();
                doc.dedup();
                doc.iter().filter(|&&d| !self.is_deleted(d)).count()
            }
        };
        (n, total)
    }

    fn term_page_slice(&self, t: u32) -> Option<(&[u32], &[u64])> {
        let t = t as usize;
        let off = &self.term_page_off;
        if t + 1 >= off.len() {
            return None;
        }
        let a = off[t] as usize;
        let b = off[t + 1] as usize;
        if b > self.term_page_id.len() || b > self.term_page_bit.len() {
            return None;
        }
        Some((&self.term_page_id[a..b], &self.term_page_bit[a..b]))
    }

    #[inline]
    fn page_live(&self, page: u32, mut bits: u64) -> u32 {
        if !self.deleted.is_empty() {
            if let Some(&d) = self.deleted.get(page as usize) {
                bits &= !d;
            }
        }
        bits.count_ones()
    }

    /// Sparse page-presence COUNT of a disjunction. Handles overlap and deletions. `None` only
    /// when the derived maps are missing (before `rebuild_meta`).
    fn count_union_page(&self, id: &[u32]) -> Option<usize> {
        if id.is_empty() {
            return Some(0);
        }
        if self.term_df.len() != self.posting.len() {
            return None;
        }
        if id.len() == 1 && self.deleted_count == 0 {
            return Some(self.term_df[id[0] as usize] as usize);
        }
        if self.term_page_off.len() != self.posting.len() + 1 {
            return None;
        }
        let mut slice: Vec<(&[u32], &[u64])> = Vec::with_capacity(id.len());
        for &t in id {
            slice.push(self.term_page_slice(t)?);
        }
        if slice.len() == 1 {
            let (page, bit) = slice[0];
            let mut n = 0usize;
            for i in 0..page.len() {
                n += self.page_live(page[i], bit[i]) as usize;
            }
            return Some(n);
        }
        let page_count = self.doc_count.div_ceil(64);
        let occupied_count: usize = slice.iter().map(|(page, _)| page.len()).sum();
        // Dense OR into a corpus-sized page bitmap when the terms cover enough pages that a
        // k-way merge would thrash. TIN's Wikipedia COUNT is this shape: 2–15 overlapping
        // terms, page bits OR'd, popcount. Scratch is thread-local so the alloc is once.
        if page_count > 0 && occupied_count > page_count / 8 {
            return Some(self.count_union_dense(&slice, page_count));
        }
        let mut at = vec![0usize; slice.len()];
        let mut n = 0usize;
        loop {
            let mut min_page = u32::MAX;
            for (i, (page, _)) in slice.iter().enumerate() {
                if at[i] < page.len() {
                    min_page = min_page.min(page[at[i]]);
                }
            }
            if min_page == u32::MAX {
                break;
            }
            let mut bits = 0u64;
            for (i, (page, bit)) in slice.iter().enumerate() {
                if at[i] < page.len() && page[at[i]] == min_page {
                    bits |= bit[at[i]];
                    at[i] += 1;
                }
            }
            n += self.page_live(min_page, bits) as usize;
        }
        Some(n)
    }

    fn count_union_dense(&self, slice: &[(&[u32], &[u64])], page_count: usize) -> usize {
        COUNT_ACC.with(|cell| {
            let mut acc = cell.borrow_mut();
            if acc.len() < page_count {
                acc.resize(page_count, 0);
            } else {
                acc[..page_count].fill(0);
            }
            for (page, bit) in slice {
                for i in 0..page.len() {
                    acc[page[i] as usize] |= bit[i];
                }
            }
            let live = &mut acc[..page_count];
            if !self.deleted.is_empty() {
                for (a, d) in live.iter_mut().zip(self.deleted.iter()) {
                    *a &= !*d;
                }
            }
            live.iter().map(|w| w.count_ones() as usize).sum()
        })
    }

    /// Sparse page-presence COUNT of a conjunction. `None` only when the derived maps are missing.
    fn count_intersect_page(&self, id: &[u32]) -> Option<usize> {
        if id.is_empty() {
            return Some(0);
        }
        if id.len() == 1 {
            return self.count_union_page(id);
        }
        if self.term_page_off.len() != self.posting.len() + 1 {
            return None;
        }
        let mut slice: Vec<(&[u32], &[u64])> = Vec::with_capacity(id.len());
        for &t in id {
            let s = self.term_page_slice(t)?;
            if s.0.is_empty() {
                return Some(0);
            }
            slice.push(s);
        }
        slice.sort_unstable_by_key(|(page, _)| page.len());
        let (head_page, head_bit) = slice[0];
        let rest = &slice[1..];
        let mut n = 0usize;
        for i in 0..head_page.len() {
            let page = head_page[i];
            let mut bits = head_bit[i];
            let mut miss = false;
            for (pg, bit) in rest {
                match pg.binary_search(&page) {
                    Ok(j) => bits &= bit[j],
                    Err(_) => {
                        miss = true;
                        break;
                    }
                }
                if bits == 0 {
                    miss = true;
                    break;
                }
            }
            if miss || bits == 0 {
                continue;
            }
            n += self.page_live(page, bits) as usize;
        }
        Some(n)
    }

    /// **`COUNT(*)` of documents containing EVERY query token**, exactly as written. A token absent
    /// from the dictionary makes the answer 0, which is what a conjunction means.
    pub fn count_all(&self, query: &str) -> usize {
        self.count_all_cost(query).0
    }

    /// `(count, posting_entry_visited)`. The page-presence path visits 0 postings.
    pub fn count_all_cost(&self, query: &str) -> (usize, usize) {
        let tok = self.count_token(query);
        if tok.is_empty() {
            return (0, 0);
        }
        let Some(mut id) = tok.into_iter().collect::<Option<Vec<u32>>>() else {
            return (0, 0);
        };
        id.sort_unstable();
        id.dedup();
        if let Some(n) = self.count_intersect_page(&id) {
            return (n, 0);
        }
        let mut list: Vec<&[Posting]> = id
            .iter()
            .map(|&t| self.posting[t as usize].as_slice())
            .collect();
        // Shortest first: every candidate comes from the rarest list, and each longer list is
        // probed by a forward-only binary search, so the cost follows the rarest term.
        list.sort_unstable_by_key(|l| l.len());
        let (head, rest) = list.split_first().expect("non-empty");
        let mut from = vec![0usize; rest.len()];
        let mut n = 0;
        'doc: for p in head.iter() {
            for (l, at) in rest.iter().zip(from.iter_mut()) {
                *at += l[*at..].partition_point(|q| q.doc < p.doc);
                if *at >= l.len() {
                    break 'doc;
                }
                if l[*at].doc != p.doc {
                    continue 'doc;
                }
            }
            if !self.is_deleted(p.doc) {
                n += 1;
            }
        }
        (n, head.len())
    }

    /// **`COUNT(*)` of documents containing the query's tokens consecutive and in order in one
    /// field.** Lucene PhraseQuery with slop 0: order matters; reversed is a different phrase.
    ///
    /// No ranking, no typo expansion, no bag-of-words fallback. An index built without positions
    /// returns 0, matching [`Index::search_phrase`]. An empty query or a token absent from the
    /// dictionary is 0. Deleted documents are not counted.
    pub fn count_phrase(&self, query: &str) -> usize {
        if !self.has_position() {
            return 0;
        }
        let Some(phrase) = self.phrase_term(query) else {
            return 0;
        };
        if phrase.is_empty() {
            return 0;
        }
        // Every candidate comes from the rarest term; `phrase_allows` then checks consecutive
        // in-order positions in one field. Postings are one entry per document.
        let rarest = phrase
            .iter()
            .copied()
            .min_by_key(|&t| self.posting[t as usize].len())
            .expect("non-empty");
        let mut n = 0;
        for p in &self.posting[rarest as usize] {
            if self.is_deleted(p.doc) {
                continue;
            }
            if self.phrase_allows(p.doc, &phrase) {
                n += 1;
            }
        }
        n
    }

    /// The query's tokens through the index's own analysis, resolved to exact term ids.
    fn count_token(&self, query: &str) -> Vec<Option<u32>> {
        let mut tok = crate::analyze::tokenize(query);
        crate::analyze::apply_alias(&mut tok, &self.alias);
        tok.iter().map(|t| self.dict.exact(&t.text)).collect()
    }

    /// Distinct posting lists of the query tokens present in the dictionary.
    fn count_list(&self, query: &str) -> Vec<&[Posting]> {
        let mut id: Vec<u32> = self.count_token(query).into_iter().flatten().collect();
        id.sort_unstable();
        id.dedup();
        id.iter()
            .map(|&t| self.posting[t as usize].as_slice())
            .collect()
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
    /// Brute-force reference ordering: score every matching document, sort by [`rank_cmp`], done.
    ///
    /// **No pool, no pruning, no block skipping.** This is the ground truth that neither
    /// [`Self::search`] nor [`Self::search_exhaustive`] can provide, because both apply the same
    /// candidate pool — and the pool is exactly what
    /// `bench/roadmap/p21-pool-eviction.md` needs to test. It exists for differential testing and
    /// is O(all postings); do not put it in a query path.
    pub fn search_exhaustive_unpooled(&self, query: &str, k: usize) -> Vec<Hit> {
        let (term, group_is_quantity, _) = self.plan(query, false, MAX_EXPANSION);
        let group_count = group_is_quantity.len().max(1);
        let mut acc: BTreeMap<u32, (f64, Vec<u8>)> = BTreeMap::new();
        for t in &term {
            for p in &self.posting[t.term_id as usize] {
                if self.is_deleted(p.doc) {
                    continue;
                }
                let e = acc
                    .entry(p.doc)
                    .or_insert_with(|| (0.0, vec![u8::MAX; group_count]));
                e.0 += (t.weight * p.sat) as f64;
                let g = t.group as usize;
                e.1[g] = e.1[g].min(t.distance);
            }
        }
        let mut hit: Vec<Hit> = acc
            .into_iter()
            .map(|(doc, (score, gd))| {
                let bucket = Self::bucket_of(&gd, &group_is_quantity);
                Hit {
                    doc,
                    score: score as f32
                        * self.prior_of(doc)
                        * self.exact_field_factor(doc, bucket, group_count),
                    typo_bucket: bucket,
                }
            })
            .collect();
        hit.sort_by(rank_cmp);
        hit.truncate(k);
        hit
    }

    pub fn search_exhaustive(&self, query: &str, k: usize) -> Vec<Hit> {
        let (term, group_is_quantity, _) = self.plan(query, false, MAX_EXPANSION);
        let group_count = group_is_quantity.len().max(1);
        let mut acc: BTreeMap<u32, (f64, Vec<u8>)> = BTreeMap::new();
        for t in &term {
            for p in &self.posting[t.term_id as usize] {
                let e = acc
                    .entry(p.doc)
                    .or_insert_with(|| (0.0, vec![u8::MAX; group_count]));
                e.0 += (t.weight * p.sat) as f64;
                let g = t.group as usize;
                e.1[g] = e.1[g].min(t.distance);
            }
        }
        let mut hit: Vec<Hit> = acc
            .into_iter()
            .map(|(doc, (score, gd))| {
                let bucket = Self::bucket_of(&gd, &group_is_quantity);
                Hit {
                    doc,
                    // No prior here by long-standing design (see this function's docs); the
                    // exact-field factor IS applied, because it is part of the ranking contract
                    // rather than an importance signal, and an oracle that skipped it would
                    // disagree with `search` about ordering for reasons unrelated to pruning.
                    score: score as f32 * self.exact_field_factor(doc, bucket, group_count),
                    typo_bucket: bucket,
                }
            })
            .collect();
        // Mirror `search`'s pool semantics exactly: best `pool` by score, then bucket sort.
        let pool = (k * 3).max(32);
        hit.sort_by(|a, b| {
            // Same quantization as `rank_cmp`, and for the same reason: a tolerance compare
            // is not transitive and `sort_by` is entitled to panic on it.
            canon_score(b.score)
                .total_cmp(&canon_score(a.score))
                .then_with(|| a.doc.cmp(&b.doc))
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

    /// A corpus big enough that `build()` crosses [`PARALLEL_MIN_WORK`] on its posting-side
    /// regions, built from a deterministic recombination so the test is reproducible.
    fn wide_corpus() -> Vec<[String; 3]> {
        let word = [
            "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel",
        ];
        (0..7_000)
            .map(|i| {
                let w = |k: usize| word[(i * 7 + k * 13) % word.len()];
                [
                    format!("{} {} {}", w(0), w(1), i % 97),
                    format!("{} {} store {}", w(2), w(3), i % 31),
                    format!("{} region", w(4)),
                ]
            })
            .collect()
    }

    fn wide_index() -> Index {
        let schema = Schema::new(vec![
            Field::new("name", 3.0, 0.4),
            Field::new("locality", 1.5, 0.5),
            Field::new("region", 1.0, 0.6),
        ]);
        let mut b = IndexBuilder::new(schema);
        for row in wide_corpus() {
            b.add(&Doc::new([
                row[0].as_str(),
                row[1].as_str(),
                row[2].as_str(),
            ]));
        }
        b.build().unwrap()
    }

    /// **The constraint the threading exists under.** A build that uses every core must serialize
    /// to the same bytes as one that uses a single core, or an index can no longer be
    /// content-hashed and every consumer's cache key becomes a function of the build machine.
    ///
    /// Compared against a genuinely serial build of the same corpus, not against a second threaded
    /// one: repeating the threaded build would only prove the chunk order does not matter.
    #[test]
    fn a_threaded_build_is_byte_identical_to_a_serial_one() {
        let threaded = wide_index();
        // The corpus has to actually reach the threshold, or this test passes by never threading.
        assert!(
            parallel_thread(threaded.posting.iter().map(Vec::len).sum::<usize>()) > 1,
            "test corpus does not cross PARALLEL_MIN_WORK, so nothing was run in parallel"
        );

        FORCE_SERIAL.with(|f| f.set(true));
        let serial = wide_index();
        FORCE_SERIAL.with(|f| f.set(false));

        assert_eq!(threaded.to_bytes(), serial.to_bytes());
    }

    /// The parallel helper returns exactly what the serial map returns, above and below the
    /// threshold, and the threshold itself keeps a small index on one thread.
    #[test]
    fn the_parallel_map_agrees_with_the_serial_one() {
        let n = 60_000;
        let f = |i: usize| ((i % 977) as f32).sqrt() * 3.7;
        let serial: Vec<f32> = (0..n).map(f).collect();

        assert_eq!(par_index_map(n, 40 * PARALLEL_MIN_WORK, f), serial);
        assert_eq!(par_index_map(n, 0, f), serial);
        assert_eq!(
            par_index_map(0, 40 * PARALLEL_MIN_WORK, f),
            Vec::<f32>::new()
        );

        let mut in_place: Vec<f32> = vec![0.0; n];
        par_slice_mut(&mut in_place, 40 * PARALLEL_MIN_WORK, |i, slot| {
            *slot = f(i)
        });
        assert_eq!(in_place, serial);

        assert_eq!(parallel_thread(0), 1);
        assert_eq!(parallel_thread(2 * PARALLEL_MIN_WORK - 1), 1);
        assert!(parallel_thread(2 * PARALLEL_MIN_WORK) >= 1);
    }

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
            (
                "Colgate",
                "Colgate Total Charcoal Deep Clean 80g",
                "Toothpaste",
            ),
            ("Nescafe", "Nescafe Classic Reseal 200g", "Kape"),
            (
                "Lucky Me",
                "Lucky Me Pancit Canton Chilimansi 60g",
                "Instant Noodles",
            ),
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
        assert!(
            h.iter().any(|x| x.doc == 6),
            "`kape` must reach Nescafe: {h:?}"
        );
    }

    /// The `joined` category in presyo's fixture: users type brand names with no space.
    #[test]
    fn prefix_search_supports_typeahead() {
        let ix = grocery();
        let h = ix.search_prefix("nesc", 5);
        assert!(
            h.iter().any(|x| x.doc == 6),
            "typeahead on a partial brand: {h:?}"
        );
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
        assert_eq!(
            h[0].doc, 0,
            "a boosted brand-field match must outrank a title-only one: {h:?}"
        );
    }

    /// Exact field lengths are the reason this scorer exists — prove they are not quantized away.
    #[test]
    fn document_length_affects_score() {
        let mut b = IndexBuilder::new(Schema::new(vec![Field::new("title", 1.0, 0.9)]));
        b.add(&Doc::new(["colgate"]));
        b.add(&Doc::new([
            "colgate with many many many other additional trailing words here",
        ]));
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
        assert_eq!(
            h.len(),
            2,
            "no size matches, but the query must still be answered: {h:?}"
        );
        // Both documents pay the same quantity penalty, so it cancels and word overlap decides.
        // Document 0 carries all five query words; document 1 is missing `roll` and `pack`.
        assert_eq!(
            h[0].doc, 0,
            "with size unavailable, word overlap decides: {h:?}"
        );
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
    fn highlight_marks_the_words_that_matched_including_typos() {
        let mut b = IndexBuilder::new(Schema::new(vec![Field::new("name", 3.0, 0.4)]));
        b.add(&Doc::new(vec!["Colgate Total Toothpaste 150g"]));
        b.add(&Doc::new(vec!["Safeguard Soap"]));
        let ix = b.build().unwrap();
        let text = "Colgate Total Toothpaste 150g";

        let mark = |q: &str| -> Vec<&str> {
            ix.highlight(q, text)
                .into_iter()
                .map(|(a, b)| &text[a..b])
                .collect()
        };

        assert_eq!(mark("Colgate"), vec!["Colgate"]);
        assert_eq!(
            mark("Toothpaste Colgate"),
            vec!["Colgate", "Toothpaste"],
            "ascending order"
        );
        // The point of doing this through the query planner: a typo highlights the real word.
        assert_eq!(
            mark("Colgte"),
            vec!["Colgate"],
            "a typo marks the corrected word"
        );
        assert!(
            mark("Safeguard").is_empty(),
            "a term that is not in THIS text marks nothing"
        );
        assert!(mark("").is_empty(), "an empty query marks nothing");

        // Spans are usable directly for wrapping, without overlap or reordering.
        let span = ix.highlight("Colgate Toothpaste", text);
        assert!(
            span.windows(2).all(|w| w[0].1 <= w[1].0),
            "non-overlapping and ascending"
        );

        // Highlighting text the index never saw must not panic.
        assert!(ix
            .highlight("Colgate", "some other product entirely")
            .is_empty());
        assert_eq!(
            ix.highlight("Colgate", "colgate cheap").len(),
            1,
            "case folds"
        );
    }

    /// The test that would have caught the panic in `p42`.
    ///
    /// `sort_by` requires a total order and is entitled to panic when it does not get one — which
    /// it did, on the first corpus with enough near-tied scores in a single result set. The old
    /// comparator asked `(a - b).abs() <= eps`, which is not transitive.
    ///
    /// Checked exhaustively over a ladder of scores spaced FINER than the grid, which is precisely
    /// where a tolerance-based comparator loses transitivity: adjacent pairs each compare equal
    /// while the ends do not.
    #[test]
    fn rank_cmp_is_a_total_order() {
        use std::cmp::Ordering;
        let mut hit = Vec::new();
        // Scores a hair apart, so many adjacent pairs quantize together.
        let mut x = 1.0f32;
        for i in 0..40u32 {
            hit.push(Hit {
                doc: i,
                score: x,
                typo_bucket: i % 3,
            });
            x = f32::from_bits(x.to_bits() + 1);
        }
        // ... plus a few far apart, so the set is not uniformly tied.
        for (i, s) in [0.0f32, 0.5, 2.0, 1e6].iter().enumerate() {
            hit.push(Hit {
                doc: 100 + i as u32,
                score: *s,
                typo_bucket: i as u32 % 3,
            });
        }

        // Antisymmetry and totality.
        for a in &hit {
            for b in &hit {
                let ab = rank_cmp(a, b);
                let ba = rank_cmp(b, a);
                assert_eq!(ab, ba.reverse(), "asymmetric on {a:?} vs {b:?}");
                if a.doc == b.doc {
                    assert_eq!(ab, Ordering::Equal);
                }
            }
        }
        // Transitivity — the property the old comparator actually violated.
        for a in &hit {
            for b in &hit {
                for c in &hit {
                    let (ab, bc, ac) = (rank_cmp(a, b), rank_cmp(b, c), rank_cmp(a, c));
                    if ab != Ordering::Greater && bc != Ordering::Greater {
                        assert_ne!(ac, Ordering::Greater, "not transitive: {a:?} {b:?} {c:?}");
                    }
                    if ab == Ordering::Equal && bc == Ordering::Equal {
                        assert_eq!(ac, Ordering::Equal, "equality not transitive");
                    }
                }
            }
        }
        // And the thing that panicked: sorting must simply work.
        let mut v = hit.clone();
        v.sort_by(rank_cmp);
        assert_eq!(v.len(), hit.len());
    }

    /// The quantization grid must stay near the tolerance it replaced, or the documented reason for
    /// it — absorbing `f32` non-associativity between the pruned and exhaustive paths — stops
    /// holding. This is what keeps `SCORE_TIE_REL` meaningful now that nothing compares against it
    /// directly.
    #[test]
    fn the_score_grid_matches_the_documented_tolerance() {
        for at in [1.0f32, 16.0, 1024.0] {
            let step = f32::from_bits(at.to_bits() + (1 << SCORE_TIE_BITS)) - at;
            let rel = step / at;
            assert!(
                (SCORE_TIE_REL / 4.0..=SCORE_TIE_REL * 4.0).contains(&rel),
                "grid at {at} is {rel} relative, tolerance is {SCORE_TIE_REL}"
            );
        }
        // Quantization must be monotone, or it could reorder two scores.
        let mut prev = canon_score(0.0);
        let mut x = 0.5f32;
        for _ in 0..2000 {
            let q = canon_score(x);
            assert!(q >= prev, "canon_score went backwards at {x}");
            prev = q;
            x = f32::from_bits(x.to_bits() + 7);
        }
    }

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
                fast.iter()
                    .map(|h| (h.doc, h.typo_bucket))
                    .collect::<Vec<_>>(),
                slow.iter()
                    .map(|h| (h.doc, h.typo_bucket))
                    .collect::<Vec<_>>(),
                "pruned and exhaustive disagree on {q:?}"
            );
            for (a, c) in fast.iter().zip(slow.iter()) {
                assert!(
                    (a.score - c.score).abs() < 1e-3,
                    "score mismatch on {q:?}: {a:?} {c:?}"
                );
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
        assert!(
            ix.search("zzzzqqqq", 3).is_empty(),
            "an unsplittable unknown must stay unknown"
        );
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
        assert_eq!(
            h[0].doc, 1,
            "the exact match must win regardless of the prior"
        );
        assert_eq!(h[0].typo_bucket, 0);
        // Only the exact match may come back at all: typo expansion is LAZY, so with an exact hit
        // present the fuzzy alternative is never fired. That is the engine being right, and the
        // first version of this test asserted `h[1]` existed and failed for that reason -- the
        // property under test is "the prior did not promote the typo", not "a typo hit exists".
        for x in h.iter().skip(1) {
            assert!(
                x.typo_bucket > 0,
                "an exact match cannot rank below a typo match"
            );
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
        for n in [
            "baking tray steel",
            "baking sheet paper",
            "needs assessment binder",
        ] {
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
            for n in [
                "camote powder 500g",
                "sago tapioca 200g",
                "baking tray steel",
            ] {
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
        assert!(
            ix.expansion_count() > 0,
            "expansion should have been learned"
        );
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
        let schema = Schema::new(vec![
            Field::new("name", 1.0, 0.5),
            Field::new("cat", 1.0, 0.75),
        ]);
        let mut b = IndexBuilder::new(schema);
        for n in ["camote powder", "sago tapioca"] {
            b.add(&Doc::new([n, "baking needs"]));
        }
        let ix = b.build().unwrap();
        assert_eq!(
            ix.expansion_count(),
            0,
            "opt-in only: no learn_expansion, no table"
        );
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
            assert!(
                !ids.contains(&o),
                "expansion must not contain the value's own words"
            );
        }
    }

    /// `p65`: the seven-column image document that surfaced this row -- one scored text field and
    /// six unscored columns -- builds, facets and ranges, on a `MAX_FIELD` that did not move.
    #[test]
    fn a_seven_column_schema_fits_in_four_scored_fields() {
        assert_eq!(
            MAX_FIELD, 4,
            "p56 must not be paid for by widening the per-posting array"
        );

        let schema = Schema::new(vec![Field::new("path", 1.0, 0.75)])
            .with_column("format")
            .with_column("orientation")
            .with_column("colour")
            .with_column("width")
            .with_column("height")
            .with_column("byte");
        assert_eq!(schema.column_count(), 7);
        assert_eq!(schema.field_count(), 1, "only the path is scored");

        let mut b = IndexBuilder::new(schema)
            .with_facet_of("format")
            .with_facet_of("orientation")
            .with_facet_of("colour")
            .with_numeric_of("width")
            .with_numeric_of("height")
            .with_numeric_of("byte");
        b.add(&Doc::new([
            "cdn/hero banner.jpg",
            "jpeg",
            "landscape",
            "warm",
            "1920",
            "1080",
            "204800",
        ]));
        b.add(&Doc::new([
            "cdn/hero icon.png",
            "png",
            "square",
            "cool",
            "64",
            "64",
            "1024",
        ]));
        b.add(&Doc::new([
            "cdn/hero strip.webp",
            "webp",
            "landscape",
            "cool",
            "1600",
            "400",
            "5120",
        ]));
        let ix = b.build().unwrap();

        // Nothing but the path is tokenized: "jpeg" is a stored value, not a searchable term.
        assert_eq!(ix.search("hero", 5).len(), 3);
        for stored in ["landscape", "warm", "204800"] {
            assert!(
                ix.search(stored, 5).is_empty(),
                "{stored:?} is a stored column value, it must not enter the dictionary"
            );
        }

        assert_eq!(ix.facet_slot_count(), 3);
        assert_eq!(ix.facet_label_at(1), ["landscape", "square"]);
        let landscape: Vec<u32> = ix
            .search_facet_at("hero", 5, 1, "landscape")
            .iter()
            .map(|h| h.doc)
            .collect();
        assert_eq!(landscape.len(), 2);
        assert!(!landscape.contains(&1));

        assert_eq!(ix.numeric_of(0, 0), Some(1920.0), "width");
        let big: Vec<u32> = ix
            .search_range("hero", 5, 2, 100_000.0, f64::MAX)
            .iter()
            .map(|h| h.doc)
            .collect();
        assert_eq!(big, vec![0], "only the banner is over 100 kB");
    }

    /// The old index-by-scored-field path keeps working unchanged, because shipping benchmarks use
    /// it. `p65` adds a road, it does not close one.
    #[test]
    fn declaring_a_facet_by_scored_field_index_still_works() {
        let schema = Schema::new(vec![
            Field::new("name", 2.0, 0.4),
            Field::new("category", 1.0, 0.4),
        ]);
        let mut b = IndexBuilder::new(schema).with_facet(1);
        b.add(&Doc::new(["bear brand milk", "Dairy"]));
        b.add(&Doc::new(["colgate toothpaste", "Personal Care"]));
        let ix = b.build().unwrap();
        assert_eq!(ix.facet_field(), [1]);
        assert_eq!(ix.facet_label(), ["Dairy", "Personal Care"]);
        assert_eq!(ix.search_facet("milk", 5, "Dairy").len(), 1);
        // ...and the category is still scored, which is the difference from an unscored column.
        assert_eq!(ix.search("dairy", 5).len(), 1);
    }

    /// A column budget is a budget: `add_column` refuses rather than panicking, for the C ABI.
    #[test]
    fn the_column_budget_is_bounded_and_refuses_rather_than_traps() {
        let mut schema = Schema::new(vec![Field::new("name", 1.0, 0.4)]);
        while schema.add_column(&format!("c{}", schema.column_count())) {}
        assert_eq!(schema.column_count(), MAX_COLUMN);
        assert!(!schema.add_column("one too many"));

        let mut b = IndexBuilder::new(schema);
        assert!(
            !b.set_facet_field(MAX_COLUMN),
            "a column past the end is refused, not stored"
        );
        assert!(b.set_facet_field(MAX_COLUMN - 1));
    }

    /// `count_any` / `count_all` against a brute-force set over the raw words, with deletions,
    /// across both the merged and the bitmap union paths.
    #[test]
    fn counts_agree_with_brute_force_sets_including_deleted_documents() {
        let word = [
            "red", "blue", "green", "milk", "bread", "soap", "tea", "rice",
        ];
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut row: Vec<Vec<&str>> = Vec::new();
        let mut b = IndexBuilder::new(Schema::new(vec![Field::new("name", 1.0, 0.4)]));
        for _ in 0..600 {
            let n = 1 + (next() % 4) as usize;
            let r: Vec<&str> = (0..n)
                .map(|_| word[(next() % word.len() as u64) as usize])
                .collect();
            b.add(&Doc::new([r.join(" ")]));
            row.push(r);
        }
        let mut ix = b.build().unwrap();
        for d in (0..600u32).step_by(7) {
            ix.delete(d);
        }
        let query = [
            "rice",
            "milk tea",
            "red blue green",
            "tea nosuchword",
            "nosuchword",
            "soap soap",
        ];
        for q in query {
            let tok: Vec<&str> = q.split(' ').collect();
            let live = |i: &usize| *i % 7 != 0;
            let any = (0..600)
                .filter(live)
                .filter(|&i| tok.iter().any(|t| row[i].contains(t)))
                .count();
            let all = (0..600)
                .filter(live)
                .filter(|&i| tok.iter().all(|t| row[i].contains(t)))
                .count();
            assert_eq!(ix.count_any(q), any, "any {q}");
            assert_eq!(ix.count_all(q), all, "all {q}");
        }
        assert_eq!(ix.count_any(""), 0);
        assert_eq!(ix.count_all(""), 0);
    }

    /// Stored per-term df: COUNT of a live term does not walk the posting list. List length
    /// grows; posting visits stay 0; the integer still matches the document count.
    #[test]
    fn count_any_of_a_live_term_does_not_walk_postings() {
        let mk = |n: usize| {
            let mut b = IndexBuilder::new(Schema::new(vec![Field::new("name", 1.0, 0.4)]));
            for i in 0..n {
                b.add(&Doc::new([format!("token{i} shared")]));
            }
            b.build().unwrap()
        };
        let small = mk(64);
        let large = mk(4096);
        assert_eq!(small.count_any("shared"), 64);
        assert_eq!(large.count_any("shared"), 4096);
        assert_eq!(
            small.count_any_cost("shared").1,
            0,
            "must not walk the 64-posting list"
        );
        assert_eq!(
            large.count_any_cost("shared").1,
            0,
            "must not walk the 4096-posting list"
        );
        assert_eq!(large.count_all("shared"), 4096);
        // Disjoint terms live on disjoint pages (token0 only in doc 0, token4000 only in doc 4000).
        assert_eq!(large.count_any("token0 token4000"), 2);
        assert_eq!(large.count_any_cost("token0 token4000").1, 0);
    }

    /// Overlapping pages and deletions used to force a posting walk. Sparse 64-doc presence
    /// answers both without visiting a posting; the integer still matches a brute-force set.
    #[test]
    fn count_any_of_overlapping_or_deleted_terms_does_not_walk_postings() {
        let mk = |n: usize| {
            let mut b = IndexBuilder::new(Schema::new(vec![Field::new("name", 1.0, 0.4)]));
            for i in 0..n {
                let extra = if i % 2 == 0 { "even" } else { "odd" };
                b.add(&Doc::new([format!("token{i} shared {extra}")]));
            }
            b.build().unwrap()
        };
        let n = 4096usize;
        let large = mk(n);
        assert_eq!(large.count_any("shared even"), n);
        assert_eq!(
            large.count_any_cost("shared even").1,
            0,
            "overlapping OR must not walk"
        );
        assert_eq!(large.count_all("shared even"), n / 2);
        assert_eq!(
            large.count_all_cost("shared even").1,
            0,
            "overlapping AND must not walk"
        );
        assert_eq!(large.count_any("even odd"), n);
        assert_eq!(large.count_any_cost("even odd").1, 0);
        assert_eq!(large.count_all("even odd"), 0);
        assert_eq!(large.count_all_cost("even odd").1, 0);

        let mut gone = mk(n);
        for d in (0..n as u32).step_by(3) {
            gone.delete(d);
        }
        let live: Vec<usize> = (0..n).filter(|i| i % 3 != 0).collect();
        let any_shared_even = live.len();
        let all_shared_even = live.iter().filter(|i| *i % 2 == 0).count();
        assert_eq!(gone.count_any("shared"), any_shared_even);
        assert_eq!(
            gone.count_any_cost("shared").1,
            0,
            "deleted COUNT must not walk"
        );
        assert_eq!(gone.count_any("shared even"), any_shared_even);
        assert_eq!(gone.count_any_cost("shared even").1, 0);
        assert_eq!(gone.count_all("shared even"), all_shared_even);
        assert_eq!(gone.count_all_cost("shared even").1, 0);
        assert_eq!(gone.count_any("even odd"), any_shared_even);
        assert_eq!(gone.count_all("even odd"), 0);
    }

    /// Holdable proxy for TIN's disjunction COUNT: overlapping OR must stay 0 posting visits
    /// as the list grows, so cost tracks occupied 64-doc pages rather than postings. Run
    /// `--ignored --release` to print QPS. Does not stand in for 10,260 QPS on 8.0 GB.
    #[test]
    #[ignore]
    fn count_any_overlapping_qps_is_not_linear_in_postings() {
        use std::time::Instant;
        for n in [4_096usize, 16_384, 65_536] {
            let mut b = IndexBuilder::new(Schema::new(vec![Field::new("name", 1.0, 0.4)]));
            for i in 0..n {
                let extra = if i % 2 == 0 { "even" } else { "odd" };
                b.add(&Doc::new([format!("token{i} shared {extra}")]));
            }
            let ix = b.build().unwrap();
            assert_eq!(ix.count_any_cost("shared even").1, 0);
            assert_eq!(ix.count_any("shared even"), n);
            let warm = 64;
            for _ in 0..warm {
                let _ = ix.count_any("shared even");
            }
            let want = 8_192;
            let t0 = Instant::now();
            for _ in 0..want {
                let _ = ix.count_any("shared even");
            }
            let dt = t0.elapsed();
            let qps = want as f64 / dt.as_secs_f64();
            let p99_ms = dt.as_secs_f64() / want as f64 * 1e3;
            eprintln!(
                "n={n} postings~{} qps={:.0} mean_ms={:.4} visits={}",
                n + n / 2,
                qps,
                p99_ms,
                ix.count_any_cost("shared even").1
            );
        }
    }

    fn colgate_pair(position: bool) -> Index {
        let mut b = IndexBuilder::new(Schema::new(vec![Field::new("name", 1.0, 0.4)]));
        if position {
            b = b.with_position();
        }
        b.add(&Doc::new(["Colgate Total Toothpaste"]));
        b.add(&Doc::new(["Total Colgate Toothpaste"]));
        b.build().unwrap()
    }

    /// Order matters. Both rows contain both words, so `count_all` is 2 either way; a phrase
    /// count that called `count_all` would report 2 for each query instead of one document each.
    #[test]
    fn count_phrase_is_order_sensitive_and_does_not_fall_back_to_count_all() {
        let ix = colgate_pair(true);
        assert_eq!(
            ix.count_all("Colgate Total"),
            2,
            "both rows are a bag-of-words match"
        );
        assert_eq!(ix.count_phrase("Colgate Total"), 1);
        assert_eq!(
            ix.count_phrase("Total Colgate"),
            1,
            "the reversed row is a different phrase"
        );
        assert_eq!(
            ix.search_phrase("Colgate Total", 10)
                .iter()
                .map(|h| h.doc)
                .collect::<Vec<_>>(),
            [0]
        );
        assert_eq!(
            ix.search_phrase("Total Colgate", 10)
                .iter()
                .map(|h| h.doc)
                .collect::<Vec<_>>(),
            [1]
        );
        assert_eq!(ix.count_phrase(""), 0);
        assert_eq!(ix.count_phrase("Colgate Nosuchword"), 0);
    }

    /// Adjacent vs reversed with a gap: `"Total Toothpaste Colgate"` has both words but not
    /// consecutive in reverse, so `"Total Colgate"` is 0. A `count_all` fallback would be 2.
    #[test]
    fn count_phrase_of_a_gapped_reverse_is_zero() {
        let mut b =
            IndexBuilder::new(Schema::new(vec![Field::new("name", 1.0, 0.4)])).with_position();
        b.add(&Doc::new(["Colgate Total Toothpaste"]));
        b.add(&Doc::new(["Total Toothpaste Colgate"]));
        let ix = b.build().unwrap();
        assert_eq!(ix.count_all("Colgate Total"), 2);
        assert_eq!(ix.count_phrase("Colgate Total"), 1);
        assert_eq!(ix.count_phrase("Total Colgate"), 0);
    }

    /// Without positions there is nothing to verify; falling back to `count_all` would return 2.
    #[test]
    fn count_phrase_without_positions_is_zero_not_count_all() {
        let ix = colgate_pair(false);
        assert!(!ix.has_position());
        assert_eq!(ix.count_all("Colgate Total"), 2);
        assert_eq!(ix.count_phrase("Colgate Total"), 0);
        assert_eq!(ix.count_phrase("Total Colgate"), 0);
    }

    #[test]
    fn count_phrase_does_not_count_a_deleted_document() {
        let mut ix = colgate_pair(true);
        ix.delete(0);
        assert_eq!(ix.count_phrase("Colgate Total"), 0);
        assert_eq!(
            ix.count_phrase("Total Colgate"),
            1,
            "the surviving row is the reversed phrase"
        );
        assert_eq!(
            ix.count_all("Colgate Total"),
            1,
            "the surviving row still has both words"
        );
    }
}
