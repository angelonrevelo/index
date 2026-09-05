//! FM-index — a compressed full-text substring index (BWT + backward search).
//!
//! Supports `count` (how many times a pattern occurs) and `locate` (where) over the text without
//! storing the suffix array in full — only a sampled SA. This v0 prioritizes *correctness* over
//! succinctness: the rank structure is a checkpointed occurrence table, not a wavelet tree, so
//! space is higher than the `nH_k + o(n)` bound the literature targets (the wavelet-tree swap is
//! the tracked P2 refinement). It also carries a **k-mismatch fuzzy search** used by the
//! viability experiment in `bench/roadmap/p2-fuzzy-decision.md`.

use std::collections::BTreeSet;

use crate::wavelet::{BitRank, WaveletTree};

const SAMPLE: usize = 32; // suffix-array sampling for locate

/// O(n log² n) prefix-doubling suffix array over `s` (which must end in a unique smallest byte).
fn suffix_array(s: &[u8]) -> Vec<u32> {
    let n = s.len();
    let mut sa: Vec<u32> = (0..n as u32).collect();
    let mut rank: Vec<i64> = s.iter().map(|&b| b as i64).collect();
    let mut tmp = vec![0i64; n];
    let mut k = 1usize;
    loop {
        let key = |i: usize| -> (i64, i64) {
            (rank[i], if i + k < n { rank[i + k] } else { -1 })
        };
        sa.sort_by_key(|&a| key(a as usize));
        tmp[sa[0] as usize] = 0;
        for w in 1..n {
            let prev = sa[w - 1] as usize;
            let cur = sa[w] as usize;
            tmp[cur] = tmp[prev] + i64::from(key(cur) != key(prev));
        }
        rank.copy_from_slice(&tmp);
        if rank[sa[n - 1] as usize] as usize == n - 1 {
            break; // all suffixes distinct → fully sorted
        }
        k <<= 1;
        if k >= n {
            break;
        }
    }
    sa
}

/// A compressed full-text index over a byte string.
pub struct FmIndex {
    wt: WaveletTree,      // BWT, supporting rank/access succinctly
    c: [usize; 257],      // c[ch] = number of bytes < ch in the text (+sentinel)
    sa_marker: BitRank,   // bit i set iff BWT index i is an SA sample
    sa_vals: Vec<u32>,    // text positions of the samples, in BWT-index order
    n: usize,             // length incl. sentinel
    text_len: usize,
}

impl FmIndex {
    /// Build over `text`, which must not contain a 0 byte (reserved as the sentinel).
    pub fn new(text: &[u8]) -> Self {
        assert!(!text.contains(&0), "text must not contain the 0 byte (FM-index sentinel)");
        let mut s = Vec::with_capacity(text.len() + 1);
        s.extend_from_slice(text);
        s.push(0); // sentinel: strictly smallest
        let n = s.len();

        let sa = suffix_array(&s);
        let mut bwt = vec![0u8; n];
        for i in 0..n {
            let p = sa[i] as usize;
            bwt[i] = s[(p + n - 1) % n];
        }

        // C array.
        let mut counts = [0usize; 256];
        for &b in &bwt {
            counts[b as usize] += 1;
        }
        let mut c = [0usize; 257];
        let mut acc = 0;
        for ch in 0..256 {
            c[ch] = acc;
            acc += counts[ch];
        }
        c[256] = acc;

        let wt = WaveletTree::new(&bwt);

        // Sparse SA sample: a marker bitvector + the sampled text positions (≈ n/SAMPLE of them).
        let mut marker_bits = vec![false; n];
        let mut sa_vals = Vec::with_capacity(n / SAMPLE + 1);
        for i in 0..n {
            let p = sa[i] as usize;
            if p % SAMPLE == 0 {
                marker_bits[i] = true;
                sa_vals.push(p as u32);
            }
        }
        let sa_marker = BitRank::from_bits(&marker_bits);

        FmIndex { wt, c, sa_marker, sa_vals, n, text_len: text.len() }
    }

    /// Number of `ch` in `bwt[0..i]`.
    #[inline]
    fn rank(&self, ch: u8, i: usize) -> usize {
        self.wt.rank(ch, i)
    }

    /// Backward-search range `[sp, ep)` of suffixes prefixed by `pat`. Empty if absent.
    #[inline]
    fn range(&self, pat: &[u8]) -> (usize, usize) {
        let (mut sp, mut ep) = (0usize, self.n);
        for &ch in pat.iter().rev() {
            sp = self.c[ch as usize] + self.rank(ch, sp);
            ep = self.c[ch as usize] + self.rank(ch, ep);
            if sp >= ep {
                return (0, 0);
            }
        }
        (sp, ep)
    }

    /// Number of occurrences of `pat` in the text.
    pub fn count(&self, pat: &[u8]) -> usize {
        let (sp, ep) = self.range(pat);
        ep - sp
    }

