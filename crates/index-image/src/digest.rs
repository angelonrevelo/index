//! SHA-256 over file bytes — the content address behind [`crate::ImageDoc::digest`].
//!
//! # Why a cryptographic hash, when a faster one would fit in ten lines
//!
//! `bench/roadmap/p52-content-address.md` spends this one 32-byte field on **two** jobs at once:
//!
//!   - the **dedup key** — ~30 % of a scraped corpus is duplicated, and dedup saves more bytes
//!     than the best lossless codec does;
//!   - the **proof that a lossless transcode round-tripped 1:1** — "restore the original, byte for
//!     byte" reduces to a single digest comparison, and a transcode that cannot be proven exact is
//!     reported as a *failure* rather than rounded up to success.
//!
//! Both jobs live in an OSINT corpus, where the file bytes are supplied by whoever put the image
//! on the internet — which is to say, by a potential adversary. That is the whole argument. A
//! non-cryptographic hash (FNV, xxHash, CRC-32/64) has no collision resistance by design: given a
//! target digest, a collision can be constructed to order in negligible time. Under such a hash,
//! two attacks are free rather than merely cheap:
//!
//!   1. **Identity impersonation.** A crafted file collides with a target image's digest, so the
//!      index treats it as the same content — the attacker chooses which image is "already in the
//!      corpus" and which is silently dropped as a duplicate.
//!   2. **A forged 1:1 proof.** A corrupted or substituted restore collides with the original's
//!      digest, and the round-trip check — the entire "byte-exact" claim — reports success on
//!      bytes that are not the original.
//!
//! Neither risk is hypothetical enough to trade for throughput. A digest is computed **once per
//! file at ingest**, against a multi-megabyte read from disk that dominates it; the hash is not on
//! any query path. So the cryptographic hash is not the expensive choice here, it is the correct
//! one, and this comment exists so the choice does not read as an arbitrary pick.
//!
//! # The honest part: BLAKE3 would be better, and it is not here for a bad-ish reason
//!
//! BLAKE3 offers the same 128-bit collision resistance and is **several times faster** than
//! SHA-256 on the same hardware — more still if its tree structure is parallelised. On the merits
//! it is the better hash for this exact workload.
//!
//! It is absent for one reason and it is not a technical one: this crate takes **no
//! dependencies** (see `crates/index-image/Cargo.toml` — the licence and footprint argument the
//! whole crate rests on), so anything used here must be hand-written and hand-verified.
//! SHA-256 is ~150 lines with a published, universally available set of test vectors
//! (FIPS 180-4) that pin every branch, including the padding edge cases. BLAKE3 is materially
//! more machinery — a chunk tree, chaining-value stacks, flag words, subtree parenting — and a
//! hand-rolled version that is *subtly* wrong still produces plausible-looking digests. Correct
//! and slower beats fast and unverifiable when the output is a security claim.
//!
//! So: this is a **trade**, made by the zero-dependency rule, and a reader is entitled to know it
//! was one. If this crate ever takes a dependency, BLAKE3 is the first thing to reconsider.
//!
//! # Shape
//!
//! [`sha256`] is the one-shot form. [`Sha256`] is the streaming form — `new` / `update` / `finish`
//! — because the corpus this serves holds multi-megabyte files and the reference ingest reads them
//! in chunks; requiring a whole file to be resident just to hash it would be a memory cost with no
//! purpose. `test::stream_in_any_chunking_equal_one_shot` asserts the two agree for every chunk
//! size, which is the property that makes the streaming form safe to prefer.
//!
//! No `unsafe`, no clock, no allocation beyond the 64-byte block buffer. Every arithmetic
//! operation is explicitly wrapping, so a debug build cannot panic on the overflow that SHA-256's
//! mod-2^32 addition *requires*.

