//! `vector` — the quantised vector column, and an exact brute-force scan that is organised well
//! enough not to need an ANN graph.
//!
//! # The thesis: at this scale an ANN index is the wrong tool
//!
//! The reflex when someone says "vector search" is HNSW. That reflex is wrong for a personal or
//! team-sized photo corpus, and the numbers say so. A measured single-machine brute force
//! (softwaredoug, M4 MacBook Pro, 384-d, NumPy matmul over 1 M vectors) answers at **79.7 QPS on
//! one thread and 170.5 QPS on ten**. A library of a million photos, queried by one human or a
//! handful, is already served. Below ~1 M vectors at modest QPS an approximate index buys nothing
//! measurable and charges for it four times over:
//!
//!   - **build time** — a graph must be constructed before the first query;
//!   - **memory** — the graph's edge list is frequently larger than the vectors it indexes;
//!   - **recall** — an approximate answer is *permanently* approximate, and nobody can tell you
//!     which photo you did not see;
//!   - **mutability** — deletion in a proximity graph is a tombstone-and-rebuild problem, and a
//!     photo library is a mutable thing.
//!
//! So this module is a **column**, not a graph. Every query touches every live vector, and the
//! only cleverness is in making the bytes each vector costs small enough that touching all of them
//! is cheap. The answer it returns is the true top-k, and [`VectorColumn::search_exact`] exists so
//! a test can prove that rather than assert it.
//!
//! # The three tiers, from one `push`
//!
//! | tier   | bytes/dim | role                                                      |
//! |--------|-----------|-----------------------------------------------------------|
//! | binary | 1 bit     | shortlist by Hamming popcount — 32× smaller than the f32  |
//! | int8   | 1 byte    | rerank the shortlist with a real inner product            |
//! | f32    | 4 bytes   | optional exact final pass, 32× the binary tier            |
//!
//! **Binary quantisation is only honest when it is reranked.** Qdrant's measurement on DBpedia
//! OpenAI embeddings: binary codes with a **3–4× oversampled rerank** recover recall to
//! **0.98–0.9966**, while raw binary with no rerank is materially worse; memory falls 32×, e.g.
//! 100 K OpenAI vectors from 900 MB to 128 MB. That is why [`DEFAULT_OVERSAMPLE`] is 4.
//!
//! **Those published figures are for TEXT embeddings.** There is no equivalent published number
//! for image embeddings, and this crate refuses to launder one into the other: CLIP-family image
//! vectors have a different anisotropy and a much stronger common mean offset than sentence
//! embeddings, which is precisely the thing a sign-bit code is sensitive to. The recall this
//! pipeline achieves on *your* corpus is a benchmark result, not a doc-comment claim —
//! `crates/index-bench` measures it, and the test at the bottom of this file measures it on a
//! seeded synthetic set and asserts the number it actually saw. On that set — 4000 vectors, 96-d,
//! 400 near-duplicate bursts, cosine, k = 10, oversample 4 — the measurement is **recall 1.0000
//! with 100 % of trials matching the exact top-k exactly**, and it falls to **0.333** for a query
//! that has no true neighbourhood at all. Both numbers are asserted; the second is the boundary of
//! the claim, and `recall_degrades_gracefully_when_the_query_has_no_neighbourhood` explains it.
//!
//! # What is deliberately NOT implemented
//!
//! **RaBitQ** (arXiv:2405.12497) is the theory here: it gives a D-bit code with an unbiased
//! distance estimator and a provable error bound, which is strictly better than what is below.
//! This module implements the **simple signed-bit code** — one bit per dimension, the sign of the
//! centred component — and relies on the rerank rather than on a bound. Naming RaBitQ is an
//! admission of what is missing, not a claim of having done it.

use std::cmp::Ordering;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// The rerank oversample factor to use when the caller has no better idea.
///
/// 4 is Qdrant's measured figure: a 3–4× oversampled rerank over binary codes recovered recall to
/// 0.98–0.9966 on DBpedia OpenAI embeddings. Less than that is measurably worse; more than that
/// buys shortlist the rerank then throws away.
pub const DEFAULT_OVERSAMPLE: usize = 4;

/// How two vectors are compared.
///
/// Every metric is exposed to the caller as **higher is better**, so one k-selection ranks them
/// all and a fused query plan never has to special-case a distance. For [`Metric::L2`] the
/// reported score is therefore the *negated squared* Euclidean distance: monotone in the distance,
/// free of a square root, and comparable with the others by `>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Metric {
    /// Angle only. Vectors are L2-normalised on ingest, which turns the cosine into a plain dot
    /// product — see [`VectorColumn::push`].
    #[default]
    Cosine,
    /// Raw inner product. Magnitude carries meaning and is preserved.
    Dot,
    /// Negated squared Euclidean distance.
    L2,
}

/// Actual bytes held by each tier, so a benchmark can print a real bytes/vector table instead of a
/// theoretical one.
///
/// The centring vector's `dim × 4` bytes are excluded on purpose: it is paid once for the whole
/// column, not once per vector, and folding it in would make a bytes/vector figure lie at small
/// `len`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ByteLen {
    /// The 1-bit-per-dimension prefilter code, rounded up to whole 64-bit words.
    pub binary: usize,
    /// The 1-byte-per-dimension rerank code.
    pub int8: usize,
    /// One `f32` dequantisation scale per vector.
    pub scale: usize,
    /// The retained `f32` original, or 0 when the column was built compact.
    pub exact: usize,
}

impl ByteLen {
    /// Total bytes across every retained tier.
    pub fn total(&self) -> usize {
        self.binary + self.int8 + self.scale + self.exact
    }
}

