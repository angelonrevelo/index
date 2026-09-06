//! The fusion layer: one index, one scoring pass, one top-k, over text **and** pixels.
//!
//! # The claim this module exists to make
//!
//! [`docs/research/image.md`](../../../docs/research/image.md) §3 states it as narrowly as it can
//! be stated while remaining false of every shipping system:
//!
//! > **No system answers text, facet, numeric-range, vector and perceptual-hash predicates in one
//! > query plan over one index, selecting top-k once.**
//!
//! Immich and LibrePhotos separate pgvector/FAISS from the relational row from a separately
//! scheduled face-clustering job, and intersect the three in application code. PhotoPrism combines
//! them in its *query syntax* and fans out underneath — a real partial counterexample, recorded as
//! one rather than erased.
//!
//! # Why fan-out returns short pages, and this does not
//!
//! This is the whole argument, so it is worth being concrete.
//!
//! A fan-out architecture asks each subsystem for its own top-k and then intersects. Consider a
//! document ranked 51st by text and 51st by vector, with k = 50. It is plausibly the single best
//! *fused* result — two independent signals both like it — and **both subsystems have already
//! discarded it before the intersection runs.** The user sees a short page, or a worse one, and
//! there is no amount of tuning at the intersection layer that recovers a document neither arm
//! returned.
//!
//! So the rule here is: **generate candidates generously, score once, select once.**
//!
//! # What is honestly one pass, and what is not
//!
//! Overclaiming here would be easy and would poison the benchmark, so the boundary is drawn
//! explicitly:
//!
//! - **Hard predicates are genuinely fused into the text pass.** Facet and numeric-range clauses go
//!   to [`index_text::Index::search_clause`], which applies them *during* scoring. That is one
//!   traversal of the posting list, and it is `p30`/`p31` work this crate did not have to redo.
//! - **The vector column is one linear pass** over its own storage (`p58`).
//! - **The hash column is one linear popcount pass.**
//! - **The three are then fused and top-k is selected exactly once.**
//!
//! What this is **not** is a single interleaved traversal that advances postings and vectors in
//! lockstep. Three passes over three columns, one selection. The distinction that matters for
//! correctness — and the one the incumbents get wrong — is where **top-k** happens, not how many
//! times memory is walked. `bench/roadmap/p59-image-fusion.md` asserts the property that actually
//! bites (no short page, no leak), not the implementation detail.

use crate::hash::Hash64;
use crate::vector::{Metric, VectorColumn, DEFAULT_OVERSAMPLE};
use crate::Why;
use index_text::{Doc, FacetClause, IndexBuilder, Schema};
use std::collections::HashMap;

/// How many candidates each soft signal contributes before fusion.
///
/// The short-page defect above is a *candidate generation* problem, not a scoring problem, so the
/// only defence is to over-generate. A multiplier of 8 means a document must be outside the top
/// `8k` of **every** signal before it becomes unreachable — versus outside the top `k` of any one
/// signal in a fan-out design.
///
/// It is a multiplier rather than a constant because the cost that matters is relative to the page
/// being filled: over-generating 400 candidates to fill a page of 50 is cheap, and over-generating
/// 400 to fill a page of 5 is waste.
pub const CANDIDATE_MULTIPLIER: usize = 8;

/// Default weight of the text arm in the convex combination.
///
/// `fuse::convex` exposes `alpha` for exactly this and its doc comment gives the rule this follows:
/// use a tuned convex combination once ~40 judgments exist, and until then prefer a default that
/// needs no tuning set. 0.5 is that default — it asserts nothing about which signal is better,
/// which is the honest position before the labelled set of `p59` acceptance check 3 exists.
pub const TEXT_ALPHA: f32 = 0.5;

/// The "no content address computed" sentinel. See [`ImageIndexBuilder::build`] for why the zero
/// value can safely mean this.
const ZERO_DIGEST: [u8; 32] = [0u8; 32];

/// A fused query: every predicate the engine can apply, in one place.
///
/// Every field is optional, and a query with all of them `None` is a legitimate match-everything.
/// That is deliberate — `docs/research/image.md` §7 measured that social platforms strip EXIF from
/// essentially every downloadable image, so a corpus where most documents have almost no signal is
/// the **normal** case, and the query surface has to degrade to whatever is present rather than
/// insist on a full predicate set.
#[derive(Default)]
pub struct FusedQuery<'a> {
    /// Free text, scored with BM25F by the existing engine.
    pub text: Option<&'a str>,
    /// Categorical predicates. **Hard** — a document failing one cannot appear, at any score.
    pub facet: &'a [FacetClause<'a>],
    /// Numeric half-open ranges as `(slot, lo, hi)`. **Hard.**
    pub range: &'a [(usize, f64, f64)],
    /// Find images that look like this embedding. **Soft** — contributes score, never excludes.
    pub vector: Option<&'a [f32]>,
    /// Find near-duplicates of this perceptual hash within a Hamming radius. **Soft.**
    pub hash: Option<(Hash64, u32)>,
    /// Weight of the text arm; the vector and hash arms share the remainder.
    pub alpha: Option<f32>,
}