    /// Text position of the suffix at BWT index `i`, via LF-stepping to a sampled position.
    fn locate_one(&self, mut i: usize) -> usize {
        let mut steps = 0;
        loop {
            if self.sa_marker.get(i) {
                return self.sa_vals[self.sa_marker.rank1(i)] as usize + steps;
            }
            let ch = self.wt.access(i);
            i = self.c[ch as usize] + self.rank(ch, i);
            steps += 1;
        }
    }

    /// All text positions where `pat` occurs (sorted).
    pub fn locate(&self, pat: &[u8]) -> Vec<usize> {
        let (sp, ep) = self.range(pat);
        let mut v: Vec<usize> = (sp..ep).map(|i| self.locate_one(i)).collect();
        v.sort_unstable();
        v
    }

    pub fn text_len(&self) -> usize {
        self.text_len
    }

    /// Index size in bits per text character (wavelet tree + SA marker + sampled positions).
    pub fn bits_per_char(&self) -> f64 {
        let bytes = self.wt.size_bytes() + self.sa_marker.size_bytes() + self.sa_vals.len() * 4;
        (bytes * 8) as f64 / self.text_len.max(1) as f64
    }

    /// k-mismatch (Hamming) fuzzy search: all start positions of length-`|pat|` substrings within
    /// `k` substitutions of `pat`. `node_cap` bounds the backtracking; returns `(positions,
    /// capped)` where `capped` means the search was truncated (result possibly incomplete).
    pub fn fuzzy_kmismatch(&self, pat: &[u8], k: usize, node_cap: u64) -> (BTreeSet<usize>, bool) {
        let mut out = BTreeSet::new();
        let mut nodes = 0u64;
        let capped =
            self.fuzzy_rec(pat.len() as i64 - 1, 0, self.n, k, pat, &mut out, &mut nodes, node_cap);
        (out, capped)
    }

    #[allow(clippy::too_many_arguments)]
    fn fuzzy_rec(
        &self,
        j: i64,
        sp: usize,
        ep: usize,
        budget: usize,
        pat: &[u8],
        out: &mut BTreeSet<usize>,
        nodes: &mut u64,
        node_cap: u64,
    ) -> bool {
        if sp >= ep {
            return false;
        }
        if j < 0 {
            for i in sp..ep {
                out.insert(self.locate_one(i));
            }
            return false;
        }
        for ch in 1u16..256 {
            // skip sentinel (0)
            let ch = ch as u8;
            *nodes += 1;
            if *nodes > node_cap {
                return true;
            }
            let nsp = self.c[ch as usize] + self.rank(ch, sp);
            let nep = self.c[ch as usize] + self.rank(ch, ep);
            if nsp >= nep {
                continue;
            }
            let cost = usize::from(ch != pat[j as usize]);
            if budget >= cost
                && self.fuzzy_rec(j - 1, nsp, nep, budget - cost, pat, out, nodes, node_cap)
            {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::gen_text;

    fn brute_count(text: &[u8], pat: &[u8]) -> usize {
        if pat.is_empty() || pat.len() > text.len() {
            return 0;
        }
        (0..=text.len() - pat.len()).filter(|&i| &text[i..i + pat.len()] == pat).count()
    }

    fn brute_kmismatch(text: &[u8], pat: &[u8], k: usize) -> BTreeSet<usize> {
        let m = pat.len();
        let mut out = BTreeSet::new();
        if m == 0 || m > text.len() {
            return out;
        }
        for i in 0..=text.len() - m {
            let d = (0..m).filter(|&j| text[i + j] != pat[j]).count();
            if d <= k {
                out.insert(i);
            }
        }
        out
    }

    #[test]
    fn count_matches_brute_force() {
        let text = gen_text(4000, 4, 7); // small alphabet → many repeats
        let fm = FmIndex::new(&text);
        for start in [0usize, 1, 100, 2000] {
            for len in [1usize, 2, 3, 5, 8] {
                if start + len <= text.len() {
                    let pat = &text[start..start + len];
                    assert_eq!(fm.count(pat), brute_count(&text, pat), "count for {pat:?}");
                }
            }
        }
        assert_eq!(fm.count(b"\xff\xfe"), 0); // absent
    }

    #[test]
    fn locate_matches_brute_force() {
        let text = gen_text(3000, 4, 11);
        let fm = FmIndex::new(&text);
        for len in [2usize, 4, 6] {
            let pat = &text[500..500 + len];
            let got = fm.locate(pat);
            let want: Vec<usize> = (0..=text.len() - len)
                .filter(|&i| &text[i..i + len] == pat)
                .collect();
            assert_eq!(got, want, "locate for len {len}");
        }
    }

    #[test]
    fn fuzzy_kmismatch_matches_brute_force() {
        let text = gen_text(2500, 5, 3);
        let fm = FmIndex::new(&text);
        for &k in &[0usize, 1, 2] {
            for start in [10usize, 300, 1200] {
                let pat = text[start..start + 6].to_vec();
                let (got, capped) = fm.fuzzy_kmismatch(&pat, k, 50_000_000);
                assert!(!capped, "test should not hit the node cap");
                assert_eq!(got, brute_kmismatch(&text, &pat, k), "k={k} start={start}");
            }
        }
    }
}