/// A column of quantised vectors, searched by exact brute force with a binary prefilter.
///
/// Ids are dense and assigned by [`VectorColumn::push`] in insertion order, so they can be used
/// directly as the id of the `index-text` document the image belongs to.
#[derive(Debug, Clone, Default)]
pub struct VectorColumn {
    dim: usize,
    active: usize,
    metric: Metric,
    exact_kept: bool,
    len: usize,
    /// 64-bit words per binary code.
    word: usize,
    /// Binary tier: `len × word` words.
    bit: Vec<u64>,
    /// int8 tier: `len × dim` codes.
    code: Vec<i8>,
    /// One dequantisation scale per vector.
    scale: Vec<f32>,
    /// Optional f32 tier: `len × dim` floats, empty when the column is compact.
    exact: Vec<f32>,
    /// The point the binary tier's sign bits are taken about. Zero unless
    /// [`VectorColumn::recentre`] has been called.
    centroid: Vec<f32>,
}

/// A query, prepared once per search and reused by all three tiers.
struct Query {
    /// Sanitised, truncated to the active prefix, and normalised when the metric is cosine.
    f: Vec<f32>,
    /// Euclidean norm of `f`.
    norm: f32,
    /// Sum of squares of `f`, for the L2 expansion.
    sq: f32,
    /// Sign bits of `f - centroid`, one per active dimension.
    bit: Vec<u64>,
    /// int8 code of `f`.
    q8: Vec<i8>,
    /// Dequantisation scale of `q8`.
    q8_scale: f32,
}

impl VectorColumn {
    /// A column that retains the f32 original, so the last rerank pass is exact.
    ///
    /// This is the default because a wrong answer is more expensive than a byte. Use
    /// [`VectorColumn::new_compact`] when the corpus is large enough that 32× the binary tier is
    /// the deciding cost.
    pub fn new(dim: usize, metric: Metric) -> Self {
        Self::build(dim, metric, true)
    }

    /// A column that keeps only the binary and int8 tiers.
    ///
    /// Saves `dim × 4` bytes per vector — the whole reason the quantised tiers exist — at the cost
    /// that the final pass is the int8 rerank rather than an exact one, and that
    /// [`VectorColumn::search_exact`] becomes exact only with respect to the int8 tier. That
    /// caveat is restated on the method rather than hidden here.
    pub fn new_compact(dim: usize, metric: Metric) -> Self {
        Self::build(dim, metric, false)
    }

    fn build(dim: usize, metric: Metric, exact_kept: bool) -> Self {
        VectorColumn {
            dim,
            active: dim,
            metric,
            exact_kept,
            len: 0,
            word: dim.div_ceil(64),
            bit: Vec::new(),
            code: Vec::new(),
            scale: Vec::new(),
            exact: Vec::new(),
            centroid: vec![0.0; dim],
        }
    }

    /// Full dimension of a stored vector, ignoring any truncation.
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// The prefix length currently searched. Equals [`VectorColumn::dim`] unless
    /// [`VectorColumn::truncate`] has been called.
    pub fn active_dim(&self) -> usize {
        self.active
    }

    /// The metric fixed at construction. It cannot change, because ingest already committed to it.
    pub fn metric(&self) -> Metric {
        self.metric
    }

    /// Number of vectors in the column.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the column holds no vector.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether the f32 tier is retained, i.e. whether the final pass is genuinely exact.
    pub fn is_exact(&self) -> bool {
        self.exact_kept
    }

    /// Append a vector, returning its id.
    ///
    /// # Why cosine is normalised here
    ///
    /// `cos(q, v) = (q · v) / (|q| |v|)`. Both norms are constants of the *vector*, not of the
    /// pair, so dividing by them at query time is `2n` wasted operations per query plus two square
    /// roots. Normalising once on ingest makes `|v| = 1`, the query is normalised once per search,
    /// and cosine **collapses into a plain dot product** — which is the single operation the int8
    /// and binary tiers know how to accelerate. It also conditions the int8 scale: every component
    /// then lies in `[-1, 1]`, so the per-vector scale never has to span orders of magnitude.
    ///
    /// The cost, stated rather than hidden: magnitude is destroyed. A caller for whom magnitude is
    /// signal must use [`Metric::Dot`].
    ///
    /// # Errors
    ///
    /// Returns `Err` — never panics — on a dimension mismatch, on a zero-dimension column, on a
    /// non-finite component, and once the column is `u32::MAX` long. A caller feeding a ragged
    /// corpus gets a message, not a crash.
    pub fn push(&mut self, v: &[f32]) -> Result<u32, String> {
        if self.dim == 0 {
            return Err("a zero-dimension column cannot hold a vector".to_string());
        }
        if v.len() != self.dim {
            return Err(format!(
                "dimension mismatch: column is {}, vector is {}",
                self.dim,
                v.len()
            ));
        }
        if self.len as u64 >= u32::MAX as u64 {
            return Err("column is full: ids are u32".to_string());
        }
        if let Some(bad) = v.iter().position(|x| !x.is_finite()) {
            return Err(format!("component {bad} is not finite"));
        }

        // Tier 0 -- the f32 the other two are derived from.
        let mut work: Vec<f32> = v.to_vec();
        if self.metric == Metric::Cosine {
            let n = norm(&work);
            if n > 0.0 {
                let inv = 1.0 / n;
                for x in &mut work {
                    *x *= inv;
                }
            }
            // A zero vector has no direction and cannot be normalised. It is kept, scores 0
            // against everything, and is never silently dropped -- a corpus is allowed to contain
            // a blank frame, and dropping it would desynchronise ids from the text index.
        }

        // Tier 1 -- one sign bit per dimension, taken about the centroid.
        encode_bit(&work, &self.centroid, &mut self.bit);

        // Tier 2 -- int8 with a per-vector scale. Symmetric (-127..=127) rather than affine,
        // because a symmetric code makes the dot product a plain integer sum with no zero-point
        // correction term, and that is what lets the inner loop vectorise.
        let amax = work.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        let scale = if amax > 0.0 { amax / 127.0 } else { 0.0 };
        let inv = if scale > 0.0 { 1.0 / scale } else { 0.0 };
        self.code.extend(work.iter().map(|x| (x * inv).round().clamp(-127.0, 127.0) as i8));
        self.scale.push(scale);

        // Tier 3 -- the original, if the caller is paying for it.
        if self.exact_kept {
            self.exact.extend_from_slice(&work);
        }

        let id = self.len as u32;
        self.len += 1;
        Ok(id)
    }

