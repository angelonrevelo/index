//! Perceptual hashing and Hamming near-duplicate search.
//!
//! # What a perceptual hash is for, and the line it cannot cross
//!
//! These codes answer exactly one question: *are these two files the *same picture*, after a
//! re-encode, a resize, a brightness tweak, or a watermark?* They are computed in the pixel and
//! frequency domain, so they are **structurally incapable** of seeing a semantic duplicate — a
//! different photograph of the same building is, to a DCT, an unrelated image. That is not a
//! tuning problem and no threshold fixes it. Semantic near-duplication is the [`crate::vector`]
//! column's job, and the point of `ImageDoc` carrying both is that the caller can ask each
//! question of the operator that can actually answer it.
//!
//! The measured limits, which [`test`] asserts rather than merely claims:
//!
//!   - Meta's own PDQ guidance fails past roughly **5 degrees of rotation** and roughly **5 % of
//!     crop**. Our 90-degree and 40-%-crop tests below prove the failure rather than describe it.
//!   - Around **50 %** of *watermarked* images are still missed at a PDQ distance of 30.
//!
//! # No decoder lives here
//!
//! This crate has no image decoder and never will — that is the whole licence and footprint
//! argument in `crates/index-image/Cargo.toml`. Every entry point therefore takes an already
//! decoded 8-bit **luma** plane plus its `width`/`height`. [`luma_from_rgb`] converts an
//! interleaved RGB buffer if that is what the host has.
//!
//! # Bit-length is not a unit
//!
//! The single most common misuse of these codes is carrying a threshold across code lengths. A
//! Hamming distance of 31 is *tight* on a 256-bit PDQ code (12 % of the code) and *nonsense* on a
//! 64-bit pHash (48 % of the code — barely better than the 50 % two unrelated images score by
//! chance). Every threshold constant here is therefore named after the code it belongs to, and
//! each documents the measurement that produced it. See [`HASH64_NEAR_MAX`] and
//! [`HASH256_NEAR_MAX`].

use std::f64::consts::PI;

/// Recommended default Hamming radius for a **64-bit** code (`ahash`/`dhash`/`phash`).
///
/// The popular advice for 64-bit pHash is "threshold 10". It is too loose, and the number that
/// says so is direct: in one measured study, **31 of 63** pairs flagged at Hamming ≤ 10 were false
/// positives — a worse-than-coin-flip precision. Tightening the same run to **4** dropped that to
/// **6** false positives. 4 is therefore the default here, and a caller who widens it is buying
/// recall with precision and should measure the trade on their own corpus.
///
/// Note this is 6.25 % of the code, *not* the 12 % that [`HASH256_NEAR_MAX`] allows. Short codes
/// need proportionally tighter radii because they have fewer independent bits to disagree on.
pub const HASH64_NEAR_MAX: u32 = 4;

/// Recommended default Hamming radius for the **256-bit** [`Hash256::pdq`] code.
///
/// Meta's own recommended PDQ threshold is **≤ 31 of 256 bits**, and the reference Rust port
/// `darwinium-com/pdqhash` hardcodes the same cut as `< 32`. Matching a widely-deployed operating
/// point is worth more than a locally-tuned one, so this is 31.
pub const HASH256_NEAR_MAX: u32 = 31;

/// The distance two *unrelated* images are expected to score, as a fraction of code length.
///
/// Independent, unbiased bits disagree half the time, so a random pair sits at ~50 % of the code:
/// ~32 of 64, ~128 of 256. This is the null hypothesis every threshold is measured against, and
/// `test::an_unrelated_pair_sit_near_half_the_code_length` asserts our codes actually hit it —
/// a hash whose unrelated pairs cluster below 50 % is wasting bits.
pub const UNRELATED_FRACTION: f64 = 0.5;

/// Informative bit count of [`Hash64::phash`].
///
/// `phash` reads the top-left 8×8 block of the DCT **excluding the DC term**, which is 63
/// coefficients, not 64. The DC term is the mean brightness of the image — precisely the quantity
/// the hash is supposed to be blind to — so including it would spend a bit on the one signal we
/// deliberately discard. Rather than silently substitute an arbitrary extra coefficient to round
/// the code up, bit 0 (the DC slot) is held at zero and this constant states the honest length.
/// [`Hash64::ahash`] and [`Hash64::dhash`] carry a full 64.
///
/// [`Hash256::pdq`] does not have this problem: its 16×16 block is taken at `u, v ∈ 1..=16`, which
/// skips DC by construction and yields exactly 256 informative bits.
pub const PHASH_BIT_LEN: u32 = 63;

/// A 64-bit perceptual hash.
///
/// Bit `i` is `1 << i`. Which grid cell bit `i` corresponds to depends on the constructor and is
/// documented on each; codes from different constructors are **not** comparable to each other, and
/// nothing here can stop you subtracting a `dhash` from a `phash`, so don't.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Hash64(pub u64);

/// A 256-bit perceptual hash, little-endian by word: bit `i` lives in word `i / 64` at `1 << (i % 64)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Hash256(pub [u64; 4]);

