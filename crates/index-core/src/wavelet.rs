//! Succinct building blocks for the FM-index: a bit-rank dictionary and a balanced wavelet tree.
//!
//! These replace the FM-index's fat checkpoint occurrence table. A wavelet tree stores the BWT in
//! ~`n·H_0 + o(n log σ)` bits and answers `rank(c, i)` / `access(i)` in `O(log σ)` — so the index
//! size approaches the text's entropy instead of `256 × n/bucket` words.

const NONE: u32 = u32::MAX;

/// Bitvector with O(1) `rank1` via per-word popcount prefix sums.
pub struct BitRank {
    words: Vec<u64>,
    cum: Vec<u32>, // cum[w] = number of 1s in words[0..w]
}

impl BitRank {
    pub fn from_bits(bits: &[bool]) -> Self {
        let nwords = bits.len().div_ceil(64);
        let mut words = vec![0u64; nwords];
        for (i, &b) in bits.iter().enumerate() {
            if b {
                words[i / 64] |= 1u64 << (i % 64);
            }
        }
        let mut cum = vec![0u32; nwords + 1];
        for w in 0..nwords {
            cum[w + 1] = cum[w] + words[w].count_ones();
        }
        BitRank { words, cum }
    }

    /// Number of 1-bits in `bits[0..i]`.
    #[inline]
    pub fn rank1(&self, i: usize) -> usize {
        let w = i / 64;
        let off = i % 64;
        let mut r = self.cum[w] as usize;
        if off > 0 {
            r += (self.words[w] & ((1u64 << off) - 1)).count_ones() as usize;
        }
        r
    }

    #[inline]
    pub fn rank0(&self, i: usize) -> usize {
        i - self.rank1(i)
    }

    #[inline]
    pub fn get(&self, i: usize) -> bool {
        (self.words[i / 64] >> (i % 64)) & 1 == 1
    }

    pub fn size_bytes(&self) -> usize {
        self.words.len() * 8 + self.cum.len() * 4
    }
}

struct Node {
    lo: u8,
    hi: u8,
    bits: BitRank,
    left: u32,
    right: u32,
}

/// Balanced wavelet tree over a byte sequence.
pub struct WaveletTree {
    nodes: Vec<Node>,
    root: u32,
}

impl WaveletTree {
    pub fn new(seq: &[u8]) -> Self {
        let mut nodes = Vec::new();
        let (lo, hi) = match (seq.iter().min(), seq.iter().max()) {
            (Some(&a), Some(&b)) => (a, b),
            _ => (0, 0),
        };
        let root = build(&mut nodes, seq, lo, hi);
        WaveletTree { nodes, root }
    }

    /// Number of occurrences of `c` in `seq[0..i]`. Returns 0 for symbols outside the alphabet.
    #[inline]
    pub fn rank(&self, c: u8, mut i: usize) -> usize {
        let r = &self.nodes[self.root as usize];
        if c < r.lo || c > r.hi {
            return 0;
        }
        let mut node = self.root as usize;
        loop {
            let nd = &self.nodes[node];
            if nd.lo == nd.hi {
                return i;
            }
            let mid = nd.lo + (nd.hi - nd.lo) / 2;
            if c <= mid {
                i = nd.bits.rank0(i);
                node = nd.left as usize;
            } else {
                i = nd.bits.rank1(i);
                node = nd.right as usize;
            }
        }
    }

    /// The symbol at position `i`.
    #[inline]
    pub fn access(&self, mut i: usize) -> u8 {
        let mut node = self.root as usize;
        loop {
            let nd = &self.nodes[node];
            if nd.lo == nd.hi {
                return nd.lo;
            }
            if nd.bits.get(i) {
                i = nd.bits.rank1(i);
                node = nd.right as usize;
            } else {
                i = nd.bits.rank0(i);
                node = nd.left as usize;
            }
        }
    }

    pub fn size_bytes(&self) -> usize {
        self.nodes.iter().map(|n| n.bits.size_bytes()).sum()
    }
}

fn build(nodes: &mut Vec<Node>, seq: &[u8], lo: u8, hi: u8) -> u32 {
    if lo == hi {
        nodes.push(Node { lo, hi, bits: BitRank::from_bits(&[]), left: NONE, right: NONE });
        return (nodes.len() - 1) as u32;
    }
    let mid = lo + (hi - lo) / 2;
    let bits: Vec<bool> = seq.iter().map(|&c| c > mid).collect();
    let left_seq: Vec<u8> = seq.iter().copied().filter(|&c| c <= mid).collect();
    let right_seq: Vec<u8> = seq.iter().copied().filter(|&c| c > mid).collect();
    let idx = nodes.len();
    nodes.push(Node { lo, hi, bits: BitRank::from_bits(&bits), left: NONE, right: NONE });
    let l = build(nodes, &left_seq, lo, mid);
    let r = build(nodes, &right_seq, mid + 1, hi);
    nodes[idx].left = l;
    nodes[idx].right = r;
    idx as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_and_access_match_naive() {
        let seq: &[u8] = b"the quick brown fox jumps over the lazy dog, the end.";
        let wt = WaveletTree::new(seq);
        for (i, &c) in seq.iter().enumerate() {
            assert_eq!(wt.access(i), c, "access at {i}");
        }
        for &c in b"the qz." {
            for i in 0..=seq.len() {
                let naive = seq[..i].iter().filter(|&&x| x == c).count();
                assert_eq!(wt.rank(c, i), naive, "rank({},{i})", c as char);
            }
        }
    }

    #[test]
    fn single_symbol_sequence() {
        let seq = vec![7u8; 100];
        let wt = WaveletTree::new(&seq);
        assert_eq!(wt.rank(7, 50), 50);
        assert_eq!(wt.rank(8, 50), 0);
        assert_eq!(wt.access(0), 7);
    }
}