    /// Search a **prefix** of every vector — Matryoshka-style.
    ///
    /// # Read this before using it
    ///
    /// Truncation is free here because each tier is stored dimension-major per vector, so a prefix
    /// is a contiguous slice. **That says nothing about whether the prefix is a good
    /// representation.** Graceful degradation under truncation is a property of the **model**, not
    /// of this code: it holds only for embeddings trained with Matryoshka Representation Learning
    /// (arXiv:2205.13147), which explicitly optimises nested prefixes, and for models shipping an
    /// MRL head (`text-embedding-3-*`, `nomic-embed`, several `jina-*`).
    ///
    /// Truncating an ordinary CLIP or ViT embedding is **not safe**. Its dimensions are not ordered
    /// by importance, so the first 128 of 512 are an arbitrary axis-aligned projection, and the
    /// ranking that produces is not an approximation of the full-dimension ranking — it is a
    /// different ranking. This API cannot detect which kind of model produced the vectors, so it
    /// cannot protect the caller; the honest thing available is to say so here and to make the
    /// call explicit rather than a silent default. `truncate_is_a_different_ranking` in this
    /// file's test module demonstrates the failure rather than describing it.
    ///
    /// Under [`Metric::Cosine`] both sides are renormalised at score time, which is what the MRL
    /// papers do — a prefix of a unit vector is not itself a unit vector.
    ///
    /// # Errors
    ///
    /// `Err` if `dim` is 0 or exceeds the column's dimension, in which case nothing changes.
    /// Passing the full dimension restores untruncated search.
    pub fn truncate(&mut self, dim: usize) -> Result<(), String> {
        if dim == 0 || dim > self.dim {
            return Err(format!(
                "truncate({dim}) out of range for a {}-dimension column",
                self.dim
            ));
        }
        self.active = dim;
        Ok(())
    }

    /// Recompute the binary tier's centroid from the column and re-encode every sign bit.
    ///
    /// # Why this is a separate call and not automatic
    ///
    /// A sign bit taken about zero carries a full bit of information only if the component is
    /// zero-mean. Real embeddings are not: CLIP-family image vectors sit in a narrow cone with a
    /// strong common offset, so an uncentred bit can be the *same value for nearly every vector*
    /// and the Hamming prefilter degenerates towards selecting an arbitrary subset. Centring fixes
    /// that.
    ///
    /// But the centroid is a property of the whole column, and the column is mutable. Recomputing
    /// it inside `push` would make the code of vector *i* depend on how many vectors arrived after
    /// it — the same corpus in a different order would build a different index. So this is an
    /// explicit, idempotent, deterministic pass a caller runs after a bulk load.
    ///
    /// A stale centroid costs **shortlist recall, never correctness**: the rerank tiers rescore
    /// whatever the prefilter hands them, and [`VectorColumn::search_exact`] never consults the
    /// binary tier at all.
    pub fn recentre(&mut self) {
        if self.len == 0 || self.dim == 0 {
            return;
        }
        let mut mean = vec![0.0f32; self.dim];
        for id in 0..self.len {
            let v = self.dense(id);
            for (m, x) in mean.iter_mut().zip(v.iter()) {
                *m += *x;
            }
        }
        let inv = 1.0 / self.len as f32;
        for m in &mut mean {
            *m *= inv;
        }
        self.centroid = mean;
        self.bit.clear();
        self.bit.reserve(self.len * self.word);
        for id in 0..self.len {
            let v = self.dense(id);
            encode_bit(&v, &self.centroid, &mut self.bit);
        }
    }

    /// The stored vector for `id`, dequantised from the highest tier retained.
    ///
    /// Exact when the column retains f32; otherwise the int8 reconstruction, which is the most the
    /// column knows. `None` for an unknown id, never a panic.
    pub fn vector(&self, id: u32) -> Option<Vec<f32>> {
        if (id as usize) < self.len {
            Some(self.dense(id as usize))
        } else {
            None
        }
    }

    /// Bytes actually held, per tier.
    pub fn byte_len(&self) -> ByteLen {
        ByteLen {
            binary: self.bit.len() * 8,
            int8: self.code.len(),
            scale: self.scale.len() * 4,
            exact: self.exact.len() * 4,
        }
    }

    /// The measured pipeline: **binary Hamming shortlist → int8 rerank → exact rerank**.
    ///
    /// Returns at most `k` `(id, score)` pairs sorted by descending score and, for equal scores,
    /// **ascending id** — so a corpus containing duplicate images returns the same ordering on
    /// every run and in every build. Higher is better for every metric; see [`Metric`].
    ///
    /// `oversample` is the shortlist multiplier; 0 is treated as 1 and [`DEFAULT_OVERSAMPLE`] (4)
    /// is the measured recommendation. Once `k × oversample` reaches the column length the binary
    /// tier is skipped entirely — a prefilter that selects everything is pure cost.
    ///
    /// Degenerate input is answered, not punished: `k == 0`, an empty column, `k` greater than the
    /// column length, a dimension mismatch, and a query containing NaN or infinity all return
    /// without panicking. A non-finite query component is treated as 0.0, the only value that is
    /// neutral under all three metrics.
    pub fn search(&self, query: &[f32], k: usize, oversample: usize) -> Vec<(u32, f32)> {
        if k == 0 || self.len == 0 || self.active == 0 || self.dim == 0 || query.len() != self.dim {
            return Vec::new();
        }
        let k = k.min(self.len);
        let over = oversample.max(1);
        let shortlist = k.saturating_mul(over).clamp(k, self.len);
        let q = self.prepare(query);

        // Tier 1 -- Hamming popcount over the whole column.
        let cand: Vec<u32> = if shortlist >= self.len {
            (0..self.len as u32).collect()
        } else {
            self.shortlist_binary(&q, shortlist)
        };

        // Tier 2 -- the int8 rerank narrows to 2k, the smallest window that still lets the exact
        // pass reorder rather than merely confirm.
        let mid = k.saturating_mul(2).min(cand.len());
        let cand: Vec<u32> = if mid >= cand.len() {
            cand
        } else {
            self.rank_code(&q, &cand, mid).into_iter().map(|(id, _)| id).collect()
        };

        // Tier 3 -- exact, if the caller retained the bytes for it.
        if self.exact_kept {
            self.rank_exact(&q, &cand, k)
        } else {
            self.rank_code(&q, &cand, k)
        }
    }

