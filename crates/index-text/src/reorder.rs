//! Document-id reordering by recursive graph bisection — the one lever
//! `docs/research/speed.md` ranks as the cheapest remaining multiplier, and the one this repo
//! has tried once before in a DIFFERENT shape and measured worse.
//!
//! The earlier attempt (recorded on `Index::posting`, and in `bench/roadmap/p7-scale.md`) sorted
//! documents by total field length. It lost at 1 M: typo p99 6.40 → 7.02 ms, because the DepEd
//! corpus has nearly uniform lengths, so length-sort separated scores hardly at all while the
//! indirection cost unconditionally. Length is the wrong objective anyway: what compresses and
//! skips a posting list is **documents that share terms standing adjacent in doc-id order**,
//! which is what bisection clusters directly.
//!
//! The split is a **cut-minimizing refinement**: a bisection starts balanced (alternating by
//! current order), then documents move to the side where the documents sharing their terms
//! already sit, one at a time, each move scored by how many crossing term pairs it removes and
//! refused when it would tip the balance. The cost being minimized is
//! `sum over terms of L(t) x R(t)` — the crossing pairs, i.e. exactly the big gaps a delta or
//! block-FOR encoder pays for. Greedy sequential moves are deterministic given the traversal
//! order, which is what `serialization_is_deterministic` needs; there is no randomness anywhere.
//!
//! PISA ships a BFS-seeded variant of the same idea (Dhulipala et al. 2016). A one-shot BFS
//! flood was tried first here and collapsed on any corpus with a near-universal term — the
//! flood reaches everything in two hops and the parity partition goes lopsided — so the
//! refinement form is what shipped.

/// Below this size a range is emitted in its current order: the refinement's bookkeeping costs
/// more than the compression a few hundred documents leave on the table.
const MIN_RANGE: usize = 512;

/// A move is refused when it would push its receiving side past half the range plus this much
/// slack — balance is what makes the recursion terminate in log-depth halves.
const SLACK: usize = 16;

/// Refinement rounds per bisection. The first two do almost all of the moving; the cap only
/// bounds the tail.
const ROUNDS: usize = 8;

/// One bisection's side bookkeeping, stamped per generation so it resets in O(1) between
/// ranges instead of O(n).
struct Sides {
    stamp: Vec<u32>,
    side: Vec<u8>,
    generation: u32,
}

impl Sides {
    fn new(n: usize) -> Self {
        Sides { stamp: vec![0; n], side: vec![0; n], generation: 0 }
    }
    fn get(&self, doc: usize) -> u8 {
        if self.stamp[doc] == self.generation {
            self.side[doc]
        } else {
            0
        }
    }
    fn set(&mut self, doc: usize, side: u8) {
        self.stamp[doc] = self.generation;
        self.side[doc] = side;
    }
}

