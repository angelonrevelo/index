//! Rank fusion.
//!
//! **RRF at k=60 is the built-in default, not a configuration option.** Three unconnected codebases
//! on this machine — presyo, sisia-app and maphy — independently converged on exactly that constant
//! (`docs/research/demand.md` Finding 1). The literature agrees it is the right zero-config choice:
//! Elastic measured BEIR nDCG@10 of BM25 0.425 → RRF 0.551.
//!
//! It is not, however, the ceiling. A *tuned convex combination* reaches 0.576 on the same
//! benchmark, and Bruch et al. (TOIS) find convex combination beats RRF in- and out-of-domain while
//! tuning α from a small labelled set. So both are here, RRF is the default, and α is exposed.

/// Cormack's 2009 constant. Do not change it without a measurement.
pub const RRF_K: f32 = 60.0;

/// Reciprocal Rank Fusion over any number of ranked arms.
///
/// Each arm is a list of document ids in descending relevance. Score is
/// `Σ_arm 1 / (k + rank_arm(doc))` with `rank` 1-based. Documents missing from an arm simply
/// contribute nothing from it — which is the property that makes RRF robust to arms whose score
/// scales are incomparable, and the reason it needs no normalization.
pub fn rrf(arm: &[&[u32]], k: f32, limit: usize) -> Vec<(u32, f32)> {
    let mut acc: std::collections::HashMap<u32, f32> = std::collections::HashMap::new();
    for list in arm {
        for (i, &doc) in list.iter().enumerate() {
            *acc.entry(doc).or_insert(0.0) += 1.0 / (k + (i + 1) as f32);
        }
    }
    let mut out: Vec<(u32, f32)> = acc.into_iter().collect();
    // Deterministic: score descending, then doc id ascending.
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(&b.0)));
    out.truncate(limit);
    out
}

/// Convex combination of two scored arms after min-max normalization.
///
/// `alpha` weights the first arm: `alpha * norm(a) + (1 - alpha) * norm(b)`. Use once ~40
/// judgments exist; until then [`rrf`] is the honest default because it needs no tuning set.
pub fn convex(
    a: &[(u32, f32)],
    b: &[(u32, f32)],
    alpha: f32,
    limit: usize,
) -> Vec<(u32, f32)> {
    let norm = |v: &[(u32, f32)]| -> std::collections::HashMap<u32, f32> {
        let (lo, hi) = v.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &(_, s)| {
            (lo.min(s), hi.max(s))
        });
        let span = (hi - lo).max(f32::MIN_POSITIVE);
        v.iter().map(|&(d, s)| (d, if v.len() == 1 { 1.0 } else { (s - lo) / span })).collect()
    };
    let na = norm(a);
    let nb = norm(b);
    let mut acc: std::collections::HashMap<u32, f32> = std::collections::HashMap::new();
    for (d, s) in na {
        *acc.entry(d).or_insert(0.0) += alpha * s;
    }
    for (d, s) in nb {
        *acc.entry(d).or_insert(0.0) += (1.0 - alpha) * s;
    }
    let mut out: Vec<(u32, f32)> = acc.into_iter().collect();
    out.sort_by(|x, y| y.1.partial_cmp(&x.1).unwrap_or(std::cmp::Ordering::Equal).then(x.0.cmp(&y.0)));
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rrf_rewards_agreement_across_arms() {
        // Doc 7 is second in both arms; doc 1 is first in one and absent from the other.
        let a = [1u32, 7, 3];
        let b = [9u32, 7, 4];
        let out = rrf(&[&a, &b], RRF_K, 10);
        assert_eq!(out[0].0, 7, "a document both arms like must win: {out:?}");
    }

    #[test]
    fn rrf_is_deterministic_and_bounded() {
        let a = [1u32, 2, 3];
        let b = [3u32, 2, 1];
        let x = rrf(&[&a, &b], RRF_K, 2);
        let y = rrf(&[&a, &b], RRF_K, 2);
        assert_eq!(x, y);
        assert_eq!(x.len(), 2);
    }

    #[test]
    fn rrf_handles_an_empty_arm() {
        let a = [1u32, 2];
        let empty: [u32; 0] = [];
        let out = rrf(&[&a, &empty], RRF_K, 5);
        assert_eq!(out.iter().map(|x| x.0).collect::<Vec<_>>(), vec![1, 2]);
    }

    #[test]
    fn convex_alpha_one_is_the_first_arm() {
        let a = [(1u32, 10.0f32), (2, 5.0)];
        let b = [(3u32, 99.0f32)];
        let out = convex(&a, &b, 1.0, 5);
        assert_eq!(out[0].0, 1, "alpha=1 must ignore the second arm entirely: {out:?}");
    }

    #[test]
    fn convex_alpha_zero_is_the_second_arm() {
        let a = [(1u32, 10.0f32)];
        let b = [(3u32, 99.0f32), (4, 1.0)];
        let out = convex(&a, &b, 0.0, 5);
        assert_eq!(out[0].0, 3, "{out:?}");
    }
}