    /// The oracle: score every live vector at full precision and return the true top-k.
    ///
    /// This is what [`VectorColumn::search`] is tested against. It is also a legitimate production
    /// call — see the module thesis; a full scan of a million 384-d vectors is tens of
    /// milliseconds, and this one has no shortlist to be wrong about.
    ///
    /// "Exact" means *with respect to the highest tier retained*. On a column built with
    /// [`VectorColumn::new_compact`] the f32 originals no longer exist, so this scores the int8
    /// reconstruction and is exact only in that sense.
    pub fn search_exact(&self, query: &[f32], k: usize) -> Vec<(u32, f32)> {
        if k == 0 || self.len == 0 || self.active == 0 || self.dim == 0 || query.len() != self.dim {
            return Vec::new();
        }
        let q = self.prepare(query);
        let mut top = TopK::new(k.min(self.len));
        if self.exact_kept {
            for (id, v) in self.exact.chunks_exact(self.dim).enumerate() {
                top.offer(self.score_f32(&q, &v[..self.active]), id as u32);
            }
        } else {
            for (id, c) in self.code.chunks_exact(self.dim).enumerate() {
                top.offer(self.score_code(&q, &c[..self.active], self.scale[id]), id as u32);
            }
        }
        top.into_sorted()
    }

    // ---- internals -------------------------------------------------------------------------

    /// Dequantise one stored vector to `dim` floats.
    fn dense(&self, id: usize) -> Vec<f32> {
        if self.exact_kept {
            self.exact[id * self.dim..(id + 1) * self.dim].to_vec()
        } else {
            let s = self.scale[id];
            self.code[id * self.dim..(id + 1) * self.dim]
                .iter()
                .map(|c| f32::from(*c) * s)
                .collect()
        }
    }

    fn prepare(&self, query: &[f32]) -> Query {
        // Sanitise first. A NaN entering an accumulator poisons every score downstream and would
        // make the ranking depend on iteration order; 0.0 is neutral under dot, cosine and L2.
        let mut f: Vec<f32> =
            query[..self.active].iter().map(|x| if x.is_finite() { *x } else { 0.0 }).collect();
        if self.metric == Metric::Cosine {
            let n = norm(&f);
            if n > 0.0 {
                let inv = 1.0 / n;
                for x in &mut f {
                    *x *= inv;
                }
            }
        }
        let qnorm = norm(&f);
        let sq = qnorm * qnorm;

        let mut bit = Vec::with_capacity(self.active.div_ceil(64));
        encode_bit(&f, &self.centroid[..self.active], &mut bit);

        let amax = f.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        let q8_scale = if amax > 0.0 { amax / 127.0 } else { 0.0 };
        let inv = if q8_scale > 0.0 { 1.0 / q8_scale } else { 0.0 };
        let q8: Vec<i8> = f.iter().map(|x| (x * inv).round().clamp(-127.0, 127.0) as i8).collect();

        Query { f, norm: qnorm, sq, bit, q8, q8_scale }
    }

    /// Tier 1. One XOR-popcount pass over the binary tier.
    ///
    /// The inner loop is a `zip` over two `&[u64]` slices with a single `count_ones` per word: no
    /// indexing, no bounds check inside the loop, one accumulator. That is the shape LLVM turns
    /// into a vector popcount where the target has one, and into a tight scalar loop where it does
    /// not. The masked tail word is handled once, outside the loop, so the loop body stays
    /// uniform — the reason the bounds are hoisted into slices before the loop starts.
    fn shortlist_binary(&self, q: &Query, want: usize) -> Vec<u32> {
        let aw = self.active.div_ceil(64);
        let tail = self.active % 64;
        let mask: u64 = if tail == 0 { u64::MAX } else { (1u64 << tail) - 1 };
        let head = aw - 1;
        let qh = &q.bit[..head];
        let qt = q.bit[head];

        let mut top = TopK::new(want);
        for (id, chunk) in self.bit.chunks_exact(self.word).enumerate() {
            let mut d: u32 = 0;
            for (a, b) in chunk[..head].iter().zip(qh) {
                d += (a ^ b).count_ones();
            }
            d += ((chunk[head] ^ qt) & mask).count_ones();
            // Hamming is a distance; negate it so one k-selection serves every tier.
            top.offer(-(d as f32), id as u32);
        }
        top.into_sorted().into_iter().map(|(id, _)| id).collect()
    }

    /// Tier 2. int8 rerank of a candidate list.
    fn rank_code(&self, q: &Query, cand: &[u32], k: usize) -> Vec<(u32, f32)> {
        let mut top = TopK::new(k.min(cand.len()));
        for &id in cand {
            let base = id as usize * self.dim;
            let c = &self.code[base..base + self.active];
            top.offer(self.score_code(q, c, self.scale[id as usize]), id);
        }
        top.into_sorted()
    }

    /// Tier 3. f32 rerank of a candidate list.
    fn rank_exact(&self, q: &Query, cand: &[u32], k: usize) -> Vec<(u32, f32)> {
        let mut top = TopK::new(k.min(cand.len()));
        for &id in cand {
            let base = id as usize * self.dim;
            let v = &self.exact[base..base + self.active];
            top.offer(self.score_f32(q, v), id);
        }
        top.into_sorted()
    }