/// Split `order[start..start+len]` into a left and a right half that minimize the crossing-pair
/// cost, written back contiguously; returns the left half's length, or `None` when the range is
/// too small to bother with. `doc_terms`/`doc_terms_at` is the CSR of each document's term ids;
/// `cnt_l`/`cnt_r` are per-term counters the caller owns the storage of and this call owns the
/// values of.
#[allow(clippy::too_many_arguments)]
fn bisect(
    order: &mut [u32],
    start: usize,
    len: usize,
    doc_terms: &[u32],
    doc_terms_at: &[usize],
    cnt_l: &mut [u32],
    cnt_r: &mut [u32],
    sides: &mut Sides,
) -> Option<usize> {
    if len < 2 * MIN_RANGE {
        return None;
    }
    sides.generation += 1;
    // Balanced start: alternating by current order. Deterministic, exactly half/half, and it
    // gives the refinement a real gradient — every term starts with docs on both sides.
    for (i, &d) in order[start..start + len].iter().enumerate() {
        sides.set(d as usize, (i % 2) as u8);
    }
    // Per-term side counts, restricted to this range's documents. The counters are caller-owned
    // storage reused across bisection levels, so every term this range touches is reset FIRST —
    // stale counts from a sibling level would corrupt every gain below.
    for &d in &order[start..start + len] {
        let du = d as usize;
        for &t in &doc_terms[doc_terms_at[du]..doc_terms_at[du + 1]] {
            cnt_l[t as usize] = 0;
            cnt_r[t as usize] = 0;
        }
    }
    for &d in &order[start..start + len] {
        let du = d as usize;
        for &t in &doc_terms[doc_terms_at[du]..doc_terms_at[du + 1]] {
            if sides.get(du) == 0 {
                cnt_l[t as usize] += 1;
            } else {
                cnt_r[t as usize] += 1;
            }
        }
    }
    let mut n_l = len as u64 / 2;
    let mut n_r = len as u64 - n_l;
    let half = len as u64 / 2;
    let slack = half.min(SLACK as u64);
    for _ in 0..ROUNDS {
        let mut round_moves = 0u64;
        // Docs are visited in current relative order, which is what makes the same corpus
        // produce the same permutation on every machine.
        for &d in &order[start..start + len] {
            let d = d as usize;
            let is_left = sides.get(d) == 0;
            // Gain of moving this document: each of its terms contributes `l_t x r_t` crossings
            // before and `(l_t -+ 1) x (r_t -+ 1)` after, so the summed improvement is
            // `sum(l_t - r_t - 1)` for left->right, symmetric for the other direction.
            let mut gain: i64 = 0;
            for &t in &doc_terms[doc_terms_at[d]..doc_terms_at[d + 1]] {
                let (l, r) = (cnt_l[t as usize] as i64, cnt_r[t as usize] as i64);
                gain += if is_left { l - r - 1 } else { r - l - 1 };
            }
            if gain <= 0 {
                continue;
            }
            // Balance capacity for the RECEIVING side.
            let ok = if is_left {
                n_l > half - slack && n_r < half + slack
            } else {
                n_r > half - slack && n_l < half + slack
            };
            if !ok {
                continue;
            }
            for &t in &doc_terms[doc_terms_at[d]..doc_terms_at[d + 1]] {
                if is_left {
                    cnt_l[t as usize] -= 1;
                    cnt_r[t as usize] += 1;
                } else {
                    cnt_r[t as usize] -= 1;
                    cnt_l[t as usize] += 1;
                }
            }
            sides.set(d, u8::from(!is_left));
            if is_left {
                n_l -= 1;
            } else {
                n_r -= 1;
            }
            round_moves += 1;
        }
        if round_moves * 100 < len as u64 {
            break;
        }
    }
    // Write back: left half then right half, each preserving its relative order.
    let mut left: Vec<u32> = Vec::with_capacity(n_l as usize);
    let mut right: Vec<u32> = Vec::with_capacity(n_r as usize);
    for &d in &order[start..start + len] {
        if sides.get(d as usize) == 0 {
            left.push(d);
        } else {
            right.push(d);
        }
    }
    order[start..start + left.len()].copy_from_slice(&left);
    order[start + left.len()..start + len].copy_from_slice(&right);
    Some(left.len())
}