// ---------------------------------------------------------------------------------------------
// Input conditioning
// ---------------------------------------------------------------------------------------------

/// Collapse an interleaved 8-bit RGB buffer to a luma plane, Rec. 601.
///
/// Returns `None` on zero dimensions or a buffer shorter than `w * h * 3`, because a caller who
/// got the stride wrong deserves a value back rather than a panic in a search path.
///
/// Rec. 601 (`0.299 / 0.587 / 0.114`) rather than Rec. 709 is chosen only for consistency with the
/// reference PDQ and pHash implementations a caller is likely to be comparing against; the
/// difference is a fraction of a grey level and does not move a hash bit in practice.
pub fn luma_from_rgb(rgb: &[u8], w: u32, h: u32) -> Option<Vec<u8>> {
    let px = pixel_count(w, h)?;
    let need = px.checked_mul(3)?;
    if rgb.len() < need {
        return None;
    }
    let mut luma = Vec::with_capacity(px);
    for i in 0..px {
        let r = rgb[i * 3] as f64;
        let g = rgb[i * 3 + 1] as f64;
        let b = rgb[i * 3 + 2] as f64;
        luma.push((0.299 * r + 0.587 * g + 0.114 * b).round().clamp(0.0, 255.0) as u8);
    }
    Some(luma)
}

/// `w * h` as a `usize`, or `None` if either side is zero or the product overflows.
fn pixel_count(w: u32, h: u32) -> Option<usize> {
    if w == 0 || h == 0 {
        return None;
    }
    (w as usize).checked_mul(h as usize)
}

/// Area-weighted box downscale to `ow * oh`, returning values still in `0.0..=255.0`.
///
/// # Why an area filter and not a nearest-neighbour subsample
///
/// The property the whole crate rests on is that the *same picture* at two resolutions hashes to
/// the same code. Nearest-neighbour breaks that immediately: it picks a different set of source
/// pixels for a 300×300 source than for a 512×512 one, so the 32×32 working image differs and the
/// DCT differs. An area filter integrates each output cell over its exact source footprint,
/// including **fractional** edge coverage, so a non-integer scale ratio converges to the same
/// answer as an integer one. `test::a_hash_is_stable_across_a_resize` uses a deliberately
/// non-integer ratio for exactly this reason.
///
/// Upscaling (`ow > w`) degenerates to a point sample, which is correct-enough: a hash of an image
/// smaller than its own working grid has already lost the information the hash wanted.
fn box_scale(luma: &[u8], w: usize, h: usize, ow: usize, oh: usize) -> Vec<f64> {
    let wx = axis_weight(w, ow);
    let wy = axis_weight(h, oh);

    // Horizontal pass into an `ow x h` intermediate, then vertical. Separating the passes turns an
    // O(w*h) per-output-pixel filter into two cheap ones and keeps the arithmetic order fixed,
    // which is what makes the result bit-for-bit reproducible run to run.
    let mut mid = vec![0.0f64; ow * h];
    for y in 0..h {
        for (ox, span) in wx.iter().enumerate() {
            let mut acc = 0.0;
            for &(sx, weight) in span {
                acc += luma[y * w + sx] as f64 * weight;
            }
            mid[y * ow + ox] = acc;
        }
    }

    let mut out = vec![0.0f64; ow * oh];
    for (oy, span) in wy.iter().enumerate() {
        for ox in 0..ow {
            let mut acc = 0.0;
            for &(sy, weight) in span {
                acc += mid[sy * ow + ox] * weight;
            }
            out[oy * ow + ox] = acc;
        }
    }
    out
}

/// Per-output-cell `(source index, weight)` list for one axis. Weights sum to 1.
fn axis_weight(src: usize, dst: usize) -> Vec<Vec<(usize, f64)>> {
    let scale = src as f64 / dst as f64;
    let mut plan = Vec::with_capacity(dst);
    for o in 0..dst {
        let s0 = o as f64 * scale;
        let s1 = s0 + scale;
        let first = s0.floor() as usize;
        let last = ((s1.ceil() as usize).max(first + 1)).min(src);
        let mut span: Vec<(usize, f64)> = Vec::with_capacity(last - first);
        let mut total = 0.0;
        for i in first..last {
            let lo = s0.max(i as f64);
            let hi = s1.min(i as f64 + 1.0);
            let weight = (hi - lo).max(0.0);
            if weight > 0.0 {
                span.push((i.min(src - 1), weight));
                total += weight;
            }
        }
        if span.is_empty() {
            // Upscaling: the output cell is narrower than a source pixel. Point-sample it.
            span.push((first.min(src - 1), 1.0));
            total = 1.0;
        }
        for entry in span.iter_mut() {
            entry.1 /= total;
        }
        plan.push(span);
    }
    plan
}

// ---------------------------------------------------------------------------------------------
// DCT
// ---------------------------------------------------------------------------------------------