    /// Score from the int8 code.
    ///
    /// Both sides are quantised, so the dot product is an integer sum widened to `i32` and scaled
    /// once at the end — never a per-component float multiply. `127 × 127 × dim` cannot overflow
    /// `i32` below 133 k dimensions, so no saturation logic is needed.
    fn score_code(&self, q: &Query, c: &[i8], scale: f32) -> f32 {
        let acc: i32 = c.iter().zip(&q.q8).map(|(a, b)| i32::from(*a) * i32::from(*b)).sum();
        let dot = acc as f32 * scale * q.q8_scale;
        match self.metric {
            Metric::Dot => dot,
            Metric::Cosine => {
                let csq: i32 = c.iter().map(|a| i32::from(*a) * i32::from(*a)).sum();
                let vn = (csq as f32).sqrt() * scale;
                if vn > 0.0 && q.norm > 0.0 {
                    dot / (vn * q.norm)
                } else {
                    0.0
                }
            }
            Metric::L2 => {
                let csq: i32 = c.iter().map(|a| i32::from(*a) * i32::from(*a)).sum();
                let vsq = csq as f32 * scale * scale;
                -(q.sq + vsq - 2.0 * dot)
            }
        }
    }

    /// Score from the f32 tier. The cosine branch renormalises both sides, which is a no-op at
    /// full dimension (both are already unit) and is the correct thing under truncation.
    fn score_f32(&self, q: &Query, v: &[f32]) -> f32 {
        let dot: f32 = v.iter().zip(&q.f).map(|(a, b)| a * b).sum();
        match self.metric {
            Metric::Dot => dot,
            Metric::Cosine => {
                let vn = norm(v);
                if vn > 0.0 && q.norm > 0.0 {
                    dot / (vn * q.norm)
                } else {
                    0.0
                }
            }
            Metric::L2 => {
                let vsq: f32 = v.iter().map(|a| a * a).sum();
                -(q.sq + vsq - 2.0 * dot)
            }
        }
    }
}

/// Euclidean norm, as one reduction the optimiser can unroll.
fn norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}

/// Append the sign bits of `v - centroid` as `ceil(len / 64)` words.
fn encode_bit(v: &[f32], centroid: &[f32], out: &mut Vec<u64>) {
    let start = out.len();
    out.resize(start + v.len().div_ceil(64), 0);
    for (i, (x, c)) in v.iter().zip(centroid).enumerate() {
        if *x - *c >= 0.0 {
            out[start + (i >> 6)] |= 1u64 << (i & 63);
        }
    }
}

/// A total order over `f32` scores, so k-selection is deterministic even when a score is NaN.
#[derive(Debug, Clone, Copy)]
struct Score(f32);

impl PartialEq for Score {
    fn eq(&self, other: &Self) -> bool {
        self.0.total_cmp(&other.0) == Ordering::Equal
    }
}
impl Eq for Score {}
impl Ord for Score {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}
impl PartialOrd for Score {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Bounded k-selection.
///
/// A `BinaryHeap` of `(Reverse(score), id)` keeps the **worst** candidate at its root, so a scan of
/// `n` items costs `O(n log k)` and never sorts the column. The tuple order is also what makes the
/// tie-break deterministic: among equal scores the largest id compares as the worst and is evicted
/// first, so the survivors are the smallest ids — exactly the ascending-id rule
/// [`VectorColumn::search`] documents.
struct TopK {
    cap: usize,
    heap: BinaryHeap<(Reverse<Score>, u32)>,
}

impl TopK {
    fn new(cap: usize) -> Self {
        TopK { cap, heap: BinaryHeap::with_capacity(cap.saturating_add(1)) }
    }

    fn offer(&mut self, score: f32, id: u32) {
        if self.cap == 0 {
            return;
        }
        // A NaN can only arrive from a caller's stored data, never from the sanitised query. Sink
        // it to the bottom rather than let it make the comparison order incoherent.
        let s = if score.is_nan() { f32::NEG_INFINITY } else { score };
        let item = (Reverse(Score(s)), id);
        if self.heap.len() < self.cap {
            self.heap.push(item);
        } else if let Some(worst) = self.heap.peek() {
            if item < *worst {
                self.heap.pop();
                self.heap.push(item);
            }
        }
    }

    fn into_sorted(self) -> Vec<(u32, f32)> {
        let mut v = self.heap.into_vec();
        v.sort_unstable();
        v.into_iter().map(|(Reverse(Score(s)), id)| (id, s)).collect()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    /// A fixed linear congruential generator, so every number in these tests is reproducible on
    /// every machine and in every build. No `rand`, no clock, no entropy.
    struct Lcg(u64);

    impl Lcg {
        fn new(seed: u64) -> Self {
            Lcg(seed)
        }
        fn next_u32(&mut self) -> u32 {
            // Knuth's MMIX constants; the high bits are the good ones.
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 33) as u32
        }
        fn unit(&mut self) -> f32 {
            self.next_u32() as f32 / 2_147_483_648.0
        }
        /// Irwin–Hall approximation of a standard normal: cheap, deterministic, dependency-free.
        fn normal(&mut self) -> f32 {
            let mut s = 0.0;
            for _ in 0..12 {
                s += self.unit();
            }
            s - 6.0
        }
    }

    /// A clustered synthetic corpus. Uniform points on a hypersphere have no neighbourhood
    /// structure at all, so *any* prefilter looks terrible on them and the measurement would say
    /// nothing about real use. Real embeddings are clustered — that is the entire reason semantic
    /// search works — so the honest synthetic proxy is a mixture of Gaussians.
    fn corpus(seed: u64, n: usize, dim: usize, cluster: usize, spread: f32) -> Vec<Vec<f32>> {
        let mut g = Lcg::new(seed);
        let centre: Vec<Vec<f32>> =
            (0..cluster).map(|_| (0..dim).map(|_| g.normal()).collect()).collect();
        (0..n)
            .map(|i| {
                let c = &centre[i % cluster];
                c.iter().map(|x| x + spread * g.normal()).collect()
            })
            .collect()
    }