/// One fused result.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FusedHit {
    pub doc: u32,
    /// Convex combination of the normalised per-signal scores.
    pub score: f32,
    /// Which signal(s) found this document.
    ///
    /// A fused answer is only worth having if it is explicable: "matched your caption **and** looks
    /// like your reference" is a materially different result from either alone, and the caller is
    /// entitled to know which it got.
    pub why: Why,
}

/// Accumulates images, then produces an immutable [`ImageIndex`].
///
/// This mirrors [`index_text::IndexBuilder`] rather than inventing a second idiom, because an image
/// **is** a document here — that is the entire architectural bet.
pub struct ImageIndexBuilder {
    text: IndexBuilder,
    vector: VectorColumn,
    dhash: Vec<Option<Hash64>>,
    phash: Vec<Option<Hash64>>,
    digest: Vec<[u8; 32]>,
    /// doc -> vector slot. `VectorColumn` is dense by design (it is a flat scan; a hole would cost
    /// a branch in the hot loop for every document, forever, to serve the ingest's transient
    /// state). So sparsity lives here instead, as one indirection consulted only on a hit.
    slot_of_doc: Vec<Option<u32>>,
    /// vector slot -> doc, the inverse, so a vector result can name a document.
    doc_of_slot: Vec<u32>,
    has_vector: bool,
}

impl ImageIndexBuilder {
    /// `schema` describes the *text* fields an image carries — filename, caption, alt text, OCR,
    /// camera model. `dim` is the embedding width, or 0 for a corpus with no embeddings at all.
    pub fn new(schema: Schema, dim: usize, metric: Metric) -> Self {
        ImageIndexBuilder {
            text: IndexBuilder::new(schema),
            vector: VectorColumn::new(dim, metric),
            dhash: Vec::new(),
            phash: Vec::new(),
            digest: Vec::new(),
            slot_of_doc: Vec::new(),
            doc_of_slot: Vec::new(),
            has_vector: dim > 0,
        }
    }

    /// Declare a facet slot, in slot order. Passthrough to the text builder so a caller never has
    /// to reach past this type to reach `p30`.
    pub fn with_facet(mut self, field: usize) -> Self {
        self.text = self.text.with_facet(field);
        self
    }

    /// Declare a numeric column, in slot order. Passthrough to `p31`.
    pub fn with_numeric(mut self, field: usize) -> Self {
        self.text = self.text.with_numeric(field);
        self
    }

    /// Declare a facet on an **unscored column**, resolved by name (`p65`).
    ///
    /// This is the declaration an image document actually wants. `p60` measured the collision that
    /// produced `p65`: a scraped image yields seven columns — path, format, orientation, colour,
    /// width, height, byte length — against a four-scored-field budget, so three of them could not
    /// be indexed at all and width and height carried no range predicate. An unscored column costs
    /// nothing per posting, which is why the fix was to stop spending a *scoring* slot on a value
    /// nothing scores rather than to widen the slot for everybody.
    pub fn with_facet_of(mut self, name: &str) -> Self {
        self.text = self.text.with_facet_of(name);
        self
    }

    /// Declare a numeric range column on an **unscored column**, resolved by name (`p65`).
    pub fn with_numeric_of(mut self, name: &str) -> Self {
        self.text = self.text.with_numeric_of(name);
        self
    }

    /// Add one image.
    ///
    /// `embedding` is `None` for a document nobody has run a model over yet, and such a document is
    /// still fully searchable by text, facet, range and hash. **This is the property that lets a
    /// first result appear before an embedding pass has finished**, and it is why the vector column
    /// stores a presence flag rather than assuming density.
    ///
    /// Returns the dense ordinal, or an error if the embedding width disagrees with the schema —
    /// never a panic, because in a real ingest that mismatch means a model was swapped mid-run and
    /// the operator needs to be told, not crashed.
    ///
    /// # Why `image.palette` and `image.exif` are not read here
    ///
    /// This looks like an oversight and is not. [`crate::ImageDoc`] is the *extraction* result —
    /// everything the cheap tier could recover from the bytes. Which of it becomes a **scored text
    /// field**, a **facet**, or a **numeric column** is a schema decision that only the caller can
    /// make: one corpus wants `exif.camera` as a facet and the colour bucket as text, the next
    /// wants the reverse, and a corpus of stripped CDN assets (`docs/research/image.md` §7) has
    /// neither and leans entirely on the filename.
    ///
    /// So the caller flattens with [`crate::meta::Exif::term`], [`crate::meta::Exif::numeric`] and
    /// [`crate::color::Palette::term`], and passes the result in `doc`. Choosing that mapping here
    /// would bake one corpus's schema into the engine — the exact mistake that makes every
    /// incumbent in §1 a photo *application* rather than an index.
    pub fn add(
        &mut self,
        image: &crate::ImageDoc,
        doc: &Doc,
        embedding: Option<&[f32]>,
    ) -> Result<u32, String> {
        let id = self.text.add(doc);
        match (self.has_vector, embedding) {
            (true, Some(v)) => {
                let slot = self.vector.push(v)?;
                self.slot_of_doc.push(Some(slot));
                self.doc_of_slot.push(id);
            }
            (true, None) => self.slot_of_doc.push(None),
            (false, Some(_)) => {
                return Err("embedding supplied but the builder was created with dim = 0".into())
            }
            (false, None) => self.slot_of_doc.push(None),
        }
        self.dhash.push(image.dhash);
        self.phash.push(image.phash);
        self.digest.push(image.digest);
        Ok(id)
    }