/// Separable 2-D DCT-II, written out rather than pulled in.
///
/// # Why a naive O(n³) transform is the right call here
///
/// The only sizes this crate ever transforms are 32×32 and 64×64, both fixed at compile time. A
/// separable naive DCT costs `2·n³` multiply-adds: **65 536** for pHash and **524 288** for PDQ —
/// microseconds, and utterly dominated by whatever decoded the image in the first place. An FFT-
/// based O(n² log n) rewrite would save a fraction of a millisecond per image in exchange for
/// bit-reversal tables, a butterfly, and a class of off-by-one bug that produces a *plausible but
/// wrong* hash — the worst possible failure for a dedup index, because it is silent. The cost
/// model says loop; the risk model says loop louder.
///
/// Unnormalised DCT-II (`X[k] = Σ x[i]·cos(π(i+½)k/n)`). Orthonormal scaling factors are omitted
/// deliberately: every coefficient is only ever compared against a median of its own siblings, and
/// a per-row constant cannot change an ordering, so the scaling would be arithmetic we pay for and
/// then cancel out.
fn dct_2d(src: &[f64], n: usize) -> Vec<f64> {
    let mut table = vec![0.0f64; n * n];
    for k in 0..n {
        for i in 0..n {
            table[k * n + i] = (PI * (i as f64 + 0.5) * k as f64 / n as f64).cos();
        }
    }

    let mut row = vec![0.0f64; n * n];
    for r in 0..n {
        for k in 0..n {
            let mut acc = 0.0;
            for i in 0..n {
                acc += src[r * n + i] * table[k * n + i];
            }
            row[r * n + k] = acc;
        }
    }

    let mut out = vec![0.0f64; n * n];
    for c in 0..n {
        for k in 0..n {
            let mut acc = 0.0;
            for i in 0..n {
                acc += row[i * n + c] * table[k * n + i];
            }
            out[k * n + c] = acc;
        }
    }
    out
}

/// Median of a coefficient set, by value.
///
/// A **median** threshold rather than a mean is not a style choice: it guarantees the code is
/// half ones and half zeros for any input, which is what makes the unrelated-pair distribution
/// centre on 50 % of the code length and therefore what makes a Hamming threshold mean the same
/// thing on a dark image as on a bright one. A mean threshold on a coefficient set with one large
/// outlier produces a nearly-all-zero code, and nearly-all-zero codes collide.
///
/// `total_cmp` rather than `partial_cmp(..).unwrap()`: a NaN coefficient would be a bug, but an
/// index in a search path must not turn a bug into a panic.
fn median(v: &[f64]) -> f64 {
    let mut sorted = v.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let n = sorted.len();
    if n == 0 {
        return 0.0;
    }
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) * 0.5
    }
}

/// Per-pixel-unit coefficient spread below which a frame is treated as featureless.
///
/// Found by test, not by theory. A uniform frame's non-DC coefficients are not zero — they are the
/// floating-point residue of summing 4 096 identical values against a cosine that sums to zero,
/// around `1e-9` in magnitude. A median threshold does not care how small a number is, only which
/// side of the median it falls on, so **without this guard a blank scan hashes to a pseudo-random
/// 256-bit code, and two blank scans at different grey levels do not match each other.** For a
/// corpus with any quantity of blank pages, solid backdrops or letterboxed bars that is a silent
/// recall hole, so a featureless block collapses to the all-zero code instead.
///
/// `1e-6` per pixel-unit, scaled by `n * n`, sits nine orders of magnitude above the residue and
/// six below the coefficient a one-grey-level ripple produces — there is no plausible image in the
/// gap.
const FLAT_COEF_EPS: f64 = 1e-6;

/// `true` if a coefficient block carries no structure worth thresholding. See [`FLAT_COEF_EPS`].
fn is_flat(block: &[f64], n: usize) -> bool {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for &c in block {
        lo = lo.min(c);
        hi = hi.max(c);
    }
    let span = hi - lo;
    // A non-finite span means an empty block or a NaN coefficient — both bugs, neither an excuse
    // to hand back a code derived from garbage, so they take the flat path too.
    !span.is_finite() || span <= FLAT_COEF_EPS * (n * n) as f64
}

// ---------------------------------------------------------------------------------------------
// Hash64
// ---------------------------------------------------------------------------------------------

impl Hash64 {
    /// Average hash: 8×8 box downscale, bit set where the cell exceeds the frame mean.
    ///
    /// The cheapest useful code and the weakest. It survives re-encode and resize and nothing
    /// else; a gamma curve or a colour grade moves cells across the mean and the code walks. Keep
    /// it for a first-pass filter over a huge corpus and confirm with [`Self::phash`].
    ///
    /// Bit `row * 8 + col`, row-major from the top-left. `None` if `w`/`h` are zero or `luma` is
    /// shorter than `w * h`.
    pub fn ahash(luma: &[u8], w: u32, h: u32) -> Option<Self> {
        let px = pixel_count(w, h)?;
        if luma.len() < px {
            return None;
        }
        let cell = box_scale(luma, w as usize, h as usize, 8, 8);
        let mean = cell.iter().sum::<f64>() / cell.len() as f64;
        let mut bit = 0u64;
        for (i, &v) in cell.iter().enumerate() {
            if v > mean {
                bit |= 1 << i;
            }
        }
        Some(Hash64(bit))
    }