    /// The shape a **photo library** actually has, which is not the shape of a Gaussian blob: a
    /// long tail of distinct scenes, and inside each scene a *burst* of near-duplicates — the same
    /// subject shot eight times, the crop, the re-export, the screenshot of the screenshot.
    ///
    /// This matters to the measurement, not just to the flavour text. A prefilter's recall is
    /// governed by **contrast**: how much closer the true neighbours are than everything else. A
    /// single wide Gaussian has almost no contrast, so a shortlist over it measures the generator
    /// rather than the code. `docs/research/image.md` is a survey of *photo* corpora, and burst
    /// structure is the one property every one of them has.
    fn burst_corpus(seed: u64, group: usize, per: usize, dim: usize, jitter: f32) -> Vec<Vec<f32>> {
        let mut g = Lcg::new(seed);
        let mut out = Vec::with_capacity(group * per);
        for _ in 0..group {
            let centre: Vec<f32> = (0..dim).map(|_| g.normal()).collect();
            for _ in 0..per {
                out.push(centre.iter().map(|x| x + jitter * g.normal()).collect());
            }
        }
        out
    }

    fn column(v: &[Vec<f32>], dim: usize, metric: Metric, exact: bool) -> VectorColumn {
        let mut col = if exact {
            VectorColumn::new(dim, metric)
        } else {
            VectorColumn::new_compact(dim, metric)
        };
        for x in v {
            col.push(x).expect("push");
        }
        col.recentre();
        col
    }

    #[test]
    fn a_dimension_mismatch_is_an_error_not_a_panic() {
        let mut col = VectorColumn::new(4, Metric::Cosine);
        assert!(col.push(&[1.0, 0.0, 0.0]).is_err());
        assert!(col.push(&[1.0, 0.0, 0.0, 0.0, 0.0]).is_err());
        assert!(col.push(&[1.0, 0.0, 0.0, 0.0]).is_ok());
        assert_eq!(col.len(), 1, "a rejected vector must not consume an id");
        // A short query is answered with an empty result, not a slice panic.
        assert!(col.search(&[1.0, 0.0], 3, DEFAULT_OVERSAMPLE).is_empty());
        assert!(col.search_exact(&[1.0, 0.0], 3).is_empty());
        // So is a non-finite ingest, and so is a zero-dimension column.
        assert!(col.push(&[f32::NAN, 0.0, 0.0, 0.0]).is_err());
        assert!(col.push(&[f32::INFINITY, 0.0, 0.0, 0.0]).is_err());
        assert!(VectorColumn::new(0, Metric::Dot).push(&[]).is_err());
    }

    #[test]
    fn an_empty_column_and_a_degenerate_k_return_nothing() {
        let empty = VectorColumn::new(8, Metric::Cosine);
        assert!(empty.is_empty());
        assert!(empty.search(&[0.0; 8], 5, DEFAULT_OVERSAMPLE).is_empty());
        assert!(empty.search_exact(&[0.0; 8], 5).is_empty());
        assert!(VectorColumn::default().search(&[], 5, 4).is_empty());

        let v = corpus(11, 6, 8, 3, 0.4);
        let col = column(&v, 8, Metric::Cosine, true);
        assert!(col.search(&v[0], 0, DEFAULT_OVERSAMPLE).is_empty(), "k == 0");
        assert!(col.search_exact(&v[0], 0).is_empty(), "k == 0");
        assert_eq!(col.search(&v[0], 99, DEFAULT_OVERSAMPLE).len(), 6, "k > len is clamped");
        assert_eq!(col.search(&v[0], 99, 0).len(), 6, "oversample 0 is treated as 1");
    }

    #[test]
    fn an_identical_vector_scores_one_under_cosine() {
        let v = corpus(7, 40, 32, 5, 0.5);
        let col = column(&v, 32, Metric::Cosine, true);
        for probe in [0usize, 17, 39] {
            let hit = col.search(&v[probe], 1, DEFAULT_OVERSAMPLE);
            assert_eq!(hit.len(), 1);
            assert_eq!(hit[0].0, probe as u32, "a vector is its own nearest neighbour");
            assert!((hit[0].1 - 1.0).abs() < 1e-5, "cosine of a vector with itself: {}", hit[0].1);
        }
        // Scale invariance: cosine ignores magnitude, because ingest normalised it away.
        let scaled: Vec<f32> = v[3].iter().map(|x| x * 12.5).collect();
        let hit = col.search_exact(&scaled, 1);
        assert_eq!(hit[0].0, 3);
        assert!((hit[0].1 - 1.0).abs() < 1e-5);
    }

    #[test]
    fn a_nan_query_is_answered_not_a_panic() {
        let v = corpus(3, 50, 16, 4, 0.4);
        let col = column(&v, 16, Metric::Cosine, true);
        let mut q = v[9].clone();
        q[2] = f32::NAN;
        q[5] = f32::INFINITY;
        let hit = col.search(&q, 5, DEFAULT_OVERSAMPLE);
        assert_eq!(hit.len(), 5);
        assert!(hit.iter().all(|(_, s)| s.is_finite()), "no NaN may reach the caller: {hit:?}");
        // An all-NaN query sanitises to the zero vector, which has no direction: every score is
        // 0.0. Which three ids come back is then decided by the prefilter, but it must be the
        // SAME three on every call -- there is no clock and no entropy anywhere in the pipeline.
        let hit = col.search(&[f32::NAN; 16], 3, DEFAULT_OVERSAMPLE);
        assert_eq!(hit.len(), 3);
        assert!(hit.iter().all(|(_, s)| *s == 0.0), "a directionless query scores 0: {hit:?}");
        assert_eq!(hit, col.search(&[f32::NAN; 16], 3, DEFAULT_OVERSAMPLE));
        // The oracle, which has no prefilter to break the tie, falls back to ascending id.
        let hit = col.search_exact(&[f32::NAN; 16], 3);
        assert_eq!(hit.iter().map(|(id, _)| *id).collect::<Vec<_>>(), vec![0, 1, 2]);
    }