    pub fn build(self) -> Result<ImageIndex, String> {
        let text = self.text.build()?;
        let mut by_digest = HashMap::with_capacity(self.digest.len());
        for (i, d) in self.digest.iter().enumerate() {
            // An all-zero digest means NOT COMPUTED, and must never be treated as an identity.
            //
            // `ImageDoc::default()` zeroes this field, so an ingest that has not run `sha256` yet
            // presents every document with the same key. Without this guard the index would report
            // the entire corpus as duplicates of document 0 -- a wrong answer that looks like a
            // spectacularly effective dedup, which is the worst way for it to be wrong.
            //
            // Using the zero value as the sentinel is safe rather than merely convenient: a SHA-256
            // output of 32 zero bytes has never been exhibited and finding one is a preimage break,
            // so the value cannot collide with a real digest in any corpus that will ever exist.
            if *d == ZERO_DIGEST {
                continue;
            }
            // `or_insert` and not `insert`: the FIRST document wins, so the answer does not depend
            // on directory-walk order.
            by_digest.entry(*d).or_insert(i as u32);
        }
        Ok(ImageIndex {
            by_digest,
            text,
            vector: self.vector,
            dhash: self.dhash,
            phash: self.phash,
            digest: self.digest,
            slot_of_doc: self.slot_of_doc,
            doc_of_slot: self.doc_of_slot,
        })
    }
}

/// An immutable image index: text, facets, ranges, vectors and hashes in one object.
pub struct ImageIndex {
    text: index_text::Index,
    vector: VectorColumn,
    dhash: Vec<Option<Hash64>>,
    phash: Vec<Option<Hash64>>,
    digest: Vec<[u8; 32]>,
    slot_of_doc: Vec<Option<u32>>,
    doc_of_slot: Vec<u32>,
    /// Content address -> the FIRST document carrying it. Built once at `build`.
    by_digest: HashMap<[u8; 32], u32>,
}

impl ImageIndex {
    pub fn doc_count(&self) -> usize {
        self.text.doc_count()
    }

    /// The underlying text engine, for the facet tallies and range histograms `p30`/`p31` already
    /// provide. Exposed rather than re-wrapped: a filter bar over images is the same filter bar.
    pub fn text(&self) -> &index_text::Index {
        &self.text
    }

    pub fn vector(&self) -> &VectorColumn {
        &self.vector
    }

    /// Has an embedding been computed for this document yet?
    ///
    /// A corpus is embedded incrementally -- `docs/research/image.md` §8 records `rclip` taking 15
    /// hours for 84,725 images on a weak CPU -- so "searchable now, semantically searchable later"
    /// is the normal state of an index, not a transient one. A caller showing a progress bar, or
    /// deciding whether a vector arm is worth running, needs to be able to ask.
    pub fn has_embedding(&self, doc: u32) -> bool {
        self.slot_of_doc.get(doc as usize).copied().flatten().is_some()
    }

    /// How many documents carry an embedding.
    pub fn embedded_count(&self) -> usize {
        self.doc_of_slot.len()
    }

    /// The DCT hash column. Survives the gamma and colour shifts that defeat dHash, at ~1-5 ms per
    /// image against dHash's <1 ms (`docs/research/image.md` §6) -- so both are stored and the
    /// caller picks per query rather than the crate picking once for everyone.
    pub fn phash_of(&self, doc: u32) -> Option<Hash64> {
        self.phash.get(doc as usize).copied().flatten()
    }

    /// The dHash column.
    pub fn dhash_of(&self, doc: u32) -> Option<Hash64> {
        self.dhash.get(doc as usize).copied().flatten()
    }