    /// Difference hash: 9×8 box downscale, bit set where a cell is brighter than its right neighbour.
    ///
    /// Encoding a *gradient direction* rather than a level makes this immune to any monotonic
    /// global brightness or contrast change — the comparison `a > b` survives adding a constant to
    /// both, which is exactly the re-encode and exposure-tweak case. It is correspondingly blind to
    /// vertical structure, which is why the crate offers it alongside `phash` and not instead of it.
    ///
    /// 8 rows × 8 comparisons = a full 64 informative bits, at bit `row * 8 + col`.
    pub fn dhash(luma: &[u8], w: u32, h: u32) -> Option<Self> {
        let px = pixel_count(w, h)?;
        if luma.len() < px {
            return None;
        }
        let cell = box_scale(luma, w as usize, h as usize, 9, 8);
        let mut bit = 0u64;
        for row in 0..8usize {
            for col in 0..8usize {
                if cell[row * 9 + col] > cell[row * 9 + col + 1] {
                    bit |= 1 << (row * 8 + col);
                }
            }
        }
        Some(Hash64(bit))
    }

    /// Perceptual (DCT) hash: 32×32 downscale, 2-D DCT-II, top-left 8×8 **excluding DC**, median
    /// threshold.
    ///
    /// The low-frequency corner of the DCT is where an image's *structure* lives; the high
    /// frequencies are where JPEG quantisation, resampling ringing and sensor noise live. Reading
    /// only the corner is what buys tolerance to a re-encode that [`Self::ahash`] does not have.
    ///
    /// The DC coefficient at `(0, 0)` is the frame's mean brightness — the one thing a perceptual
    /// hash must ignore — so it is excluded, leaving **63** informative bits. Bit 0 is the vacated
    /// DC slot and is always zero; see [`PHASH_BIT_LEN`] for why that is stated rather than papered
    /// over with a filler coefficient. Distances therefore run `0..=63`, and
    /// [`HASH64_NEAR_MAX`] is calibrated on that.
    ///
    /// Bit `v * 8 + u` for DCT coefficient `(u, v)`.
    pub fn phash(luma: &[u8], w: u32, h: u32) -> Option<Self> {
        let px = pixel_count(w, h)?;
        if luma.len() < px {
            return None;
        }
        let small = box_scale(luma, w as usize, h as usize, 32, 32);
        let coef = dct_2d(&small, 32);

        let mut block = Vec::with_capacity(63);
        for v in 0..8usize {
            for u in 0..8usize {
                if u == 0 && v == 0 {
                    continue;
                }
                block.push(coef[v * 32 + u]);
            }
        }
        if is_flat(&block, 32) {
            return Some(Hash64(0));
        }
        let mid = median(&block);

        let mut bit = 0u64;
        for v in 0..8usize {
            for u in 0..8usize {
                if u == 0 && v == 0 {
                    continue;
                }
                if coef[v * 32 + u] > mid {
                    bit |= 1 << (v * 8 + u);
                }
            }
        }
        Some(Hash64(bit))
    }

    /// Hamming distance. `0` is identical; ~32 is what two unrelated images score by chance.
    #[inline]
    pub fn distance(&self, other: &Hash64) -> u32 {
        (self.0 ^ other.0).count_ones()
    }

    /// Linear scan of `corpus`, returning `(index, distance)` for every entry within `max`.
    ///
    /// # Why linear and not an index
    ///
    /// A Hamming-radius index (BK-tree, multi-index hashing) only starts paying at corpus sizes and
    /// radii where the pruning beats the pointer chasing, and at `max = 4` on 64 bits the candidate
    /// set is already tiny — but so is the scan, because `count_ones` is one instruction and the
    /// corpus is a flat `&[u64]` the prefetcher walks perfectly. A branch-free scan over contiguous
    /// memory is the structure a modern core wants. When a corpus outgrows it the fix belongs in
    /// the caller's query plan, next to the text and vector candidate generation, not hidden here.
    ///
    /// Output is in corpus order, so a caller wanting the best hit sorts by `.1` themselves rather
    /// than paying for a sort they may not need.
    pub fn near(&self, corpus: &[Hash64], max: u32) -> Vec<(u32, u32)> {
        let mut hit = Vec::new();
        for (i, candidate) in corpus.iter().enumerate() {
            let d = self.distance(candidate);
            if d <= max {
                hit.push((i as u32, d));
            }
        }
        hit
    }
}

// ---------------------------------------------------------------------------------------------
// Hash256
// ---------------------------------------------------------------------------------------------