/// The permutation, as `perm[old_doc] = new_doc`. Identity below `2 * MIN_RANGE` documents.
pub(crate) fn graph_bisect_permutation(doc_count: usize, term_docs: &[&[u32]]) -> Vec<u32> {
    if doc_count < 2 * MIN_RANGE || term_docs.is_empty() {
        // No terms is a legitimate build (empty documents); there is nothing to cluster on.
        return (0..doc_count as u32).collect();
    }
    // CSR doc → term ids. Iterating terms in id order fills each document's list already
    // sorted, so adjacency needs no per-doc sort.
    let mut doc_terms_at = vec![0usize; doc_count + 1];
    for list in term_docs {
        for &d in list.iter() {
            doc_terms_at[d as usize + 1] += 1;
        }
    }
    for i in 0..doc_count {
        doc_terms_at[i + 1] += doc_terms_at[i];
    }
    let mut doc_terms = vec![0u32; doc_terms_at[doc_count]];
    let mut fill = doc_terms_at[..doc_count].to_vec();
    for (t, list) in term_docs.iter().enumerate() {
        for &d in list.iter() {
            let du = d as usize;
            doc_terms[fill[du]] = t as u32;
            fill[du] += 1;
        }
    }
    let mut order: Vec<u32> = (0..doc_count as u32).collect();
    let mut sides = Sides::new(doc_count);
    let mut cnt_l = vec![0u32; term_docs.len()];
    let mut cnt_r = vec![0u32; term_docs.len()];
    let mut stack: Vec<(usize, usize)> = vec![(0, doc_count)];
    while let Some((start, len)) = stack.pop() {
        if let Some(split) = bisect(
            &mut order,
            start,
            len,
            &doc_terms,
            &doc_terms_at,
            &mut cnt_l,
            &mut cnt_r,
            &mut sides,
        ) {
            stack.push((start, split));
            stack.push((start + split, len - split));
        }
    }
    let mut perm = vec![0u32; doc_count];
    for (new, &old) in order.iter().enumerate() {
        perm[old as usize] = new as u32;
    }
    perm
}