    /// Exact-duplicate lookup by content address (`p61`).
    ///
    /// # Why this is a map, and why it was linear first
    ///
    /// This started as a linear scan, deliberately: `docs/research/image.md` §5 puts scraped-corpus
    /// duplicate rates anywhere in **3–37 %** depending on method, which is a 12x spread — too wide
    /// to pick a structure from. Building an index before measuring the distribution is how the
    /// wrong index gets built.
    ///
    /// `p60` then measured this repo's own corpus: **9.00 % of files are redundant exact copies**,
    /// 118 digests occurring more than once across 1,500 sampled files. At that rate dedup is a
    /// real ingest path rather than a curiosity, and a linear scan makes ingesting *n* files
    /// O(n^2) — 17,311 files would be ~150 M comparisons of 32-byte keys for a check that should be
    /// one probe. So the map is now earned by a number rather than assumed.
    ///
    /// The first document wins. A later identical file reports the earlier ordinal, so the digest
    /// names the *original* rather than whichever copy the directory walk reached last — which is
    /// what makes the answer independent of filesystem ordering, and therefore reproducible.
    pub fn duplicate_of(&self, digest: &[u8; 32]) -> Option<u32> {
        if *digest == ZERO_DIGEST {
            return None;
        }
        self.by_digest.get(digest).copied()
    }

    /// Every group of two or more documents sharing a content address.
    ///
    /// Returned in ascending order of the first ordinal in each group, and each group internally
    /// ascending, so the report `p61` prints is stable across runs on the same corpus.
    pub fn duplicate_group(&self) -> Vec<Vec<u32>> {
        let mut group: HashMap<&[u8; 32], Vec<u32>> = HashMap::new();
        for (i, d) in self.digest.iter().enumerate() {
            if *d == ZERO_DIGEST {
                continue;
            }
            group.entry(d).or_default().push(i as u32);
        }
        let mut out: Vec<Vec<u32>> = group.into_values().filter(|g| g.len() > 1).collect();
        out.sort_by_key(|g| g[0]);
        out
    }

    /// Bytes that would be saved by storing one copy of each distinct digest.
    ///
    /// Takes the per-document byte length because this index stores a digest, not a file. §5's
    /// conclusion is that on a scraped corpus **dedup beats the codec** — roughly 30 % of LAION-2B
    /// is duplicated, against a ~20 % ceiling for the best lossless transcode — so this number is
    /// the one that should be computed before anyone reaches for libjxl.
    pub fn duplicate_byte(&self, byte_len: &[u64]) -> u64 {
        let mut seen: std::collections::HashSet<&[u8; 32]> = std::collections::HashSet::new();
        let mut saved = 0u64;
        for (i, d) in self.digest.iter().enumerate() {
            if *d == ZERO_DIGEST {
                continue;
            }
            let len = byte_len.get(i).copied().unwrap_or(0);
            if !seen.insert(d) {
                saved = saved.saturating_add(len);
            }
        }
        saved
    }