impl Hash256 {
    /// A **PDQ-shaped** 256-bit hash: 64×64 downscale, 2-D DCT-II, the 16×16 block at
    /// `u, v ∈ 1..=16`, median threshold.
    ///
    /// # This is NOT Meta's PDQ and does NOT interoperate with it
    ///
    /// State this plainly because getting it wrong is a safety failure, not a bug report. This
    /// function reproduces PDQ's *shape* — its working resolution, its transform, its
    /// low-frequency block geometry, its median threshold, and therefore its distance
    /// distribution and its ≤ 31/256 operating point. It is **not bit-compatible** with Meta's
    /// reference implementation, because that implementation additionally applies a specific
    /// two-pass Jarosz box blur before decimation and its own tent-filter decimation, and those
    /// choices change individual bits.
    ///
    /// The consequence: a code from this function **cannot be matched against a ThreatExchange
    /// PDQ blocklist**, or against any other corpus of real PDQ hashes. Comparing them would
    /// produce distances near the 128/256 chance level and silently find nothing. If you need
    /// interoperation with published PDQ hashes, use Meta's implementation or a certified port —
    /// this crate is dependency-free by design and that is the price it pays.
    ///
    /// What it *is* good for is exactly what `ImageDoc` wants: a wider, better-discriminating code
    /// than 64 bits for a corpus this engine owns end to end.
    ///
    /// # Why 256 bits at all
    ///
    /// A longer code buys separation. Unrelated pairs sit at ~128/256 with a tight binomial spread,
    /// so a 31-bit radius is ~12 % of the code and still ~97 standard-deviations-worth of margin
    /// from chance, where the equivalent 12 % of a 64-bit code is 8 bits — inside the region where
    /// a 64-bit pHash was measured to be majority-false-positive.
    ///
    /// Taking `u, v ∈ 1..=16` skips the DC term *and* the pure-horizontal/pure-vertical
    /// fundamentals by construction, so all 256 bits carry two-dimensional structure and none is
    /// spent on mean brightness. Bit `(v - 1) * 16 + (u - 1)`.
    pub fn pdq(luma: &[u8], w: u32, h: u32) -> Option<Self> {
        let px = pixel_count(w, h)?;
        if luma.len() < px {
            return None;
        }
        let small = box_scale(luma, w as usize, h as usize, 64, 64);
        let coef = dct_2d(&small, 64);

        let mut block = Vec::with_capacity(256);
        for v in 1..=16usize {
            for u in 1..=16usize {
                block.push(coef[v * 64 + u]);
            }
        }
        if is_flat(&block, 64) {
            return Some(Hash256([0; 4]));
        }
        let mid = median(&block);

        let mut word = [0u64; 4];
        for (i, &c) in block.iter().enumerate() {
            if c > mid {
                word[i / 64] |= 1 << (i % 64);
            }
        }
        Some(Hash256(word))
    }

    /// Hamming distance over all 256 bits. `0` is identical; ~128 is chance.
    #[inline]
    pub fn distance(&self, other: &Hash256) -> u32 {
        let mut d = 0;
        for i in 0..4 {
            d += (self.0[i] ^ other.0[i]).count_ones();
        }
        d
    }

    /// Linear scan of `corpus`, returning `(index, distance)` for every entry within `max`.
    /// Same argument as [`Hash64::near`]; four `count_ones` instead of one.
    pub fn near(&self, corpus: &[Hash256], max: u32) -> Vec<(u32, u32)> {
        let mut hit = Vec::new();
        for (i, candidate) in corpus.iter().enumerate() {
            let d = self.distance(candidate);
            if d <= max {
                hit.push((i as u32, d));
            }
        }
        hit
    }
}

#[cfg(test)]
mod test {
    use super::*;

    /// One xorshift64 step, returning a unit float. A hand-rolled generator rather than a crate,
    /// because a test corpus that changes when a dependency bumps is a test corpus that cannot
    /// pin a distance distribution.
    fn next_unit(s: &mut u64) -> f64 {
        *s ^= *s << 13;
        *s ^= *s >> 7;
        *s ^= *s << 17;
        ((*s >> 11) as f64) / ((1u64 << 53) as f64)
    }

    /// A deliberately asymmetric, band-limited synthetic image, sampled from a *continuous*
    /// function so the same picture can be rendered at any resolution.
    ///
    /// That is the point: a resize-stability test that resamples one raster with the crate's own
    /// `box_scale` would be testing `box_scale` against itself and would pass even if the filter
    /// were nonsense. Rendering the same continuous function at two sizes makes the two rasters
    /// genuinely independent approximations of one image, which is what a real re-encode is.
    ///
    /// `seed` selects a genuinely different picture — different spatial frequencies, not merely a
    /// phase shift of one picture, because a family of phase-shifted siblings would understate the
    /// unrelated-pair distance and make `HASH256_NEAR_MAX` look tighter than it is. Frequencies
    /// stay under ~3 cycles across the frame so the image is representable on the 32x32 working
    /// grid; an image with energy above that grid's Nyquist limit would alias, and aliasing is a
    /// property of the test image rather than of the hash.
    ///
    /// `crop` restricts the sampled window (a crop of the same picture); `swap` transposes and
    /// mirrors the axes (a 90-degree rotation).
    fn render(w: usize, h: usize, seed: u64, crop: f64, swap: bool) -> Vec<u8> {
        let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let comp: Vec<(f64, f64, f64, f64)> = (0..6)
            .map(|k| {
                let ax = (next_unit(&mut s) - 0.5) * 36.0;
                let ay = (next_unit(&mut s) - 0.5) * 36.0;
                let phase = next_unit(&mut s) * std::f64::consts::TAU;
                (ax, ay, phase, 0.20 / (k as f64 + 1.4))
            })
            .collect();
        // One hard edge, so the corpus is not purely sinusoidal: real photographs have occluding
        // contours and a DCT responds to them very differently than to a smooth field.
        let ex = next_unit(&mut s) - 0.5;
        let ey = next_unit(&mut s) - 0.5;
        let eo = next_unit(&mut s) * 0.4 - 0.2;

        let mut px = Vec::with_capacity(w * h);
        let lo = (1.0 - crop) * 0.5;
        for y in 0..h {
            for x in 0..w {
                let mut u = lo + ((x as f64 + 0.5) / w as f64) * crop;
                let mut v = lo + ((y as f64 + 0.5) / h as f64) * crop;
                if swap {
                    core::mem::swap(&mut u, &mut v);
                    u = 1.0 - u;
                }
                let mut val = 0.5;
                for &(ax, ay, phase, amp) in &comp {
                    val += amp * (ax * u + ay * v + phase).sin();
                }
                if ex * u + ey * v < eo {
                    val += 0.10;
                }
                px.push((val * 255.0).round().clamp(0.0, 255.0) as u8);
            }
        }
        px
    }

