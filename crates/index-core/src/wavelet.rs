//! Succinct building blocks for the FM-index: a bit-rank dictionary and a Huffman-shaped
//! wavelet tree.
//!
//! These replace the FM-index's fat checkpoint occurrence table. A wavelet tree stores the BWT in
//! ~`n·H_0 + o(n log σ)` bits and answers `rank(c, i)` / `access(i)` in `O(log σ)` — so the index
//! size approaches the text's entropy instead of `256 × n/bucket` words.
//!
//! Two shape decisions carry the weight here, both measured in `bench/roadmap/p74-core-primitive.md`:
//!
//! * The rank directory carries **one packed 8-byte entry per 256 bits** instead of a `u32` per
//!   64 bits. That is 1.25 stored bits per payload bit instead of 1.50, at the same rank latency
//!   — the directory is small enough to stay resident either way, so the space is free. Moving
//!   the directory *into* the payload (one cache line per block) was tried and rejected: see the
//!   note on `BLOCK_BIT`.
//! * The tree is **Huffman-shaped over the symbols actually present**, not balanced over
//!   `min..=max`. A balanced tree spends `log2 σ` bits per symbol whatever the distribution; a
//!   BWT is skewed by construction (that is what the transform is for), so entropy shaping is the
//!   difference between the `n·H_0` this module documents and the `n·log σ` a balanced tree
//!   actually delivers. It also shortens the descent for frequent symbols, which is the same
//!   thing as making `rank` faster.

const NONE: u32 = u32::MAX;

/// Bits covered by one rank-directory entry. A **power of two** matters more than it looks:
/// `i / BLOCK_BIT` sits on the dependency chain of every rank, and a non-power-of-two block
/// (tried: 448-bit blocks interleaved into the payload, one cache line each) costs more in
/// division latency than it saves in cache misses — measured 2x slower at every size.
const BLOCK_BIT: usize = 256;

/// One directory entry per `BLOCK_BIT` bits: the running 1-count before the block, plus the
/// three intra-block word prefixes packed 8 bits each (a block holds at most 256 ones, and the
/// slot for word 0 is a constant zero). 8 bytes per 32 bytes of payload, against the 4-bytes-per-8
/// a per-word `u32` directory costs.
#[derive(Clone, Copy)]
struct Dir {
    abs: u32,
    sub: u32,
}

/// Bitvector with O(1) `rank1`.
pub struct BitRank {
    word: Vec<u64>,
    dir: Vec<Dir>,
    len: usize,
}

impl BitRank {
    pub fn from_bits(bit: &[bool]) -> Self {
        let mut b = BitBuilder::with_capacity(bit.len());
        for &x in bit {
            b.push(x);
        }
        b.finish()
    }

    /// Number of 1-bits in `bit[0..i]`. `i == len` is allowed: the payload and directory both
    /// carry a spare trailing entry, so this needs no bounds branch.
    #[inline]
    pub fn rank1(&self, i: usize) -> usize {
        debug_assert!(i <= self.len);
        let d = self.dir[i / BLOCK_BIT];
        let w = (i >> 6) & 3;
        let rem = i & 63;
        let mut r = d.abs as usize + ((d.sub >> (8 * w)) & 255) as usize;
        r += (self.word[i >> 6] & ((1u64 << rem) - 1)).count_ones() as usize;
        r
    }

    #[inline]
    pub fn rank0(&self, i: usize) -> usize {
        i - self.rank1(i)
    }

    /// `(bit at i, rank1 up to i)` — one word load answers both, which is what an LF-step needs.
    #[inline]
    pub fn get_rank1(&self, i: usize) -> (bool, usize) {
        debug_assert!(i < self.len);
        let d = self.dir[i / BLOCK_BIT];
        let w = (i >> 6) & 3;
        let rem = i & 63;
        let word = self.word[i >> 6];
        let mut r = d.abs as usize + ((d.sub >> (8 * w)) & 255) as usize;
        r += (word & ((1u64 << rem) - 1)).count_ones() as usize;
        (((word >> rem) & 1) == 1, r)
    }