    /// Near-duplicates of `probe` within a Hamming radius, over the dHash column.
    ///
    /// The radius is the caller's, deliberately: `hash::HASH64_NEAR_MAX` documents the measured
    /// default, and `docs/research/image.md` §6 records that thresholds do **not** port across code
    /// lengths, so a hard-wired constant here would be a trap for anyone switching hash widths.
    pub fn hash_near(&self, probe: Hash64, max: u32) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        for (i, slot) in self.dhash.iter().enumerate() {
            if let Some(h) = slot {
                let d = h.distance(&probe);
                if d <= max {
                    out.push((i as u32, d));
                }
            }
        }
        out.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
        out
    }

    /// **The fused query.** Text, facets, ranges, vector proximity and hash proximity, scored
    /// together, top-k selected once.
    ///
    /// # The order of operations, and why it is this order
    ///
    /// 1. **Hard predicates first, inside the text pass.** `search_clause` applies facet and range
    ///    during scoring, so they cost one traversal and cannot be violated by construction.
    /// 2. **Soft arms next, each over-generating `CANDIDATE_MULTIPLIER * k`.**
    /// 3. **Hard predicates re-applied to the soft arms.** This is the step a fan-out design
    ///    routinely skips, and skipping it is precisely how a vector search leaks a document the
    ///    filter bar excluded. A soft arm knows nothing about facets; it must be filtered, not
    ///    trusted.
    /// 4. **Min-max normalise per arm, combine, sort once.**
    ///
    /// Ties break by agreement count, then by ascending doc id. Agreement first because a document
    /// two independent signals found is genuinely better evidence than one signal's marginally
    /// higher score; doc id last so the result is deterministic, which `p60` requires.
    pub fn search_fused(&self, q: &FusedQuery, k: usize) -> Vec<FusedHit> {
        if k == 0 || self.doc_count() == 0 {
            return Vec::new();
        }
        let wide = k.saturating_mul(CANDIDATE_MULTIPLIER);
        let alpha = q.alpha.unwrap_or(TEXT_ALPHA).clamp(0.0, 1.0);

        // --- arm 1: text + hard predicates, one pass -------------------------------------------
        let text_arm: Vec<(u32, f32)> = match q.text {
            Some(t) if !t.trim().is_empty() => self
                .text
                .search_clause(t, wide, 0, q.facet, q.range)
                .into_iter()
                .map(|h| (h.doc, h.score))
                .collect(),
            _ => Vec::new(),
        };

        // --- arm 2: vector ---------------------------------------------------------------------
        let vector_arm: Vec<(u32, f32)> = match q.vector {
            Some(v) if !self.vector.is_empty() => self
                .vector
                .search(v, wide, DEFAULT_OVERSAMPLE)
                .into_iter()
                // Map slot back to document. A slot with no document is impossible by
                // construction, so it is dropped rather than defaulted -- a silent 0 here would
                // attribute someone else's embedding to document 0.
                .filter_map(|(slot, s)| self.doc_of_slot.get(slot as usize).map(|&d| (d, s)))
                .collect(),
            _ => Vec::new(),
        };

        // --- arm 3: hash -----------------------------------------------------------------------
        // Distance is inverted into a similarity so every arm is "higher is better" before
        // normalisation. Without this the min-max step would rank the *least* similar first.
        // Capped at `wide`, like every other arm.
        //
        // `hash_near` returns EVERY document inside the radius, and on a real corpus that is not a
        // small number: `p60` measured **67.09 % of images within dHash radius 4 of another** on
        // the 17,311-file scrape. An uncapped arm therefore hands the fusion step most of the
        // corpus, and since `hash_near` already sorts by ascending distance, everything past the
        // first `wide` entries is strictly worse than what is already there -- it cannot change the
        // top-k, only pay for it.
        let hash_arm: Vec<(u32, f32)> = match q.hash {
            Some((probe, max)) => {
                let scale = max.max(1) as f32;
                self.hash_near(probe, max)
                    .into_iter()
                    .take(wide)
                    .map(|(doc, d)| (doc, 1.0 - (d as f32 / scale)))
                    .collect()
            }
            None => Vec::new(),
        };

        // --- step 3: the hard predicates the soft arms know nothing about -----------------------
        let admissible = |doc: u32| -> bool { self.satisfies_hard(doc, q.facet, q.range) };

        // Weight is distributed over the arm that actually FIRED, not over every arm that could
        // have. Dropping an absent arm's share instead would make a vector-only query top out at
        // `1 - alpha` while the same query alongside text reaches 1.0 -- so the score would encode
        // *which arms the caller supplied* rather than how good the match is, and any caller
        // thresholding on score would silently get a different bar per query shape. §7's finding
        // that most documents in a real corpus carry almost no signal makes the single-arm case the
        // common one, not the edge case.
        let soft_count = !vector_arm.is_empty() as u32 + !hash_arm.is_empty() as u32;
        let (text_weight, soft_weight) = match (text_arm.is_empty(), soft_count) {
            (true, 0) => (0.0, 0.0),
            (true, n) => (0.0, 1.0 / n as f32),
            (false, 0) => (1.0, 0.0),
            (false, n) => (alpha, (1.0 - alpha) / n as f32),
        };

        let mut acc: HashMap<u32, (f32, Why)> = HashMap::new();
        merge(&mut acc, &text_arm, text_weight, |w| &mut w.text, &admissible);
        merge(&mut acc, &vector_arm, soft_weight, |w| &mut w.vector, &admissible);
        merge(&mut acc, &hash_arm, soft_weight, |w| &mut w.hash, &admissible);

        // --- step 4: one selection --------------------------------------------------------------
        let mut out: Vec<FusedHit> =
            acc.into_iter().map(|(doc, (score, why))| FusedHit { doc, score, why }).collect();
        out.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.why.agreement().cmp(&a.why.agreement()))
                .then(a.doc.cmp(&b.doc))
        });
        out.truncate(k);
        out
    }

    /// Does this document satisfy every hard predicate?
    ///
    /// Mirrors the semantics `FacetClause` documents: an *include* drops a document with no value
    /// in the slot, an *exclude* keeps it. Getting that backwards is, in that type's own words, how
    /// a filter silently returns the entire corpus.
    fn satisfies_hard(
        &self,
        doc: u32,
        facet: &[FacetClause],
        range: &[(usize, f64, f64)],
    ) -> bool {
        for clause in facet {
            let value = self.text.facet_of_at(doc, clause.slot);
            let hit = value.is_some_and(|v| clause.value.contains(&v));
            if hit == clause.exclude {
                return false;
            }
        }
        for &(slot, lo, hi) in range {
            // Half-open `lo <= v < hi`, matching `p31`. An unparseable value is NaN and is in no
            // range at all — not even an infinite one — which is that row's stated decision.
            match self.text.numeric_of(doc, slot) {
                Some(v) if v >= lo && v < hi => {}
                _ => return false,
            }
        }
        true
    }
}