    fn frac(d: u32, len: u32) -> f64 {
        d as f64 / len as f64
    }

    #[test]
    fn a_hash_is_stable_across_a_resize() {
        // 512x512 against 300x300 — a deliberately NON-integer ratio (1.7066…) against the 32x32
        // and 64x64 working grids, because integer ratios hide fractional-coverage bugs in the
        // area filter and every real thumbnail pipeline produces non-integer ones.
        let big = render(512, 512, 3, 1.0, false);
        let small = render(300, 300, 3, 1.0, false);

        let pa = Hash64::phash(&big, 512, 512).unwrap();
        let pb = Hash64::phash(&small, 300, 300).unwrap();
        assert!(
            pa.distance(&pb) <= HASH64_NEAR_MAX,
            "phash across a resize: {} > {}",
            pa.distance(&pb),
            HASH64_NEAR_MAX
        );

        let da = Hash64::dhash(&big, 512, 512).unwrap();
        let db = Hash64::dhash(&small, 300, 300).unwrap();
        assert!(da.distance(&db) <= HASH64_NEAR_MAX, "dhash across a resize: {}", da.distance(&db));

        let qa = Hash256::pdq(&big, 512, 512).unwrap();
        let qb = Hash256::pdq(&small, 300, 300).unwrap();
        assert!(
            qa.distance(&qb) <= HASH256_NEAR_MAX,
            "pdq across a resize: {} > {}",
            qa.distance(&qb),
            HASH256_NEAR_MAX
        );
    }

    #[test]
    fn a_hash_is_deterministic() {
        // Cheap, but the whole index is worthless without it: an f64 DCT whose accumulation order
        // varied would produce a corpus that stops matching itself after a rebuild.
        let img = render(257, 193, 11, 1.0, false);
        for _ in 0..4 {
            assert_eq!(Hash64::phash(&img, 257, 193), Hash64::phash(&img, 257, 193));
            assert_eq!(Hash256::pdq(&img, 257, 193), Hash256::pdq(&img, 257, 193));
        }
    }

    #[test]
    fn an_unrelated_pair_sit_near_half_the_code_length() {
        // The null hypothesis from UNRELATED_FRACTION. A hash whose unrelated pairs cluster well
        // below 50 % has correlated bits and is quietly weaker than its length claims.
        let mut sample64 = Vec::new();
        let mut sample256 = Vec::new();
        let mut best_wide = u32::MAX;

        let code: Vec<(Hash64, Hash256)> = (0..12u64)
            .map(|seed| {
                let img = render(256, 256, seed * 7 + 1, 1.0, false);
                (
                    Hash64::phash(&img, 256, 256).unwrap(),
                    Hash256::pdq(&img, 256, 256).unwrap(),
                )
            })
            .collect();

        for i in 0..code.len() {
            for j in (i + 1)..code.len() {
                let d64 = code[i].0.distance(&code[j].0);
                let d256 = code[i].1.distance(&code[j].1);
                sample64.push(frac(d64, PHASH_BIT_LEN));
                sample256.push(frac(d256, 256));
                best_wide = best_wide.min(d256);
            }
        }

        let mean64 = sample64.iter().sum::<f64>() / sample64.len() as f64;
        let mean256 = sample256.iter().sum::<f64>() / sample256.len() as f64;
        assert!(
            (mean64 - UNRELATED_FRACTION).abs() < 0.12,
            "unrelated 64-bit pairs averaged {mean64:.3} of the code, expected ~{UNRELATED_FRACTION}"
        );
        assert!(
            (mean256 - UNRELATED_FRACTION).abs() < 0.10,
            "unrelated 256-bit pairs averaged {mean256:.3} of the code, expected ~{UNRELATED_FRACTION}"
        );

        // And the operating point actually separates: no unrelated pair falls inside the wide
        // code's recommended radius. This is the property HASH256_NEAR_MAX is claiming.
        assert!(
            best_wide > HASH256_NEAR_MAX,
            "an unrelated pair landed at {best_wide} <= {HASH256_NEAR_MAX} on the 256-bit code"
        );
    }