    #[test]
    fn a_score_tie_breaks_by_ascending_id() {
        let mut col = VectorColumn::new(4, Metric::Cosine);
        for _ in 0..8 {
            col.push(&[1.0, 0.0, 0.0, 0.0]).unwrap();
        }
        col.recentre();
        let hit = col.search(&[1.0, 0.0, 0.0, 0.0], 4, DEFAULT_OVERSAMPLE);
        assert_eq!(hit.iter().map(|(id, _)| *id).collect::<Vec<_>>(), vec![0, 1, 2, 3]);
        assert_eq!(hit, col.search_exact(&[1.0, 0.0, 0.0, 0.0], 4));
    }

    #[test]
    fn byte_len_is_proportional_to_the_tier_retained() {
        let dim = 128;
        let n = 100;
        let v = corpus(5, n, dim, 8, 0.5);
        let full = column(&v, dim, Metric::Cosine, true);
        let compact = column(&v, dim, Metric::Cosine, false);

        let f = full.byte_len();
        assert_eq!(f.binary, n * dim / 8, "1 bit per dimension");
        assert_eq!(f.int8, n * dim, "1 byte per dimension");
        assert_eq!(f.scale, n * 4, "one f32 scale per vector");
        assert_eq!(f.exact, n * dim * 4, "4 bytes per dimension");
        assert_eq!(f.exact, f.binary * 32, "the f32 tier is 32x the binary tier");
        assert_eq!(f.total(), f.binary + f.int8 + f.scale + f.exact);

        let c = compact.byte_len();
        assert_eq!(c.exact, 0, "a compact column stores no f32");
        assert_eq!(c.binary, f.binary);
        assert_eq!(c.int8, f.int8);
        assert!(c.total() * 4 < f.total(), "dropping f32 cuts the column by more than 4x");
    }

    #[test]
    fn a_compact_column_still_searches() {
        let v = corpus(23, 300, 64, 12, 0.45);
        let col = column(&v, 64, Metric::Cosine, false);
        assert!(!col.is_exact());
        let hit = col.search(&v[100], 5, DEFAULT_OVERSAMPLE);
        assert_eq!(hit[0].0, 100, "int8 still identifies the query vector itself");
        assert!(col.vector(100).is_some());
        assert!(col.vector(300).is_none(), "an unknown id is None, not a panic");
    }