/// SHA-256 round constants: the first 32 bits of the fractional parts of the cube roots of the
/// first 64 primes (FIPS 180-4 §4.2.2). Hard-coded rather than computed — the standard's values
/// are the specification, and deriving them at build time would only add a way to be wrong.
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// Initial hash value: the first 32 bits of the fractional parts of the square roots of the first
/// eight primes (FIPS 180-4 §5.3.3).
const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// One SHA-256 block, in bytes. Every length rule in the padding follows from this number.
const BLOCK_LEN: usize = 64;

/// Content address of `byte`: the SHA-256 digest, as 32 raw bytes.
///
/// This is the one-shot form, for input already in memory. For a file large enough that holding it
/// is itself the cost, use [`Sha256`] and feed it whatever chunk size the reader produces — the
/// result is identical by construction, and `test` asserts it.
///
/// ```
/// use index_image::sha256;
/// // FIPS 180-4, the one-block example.
/// let d = sha256(b"abc");
/// assert_eq!(d[0], 0xba);
/// assert_eq!(d.len(), 32);
/// ```
pub fn sha256(byte: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(byte);
    h.finish()
}

/// Streaming SHA-256: `new` → `update`* → `finish`.
///
/// The state is fixed size — eight words, a 64-byte block buffer, and a length counter — so a
/// 4 GiB file costs exactly as much resident memory as a 4-byte one. That is the entire reason
/// this type exists next to [`sha256`]: the ingest in `crates/index-bench/src/image_corpus.rs`
/// reads files in chunks, and a digest that demanded the whole file back would defeat that.
///
/// `finish` consumes the hasher rather than taking `&mut self`. SHA-256's padding is a
/// destructive, once-only operation on the state; a `finish(&mut self)` that left the object
/// usable would invite a second call to return a digest of the *padded* stream, which is a
/// silently wrong answer. Taking `self` makes that mistake fail to compile.
#[derive(Clone)]
pub struct Sha256 {
    /// Working hash state, `H0` until the first block is compressed.
    state: [u32; 8],
    /// Bytes not yet part of a complete block. Only `buffered` of these are live.
    block: [u8; BLOCK_LEN],
    /// How many bytes of `block` are live: always `< BLOCK_LEN`, since a full block is compressed
    /// immediately rather than held.
    buffered: usize,
    /// Total message length in bytes, which the padding encodes as a bit count. `u64` bytes is
    /// 2^64 bytes of message — 2^67 bits, which overflows the standard's 2^64-bit length field
    /// only for inputs no filesystem can produce.
    total_len: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    /// A hasher over the empty message. Hashing nothing and calling `finish` yields the standard's
    /// empty-string digest, which `test::fip_vector_empty` pins.
    pub fn new() -> Self {
        Sha256 { state: H0, block: [0u8; BLOCK_LEN], buffered: 0, total_len: 0 }
    }

    /// Absorb the next `chunk` of the message.
    ///
    /// Chunk boundaries are invisible to the result: the message is the concatenation, and nothing
    /// else. Any chunking — one byte at a time, 3 at a time, 65 at a time — produces the same
    /// digest as a single call over the same bytes.
    pub fn update(&mut self, chunk: &[u8]) {
        self.total_len = self.total_len.wrapping_add(chunk.len() as u64);
        let mut rest = chunk;

        // Top up a partially filled block first, so the fast path below can stay aligned.
        if self.buffered > 0 {
            let want = BLOCK_LEN - self.buffered;
            let take = want.min(rest.len());
            self.block[self.buffered..self.buffered + take].copy_from_slice(&rest[..take]);
            self.buffered += take;
            rest = &rest[take..];
            if self.buffered == BLOCK_LEN {
                let full = self.block;
                self.compress(&full);
                self.buffered = 0;
            }
        }

        // Whole blocks straight out of the caller's slice — no copy into `block` at all.
        while rest.len() >= BLOCK_LEN {
            let (head, tail) = rest.split_at(BLOCK_LEN);
            let mut full = [0u8; BLOCK_LEN];
            full.copy_from_slice(head);
            self.compress(&full);
            rest = tail;
        }

        // Whatever is left is shorter than a block and waits for more input, or for `finish`.
        if !rest.is_empty() {
            self.block[..rest.len()].copy_from_slice(rest);
            self.buffered = rest.len();
        }
    }