    #[test]
    fn a_reencode_and_brightness_shift_stay_under_the_recommended_threshold() {
        let base = render(384, 384, 5, 1.0, false);
        // Simulate the two things that actually happen to a file in the wild: an exposure/gamma
        // nudge, and JPEG's quantisation of fine detail (modelled here as a coarse requantisation
        // plus a small deterministic dither, so it is not a pure monotonic map that dhash would
        // survive trivially).
        let shifted: Vec<u8> = base
            .iter()
            .enumerate()
            .map(|(i, &p)| {
                let gamma = ((p as f64 / 255.0).powf(0.90) * 255.0) + 14.0;
                // Zero-mean white dither, NOT a periodic pattern: a fixed-period pattern beats
                // against the frame width and synthesises a low-frequency ramp the DCT would see,
                // which is an artefact of the test rather than of any real codec.
                let mut r = (i as u64).wrapping_mul(0x2545_F491_4F6C_DD1D) ^ 0x1234_5678;
                r ^= r >> 29;
                let dither = ((r >> 40) as f64 / 16_777_216.0 - 0.5) * 6.0;
                let q = ((gamma + dither) / 6.0).round() * 6.0;
                q.clamp(0.0, 255.0) as u8
            })
            .collect();

        let pa = Hash64::phash(&base, 384, 384).unwrap();
        let pb = Hash64::phash(&shifted, 384, 384).unwrap();
        assert!(
            pa.distance(&pb) <= HASH64_NEAR_MAX,
            "phash under re-encode + brightness: {} > {}",
            pa.distance(&pb),
            HASH64_NEAR_MAX
        );

        let qa = Hash256::pdq(&base, 384, 384).unwrap();
        let qb = Hash256::pdq(&shifted, 384, 384).unwrap();
        assert!(
            qa.distance(&qb) <= HASH256_NEAR_MAX,
            "pdq under re-encode + brightness: {} > {}",
            qa.distance(&qb),
            HASH256_NEAR_MAX
        );

        // dhash encodes a gradient sign, so a monotonic tone curve should barely touch it.
        let da = Hash64::dhash(&base, 384, 384).unwrap();
        let db = Hash64::dhash(&shifted, 384, 384).unwrap();
        assert!(da.distance(&db) <= HASH64_NEAR_MAX, "dhash: {}", da.distance(&db));
    }

    #[test]
    fn rotation_and_crop_break_the_hash_by_design() {
        // The LIMITATION, asserted rather than asserted-about. Meta documents PDQ failing past
        // ~5 degrees of rotation and ~5 % of crop; these are a 90-degree transpose and a 40 %
        // crop, both far outside that envelope, and the codes must NOT match. If a future change
        // ever makes this test fail, the hash has become rotation-tolerant and every threshold
        // constant above needs recalibrating — that is why the assertion points this way.
        let base = render(320, 320, 2, 1.0, false);
        let rotated = render(320, 320, 2, 1.0, true);
        let cropped = render(320, 320, 2, 0.60, false);

        let qa = Hash256::pdq(&base, 320, 320).unwrap();
        let qr = Hash256::pdq(&rotated, 320, 320).unwrap();
        let qc = Hash256::pdq(&cropped, 320, 320).unwrap();
        assert!(
            qr.distance(&qa) > HASH256_NEAR_MAX,
            "a 90-degree rotation was matched at {} — the hash is not rotation invariant and must \
             not appear to be",
            qr.distance(&qa)
        );
        assert!(
            qc.distance(&qa) > HASH256_NEAR_MAX,
            "a 40 % crop was matched at {}",
            qc.distance(&qa)
        );

        let pa = Hash64::phash(&base, 320, 320).unwrap();
        let pr = Hash64::phash(&rotated, 320, 320).unwrap();
        let pc = Hash64::phash(&cropped, 320, 320).unwrap();
        assert!(pr.distance(&pa) > HASH64_NEAR_MAX, "phash rotation: {}", pr.distance(&pa));
        assert!(pc.distance(&pa) > HASH64_NEAR_MAX, "phash crop: {}", pc.distance(&pa));
    }

    #[test]
    fn a_degenerate_input_return_none_rather_than_panicking() {
        let img = render(16, 16, 1, 1.0, false);
        // Zero dimensions.
        assert_eq!(Hash64::ahash(&img, 0, 16), None);
        assert_eq!(Hash64::dhash(&img, 16, 0), None);
        assert_eq!(Hash64::phash(&img, 0, 0), None);
        assert_eq!(Hash256::pdq(&img, 0, 16), None);
        // Buffer shorter than the stated frame — the classic caller stride bug.
        assert_eq!(Hash64::ahash(&img, 64, 64), None);
        assert_eq!(Hash64::dhash(&img, 64, 64), None);
        assert_eq!(Hash64::phash(&img, 64, 64), None);
        assert_eq!(Hash256::pdq(&img, 64, 64), None);
        // Empty slice.
        assert_eq!(Hash64::phash(&[], 1, 1), None);
        assert_eq!(Hash256::pdq(&[], 1, 1), None);
        // Dimensions whose product overflows a usize on a 32-bit target must not wrap.
        assert_eq!(Hash64::phash(&img, u32::MAX, u32::MAX), None);
        // RGB conditioning has the same contract.
        assert_eq!(luma_from_rgb(&img, 16, 16), None, "16x16 RGB needs 768 bytes, got 256");
        assert_eq!(luma_from_rgb(&[], 0, 0), None);
    }

