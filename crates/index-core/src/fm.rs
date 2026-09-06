//! FM-index — a compressed full-text substring index (BWT + backward search).
//!
//! Supports `count` (how many times a pattern occurs) and `locate` (where) over the text without
//! storing the suffix array in full — only a sampled SA. The rank structure is the Huffman-shaped
//! wavelet tree in [`crate::wavelet`], so space tracks the BWT's zeroth-order entropy rather than
//! `log σ` per character. It also carries a **k-mismatch fuzzy search** used by the viability
//! experiment in `bench/roadmap/p2-fuzzy-decision.md`.
//!
//! Every operation here is a chain of *dependent* rank calls, so the win is in doing fewer
//! descents rather than in making one descent cheaper: backward search needs `rank` at both ends
//! of its range for the same symbol (one descent, [`WaveletTree::rank2`]), and an LF-step needs
//! the symbol *and* its rank at one position (one descent, [`WaveletTree::access_rank`]).

use std::collections::BTreeSet;

use crate::wavelet::{BitRank, WaveletTree};

const SAMPLE: usize = 32; // suffix-array sampling for locate

/// Suffix array of `s`, which must end in a unique smallest byte — then the cyclic-shift order
/// this computes *is* the suffix order.
///
/// Prefix doubling with two counting sorts per round: O(n log n) with no comparator and no
/// per-comparison key allocation. The comparison-sorted version this replaces was O(n log² n)
/// and rebuilt a tuple key inside the comparator.
fn suffix_array(s: &[u8]) -> Vec<u32> {
    let n = s.len();
    if n == 0 {
        return Vec::new();
    }
    let bucket = 256.max(n);
    let mut cnt = vec![0u32; bucket + 1];
    let mut sa = vec![0u32; n];
    let mut class = vec![0u32; n];
    let mut sa_next = vec![0u32; n];
    let mut class_next = vec![0u32; n];

    for &b in s {
        cnt[b as usize] += 1;
    }
    for i in 1..bucket {
        cnt[i] += cnt[i - 1];
    }
    for i in (0..n).rev() {
        let b = s[i] as usize;
        cnt[b] -= 1;
        sa[cnt[b] as usize] = i as u32;
    }
    let mut class_count = 1u32;
    for i in 1..n {
        if s[sa[i] as usize] != s[sa[i - 1] as usize] {
            class_count += 1;
        }
        class[sa[i] as usize] = class_count - 1;
    }

    let mut h = 0usize;
    while (1usize << h) < n && class_count < n as u32 {
        let shift = 1usize << h;
        // `sa` is already ordered by the second half of each pair; rotating it left by `shift`
        // gives an order by second key, so one stable counting sort by first key finishes it.
        for i in 0..n {
            sa_next[i] = ((sa[i] as usize + n - shift) % n) as u32;
        }
        cnt[..class_count as usize].fill(0);
        for i in 0..n {
            cnt[class[sa_next[i] as usize] as usize] += 1;
        }
        for i in 1..class_count as usize {
            cnt[i] += cnt[i - 1];
        }
        for i in (0..n).rev() {
            let c = class[sa_next[i] as usize] as usize;
            cnt[c] -= 1;
            sa[cnt[c] as usize] = sa_next[i];
        }
        class_next[sa[0] as usize] = 0;
        let mut next_count = 1u32;
        for i in 1..n {
            let cur = sa[i] as usize;
            let prev = sa[i - 1] as usize;
            let a = (class[cur], class[(cur + shift) % n]);
            let b = (class[prev], class[(prev + shift) % n]);
            if a != b {
                next_count += 1;
            }
            class_next[cur] = next_count - 1;
        }
        class.copy_from_slice(&class_next);
        class_count = next_count;
        h += 1;
    }
    sa
}

/// A compressed full-text index over a byte string.
pub struct FmIndex {
    wt: WaveletTree,    // BWT, supporting rank/access succinctly
    c: [usize; 257],    // c[ch] = number of bytes < ch in the text (+sentinel)
    sa_marker: BitRank, // bit i set iff BWT index i is an SA sample
    sa_val: Vec<u32>,   // text positions of the samples, in BWT-index order
    n: usize,           // length incl. sentinel
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
        let mut count = [0usize; 256];
        for &b in &bwt {
            count[b as usize] += 1;
        }
        let mut c = [0usize; 257];
        let mut acc = 0;
        for ch in 0..256 {
            c[ch] = acc;
            acc += count[ch];
        }
        c[256] = acc;

        let wt = WaveletTree::new(&bwt);