    /// Apply the padding and return the digest.
    ///
    /// The padding (FIPS 180-4 §5.1.1) is a `0x80` byte, then zeroes, then the message length in
    /// **bits** as a big-endian `u64`. The subtle case — and the one that a naive implementation
    /// gets wrong — is when the `0x80` and the 8 length bytes do not both fit in the current
    /// block: at a buffered length of 56..=63 the length field is pushed into a *second* block.
    /// `test::padding_boundary_length` walks exactly those lengths.
    pub fn finish(mut self) -> [u8; 32] {
        let bit_len = self.total_len.wrapping_mul(8);

        // The mandatory 1 bit, as a whole byte — the standard's message lengths are byte-aligned
        // here because the API only accepts bytes.
        self.block[self.buffered] = 0x80;
        self.buffered += 1;

        // If the 8-byte length no longer fits, flush this block zero-filled and start another.
        if self.buffered > BLOCK_LEN - 8 {
            self.block[self.buffered..].fill(0);
            let full = self.block;
            self.compress(&full);
            self.buffered = 0;
        }

        self.block[self.buffered..BLOCK_LEN - 8].fill(0);
        self.block[BLOCK_LEN - 8..].copy_from_slice(&bit_len.to_be_bytes());
        let full = self.block;
        self.compress(&full);

        let mut out = [0u8; 32];
        for (i, word) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    /// The block compression function (FIPS 180-4 §6.2.2).
    ///
    /// Every addition here is `wrapping_add` and every rotate is `rotate_right`. SHA-256 is
    /// *defined* over mod-2^32 arithmetic, so overflow is the specified behaviour, not an error —
    /// spelling it as wrapping keeps a debug build from panicking on correct operation, which is
    /// the only reason plain `+` would be a bug rather than a style choice.
    fn compress(&mut self, block: &[u8; BLOCK_LEN]) {
        let mut w = [0u32; 64];

        for (word, quad) in w[..16].iter_mut().zip(block.chunks_exact(4)) {
            *word = u32::from_be_bytes([quad[0], quad[1], quad[2], quad[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;

        for (k, word) in K.iter().zip(w.iter()) {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(*k)
                .wrapping_add(*word);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }

        let add = [a, b, c, d, e, f, g, h];
        for (slot, v) in self.state.iter_mut().zip(add.iter()) {
            *slot = slot.wrapping_add(*v);
        }
    }
}

impl std::fmt::Debug for Sha256 {
    /// Deliberately opaque. Printing the working state of a hasher mid-stream is never useful and
    /// invites someone to compare partial states as if they meant something.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sha256").field("total_len", &self.total_len).finish_non_exhaustive()
    }
}

/// Lowercase hex of a digest, for printing a content address next to a file in a report.
///
/// Hex rather than base64 because every published SHA-256 test vector, every `sha256sum` output
/// and every hash a user will paste in to compare is hex; a digest that must be re-encoded before
/// it can be checked against the standard is a digest nobody checks.
pub fn hex(digest: &[u8; 32]) -> String {
    const DIGIT: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(64);
    for byte in digest.iter() {
        s.push(DIGIT[(byte >> 4) as usize] as char);
        s.push(DIGIT[(byte & 0x0f) as usize] as char);
    }
    s
}

#[cfg(test)]
mod test {
    use super::*;

    /// The published vectors are hex strings, so the comparison is done in hex — a mismatch then
    /// prints both digests legibly instead of two 32-element byte arrays.
    fn digest_hex(byte: &[u8]) -> String {
        hex(&sha256(byte))
    }

    // ---------------------------------------------------------------------------------------
    // FIPS 180-4 / NIST CAVS vectors. These are the tests that matter: every other test in this
    // module checks that the implementation is *self-consistent*, and only these check that it is
    // *SHA-256*. The expected digests are transcribed from the standard, not from this code.
    // ---------------------------------------------------------------------------------------

    /// The empty message. Exercises the padding path where the entire block is padding.
    #[test]
    fn fip_vector_empty() {
        assert_eq!(
            digest_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    /// FIPS 180-4 §B.1 — the one-block message `"abc"`.
    #[test]
    fn fip_vector_abc() {
        assert_eq!(
            digest_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// FIPS 180-4 §B.2 — the 56-byte multi-block message. 56 bytes is chosen by the standard
    /// precisely because it forces the length field into a second block.
    #[test]
    fn fip_vector_two_block() {
        let m = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
        assert_eq!(m.len(), 56);
        assert_eq!(
            digest_hex(m),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    /// FIPS 180-4 §B.3 — one million repetitions of `"a"`. Fed as a stream in irregular chunks,
    /// because that is the shape the ingest actually uses and a one-shot 1 MB buffer would test
    /// less than this does.
    #[test]
    fn fip_vector_million_a() {
        let mut h = Sha256::new();
        let chunk = [b'a'; 997];
        let mut sent = 0usize;
        while sent + chunk.len() <= 1_000_000 {
            h.update(&chunk);
            sent += chunk.len();
        }
        h.update(&chunk[..1_000_000 - sent]);
        assert_eq!(
            hex(&h.finish()),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    /// The same million-`a` vector one-shot, proving the streaming feed above did not accidentally
    /// pass by hashing something other than the intended message.
    #[test]
    fn fip_vector_million_a_one_shot() {
        let m = vec![b'a'; 1_000_000];
        assert_eq!(
            digest_hex(&m),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    // ---------------------------------------------------------------------------------------
    // Streaming and padding properties.
    // ---------------------------------------------------------------------------------------

    /// Chunk boundaries must be invisible. Sizes 1, 3, 64 and 65 are the interesting ones: below,
    /// coprime to, exactly at, and just past the block size — the last two being where an
    /// off-by-one in the buffer top-up would show.
    #[test]
    fn stream_in_any_chunking_equal_one_shot() {
        let message: Vec<u8> = (0..1_000u32).map(|i| (i.wrapping_mul(31) % 251) as u8).collect();
        let want = sha256(&message);

        for size in [1usize, 2, 3, 7, 63, 64, 65, 127, 128, 129, 256, 999, 1_000, 1_001] {
            let mut h = Sha256::new();
            for chunk in message.chunks(size) {
                h.update(chunk);
            }
            assert_eq!(h.finish(), want, "chunk size {size} disagreed with one-shot");
        }
    }

    /// An empty `update` is a no-op, including as the very first or very last call. Readers do
    /// return zero-length chunks at EOF, and that must not disturb the digest.
    #[test]
    fn empty_update_change_nothing() {
        let mut h = Sha256::new();
        h.update(b"");
        h.update(b"index");
        h.update(b"");
        h.update(b"-image");
        h.update(b"");
        assert_eq!(h.finish(), sha256(b"index-image"));
    }

    /// The block-boundary lengths where naive padding breaks.
    ///
    /// 55 is the last length whose `0x80` and 8-byte length field both fit in one block; 56 is the
    /// first that does not. 63 and 64 straddle the block itself, and 119/120 repeat the pair one
    /// block later, catching an implementation that special-cased only the first block.
    ///
    /// Each length is checked two ways — one-shot, and byte-at-a-time — so a padding bug and a
    /// buffering bug cannot cancel each other out.
    #[test]
    fn padding_boundary_length() {
        for len in [55usize, 56, 63, 64, 119, 120] {
            let message: Vec<u8> = (0..len).map(|i| (i % 256) as u8).collect();
            let one_shot = sha256(&message);

            let mut h = Sha256::new();
            for byte in message.iter() {
                h.update(std::slice::from_ref(byte));
            }
            assert_eq!(h.finish(), one_shot, "byte-wise feed disagreed at len {len}");

            // A digest is 32 bytes and, for a real hash, is not all-zero for any of these.
            assert_ne!(one_shot, [0u8; 32], "len {len} produced an empty-looking digest");
        }
    }

    /// Every length across two full blocks plus change must differ from its neighbours. This is
    /// the cheap catch for a padding bug that makes length 56 collide with length 64, which is
    /// exactly what dropping the second padding block would do.
    #[test]
    fn adjacent_length_do_not_collide() {
        let mut seen = std::collections::HashSet::new();
        for len in 0..=140usize {
            let message = vec![b'z'; len];
            assert!(seen.insert(sha256(&message)), "length {len} collided with a shorter message");
        }
    }

    /// A digest is a pure function of the bytes: same input, same output, every time, with no
    /// dependence on how the hasher was constructed or reused.
    #[test]
    fn digest_is_deterministic() {
        let message = b"the proof that a lossless transcode round-tripped 1:1";
        let first = sha256(message);
        for _ in 0..8 {
            assert_eq!(sha256(message), first);
        }
        assert_eq!(Sha256::default().clone_and_hash(message), first);
    }

    /// A one-bit change anywhere must change the digest. Not an avalanche measurement — just the
    /// floor property that makes the dedup key a *key* rather than a bucket.
    #[test]
    fn one_bit_flip_change_the_digest() {
        let base = vec![0x5au8; 100];
        let want = sha256(&base);
        for i in 0..base.len() {
            for bit in 0..8u32 {
                let mut other = base.clone();
                other[i] ^= 1 << bit;
                assert_ne!(sha256(&other), want, "byte {i} bit {bit} did not change the digest");
            }
        }
    }

    /// Concatenation, not chunk identity, defines the message: `update("ab")` must not equal
    /// `update("a"); update("b")` differing from `sha256("ab")`, and neither may equal a hash of
    /// the pieces with a boundary marker. The negative half matters — a length-prefixed or
    /// separator-joined implementation would pass the positive half alone.
    #[test]
    fn stream_is_concatenation_not_a_join() {
        let mut h = Sha256::new();
        h.update(b"ab");
        h.update(b"c");
        assert_eq!(h.finish(), sha256(b"abc"));
        assert_ne!(sha256(b"abc"), sha256(b"ab c"));
        assert_ne!(sha256(b"abc"), sha256(b"a bc"));
    }

    /// `hex` round-trips a known digest into the exact string the standard prints, lowercase and
    /// 64 characters — the form a user compares against `sha256sum` output.
    #[test]
    fn hex_match_the_published_form() {
        let d = sha256(b"abc");
        let s = hex(&d);
        assert_eq!(s.len(), 64);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_eq!(&s[..8], "ba7816bf");
    }

    /// A multi-megabyte input hashes without holding a second copy, and agrees with the one-shot
    /// digest of the same bytes. This is the case `Sha256` exists for.
    #[test]
    fn megabyte_stream_agree_with_one_shot() {
        let message: Vec<u8> = (0..3_000_000u32).map(|i| (i % 253) as u8).collect();
        let want = sha256(&message);
        let mut h = Sha256::new();
        for chunk in message.chunks(8_192) {
            h.update(chunk);
        }
        assert_eq!(h.finish(), want);
    }

    /// Test-only helper: hash `byte` through an already-constructed hasher, to prove `Default`
    /// and `new` produce the same starting state.
    trait CloneAndHash {
        fn clone_and_hash(self, byte: &[u8]) -> [u8; 32];
    }
    impl CloneAndHash for Sha256 {
        fn clone_and_hash(mut self, byte: &[u8]) -> [u8; 32] {
            self.update(byte);
            self.finish()
        }
    }
}