    #[test]
    fn a_one_pixel_image_hashes_without_panicking() {
        // The upscale path: an image smaller than the 64x64 working grid. Useless as a hash, but
        // it must return a value, and a flat frame must land on the all-zero code rather than on
        // whatever an uninitialised buffer held.
        let one = [200u8];
        assert_eq!(Hash64::ahash(&one, 1, 1), Some(Hash64(0)));
        assert_eq!(Hash64::dhash(&one, 1, 1), Some(Hash64(0)));
        assert!(Hash256::pdq(&one, 1, 1).is_some());

        // Two flat greys ARE the same picture as far as a perceptual hash is concerned, so they
        // must collide. They did NOT before FLAT_COEF_EPS existed: the median threshold was
        // ranking floating-point residue and emitting a different pseudo-random code per grey
        // level. This assertion is the regression test for that, and it is the reason the guard
        // is in the implementation rather than left to the caller.
        let flat_a = vec![10u8; 64 * 64];
        let flat_b = vec![240u8; 64 * 64];
        assert_eq!(Hash64::phash(&flat_a, 64, 64), Some(Hash64(0)));
        assert_eq!(Hash256::pdq(&flat_a, 64, 64), Some(Hash256([0; 4])));
        assert_eq!(Hash64::phash(&flat_a, 64, 64), Hash64::phash(&flat_b, 64, 64));
        assert_eq!(Hash256::pdq(&flat_a, 64, 64), Hash256::pdq(&flat_b, 64, 64));

        // ...and the guard must not swallow a real, faint image. A ONE grey level checkerboard is
        // the weakest thing that is still a picture, and its coefficients sit six orders of
        // magnitude above the epsilon, so it has to survive. (The pattern is two-dimensional on
        // purpose: a purely horizontal ripple has all its energy in the `u = 0` column, which the
        // `1..=16` block skips by construction, so it would be legitimately blank to PDQ.)
        let faint: Vec<u8> =
            (0..64 * 64).map(|i: usize| 128 + (((i / 64) / 8 + (i % 64) / 8) % 2) as u8).collect();
        assert_ne!(Hash256::pdq(&faint, 64, 64), Some(Hash256([0; 4])));
    }

    #[test]
    fn luma_from_rgb_collapses_a_grey_pixel_to_itself() {
        // Rec. 601 weights sum to 1.0, so an achromatic pixel must survive the collapse exactly —
        // if it does not, the weights are wrong and every hash of an RGB source is off by a level.
        let rgb = [0u8, 0, 0, 128, 128, 128, 255, 255, 255];
        assert_eq!(luma_from_rgb(&rgb, 3, 1), Some(vec![0, 128, 255]));
    }

    #[test]
    fn near_return_index_and_distance_within_the_radius() {
        let corpus: Vec<Hash64> =
            vec![Hash64(0b0000), Hash64(0b0001), Hash64(0b0111), Hash64(u64::MAX)];
        let probe = Hash64(0b0000);
        assert_eq!(probe.near(&corpus, 0), vec![(0, 0)]);
        assert_eq!(probe.near(&corpus, 3), vec![(0, 0), (1, 1), (2, 3)]);
        assert_eq!(probe.near(&corpus, 64).len(), 4);
        assert!(probe.near(&[], 64).is_empty());

        let wide: Vec<Hash256> =
            vec![Hash256([0, 0, 0, 0]), Hash256([0, 3, 0, 0]), Hash256([u64::MAX; 4])];
        let probe = Hash256([0, 0, 0, 0]);
        assert_eq!(probe.near(&wide, HASH256_NEAR_MAX), vec![(0, 0), (1, 2)]);
        assert_eq!(probe.distance(&wide[2]), 256, "the maximum distance is the full code length");
    }

    #[test]
    fn a_threshold_is_not_portable_across_code_length() {
        // Guard rail for the documented trap: the two default radii are deliberately different
        // FRACTIONS of their codes, and anything that quietly equalises them has re-introduced the
        // bug the doc comment warns about.
        let short = frac(HASH64_NEAR_MAX, PHASH_BIT_LEN);
        let wide = frac(HASH256_NEAR_MAX, 256);
        assert!(short < wide, "the short code must use the TIGHTER fraction: {short} vs {wide}");
        // Naively rescaling the 256-bit operating point onto 64 bits lands at ~8, inside the
        // region a measured study found to be majority false positive at 10 and still bad at 8.
        let naive = (wide * 64.0).round() as u32;
        assert!(naive >= HASH64_NEAR_MAX * 2, "rescaling 31/256 onto 64 bits gives {naive}");
    }
}