    #[inline]
    pub fn get(&self, i: usize) -> bool {
        (self.word[i >> 6] >> (i & 63)) & 1 == 1
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn size_bytes(&self) -> usize {
        self.word.len() * 8 + self.dir.len() * 8
    }
}

/// Incremental builder: packs bits straight into words, so no `Vec<bool>` scratch (a whole byte
/// per bit) is ever materialized for a wavelet node.
struct BitBuilder {
    word: Vec<u64>,
    len: usize,
}

impl BitBuilder {
    fn with_capacity(n: usize) -> Self {
        BitBuilder { word: Vec::with_capacity(n / 64 + 2), len: 0 }
    }

    #[inline]
    fn push(&mut self, b: bool) {
        if self.len % 64 == 0 {
            self.word.push(0);
        }
        if b {
            let last = self.word.len() - 1;
            self.word[last] |= 1u64 << (self.len % 64);
        }
        self.len += 1;
    }

    fn finish(mut self) -> BitRank {
        assert!(self.len <= u32::MAX as usize, "BitRank directory holds a u32 prefix count");
        // Spare trailing word + directory entry so `rank1(len)` lands in range without a branch.
        self.word.push(0);
        let mut dir = Vec::with_capacity(self.len / BLOCK_BIT + 2);
        let mut abs = 0u32;
        for chunk in self.word.chunks(4) {
            let mut sub = 0u32;
            let mut local = 0u32;
            for (j, &w) in chunk.iter().enumerate() {
                sub |= local << (8 * j);
                local += w.count_ones();
            }
            dir.push(Dir { abs, sub });
            abs += local;
        }
        dir.push(Dir { abs, sub: 0 });
        BitRank { word: self.word, dir, len: self.len }
    }
}

struct Node {
    bit: BitRank,
    child: [u32; 2],
    sym: u8,
    leaf: bool,
}

/// Huffman-shaped wavelet tree over a byte sequence.
pub struct WaveletTree {
    node: Vec<Node>,
    root: u32,
    /// Root-to-leaf path per symbol, MSB-first within `code_len` bits. `u128` is wide enough for
    /// any Huffman code over a `u64`-length sequence (max depth ≈ log_phi(2^64) ≈ 92).
    code: Vec<u128>,
    code_len: Vec<u8>,
    present: Vec<bool>,
    len: usize,
}

impl WaveletTree {
    pub fn new(seq: &[u8]) -> Self {
        let mut freq = [0u64; 256];
        for &c in seq {
            freq[c as usize] += 1;
        }
        let mut code = vec![0u128; 256];
        let mut code_len = vec![0u8; 256];
        let present: Vec<bool> = freq.iter().map(|&f| f > 0).collect();

        let mut node = Vec::new();
        let root = if seq.is_empty() {
            NONE
        } else {
            let huff = build_huffman(&freq);
            let top = huff.len() - 1;
            assign_code(&huff, top, 0, 0, &mut code, &mut code_len);
            materialize(&mut node, &huff, top, seq, 0, &code, &code_len)
        };
        WaveletTree { node, root, code, code_len, present, len: seq.len() }
    }

    /// Is `c` anywhere in the sequence? Lets a backward-search caller skip symbols that cannot
    /// extend the range without paying a tree descent to find that out.
    #[inline]
    pub fn contain(&self, c: u8) -> bool {
        self.present[c as usize]
    }

    /// Number of occurrences of `c` in `seq[0..i]`. Returns 0 for symbols outside the alphabet.
    #[inline]
    pub fn rank(&self, c: u8, i: usize) -> usize {
        self.rank2(c, i, i).0
    }

    /// `(rank(c, a), rank(c, b))` in **one** descent. Backward search always needs both ends of
    /// its range for the same symbol, so paying two descents is pure waste.
    #[inline]
    pub fn rank2(&self, c: u8, mut a: usize, mut b: usize) -> (usize, usize) {
        if !self.present[c as usize] {
            return (0, 0);
        }
        let path = self.code[c as usize];
        let mut d = self.code_len[c as usize];
        let mut n = self.root as usize;
        while d > 0 {
            d -= 1;
            let nd = &self.node[n];
            let dir = ((path >> d) & 1) as usize;
            if dir == 0 {
                a = nd.bit.rank0(a);
                b = nd.bit.rank0(b);
            } else {
                a = nd.bit.rank1(a);
                b = nd.bit.rank1(b);
            }
            n = nd.child[dir] as usize;
        }
        (a, b)
    }