        // Sparse SA sample: a marker bitvector + the sampled text positions (≈ n/SAMPLE of them).
        let mut marker_bit = vec![false; n];
        let mut sa_val = Vec::with_capacity(n / SAMPLE + 1);
        for i in 0..n {
            let p = sa[i] as usize;
            if p % SAMPLE == 0 {
                marker_bit[i] = true;
                sa_val.push(p as u32);
            }
        }
        let sa_marker = BitRank::from_bits(&marker_bit);

        FmIndex { wt, c, sa_marker, sa_val, n, text_len: text.len() }
    }

    /// Backward-search range `[sp, ep)` of suffixes prefixed by `pat`. Empty if absent.
    #[inline]
    fn range(&self, pat: &[u8]) -> (usize, usize) {
        let (mut sp, mut ep) = (0usize, self.n);
        for &ch in pat.iter().rev() {
            if !self.wt.contain(ch) {
                return (0, 0);
            }
            let base = self.c[ch as usize];
            let (a, b) = self.wt.rank2(ch, sp, ep);
            sp = base + a;
            ep = base + b;
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
        let mut step = 0;
        loop {
            if self.sa_marker.get(i) {
                return self.sa_val[self.sa_marker.rank1(i)] as usize + step;
            }
            let (ch, r) = self.wt.access_rank(i);
            i = self.c[ch as usize] + r;
            step += 1;
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
        let byte = self.wt.size_bytes() + self.sa_marker.size_bytes() + self.sa_val.len() * 4;
        (byte * 8) as f64 / self.text_len.max(1) as f64
    }

    /// k-mismatch (Hamming) fuzzy search: all start positions of length-`|pat|` substrings within
    /// `k` substitutions of `pat`. `node_cap` bounds the backtracking; returns `(position,
    /// capped)` where `capped` means the search was truncated (result possibly incomplete).
    pub fn fuzzy_kmismatch(&self, pat: &[u8], k: usize, node_cap: u64) -> (BTreeSet<usize>, bool) {
        let mut out = BTreeSet::new();
        let mut node = 0u64;
        let capped =
            self.fuzzy_rec(pat.len() as i64 - 1, 0, self.n, k, pat, &mut out, &mut node, node_cap);
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
        node: &mut u64,
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
            // skip the sentinel (0), and any symbol the text never uses — the latter is most of
            // the byte range for real text, and asking the tree costs a full descent.
            let ch = ch as u8;
            if !self.wt.contain(ch) {
                continue;
            }
            *node += 1;
            if *node > node_cap {
                return true;
            }
            let base = self.c[ch as usize];
            let (a, b) = self.wt.rank2(ch, sp, ep);
            let (nsp, nep) = (base + a, base + b);
            if nsp >= nep {
                continue;
            }
            let cost = usize::from(ch != pat[j as usize]);
            if budget >= cost
                && self.fuzzy_rec(j - 1, nsp, nep, budget - cost, pat, out, node, node_cap)
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
            let want: Vec<usize> =
                (0..=text.len() - len).filter(|&i| &text[i..i + len] == pat).collect();
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

    /// Every count/locate answer must survive the degenerate shapes: nothing, one character,
    /// a text of a single repeated symbol, and the widest alphabet the sentinel leaves free.
    #[test]
    fn edge_texts_match_brute_force() {
        let case: Vec<Vec<u8>> = vec![
            Vec::new(),
            b"a".to_vec(),
            vec![b'a'; 500],
            gen_text(2000, 2, 5),
            gen_text(2000, 255, 5),
            (1..=255u8).cycle().take(2000).collect(),
        ];
        for text in &case {
            let fm = FmIndex::new(text);
            assert_eq!(fm.text_len(), text.len());
            for pat in [b"a".as_slice(), b"aa", b"ab", &[1u8], &[1u8, 2], &[255u8]] {
                assert_eq!(fm.count(pat), brute_count(text, pat), "count {pat:?} on len {}", text.len());
                let want: Vec<usize> = if pat.len() > text.len() {
                    Vec::new()
                } else {
                    (0..=text.len() - pat.len())
                        .filter(|&i| &text[i..i + pat.len()] == pat)
                        .collect()
                };
                assert_eq!(fm.locate(pat), want, "locate {pat:?} on len {}", text.len());
            }
        }
    }

    /// All-equal text is the worst case for the sentinel-terminated BWT: every LF-step walks the
    /// same run, so `locate` must still terminate at a sample.
    #[test]
    fn all_equal_text_locates_every_position() {
        let text = vec![b'z'; 300];
        let fm = FmIndex::new(&text);
        assert_eq!(fm.count(b"zzz"), 298);
        assert_eq!(fm.locate(b"zzzz"), (0..297).collect::<Vec<_>>());
    }
}