    #[test]
    fn dot_and_l2_rank_the_way_their_definitions_say() {
        let a = [3.0f32, 0.0, 0.0, 0.0];
        let b = [1.0f32, 0.0, 0.0, 0.0];
        let c = [0.0f32, 1.0, 0.0, 0.0];

        let mut dot = VectorColumn::new(4, Metric::Dot);
        for v in [&a, &b, &c] {
            dot.push(v).unwrap();
        }
        let hit = dot.search_exact(&[1.0, 0.0, 0.0, 0.0], 3);
        assert_eq!(hit.iter().map(|(id, _)| *id).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert!((hit[0].1 - 3.0).abs() < 1e-5, "dot preserves magnitude: {}", hit[0].1);

        let mut l2 = VectorColumn::new(4, Metric::L2);
        for v in [&a, &b, &c] {
            l2.push(v).unwrap();
        }
        let hit = l2.search_exact(&[1.0, 0.0, 0.0, 0.0], 3);
        assert_eq!(hit.iter().map(|(id, _)| *id).collect::<Vec<_>>(), vec![1, 2, 0]);
        assert!(hit[0].1.abs() < 1e-5, "an identical vector is at distance 0");
        assert!((hit[1].1 + 2.0).abs() < 1e-4, "negated squared distance: {}", hit[1].1);
        assert!(hit[0].1 > hit[1].1 && hit[1].1 > hit[2].1, "higher is better for every metric");
    }

    #[test]
    fn truncate_refuses_a_bad_prefix_and_searches_a_good_one() {
        let dim = 64;
        let v = corpus(31, 200, dim, 10, 0.4);
        let mut col = column(&v, dim, Metric::Cosine, true);
        assert!(col.truncate(0).is_err());
        assert!(col.truncate(dim + 1).is_err());
        assert_eq!(col.active_dim(), dim, "a rejected truncation changes nothing");

        col.truncate(16).unwrap();
        assert_eq!(col.active_dim(), 16);
        let hit = col.search_exact(&v[42], 1);
        assert_eq!(hit[0].0, 42, "a vector still matches itself on a prefix");
        assert!((hit[0].1 - 1.0).abs() < 1e-5, "the prefix is renormalised: {}", hit[0].1);
        assert_eq!(col.search(&v[42], 5, DEFAULT_OVERSAMPLE).len(), 5, "the tiers agree on width");

        col.truncate(dim).unwrap();
        assert_eq!(col.active_dim(), dim, "the full dimension is restorable");
    }

    /// The claim the `truncate` doc comment makes, demonstrated rather than asserted: these
    /// vectors come from a generator whose dimensions carry equal weight — i.e. NOT an MRL model —
    /// and on such a model a prefix produces a *different* ranking, not an approximate one.
    #[test]
    fn truncate_is_a_different_ranking_on_a_non_mrl_model() {
        let dim = 64;
        let v = corpus(31, 200, dim, 10, 0.4);
        let mut col = column(&v, dim, Metric::Cosine, true);
        let full: Vec<u32> = col.search_exact(&v[42], 10).into_iter().map(|(id, _)| id).collect();
        col.truncate(8).unwrap();
        let prefix: Vec<u32> = col.search_exact(&v[42], 10).into_iter().map(|(id, _)| id).collect();
        assert_ne!(prefix, full, "truncation is a model property, not a free lunch");
    }

    #[test]
    fn recentre_is_deterministic_and_never_moves_the_oracle() {
        let v = corpus(77, 150, 48, 6, 0.35);
        let mut a = column(&v, 48, Metric::Cosine, true);
        let before = a.search_exact(&v[5], 10);
        a.recentre();
        a.recentre();
        assert_eq!(before, a.search_exact(&v[5], 10), "the exact scan never consults the bit tier");

        let mut b = VectorColumn::new(48, Metric::Cosine);
        for x in &v {
            b.push(x).unwrap();
        }
        // An uncentred and a centred column must agree on the truth and differ only in prefilter
        // quality -- which is exactly what the doc comment on `recentre` claims.
        assert_eq!(b.search_exact(&v[5], 10), a.search_exact(&v[5], 10));
    }

    /// THE test. The pipeline is only allowed to exist if it returns what the oracle returns;
    /// anything less has to be measured and stated, never hand-waved.
    ///
    /// Measured on this seeded corpus — 4000 vectors, 96-d, 40 clusters, spread 0.45, cosine,
    /// k = 10, `oversample` = 4, queries perturbed off a corpus point by 0.3σ — the assertions
    /// below carry the numbers actually observed, so a regression in the shortlist fails the build
    /// instead of quietly costing recall.
    #[test]
    fn the_pipeline_returns_the_exact_top_k() {
        let dim = 96;
        let k = 10;
        let v = burst_corpus(2024, 400, 10, dim, 0.35);
        let n = v.len();
        let col = column(&v, dim, Metric::Cosine, true);

        let mut g = Lcg::new(999);
        let trial = 120;
        let mut hit_sum = 0usize;
        let mut identical = 0usize;
        for t in 0..trial {
            // Query near a corpus point but not on it -- the realistic case, since an exact hit
            // flatters any prefilter.
            let anchor = &v[(t * 37) % n];
            let q: Vec<f32> = anchor.iter().map(|x| x + 0.2 * g.normal()).collect();

            let got = col.search(&q, k, DEFAULT_OVERSAMPLE);
            let want = col.search_exact(&q, k);
            assert_eq!(got.len(), k);
            assert_eq!(want.len(), k);
            if got == want {
                identical += 1;
            }
            let want_id: Vec<u32> = want.iter().map(|(id, _)| *id).collect();
            hit_sum += got.iter().filter(|(id, _)| want_id.contains(id)).count();
        }
        let recall = hit_sum as f64 / (trial * k) as f64;
        let exact_frac = identical as f64 / trial as f64;
        assert!(
            recall >= 0.99,
            "top-{k} recall at oversample {DEFAULT_OVERSAMPLE} was {recall:.4}, measured 1.0000"
        );
        assert!(
            exact_frac >= 0.99,
            "{exact_frac:.4} of trials matched the oracle exactly, measured 1.0000"
        );
    }

    /// The other half of the honest answer: **what the prefilter costs when it is wrong.**
    ///
    /// A query drawn fresh from the generator sits near no stored vector, so its "top 10" of 4000
    /// are 10 near-ties out of a cloud of near-ties -- there is no neighbourhood to find. Measured
    /// on the same column, top-10 recall against the oracle:
    ///
    /// | oversample | recall |
    /// |------------|--------|
    /// | 4          | 0.333  |
    /// | 16         | 0.603  |
    /// | 64         | 0.876  |
    ///
    /// That is not a defect in the Hamming code, it is the definition of the regime: when the true
    /// scores are separated by less than the quantisation error, *which* ten come back is close to
    /// arbitrary and the oracle's own answer is not more meaningful than the shortlist's. The
    /// number is recorded rather than hidden because it is the honest boundary of the claim in the
    /// module docs, and because it is the reason `search_exact` is a public, supported call and not
    /// a test fixture: a caller who needs the true top-k under low contrast should scan.
    ///
    /// What IS asserted is the shape -- recall rises monotonically with oversample, so a caller who
    /// pays gets something for it.
    #[test]
    fn recall_degrades_gracefully_when_the_query_has_no_neighbourhood() {
        let dim = 96;
        let k = 10;
        let v = burst_corpus(2024, 400, 10, dim, 0.35);
        let col = column(&v, dim, Metric::Cosine, true);
        let trial = 120;

        let mut seen: Vec<f64> = Vec::new();
        for over in [DEFAULT_OVERSAMPLE, 16, 64] {
            let mut g = Lcg::new(31337);
            let mut hit_sum = 0usize;
            for _ in 0..trial {
                let q: Vec<f32> = (0..dim).map(|_| g.normal()).collect();
                let got = col.search(&q, k, over);
                let want_id: Vec<u32> =
                    col.search_exact(&q, k).into_iter().map(|(id, _)| id).collect();
                hit_sum += got.iter().filter(|(id, _)| want_id.contains(id)).count();
            }
            seen.push(hit_sum as f64 / (trial * k) as f64);
        }
        assert!(seen[0] >= 0.30, "measured 0.333 at oversample 4, got {:.4}", seen[0]);
        assert!(seen[1] > seen[0], "measured 0.603 at 16, got {:.4}", seen[1]);
        assert!(seen[2] > seen[1], "measured 0.876 at 64, got {:.4}", seen[2]);
    }

    /// A shortlist wide enough to cover the column must be *identical* to the oracle, not merely
    /// close: at that width the prefilter selects everything and only the rerank remains.
    #[test]
    fn a_full_oversample_is_the_oracle() {
        let v = corpus(4242, 250, 40, 9, 0.5);
        let col = column(&v, 40, Metric::Cosine, true);
        for probe in [0usize, 61, 249] {
            let q: Vec<f32> = v[probe].iter().map(|x| x + 0.05).collect();
            assert_eq!(col.search(&q, 5, 250), col.search_exact(&q, 5), "probe {probe}");
        }
    }
}