    /// The symbol at position `i`.
    #[inline]
    pub fn access(&self, i: usize) -> u8 {
        self.access_rank(i).0
    }

    /// `(symbol at i, rank of that symbol in seq[0..i])` — the LF-mapping primitive, in one
    /// descent instead of `access` followed by `rank`.
    #[inline]
    pub fn access_rank(&self, mut i: usize) -> (u8, usize) {
        let mut n = self.root as usize;
        loop {
            let nd = &self.node[n];
            if nd.leaf {
                return (nd.sym, i);
            }
            let (b, r1) = nd.bit.get_rank1(i);
            i = if b { r1 } else { i - r1 };
            n = nd.child[usize::from(b)] as usize;
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn size_bytes(&self) -> usize {
        self.node.iter().map(|n| n.bit.size_bytes()).sum::<usize>()
            + self.node.len() * core::mem::size_of::<Node>()
    }
}

/// Huffman tree in a flat array; the last entry is the root. `sym < 0` marks an internal node.
struct HNode {
    child: [i32; 2],
    sym: i16,
}

/// O(σ²) two-smallest selection — σ ≤ 256, so the tidy version costs ~65k comparisons once, and
/// a heap would only add code. Ties break on position, which keeps the shape reproducible.
fn build_huffman(freq: &[u64; 256]) -> Vec<HNode> {
    let mut node = Vec::new();
    let mut weight: Vec<u64> = Vec::new();
    let mut live: Vec<usize> = Vec::new();
    for (c, &f) in freq.iter().enumerate() {
        if f > 0 {
            node.push(HNode { child: [-1, -1], sym: c as i16 });
            weight.push(f);
            live.push(node.len() - 1);
        }
    }
    while live.len() > 1 {
        let a = take_min(&mut live, &weight);
        let b = take_min(&mut live, &weight);
        node.push(HNode { child: [a as i32, b as i32], sym: -1 });
        weight.push(weight[a] + weight[b]);
        live.push(node.len() - 1);
    }
    node
}

fn take_min(live: &mut Vec<usize>, weight: &[u64]) -> usize {
    let mut best = 0usize;
    for k in 1..live.len() {
        if weight[live[k]] < weight[live[best]] {
            best = k;
        }
    }
    live.remove(best)
}

fn assign_code(
    huff: &[HNode],
    at: usize,
    path: u128,
    depth: u8,
    code: &mut [u128],
    code_len: &mut [u8],
) {
    let h = &huff[at];
    if h.sym >= 0 {
        code[h.sym as usize] = path;
        code_len[h.sym as usize] = depth;
        return;
    }
    assign_code(huff, h.child[0] as usize, path << 1, depth + 1, code, code_len);
    assign_code(huff, h.child[1] as usize, (path << 1) | 1, depth + 1, code, code_len);
}

fn materialize(
    node: &mut Vec<Node>,
    huff: &[HNode],
    at: usize,
    seq: &[u8],
    depth: u8,
    code: &[u128],
    code_len: &[u8],
) -> u32 {
    let h = &huff[at];
    if h.sym >= 0 {
        node.push(Node {
            bit: BitRank { word: Vec::new(), dir: Vec::new(), len: seq.len() },
            child: [NONE, NONE],
            sym: h.sym as u8,
            leaf: true,
        });
        return (node.len() - 1) as u32;
    }
    // Bit j says which way seq[j] turns here: the `depth`-th bit of its code, MSB-first.
    let mut bit = BitBuilder::with_capacity(seq.len());
    let mut left = Vec::new();
    let mut right = Vec::new();
    for &c in seq {
        let shift = code_len[c as usize] - depth - 1;
        if (code[c as usize] >> shift) & 1 == 1 {
            bit.push(true);
            right.push(c);
        } else {
            bit.push(false);
            left.push(c);
        }
    }
    let idx = node.len();
    node.push(Node { bit: bit.finish(), child: [NONE, NONE], sym: 0, leaf: false });
    let l = materialize(node, huff, h.child[0] as usize, &left, depth + 1, code, code_len);
    drop(left);
    let r = materialize(node, huff, h.child[1] as usize, &right, depth + 1, code, code_len);
    drop(right);
    node[idx].child = [l, r];
    idx as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(seq: &[u8]) {
        let wt = WaveletTree::new(seq);
        assert_eq!(wt.len(), seq.len());
        for (i, &c) in seq.iter().enumerate() {
            assert_eq!(wt.access(i), c, "access at {i}");
            let naive = seq[..i].iter().filter(|&&x| x == c).count();
            assert_eq!(wt.access_rank(i), (c, naive), "access_rank at {i}");
        }
        for c in 0u16..256 {
            let c = c as u8;
            for i in [0, seq.len() / 3, seq.len() / 2, seq.len()] {
                let naive = seq[..i].iter().filter(|&&x| x == c).count();
                assert_eq!(wt.rank(c, i), naive, "rank({c},{i})");
            }
            let (a, b) = wt.rank2(c, seq.len() / 3, seq.len());
            assert_eq!((a, b), (wt.rank(c, seq.len() / 3), wt.rank(c, seq.len())));
            assert_eq!(wt.contain(c), seq.contains(&c));
        }
    }

    #[test]
    fn rank_and_access_match_naive() {
        check(b"the quick brown fox jumps over the lazy dog, the end.");
    }

    #[test]
    fn single_symbol_sequence() {
        let seq = vec![7u8; 100];
        let wt = WaveletTree::new(&seq);
        assert_eq!(wt.rank(7, 50), 50);
        assert_eq!(wt.rank(8, 50), 0);
        assert_eq!(wt.access(0), 7);
        // A one-symbol alphabet needs no bitvector at all — the tree shape carries the answer.
        assert_eq!(wt.size_bytes(), core::mem::size_of::<Node>());
    }

    #[test]
    fn empty_sequence() {
        let wt = WaveletTree::new(&[]);
        assert_eq!(wt.len(), 0);
        assert!(wt.is_empty());
        assert_eq!(wt.rank(0, 0), 0);
        assert_eq!(wt.rank(255, 0), 0);
        assert_eq!(wt.size_bytes(), 0);
    }

    #[test]
    fn edge_shapes() {
        check(&[]);
        check(&[42]);
        check(&vec![9u8; 500]);
        check(&[0, 255]);
        let full: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
        check(&full);
    }

    #[test]
    fn skewed_alphabet_beats_balanced_depth() {
        // 8 symbols, one of them 90% of the mass: a balanced tree spends 3 bits/symbol, an
        // entropy-shaped one spends well under that.
        let seq: Vec<u8> =
            (0..100_000usize).map(|i| if i % 10 == 0 { (1 + i % 7) as u8 } else { 0u8 }).collect();
        let wt = WaveletTree::new(&seq);
        let bit_per_sym = wt.size_bytes() as f64 * 8.0 / seq.len() as f64;
        assert!(bit_per_sym < 3.0, "expected sub-log-sigma, got {bit_per_sym:.2} bits/symbol");
        check(&seq[..2000]);
    }

    #[test]
    fn rank_spans_many_blocks() {
        // Exercise the block-boundary arithmetic well past one cache line (448 bits).
        let seq: Vec<u8> = (0..10_000u32).map(|i| ((i * 7 + i / 3) % 5) as u8).collect();
        let wt = WaveletTree::new(&seq);
        for c in 0..5u8 {
            let mut naive = 0usize;
            for i in 0..=seq.len() {
                assert_eq!(wt.rank(c, i), naive, "rank({c},{i})");
                if i < seq.len() && seq[i] == c {
                    naive += 1;
                }
            }
        }
    }

    #[test]
    fn bitrank_matches_naive_across_block_boundaries() {
        for n in [0usize, 1, 63, 64, 447, 448, 449, 1000, 4096] {
            let bit: Vec<bool> = (0..n).map(|i| i % 3 == 0 || i % 7 == 1).collect();
            let br = BitRank::from_bits(&bit);
            let mut ones = 0usize;
            for i in 0..=n {
                assert_eq!(br.rank1(i), ones, "rank1({i}) n={n}");
                assert_eq!(br.rank0(i), i - ones, "rank0({i}) n={n}");
                if let Some(&b) = bit.get(i) {
                    assert_eq!(br.get(i), b, "get({i}) n={n}");
                    assert_eq!(br.get_rank1(i), (b, ones));
                    ones += usize::from(b);
                }
            }
        }
    }
}