/// Min-max normalise one arm and fold it into the accumulator.
///
/// A single-element arm normalises to 1.0 rather than to 0.0. A naive `(s - lo) / (hi - lo)` makes
/// the only member of a one-hit arm score zero, which silently deletes the arm's contribution
/// exactly when it was most confident. `fuse::convex` handles the same case the same way.
fn merge(
    acc: &mut HashMap<u32, (f32, Why)>,
    arm: &[(u32, f32)],
    weight: f32,
    flag: impl Fn(&mut Why) -> &mut bool,
    admissible: &impl Fn(u32) -> bool,
) {
    if arm.is_empty() || weight <= 0.0 {
        return;
    }
    let (lo, hi) = arm
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &(_, s)| (lo.min(s), hi.max(s)));
    let span = (hi - lo).max(f32::MIN_POSITIVE);
    for &(doc, score) in arm {
        if !admissible(doc) {
            continue;
        }
        let norm = if arm.len() == 1 { 1.0 } else { (score - lo) / span };
        let entry = acc.entry(doc).or_insert((0.0, Why::default()));
        entry.0 += weight * norm;
        *flag(&mut entry.1) = true;
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::vector::Metric;
    use index_text::Field;

    /// A deterministic stand-in for an embedding model.
    ///
    /// `bench/roadmap/p60-image-corpus.md` forbids reporting a recall verdict over synthetic
    /// vectors, and that rule holds here too: these tests assert *plumbing* — that a document is
    /// reachable, that a filter is not leaked, that selection happens once. They do not, and must
    /// not, claim anything about retrieval quality.
    fn synthetic(seed: u64, dim: usize) -> Vec<f32> {
        let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        (0..dim)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                ((x >> 40) as f32 / 8_388_608.0) - 1.0
            })
            .collect()
    }

    const DIM: usize = 32;

    /// brand / title / year, mirroring how a real image document is laid out: a facet field, a
    /// scored text field, and a numeric column.
    fn schema() -> Schema {
        Schema::new(vec![
            Field::new("brand", 3.0, 0.4),
            Field::new("title", 2.0, 0.4),
            Field::new("year", 1.0, 0.4),
        ])
    }

    /// 40 documents in two brands and two year cohorts, every one embedded and hashed.
    fn fixture() -> ImageIndex {
        let mut b =
            ImageIndexBuilder::new(schema(), DIM, Metric::Cosine).with_facet(0).with_numeric(2);
        for i in 0..40u32 {
            let brand = if i % 2 == 0 { "acme" } else { "globex" };
            let year = if i < 20 { 2019 } else { 2023 };
            let doc = Doc::new([brand.to_string(), format!("beach photo {i}"), year.to_string()]);
            let image = crate::ImageDoc {
                digest: [i as u8; 32],
                dhash: Some(Hash64(1u64 << (i % 64))),
                phash: Some(Hash64(1u64 << ((i + 7) % 64))),
                ..Default::default()
            };
            b.add(&image, &doc, Some(&synthetic(i as u64 + 1, DIM))).unwrap();
        }
        b.build().unwrap()
    }

    #[test]
    fn a_full_page_is_returned_when_enough_document_match() {
        // THE defect the fan-out architectures have structurally. `p59` acceptance check 1.
        let ix = fixture();
        for k in [1usize, 5, 10, 20, 40] {
            let q = FusedQuery {
                text: Some("beach photo"),
                vector: Some(&synthetic(3, DIM)),
                ..Default::default()
            };
            let hit = ix.search_fused(&q, k);
            assert_eq!(hit.len(), k.min(40), "short page at k={k}");
        }
    }

    #[test]
    fn a_facet_filter_is_never_leaked_by_the_vector_arm() {
        // The vector arm knows nothing about facets. If its candidates are trusted rather than
        // re-filtered, a "brand: acme" query silently returns globex images. `p59` check 2.
        let ix = fixture();
        let clause = [FacetClause::any(0, &["acme"])];
        let q = FusedQuery {
            text: Some("beach"),
            facet: &clause,
            vector: Some(&synthetic(9, DIM)),
            hash: Some((Hash64(1), 64)),
            ..Default::default()
        };
        let hit = ix.search_fused(&q, 40);
        assert!(!hit.is_empty());
        for h in &hit {
            assert_eq!(ix.text().facet_of_at(h.doc, 0), Some("acme"), "leaked doc {}", h.doc);
        }
    }

    #[test]
    fn a_numeric_range_is_never_leaked_by_a_soft_arm() {
        let ix = fixture();
        let range = [(0usize, 2019.0, 2020.0)];
        let q = FusedQuery {
            text: Some("beach"),
            range: &range,
            vector: Some(&synthetic(11, DIM)),
            ..Default::default()
        };
        for h in ix.search_fused(&q, 40) {
            let v = ix.text().numeric_of(h.doc, 0).unwrap();
            assert!((2019.0..2020.0).contains(&v), "leaked doc {} with year {v}", h.doc);
        }
    }

    #[test]
    fn a_document_two_signal_found_outranks_one_a_single_signal_found() {
        // Explicability is not decoration: `p59` check 5 requires agreement to break ties, because
        // two independent signals agreeing is better evidence than one signal scoring marginally
        // higher.
        let ix = fixture();
        let probe = ix.dhash_of(5).unwrap();
        let q = FusedQuery {
            text: Some("beach photo 5"),
            vector: Some(&synthetic(6, DIM)),
            hash: Some((probe, 4)),
            ..Default::default()
        };
        let hit = ix.search_fused(&q, 10);
        assert!(!hit.is_empty());
        for h in &hit {
            assert!(h.why.agreement() >= 1, "a hit with no reason: {h:?}");
        }
        assert!(hit[0].why.agreement() >= hit[hit.len() - 1].why.agreement());
    }

    #[test]
    fn every_arm_can_carry_a_query_alone() {
        // §7: EXIF is stripped from essentially every downloadable image, so a corpus where only
        // one signal is present is the normal case. Each arm must stand up unaccompanied.
        let ix = fixture();
        let text_only =
            ix.search_fused(&FusedQuery { text: Some("beach"), ..Default::default() }, 5);
        assert_eq!(text_only.len(), 5);
        assert!(text_only.iter().all(|h| h.why.text && !h.why.vector));

        let vector_only = ix
            .search_fused(&FusedQuery { vector: Some(&synthetic(2, DIM)), ..Default::default() }, 5);
        assert_eq!(vector_only.len(), 5);
        assert!(vector_only.iter().all(|h| h.why.vector && !h.why.text));

        let probe = ix.dhash_of(3).unwrap();
        let hash_only =
            ix.search_fused(&FusedQuery { hash: Some((probe, 2)), ..Default::default() }, 5);
        assert!(hash_only.iter().all(|h| h.why.hash));
        assert!(hash_only.iter().any(|h| h.doc == 3));
    }

    #[test]
    fn fusion_reaches_a_document_neither_arm_rank_first() {
        // The argument in this module's header, made executable. A document that is mid-ranked by
        // BOTH arms must be able to reach a small page — which is exactly what per-arm top-k
        // followed by intersection cannot do.
        let ix = fixture();
        let q = FusedQuery {
            text: Some("beach photo"),
            vector: Some(&synthetic(17, DIM)),
            ..Default::default()
        };
        let fused = ix.search_fused(&q, 5);
        let both: Vec<u32> =
            fused.iter().filter(|h| h.why.text && h.why.vector).map(|h| h.doc).collect();
        assert!(!both.is_empty(), "no document was corroborated by both arms");
    }

    #[test]
    fn the_result_is_deterministic() {
        // `p60` requires same corpus, same verdict, every run.
        let ix = fixture();
        let q = FusedQuery {
            text: Some("beach photo"),
            vector: Some(&synthetic(4, DIM)),
            hash: Some((Hash64(1 << 3), 8)),
            ..Default::default()
        };
        let a = ix.search_fused(&q, 20);
        for _ in 0..5 {
            assert_eq!(a, ix.search_fused(&q, 20));
        }
    }

    #[test]
    fn a_degenerate_query_return_rather_than_panicking() {
        let ix = fixture();
        assert!(ix.search_fused(&FusedQuery::default(), 10).is_empty());
        assert!(ix
            .search_fused(&FusedQuery { text: Some("beach"), ..Default::default() }, 0)
            .is_empty());
        assert!(ix
            .search_fused(&FusedQuery { text: Some("   "), ..Default::default() }, 5)
            .is_empty());
        // A query vector of the wrong width must not panic; the arm simply contributes nothing.
        let short = vec![0.5f32; DIM / 2];
        let _ = ix.search_fused(&FusedQuery { vector: Some(&short), ..Default::default() }, 5);
        // NaN must not corrupt the ordering.
        let nan = vec![f32::NAN; DIM];
        let _ = ix.search_fused(&FusedQuery { vector: Some(&nan), ..Default::default() }, 5);
    }

    #[test]
    fn a_document_without_an_embedding_is_still_searchable() {
        // "Searchable now, semantically searchable later" is the normal state of an ingest.
        let mut b = ImageIndexBuilder::new(schema(), DIM, Metric::Cosine).with_facet(0);
        for i in 0..10u32 {
            let doc = Doc::new(["acme".to_string(), format!("sunset {i}"), "2020".to_string()]);
            let image = crate::ImageDoc { digest: [i as u8; 32], ..Default::default() };
            let embedding = (i % 2 == 0).then(|| synthetic(i as u64 + 1, DIM));
            b.add(&image, &doc, embedding.as_deref()).unwrap();
        }
        let ix = b.build().unwrap();
        assert_eq!(ix.embedded_count(), 5);
        assert!(ix.has_embedding(0) && !ix.has_embedding(1));

        // The unembedded half must still be reachable by text.
        let hit = ix.search_fused(&FusedQuery { text: Some("sunset 1"), ..Default::default() }, 10);
        assert!(hit.iter().any(|h| h.doc == 1), "an unembedded document became unreachable");
    }

    #[test]
    fn a_vector_hit_name_the_right_document() {
        // The slot->doc mapping is the one place a sparse column can silently attribute one
        // image's embedding to another. Assert the identity directly.
        let mut b = ImageIndexBuilder::new(schema(), DIM, Metric::Cosine);
        for i in 0..6u32 {
            let doc = Doc::new(["b".to_string(), format!("t{i}"), "2020".to_string()]);
            let image = crate::ImageDoc::default();
            // Only odd documents get an embedding, so slot != doc for every one of them.
            let embedding = (i % 2 == 1).then(|| synthetic(i as u64 + 100, DIM));
            b.add(&image, &doc, embedding.as_deref()).unwrap();
        }
        let ix = b.build().unwrap();
        for probe in [1u32, 3, 5] {
            let q = FusedQuery {
                vector: Some(&synthetic(probe as u64 + 100, DIM)),
                ..Default::default()
            };
            let hit = ix.search_fused(&q, 1);
            assert_eq!(hit[0].doc, probe, "slot/doc mapping is wrong");
        }
    }

    #[test]
    fn a_score_does_not_depend_on_which_arm_the_caller_supplied() {
        // The regression this guards: if an absent arm's weight is dropped rather than
        // redistributed, the SAME best match scores 1.0 in a two-arm query and 0.5 in a one-arm
        // query, so any caller thresholding on score gets a different bar per query shape.
        let ix = fixture();
        let text_only =
            ix.search_fused(&FusedQuery { text: Some("beach photo 7"), ..Default::default() }, 3);
        let vector_only = ix
            .search_fused(&FusedQuery { vector: Some(&synthetic(8, DIM)), ..Default::default() }, 3);
        for hit in [&text_only, &vector_only] {
            let top = hit[0].score;
            assert!((top - 1.0).abs() < 1e-5, "a single-arm top hit scored {top}, expected 1.0");
        }
        // And a two-arm query still reaches 1.0 when both arms agree on their best document.
        let fused = ix.search_fused(
            &FusedQuery {
                text: Some("beach photo 7"),
                vector: Some(&synthetic(8, DIM)),
                ..Default::default()
            },
            3,
        );
        assert!(fused[0].score <= 1.0 + 1e-5, "fused score exceeded the normalised ceiling");
    }

    #[test]
    fn an_exact_duplicate_is_found_by_digest() {
        let ix = fixture();
        assert_eq!(ix.duplicate_of(&[7u8; 32]), Some(7));
        assert_eq!(ix.duplicate_of(&[255u8; 32]), None);
    }

    #[test]
    fn an_uncomputed_digest_is_not_an_identity() {
        // The trap: `ImageDoc::default()` zeroes the digest, so an ingest that has not run sha256
        // yet presents every document with the same key. Reporting the whole corpus as duplicates
        // of document 0 would look like a spectacularly effective dedup — the worst way to be
        // wrong.
        let mut b = ImageIndexBuilder::new(schema(), 0, Metric::Cosine);
        for i in 0..6u32 {
            let doc = Doc::new(["b".to_string(), format!("t{i}"), "2020".to_string()]);
            b.add(&crate::ImageDoc::default(), &doc, None).unwrap();
        }
        let ix = b.build().unwrap();
        assert_eq!(ix.duplicate_of(&[0u8; 32]), None, "an uncomputed digest was treated as an id");
        assert!(ix.duplicate_group().is_empty(), "uncomputed digests were grouped as duplicates");
        assert_eq!(ix.duplicate_byte(&[100; 6]), 0, "uncomputed digests were counted as savings");
    }

    #[test]
    fn a_duplicate_group_names_the_first_document_and_count_its_byte() {
        let mut b = ImageIndexBuilder::new(schema(), 0, Metric::Cosine);
        // Documents 1, 3 and 4 are the same bytes; 0, 2, 5 are distinct.
        for (i, tag) in [10u8, 42, 11, 42, 42, 12].into_iter().enumerate() {
            let doc = Doc::new(["b".to_string(), format!("t{i}"), "2020".to_string()]);
            b.add(&crate::ImageDoc { digest: [tag; 32], ..Default::default() }, &doc, None).unwrap();
        }
        let ix = b.build().unwrap();
        // The FIRST occurrence wins, so the answer does not depend on walk order.
        assert_eq!(ix.duplicate_of(&[42u8; 32]), Some(1));
        assert_eq!(ix.duplicate_group(), vec![vec![1u32, 3, 4]]);
        // Two of the three copies are redundant, so two files' worth of bytes are saved.
        assert_eq!(ix.duplicate_byte(&[7, 7, 7, 7, 7, 7]), 14);
    }
}