/// Apply `perm[old] = new` to an array indexed by old document, by shadow copy. Runs once per
/// build — for a String store the copy is kilobytes against minutes of build — so the obvious
/// version beats an in-place cycle permutation whose off-by-one is a silently corrupt corpus.
pub(crate) fn apply<T: Clone>(v: &mut [T], perm: &[u32]) {
    let src = v.to_vec();
    for (old, &p) in perm.iter().enumerate() {
        v[p as usize] = src[old].clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two topic clusters with almost no shared vocabulary must separate cleanly; the metric
    /// the codec cares about is the sum of adjacent gaps inside posting lists.
    fn gaps(perm: &[u32], lists: &[Vec<u32>]) -> u64 {
        let mut total = 0u64;
        for list in lists {
            let mut ids: Vec<u32> = list.iter().map(|&d| perm[d as usize]).collect();
            ids.sort_unstable();
            for w in ids.windows(2) {
                total += (w[1] - w[0]) as u64;
            }
        }
        total
    }

    fn cluster_corpus() -> (usize, Vec<Vec<u32>>) {
        // 2048 docs, two clusters of 1024; cluster A's docs share terms a0..a15, cluster B's
        // share b0..b15, and one bridge term touches a single doc of each cluster. Deliberately
        // DISCONNECTED apart from the bridge: the refinement must still find the split that a
        // BFS flood could not.
        let n = 2048usize;
        let mut lists: Vec<Vec<u32>> = Vec::new();
        for t in 0..16u32 {
            lists.push((0..1024u32).filter(|d| (d + t) % 8 == 0).collect());
        }
        for t in 0..16u32 {
            lists.push((1024..2048u32).filter(|d| (d + t) % 8 == 0).collect());
        }
        lists.push(vec![10, 1500]);
        (n, lists)
    }

    #[test]
    fn bisection_clusters_documents_that_share_terms() {
        let (n, lists) = cluster_corpus();
        let refs: Vec<&[u32]> = lists.iter().map(|l| &l[..]).collect();
        let before = gaps(&(0..n as u32).collect::<Vec<_>>(), &lists);
        let perm = graph_bisect_permutation(n, &refs);
        let after = gaps(&perm, &lists);
        assert!(after < before / 2, "gap sum {before} -> {after}, expected at least halved");
        let mut seen = vec![false; n];
        for &p in &perm {
            assert!(!seen[p as usize], "perm maps two docs to {p}");
            seen[p as usize] = true;
        }
    }

    #[test]
    fn permutation_is_deterministic() {
        let (n, lists) = cluster_corpus();
        let refs: Vec<&[u32]> = lists.iter().map(|l| &l[..]).collect();
        assert_eq!(graph_bisect_permutation(n, &refs), graph_bisect_permutation(n, &refs));
    }

    #[test]
    fn small_corpora_get_the_identity() {
        let lists = [vec![1u32, 0], vec![2]];
        let refs: Vec<&[u32]> = lists.iter().map(|l| &l[..]).collect();
        assert_eq!(graph_bisect_permutation(3, &refs), vec![0, 1, 2]);
    }

    /// The definition the builder's shadow-copy apply must match, pinned on a cycle-heavy
    /// permutation with fixed points.
    #[test]
    fn apply_semantics() {
        let perm: Vec<u32> = vec![2, 0, 3, 1, 4, 5];
        let v: Vec<String> = (0..6).map(|i| format!("doc{i}")).collect();
        let out = {
            let mut out = v.clone();
            for (old, &p) in perm.iter().enumerate() {
                out[p as usize] = v[old].clone();
            }
            out
        };
        assert_eq!(out[2], "doc0");
        assert_eq!(out[0], "doc1");
        assert_eq!(out[3], "doc2");
        assert_eq!(out[1], "doc3");
        assert_eq!(out[4], "doc4");
        assert_eq!(out[5], "doc5");
    }
}

    /// The wiring test the byte-level tests cannot cover: with reordering FORCED ON, a build
    /// answers with the same (key, score) sets as the same rows built without it. Doc ORDINALS
    /// differ — that is the point — so the comparison is by the key each document carries,
    /// which is what an ordinal-mapping consumer must switch to before opting in.
    #[test]
    fn reordered_build_answers_by_the_same_keys_and_scores() {
        use crate::index::{Doc, Field, IndexBuilder, Schema};
        // The key rides in its own field: a key is declared by FIELD, not per row.
        let schema = Schema::new(vec![Field::new("name", 1.0, 0.4), Field::new("sku", 0.0, 0.4)]);
        let mut rows: Vec<(String, String)> = Vec::new();
        for d in 0..1500u32 {
            // Clustered vocabulary: three topic groups sharing most of their tokens, so the
            // bisection has something real to move.
            let topic = d % 3;
            rows.push((
                format!("alpha{topic} beta{} common{} delta{}", d % 7, d % 11, d % 5),
                format!("k{d:05}"),
            ));
        }
        let build = |reorder: bool| {
            let mut b = IndexBuilder::new(schema.clone()).with_key_of("sku");
            b = b.with_doc_reorder(reorder);
            for (name, sku) in &rows {
                b.add(&Doc::new([name.as_str(), sku.as_str()]));
            }
            b.build().unwrap()
        };
        let plain = build(false);
        let reordered = build(true);
        assert_ne!(plain.to_bytes(), reordered.to_bytes(), "reordering must move docids");
        for q in ["alpha0 beta3", "alpha1 common4", "delta2", "k00042", "alpha2 beta6"] {
            // The invariant is PER KEY: a given document must score identically in both builds.
            // Which exactly-tied documents fill the last top-k slots is scan-order luck — the
            // same arbitrariness `searcher` documents for segment-merged near-ties — so the
            // boundary membership of an all-ties query is allowed to differ.
            let score_of = |ix: &crate::index::Index| -> std::collections::HashMap<String, u32> {
                ix.search(q, 25)
                    .iter()
                    .map(|h| (ix.key_of(h.doc).unwrap().to_string(), h.score.to_bits()))
                    .collect()
            };
            let a = score_of(&plain);
            let c = score_of(&reordered);
            // (1) the top-k SCORES are the same multiset — no document moved tiers;
            let mut sa: Vec<u32> = a.values().copied().collect();
            let mut sc: Vec<u32> = c.values().copied().collect();
            sa.sort_unstable();
            sc.sort_unstable();
            assert_eq!(sa, sc, "query {q:?} score multiset differs");
            // (2) keys present in BOTH builds scored identically; keys in only one are the
            // documented tie-boundary arbitrariness.
            for (k, v) in &a {
                if let Some(w) = c.get(k) {
                    assert_eq!(v, w, "key {k} scored differently (query {q:?})");
                }
            }
            assert!(!a.is_empty(), "query {q:?} returned nothing from the plain build");
        }
    }
