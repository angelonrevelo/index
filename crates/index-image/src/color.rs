//! Perceptual colour: sRGB → OKLab, k-means dominant colours, and a bucketing that turns a
//! palette into **facet terms** the existing `index-text` inverted index can already filter on.
//!
//! # The shape of the product question
//!
//! "Find images containing this colour" is not a nearest-neighbour question. It is a *filter*, and
//! this repo already has a very good filter: the inverted index. So the job of this module is not
//! to build a colour search structure — it is to reduce an image to a handful of **stable string
//! identifiers plus weights**, after which colour costs the query planner nothing it does not
//! already pay for a `camera = "iPhone 15"` facet. [`Palette::term`] is the whole interface to the
//! rest of the engine.
//!
//! # Why OKLab, and why not RGB
//!
//! Euclidean distance in sRGB does not match human perception — it is the textbook failure of
//! naive palette extraction, and it is why k-means here runs in a perceptual space. The obvious
//! candidate is CIELab, which is what the FOSS reference implementation for exactly this pipeline,
//! `sergeyk/rayleigh`, uses (a fixed palette histogram in CIELab). That code predates OKLab, which
//! Björn Ottosson published in 2020 *specifically* to fix CIELab's well-known blue-hue
//! non-uniformity: in CIELab, moving along constant hue through the blues visibly shifts toward
//! purple, so a Lab hue sector does not carve a perceptually coherent set of blues. Since the
//! entire output of this module is hue sectors, that defect would land directly on the product.
//! Hence OKLab.
//!
//! OKLab is also cheap enough to implement inline: two 3×3 matrices and a cube root. This crate is
//! deliberately dependency-free, and a colour-space conversion is not worth a dependency.
//!
//! # Why up to eight dominant colours
//!
//! MPEG-7's Dominant Colour Descriptor standardised the representation this module produces — a
//! set of **up to 8 dominant colours, each with the fraction of pixels it accounts for**. That is
//! precisely [`Swatch`] and its `weight`. The bound is the precedent for [`K_MAX`]; the default
//! [`K_DEFAULT`] sits inside the 5..=8 band because below five a photograph's palette collapses to
//! mud, and above eight the extra centroid is reliably a sampling artefact rather than a colour a
//! person would name.
//!
//! Shutterstock's US 10,235,424 describes the same pipeline end to end — extract, quantise into a
//! palette, segment into spectrum bands, rank — which is the commercial confirmation that the
//! bucketing step (not the extraction step) is where the retrieval quality actually lives.

use std::collections::BTreeMap;

// ---------------------------------------------------------------------------------------------
// sRGB ⇄ linear ⇄ OKLab
// ---------------------------------------------------------------------------------------------

/// The sRGB electro-optical transfer function: one 8-bit code value to linear light in `0.0..=1.0`.
///
/// The piecewise linear segment near black is not decoration — using a pure 2.2 power law instead
/// misplaces the darkest few code values, which is the exact region where "is this black or is
/// this a very dark blue" gets decided, and that decision is a *bucket* decision here.
#[inline]
pub fn srgb_to_linear(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Inverse of [`srgb_to_linear`], clamped and rounded back to an 8-bit code value.
#[inline]
pub fn linear_to_srgb(c: f32) -> u8 {
    let c = if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    (c.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Linear sRGB → OKLab, from Ottosson's published matrices.
///
/// The middle step is a cube root, not a log: OKLab's cone-response compression is `x^(1/3)`,
/// which is what makes its lightness `l` behave like a perceptual lightness rather than like
/// luminance.
#[inline]
pub fn linear_to_oklab(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let l = 0.412_221_47 * r + 0.536_332_54 * g + 0.051_445_995 * b;
    let m = 0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b;
    let s = 0.088_302_46 * r + 0.281_718_84 * g + 0.629_978_7 * b;

    let l_ = l.cbrt();
    let m_ = m.cbrt();
    let s_ = s.cbrt();

    (
        0.210_454_26 * l_ + 0.793_617_8 * m_ - 0.004_072_047 * s_,
        1.977_998_5 * l_ - 2.428_592_2 * m_ + 0.450_593_7 * s_,
        0.025_904_037 * l_ + 0.782_771_77 * m_ - 0.808_675_77 * s_,
    )
}

/// OKLab → linear sRGB. May return components outside `0.0..=1.0` for colours outside the sRGB
/// gamut; [`oklab_to_srgb`] clamps, which is the only sane thing to do when the destination is an
/// 8-bit code value.
#[inline]
pub fn oklab_to_linear(l: f32, a: f32, b: f32) -> (f32, f32, f32) {
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;

    let l = l_ * l_ * l_;
    let m = m_ * m_ * m_;
    let s = s_ * s_ * s_;

    (
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
        -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    )
}

/// An 8-bit sRGB triple to OKLab.
#[inline]
pub fn srgb_to_oklab(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    linear_to_oklab(srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b))
}

/// OKLab back to an 8-bit sRGB triple, gamut-clamped.
#[inline]
pub fn oklab_to_srgb(l: f32, a: f32, b: f32) -> (u8, u8, u8) {
    let (r, g, bl) = oklab_to_linear(l, a, b);
    (linear_to_srgb(r), linear_to_srgb(g), linear_to_srgb(bl))
}

/// The cylindrical view of OKLab: `(lightness, chroma, hue in degrees 0..360)`.
///
/// This is the coordinate system the bucketing actually uses, because the three axes correspond to
/// the three things a person says about a colour — how light, how colourful, which colour — while
/// `a` and `b` correspond to nothing anybody says out loud.
#[inline]
pub fn oklch(l: f32, a: f32, b: f32) -> (f32, f32, f32) {
    let chroma = (a * a + b * b).sqrt();
    let mut hue = b.atan2(a).to_degrees();
    if hue < 0.0 {
        hue += 360.0;
    }
    // A hue of exactly -0.0 lands on 360.0 after the fixup above; fold it back so the domain stays
    // half-open and sector arithmetic can never produce an out-of-range sector.
    if hue >= 360.0 {
        hue -= 360.0;
    }
    (l, chroma, hue)
}

// ---------------------------------------------------------------------------------------------
// Swatch / Palette
// ---------------------------------------------------------------------------------------------

/// One dominant colour, in OKLab, with the fraction of sampled pixels it accounts for.
///
/// This is MPEG-7's Dominant Colour Descriptor entry — colour plus pixel fraction — with the
/// colour space swapped for OKLab.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Swatch {
    pub l: f32,
    pub a: f32,
    pub b: f32,
    /// Fraction of the sampled pixels assigned to this swatch. Across a [`Palette`] these sum to
    /// ~1.0 (exactly 1.0 up to `f32` accumulation error).
    pub weight: f32,
}

impl Swatch {
    /// This swatch in `(lightness, chroma, hue°)`.
    #[inline]
    pub fn oklch(&self) -> (f32, f32, f32) {
        oklch(self.l, self.a, self.b)
    }

    /// This swatch as an 8-bit sRGB triple, for display.
    #[inline]
    pub fn srgb(&self) -> (u8, u8, u8) {
        oklab_to_srgb(self.l, self.a, self.b)
    }

    /// Squared OKLab distance to another swatch, ignoring weight.
    #[inline]
    pub fn dist2(&self, other: &Swatch) -> f32 {
        let dl = self.l - other.l;
        let da = self.a - other.a;
        let db = self.b - other.b;
        dl * dl + da * da + db * db
    }
}

/// Up to [`K_MAX`] dominant colours, ordered by descending weight.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Palette(pub Vec<Swatch>);

// ---------------------------------------------------------------------------------------------
// Bucketing
// ---------------------------------------------------------------------------------------------

/// Hue sectors on the OKLCH wheel. 30° each — narrow enough that two sectors are never confusable
/// by name, wide enough that a JPEG's chroma subsampling cannot walk a colour out of its sector.
pub const HUE_SECTOR: usize = 12;

/// Lightness bands for a *chromatic* colour: dark / mid / light.
pub const LIGHT_BAND: usize = 3;

/// Chroma bands for a chromatic colour: muted / vivid.
pub const CHROMA_BAND: usize = 2;

/// Dedicated neutral buckets: black, dark grey, grey, light grey, white.
pub const NEUTRAL_BAND: usize = 5;

/// Total number of perceptual colour buckets.
///
/// `5 neutral + 3 lightness × 2 chroma × 12 hue = 77`. Small enough that a bucket is a genuinely
/// selective facet term on a corpus of any size, and small enough to enumerate in a UI swatch grid.
pub const BUCKET_COUNT: usize = NEUTRAL_BAND + LIGHT_BAND * CHROMA_BAND * HUE_SECTOR;

/// Below this OKLab chroma a colour is treated as **achromatic** and gets a neutral bucket.
///
/// This is the single most important correctness point in the module. A grey pixel has `a ≈ b ≈ 0`
/// and therefore a hue of `atan2(≈0, ≈0)` — which is *pure numerical noise*. JPEG chroma
/// subsampling, an 8-bit rounding, or a one-code-value white-balance drift will move a grey's hue
/// by a hundred degrees at no perceptual cost whatsoever. Bucketing greys by hue would therefore
/// scatter the greyscale photographs of a corpus uniformly across all twelve hue sectors, and make
/// every hue facet silently wrong. So: near-zero chroma has no meaningful hue, and is never given
/// one.
///
/// The value is ~15 % of the maximum chroma reachable in sRGB (OKLab chroma of pure red is 0.258),
/// which puts the line roughly where a person stops saying "beige" and starts saying "orange".
pub const CHROMA_NEUTRAL: f32 = 0.04;

/// Lightness cuts between the five neutral buckets.
const NEUTRAL_CUT: [f32; 4] = [0.20, 0.40, 0.62, 0.85];

/// Lightness cuts between the three chromatic lightness bands.
const LIGHT_CUT: [f32; 2] = [0.45, 0.72];

/// Chroma cut between muted and vivid.
const CHROMA_CUT: f32 = 0.12;

/// Hue sector labels, in sector order. Sector `k` covers `[15 + 30k, 45 + 30k)` degrees.
///
/// The 15° offset is not cosmetic: it centres sector 0 on sRGB pure red, whose OKLCH hue is 29.2°.
/// Without it, red straddles a sector boundary and a one-code-value change can flip its facet term.
/// The other sRGB primaries land mid-sector too — yellow 109.8°, green 142.5°, blue 264.1°,
/// magenta 328.4° — which is the property that makes the labels below approximately honest.
///
/// The label is a human-readable convenience. The **bucket index is the identity**; a label is
/// only ever derived from it, never parsed back.
const HUE_NAME: [&str; HUE_SECTOR] = [
    "red", "orange", "amber", "yellow", "green", "teal", "cyan", "azure", "blue", "violet",
    "magenta", "pink",
];

const NEUTRAL_NAME: [&str; NEUTRAL_BAND] = ["black", "dark-grey", "grey", "light-grey", "white"];

const LIGHT_NAME: [&str; LIGHT_BAND] = ["dark", "mid", "light"];

const CHROMA_NAME: [&str; CHROMA_BAND] = ["muted", "vivid"];

/// Which perceptual bucket a swatch falls in, in `0..BUCKET_COUNT`.
///
/// Buckets are carved in **OKLCH**, not on a uniform OKLab `a`/`b` grid. A uniform Lab grid spends
/// most of its cells on colours no image contains (the corners of the `a`/`b` square are outside
/// every real gamut) while giving the near-grey axis — where a large share of every photograph
/// actually lives — a single cell. Lightness × chroma × hue spends its cells where the pixels are,
/// and each axis is separately meaningful to a person forming the query.
///
/// Indices `0..NEUTRAL_BAND` are the neutral buckets; the rest are chromatic. See
/// [`CHROMA_NEUTRAL`] for why that split exists at all.
pub fn bucket_of(swatch: &Swatch) -> usize {
    let (l, chroma, hue) = swatch.oklch();

    if !chroma.is_finite() || !l.is_finite() || chroma < CHROMA_NEUTRAL {
        // NaN-safe by construction: a non-finite `l` fails every `>=` and so lands in `black`
        // rather than indexing out of range.
        let mut band = 0;
        for cut in NEUTRAL_CUT {
            if l >= cut {
                band += 1;
            }
        }
        return band;
    }

    let mut light = 0;
    for cut in LIGHT_CUT {
        if l >= cut {
            light += 1;
        }
    }
    let chroma_band = usize::from(chroma >= CHROMA_CUT);

    // `+ 345` rather than `- 15` keeps the value non-negative before the modulo.
    let sector = ((((hue + 345.0) % 360.0) / 30.0) as usize).min(HUE_SECTOR - 1);

    NEUTRAL_BAND + (light * CHROMA_BAND + chroma_band) * HUE_SECTOR + sector
}

/// The stable facet term for a bucket index, or `None` if the index is out of range.
///
/// The `col:` prefix namespaces these against every other facet an `ImageDoc` carries, so a caller
/// can post a palette into the same term dictionary as `camera` and `lens` without a collision.
pub fn term_of_bucket(bucket: usize) -> Option<String> {
    if bucket >= BUCKET_COUNT {
        return None;
    }
    if bucket < NEUTRAL_BAND {
        return Some(format!("col:{}", NEUTRAL_NAME[bucket]));
    }
    let rest = bucket - NEUTRAL_BAND;
    let sector = rest % HUE_SECTOR;
    let cell = rest / HUE_SECTOR;
    let chroma_band = cell % CHROMA_BAND;
    let light = cell / CHROMA_BAND;
    Some(format!("col:{}-{}-{}", HUE_NAME[sector], LIGHT_NAME[light], CHROMA_NAME[chroma_band]))
}

// ---------------------------------------------------------------------------------------------
// Extraction
// ---------------------------------------------------------------------------------------------

/// Upper bound on `k`, from MPEG-7's Dominant Colour Descriptor: **up to 8 dominant colours**.
pub const K_MAX: usize = 8;

/// Default `k`. Inside the 5..=8 band the MPEG-7 precedent sets; six is chosen because a
/// photograph reliably has a subject, a background, a shadow and a highlight — four — plus room
/// for two accents, and every centroid past that is reliably a sampling artefact rather than a
/// colour anyone would name.
pub const K_DEFAULT: usize = 6;

/// Longest edge of the downsampled copy k-means actually runs on.
///
/// **Downsampling first is what makes extraction cost flat regardless of input resolution.** A
/// 48 MP photo and a 640×480 thumbnail both reduce to at most `SAMPLE_EDGE²` = 4096 points, so
/// Lloyd's algorithm — the expensive part, `O(iteration × k × n)` — costs the same for both. Only
/// the stride walk sees the original, and it touches one pixel per output sample, never all of
/// them.
pub const SAMPLE_EDGE: u32 = 64;

/// Iteration cap for Lloyd's algorithm. Convergence on 4096 points in 3-D is typically under ten
/// iterations; the cap exists so a pathological input cannot spin, not because it is usually hit.
const MAX_ITER: usize = 32;

/// Fixed seed for the k-means++ picker.
///
/// Determinism is a hard requirement: the palette is a **persisted index term**, so the same bytes
/// must produce the same terms in every process, on every machine, forever. That rules out a
/// thread-local RNG, any clock seed, and any iteration order over a `HashMap`. A fixed seed is the
/// entire mechanism.
const SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// SplitMix64 over a fixed counter. Not cryptographic and not trying to be — the only property
/// required is that it is a stable, well-spread sequence with no external state.
struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Lcg(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[inline]
fn dist2(a: &[f32; 3], b: &[f32; 3]) -> f32 {
    let d0 = a[0] - b[0];
    let d1 = a[1] - b[1];
    let d2 = a[2] - b[2];
    d0 * d0 + d1 * d1 + d2 * d2
}

/// Stride-sample the image into OKLab points, at most `SAMPLE_EDGE²` of them.
///
/// Stride sampling, not box averaging. Averaging a block **invents a colour the image does not
/// contain** — a red-and-blue checkerboard averages to purple, and "find images containing this
/// colour" would then return it for purple and not for red. Nearest sampling can only ever return
/// colours that are really there, which is the property the product needs. It also keeps the walk
/// proportional to the *output* size rather than the input's.
fn sample(rgb: &[u8], w: u32, h: u32) -> Option<Vec<[f32; 3]>> {
    if w == 0 || h == 0 {
        return None;
    }
    // Checked, because `w` and `h` come from a decoder the caller does not control and a `u32`
    // product overflows well before a plausible image does.
    let need = (w as u64).checked_mul(h as u64)?.checked_mul(3)?;
    if need > rgb.len() as u64 {
        return None;
    }
    let step_x = w.div_ceil(SAMPLE_EDGE).max(1);
    let step_y = h.div_ceil(SAMPLE_EDGE).max(1);

    let mut point = Vec::new();
    let mut y = 0;
    while y < h {
        let row = y as usize * w as usize;
        let mut x = 0;
        while x < w {
            let i = (row + x as usize) * 3;
            let (l, a, b) = srgb_to_oklab(rgb[i], rgb[i + 1], rgb[i + 2]);
            point.push([l, a, b]);
            x += step_x;
        }
        y += step_y;
    }
    if point.is_empty() {
        None
    } else {
        Some(point)
    }
}

/// k-means++ seeding, deterministic.
///
/// Plain random seeding is the documented failure mode of k-means: two centroids land in the same
/// dominant region, the rare-but-vivid accent colour never gets one, and the palette loses exactly
/// the colour a person would have searched for. k-means++ picks each new centroid with probability
/// proportional to its squared distance from the nearest already-chosen centroid, which biases
/// hard toward covering that accent.
///
/// The loop stops early when every point is already *exactly* on a centroid (`total == 0`), which
/// is how a two-colour image asked for `k = 6` correctly yields two swatches instead of four
/// duplicates.
fn seed_centroid(point: &[[f32; 3]], k: usize) -> Vec<[f32; 3]> {
    let mut rng = Lcg::new(SEED);
    let mut centroid = Vec::with_capacity(k);
    centroid.push(point[(rng.next_u64() % point.len() as u64) as usize]);

    let mut near = vec![f32::INFINITY; point.len()];
    while centroid.len() < k {
        let last = centroid[centroid.len() - 1];
        let mut total = 0.0f64;
        for (p, n) in point.iter().zip(near.iter_mut()) {
            let d = dist2(p, &last);
            if d < *n {
                *n = d;
            }
            total += *n as f64;
        }
        if total <= 0.0 {
            break;
        }
        let mut target = rng.next_f64() * total;
        let mut pick = point.len() - 1;
        for (i, &n) in near.iter().enumerate() {
            target -= n as f64;
            if target <= 0.0 {
                pick = i;
                break;
            }
        }
        centroid.push(point[pick]);
    }
    centroid
}

impl Palette {
    /// Extract up to `k` dominant colours by k-means in OKLab.
    ///
    /// `rgb` is tightly packed 8-bit RGB, three bytes per pixel, `w * h` pixels. Returns `None`
    /// for degenerate input — zero width or height, `k == 0`, or a buffer shorter than
    /// `w * h * 3` — rather than panicking, because those values routinely come from a decoder the
    /// caller does not control. `k` is clamped to [`K_MAX`].
    ///
    /// The result is **deterministic**: identical bytes give an identical palette in every process
    /// and on every machine.
    pub fn extract(rgb: &[u8], w: u32, h: u32, k: usize) -> Option<Palette> {
        if k == 0 {
            return None;
        }
        let point = sample(rgb, w, h)?;
        let k = k.min(K_MAX).min(point.len());

        let mut centroid = seed_centroid(&point, k);
        let mut assign = vec![usize::MAX; point.len()];

        for _ in 0..MAX_ITER {
            let mut moved = false;
            for (p, a) in point.iter().zip(assign.iter_mut()) {
                let mut best = 0usize;
                let mut best_d = f32::INFINITY;
                for (j, c) in centroid.iter().enumerate() {
                    let d = dist2(p, c);
                    // Strict `<` makes the lowest centroid index win a tie, which is what keeps
                    // assignment independent of iteration accidents.
                    if d < best_d {
                        best_d = d;
                        best = j;
                    }
                }
                if *a != best {
                    *a = best;
                    moved = true;
                }
            }
            if !moved {
                break;
            }
            let mut sum = vec![[0.0f64; 3]; centroid.len()];
            let mut count = vec![0u32; centroid.len()];
            for (p, &a) in point.iter().zip(assign.iter()) {
                sum[a][0] += p[0] as f64;
                sum[a][1] += p[1] as f64;
                sum[a][2] += p[2] as f64;
                count[a] += 1;
            }
            for (j, c) in centroid.iter_mut().enumerate() {
                // An emptied cluster keeps its position rather than being re-seeded. Re-seeding
                // would need a fresh draw mid-loop and buys nothing: the cluster is dropped below.
                if count[j] == 0 {
                    continue;
                }
                let n = count[j] as f64;
                *c = [(sum[j][0] / n) as f32, (sum[j][1] / n) as f32, (sum[j][2] / n) as f32];
            }
        }

        let mut count = vec![0u32; centroid.len()];
        for &a in &assign {
            if a < count.len() {
                count[a] += 1;
            }
        }
        let total = point.len() as f32;
        let mut swatch: Vec<Swatch> = centroid
            .iter()
            .zip(count.iter())
            .filter(|(_, &n)| n > 0)
            .map(|(c, &n)| Swatch { l: c[0], a: c[1], b: c[2], weight: n as f32 / total })
            .collect();

        if swatch.is_empty() {
            return None;
        }

        // Descending weight, with a total-order tie-break on the coordinates so that two clusters
        // of exactly equal size still order identically on every run.
        swatch.sort_by(|x, y| {
            y.weight
                .total_cmp(&x.weight)
                .then(x.l.total_cmp(&y.l))
                .then(x.a.total_cmp(&y.a))
                .then(x.b.total_cmp(&y.b))
        });
        Some(Palette(swatch))
    }

    /// [`Palette::extract`] with [`K_DEFAULT`].
    pub fn extract_default(rgb: &[u8], w: u32, h: u32) -> Option<Palette> {
        Palette::extract(rgb, w, h, K_DEFAULT)
    }

    /// The palette as facet terms: `(term, weight)`, ready to post into `index-text`.
    ///
    /// Swatches that share a bucket are **merged and their weights summed** — two centroids that
    /// both landed in `col:blue-mid-vivid` describe one facet, not two, and emitting the term twice
    /// would double-count it in any weighted scoring. Ordering is descending weight then
    /// lexicographic term, so the output is byte-identical across runs.
    pub fn term(&self) -> Vec<(String, f32)> {
        let mut acc: BTreeMap<usize, f32> = BTreeMap::new();
        for s in &self.0 {
            *acc.entry(bucket_of(s)).or_insert(0.0) += s.weight;
        }
        let mut out: Vec<(String, f32)> = acc
            .into_iter()
            .filter_map(|(bucket, weight)| term_of_bucket(bucket).map(|t| (t, weight)))
            .collect();
        out.sort_by(|x, y| y.1.total_cmp(&x.1).then(x.0.cmp(&y.0)));
        out
    }

    /// Total weight. ~1.0 for any palette [`Palette::extract`] produced.
    pub fn weight_sum(&self) -> f32 {
        self.0.iter().map(|s| s.weight).sum()
    }

    /// Perceptual distance between two palettes: the symmetrised **weighted nearest-swatch**
    /// metric.
    ///
    /// For each swatch in `self`, find its nearest swatch in `other` in OKLab, weight that distance
    /// by the swatch's pixel fraction, and sum; do the same in the other direction and average the
    /// two. Averaging is what makes it symmetric — the one-directional form is not, and an
    /// asymmetric "distance" cannot be used to sort results without the ranking depending on which
    /// argument the caller happened to put first.
    ///
    /// **What this gives up, honestly.** The right metric here is the Earth Mover's Distance: EMD
    /// is what MPEG-7-style dominant-colour comparison actually calls for, because it accounts for
    /// how much mass has to move as well as how far. This metric does not — a swatch's weight
    /// scales its own distance but places no constraint on the *receiving* side's capacity. So one
    /// tiny 2 %-weight red swatch in `other` will absorb a 90 %-weight red swatch in `self` at zero
    /// cost, and a mostly-red image will read as close to a mostly-white image with a red speck.
    /// EMD would charge for that; this does not.
    ///
    /// It is chosen anyway because it is `O(n·m)` on `n, m ≤ 8` with no allocation and no solver,
    /// against a transportation LP, and because the *primary* colour access path in this engine is
    /// the bucket facet ([`Palette::term`]), where the inverted index does the filtering. This
    /// metric is a re-ranking tie-break over an already-filtered candidate set, not the retrieval
    /// mechanism — which is the regime where the failure above is affordable.
    ///
    /// Returns `0.0` if both palettes are empty and `f32::INFINITY` if exactly one is.
    pub fn distance(&self, other: &Palette) -> f32 {
        match (self.0.is_empty(), other.0.is_empty()) {
            (true, true) => return 0.0,
            (true, false) | (false, true) => return f32::INFINITY,
            _ => {}
        }
        (directed(self, other) + directed(other, self)) / 2.0
    }
}

/// One direction of [`Palette::distance`]: weight-scaled nearest-swatch, not symmetric on its own.
fn directed(from: &Palette, to: &Palette) -> f32 {
    from.0
        .iter()
        .map(|s| {
            let near = to.0.iter().map(|t| s.dist2(t)).fold(f32::INFINITY, f32::min);
            s.weight * near.sqrt()
        })
        .sum()
}

// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod test {
    use super::*;

    /// A solid `w × h` image of one colour.
    fn solid(w: u32, h: u32, c: (u8, u8, u8)) -> Vec<u8> {
        let mut v = Vec::with_capacity((w * h * 3) as usize);
        for _ in 0..w * h {
            v.extend_from_slice(&[c.0, c.1, c.2]);
        }
        v
    }

    /// Left half `a`, right half `b`.
    fn split(w: u32, h: u32, a: (u8, u8, u8), b: (u8, u8, u8)) -> Vec<u8> {
        let mut v = Vec::with_capacity((w * h * 3) as usize);
        for _ in 0..h {
            for x in 0..w {
                let c = if x < w / 2 { a } else { b };
                v.extend_from_slice(&[c.0, c.1, c.2]);
            }
        }
        v
    }

    #[test]
    fn oklab_round_trip_is_exact_to_one_code_value() {
        // Stated tolerance: every 8-bit channel survives sRGB → OKLab → sRGB within ±1 code value.
        // That is the tightest claim an 8-bit round trip through an f32 cube root can support, and
        // it is the only claim that matters — a one-code-value drift is invisible and cannot move
        // a colour across a bucket cut, whose narrowest span is 0.15 in lightness.
        let mut worst = 0i32;
        for r in (0..=255u16).step_by(17) {
            for g in (0..=255u16).step_by(17) {
                for b in (0..=255u16).step_by(17) {
                    let (r, g, b) = (r as u8, g as u8, b as u8);
                    let (l, a, bb) = srgb_to_oklab(r, g, b);
                    let (r2, g2, b2) = oklab_to_srgb(l, a, bb);
                    worst = worst
                        .max((r as i32 - r2 as i32).abs())
                        .max((g as i32 - g2 as i32).abs())
                        .max((b as i32 - b2 as i32).abs());
                }
            }
        }
        assert!(worst <= 1, "round-trip drifted by {worst} code values");
    }

    #[test]
    fn oklab_matches_ottosson_reference_value() {
        // White is L = 1, a = b = 0 by construction of the matrices; if the transcription were
        // wrong this is the first thing that would break.
        let (l, a, b) = srgb_to_oklab(255, 255, 255);
        assert!((l - 1.0).abs() < 1e-3, "white lightness {l}");
        assert!(a.abs() < 1e-3 && b.abs() < 1e-3, "white is not achromatic: {a}, {b}");

        let (l, _, _) = srgb_to_oklab(0, 0, 0);
        assert!(l.abs() < 1e-4, "black lightness {l}");

        // Published landmarks for the sRGB primaries in OKLCH.
        let (l, a, b) = srgb_to_oklab(255, 0, 0);
        let (l, c, h) = oklch(l, a, b);
        assert!((l - 0.6279).abs() < 2e-3, "red L {l}");
        assert!((c - 0.2577).abs() < 2e-3, "red C {c}");
        assert!((h - 29.23).abs() < 1.0, "red h {h}");

        let (l, a, b) = srgb_to_oklab(0, 0, 255);
        let (_, _, h) = oklch(l, a, b);
        assert!((h - 264.05).abs() < 1.0, "blue h {h}");
    }

    #[test]
    fn a_pure_red_image_gives_one_swatch_in_the_red_bucket() {
        let img = solid(40, 30, (255, 0, 0));
        let p = Palette::extract(&img, 40, 30, K_DEFAULT).expect("red image extracts");
        assert_eq!(p.0.len(), 1, "one colour must not become several centroids");

        let bucket = bucket_of(&p.0[0]);
        assert!(bucket >= NEUTRAL_BAND, "red must not be neutral");
        assert_eq!((bucket - NEUTRAL_BAND) % HUE_SECTOR, 0, "red is hue sector 0");
        assert_eq!(p.term(), vec![("col:red-mid-vivid".to_string(), 1.0)]);
    }

    #[test]
    fn a_grey_image_lands_in_a_neutral_bucket_not_a_hue_bucket() {
        // The load-bearing test of this module. Mid grey's hue is numerical noise; it must never
        // reach a hue sector.
        for level in [0u8, 40, 80, 128, 190, 255] {
            let img = solid(20, 20, (level, level, level));
            let p = Palette::extract(&img, 20, 20, K_DEFAULT).expect("grey extracts");
            let bucket = bucket_of(&p.0[0]);
            assert!(
                bucket < NEUTRAL_BAND,
                "grey {level} landed in chromatic bucket {bucket} ({:?})",
                term_of_bucket(bucket)
            );
        }

        let grey = Palette::extract(&solid(8, 8, (128, 128, 128)), 8, 8, K_DEFAULT).unwrap();
        assert_eq!(grey.term(), vec![("col:grey".to_string(), 1.0)]);
        let black = Palette::extract(&solid(8, 8, (0, 0, 0)), 8, 8, K_DEFAULT).unwrap();
        assert_eq!(black.term(), vec![("col:black".to_string(), 1.0)]);
        let white = Palette::extract(&solid(8, 8, (255, 255, 255)), 8, 8, K_DEFAULT).unwrap();
        assert_eq!(white.term(), vec![("col:white".to_string(), 1.0)]);
    }

    #[test]
    fn a_near_grey_does_not_scatter_across_hue_sector() {
        // The real hazard is not exact grey — it is grey plus a code value of noise, which is what
        // any real decoder hands you. Every one of these must agree on one neutral bucket.
        let noisy: [(u8, u8, u8); 6] = [
            (128, 128, 128),
            (129, 128, 128),
            (128, 129, 128),
            (128, 128, 129),
            (127, 128, 129),
            (129, 128, 127),
        ];
        let mut seen = Vec::new();
        for c in noisy {
            let (l, a, b) = srgb_to_oklab(c.0, c.1, c.2);
            seen.push(bucket_of(&Swatch { l, a, b, weight: 1.0 }));
        }
        assert!(seen.iter().all(|&b| b < NEUTRAL_BAND), "noise reached a hue bucket: {seen:?}");
        assert!(seen.windows(2).all(|w| w[0] == w[1]), "one grey split across buckets: {seen:?}");
    }

    #[test]
    fn extraction_is_deterministic_across_run() {
        // A varied synthetic image, so k-means has real work to do and real opportunity to diverge.
        let (w, h) = (57u32, 43u32);
        let mut img = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                img.extend_from_slice(&[
                    ((x * 7 + y * 3) % 256) as u8,
                    ((x * 13 + y * 29) % 256) as u8,
                    ((x * 5 + y * 17) % 256) as u8,
                ]);
            }
        }
        let a = Palette::extract(&img, w, h, K_DEFAULT).expect("extracts");
        let b = Palette::extract(&img, w, h, K_DEFAULT).expect("extracts");
        assert_eq!(a, b, "same bytes gave a different palette");
        assert_eq!(a.term(), b.term());
        assert!(a.0.len() > 1, "test image should yield several swatches, got {}", a.0.len());
    }

    #[test]
    fn a_two_colour_image_gives_two_swatch_whose_weight_sum_to_one() {
        let (w, h) = (40u32, 20u32);
        let img = split(w, h, (255, 0, 0), (0, 0, 255));
        let p = Palette::extract(&img, w, h, K_DEFAULT).expect("extracts");
        assert_eq!(p.0.len(), 2, "two colours must give exactly two swatches");
        assert!((p.weight_sum() - 1.0).abs() < 1e-4, "weights sum to {}", p.weight_sum());
        for s in &p.0 {
            assert!((s.weight - 0.5).abs() < 0.05, "half the image is {}", s.weight);
        }
        let term = p.term();
        assert_eq!(term.len(), 2);
        assert!(term.iter().any(|(t, _)| t.starts_with("col:red")), "{term:?}");
        assert!(term.iter().any(|(t, _)| t.starts_with("col:blue")), "{term:?}");
    }

    #[test]
    fn degenerate_input_returns_none() {
        let img = solid(4, 4, (10, 20, 30));
        assert!(Palette::extract(&img, 0, 4, 4).is_none(), "zero width");
        assert!(Palette::extract(&img, 4, 0, 4).is_none(), "zero height");
        assert!(Palette::extract(&img, 4, 4, 0).is_none(), "k == 0");
        assert!(Palette::extract(&img, 40, 40, 4).is_none(), "buffer too small");
        assert!(Palette::extract(&[], 1, 1, 1).is_none(), "empty buffer");
        assert!(Palette::extract(&img, u32::MAX, u32::MAX, 4).is_none(), "overflowing dimension");
        // An exactly-sized buffer is accepted; the check is `>`, not `>=`.
        assert!(Palette::extract(&img, 4, 4, 4).is_some(), "exact buffer must be accepted");
    }

    #[test]
    fn k_is_clamped_to_the_mpeg7_bound() {
        let (w, h) = (32u32, 32u32);
        let mut img = Vec::new();
        for y in 0..h {
            for x in 0..w {
                img.extend_from_slice(&[(x * 8) as u8, (y * 8) as u8, ((x + y) * 4) as u8]);
            }
        }
        let p = Palette::extract(&img, w, h, 64).expect("extracts");
        assert!(p.0.len() <= K_MAX, "{} swatches exceeds K_MAX", p.0.len());
        assert!((5..=8).contains(&K_DEFAULT), "K_DEFAULT must sit in the MPEG-7 band");
        assert_eq!(Palette::extract_default(&img, w, h), Palette::extract(&img, w, h, K_DEFAULT));
    }

    #[test]
    fn cost_is_flat_in_input_resolution() {
        // Not a timing assertion — a structural one. The sample count is bounded by SAMPLE_EDGE²
        // no matter how large the input, which is the property the flat-cost claim rests on.
        let small = sample(&solid(10, 10, (1, 2, 3)), 10, 10).unwrap();
        let large = sample(&solid(400, 300, (1, 2, 3)), 400, 300).unwrap();
        assert_eq!(small.len(), 100);
        assert!(large.len() <= (SAMPLE_EDGE * SAMPLE_EDGE) as usize, "{}", large.len());
        assert!(large.len() >= 3000, "downsample threw away too much: {}", large.len());
    }

    #[test]
    fn every_bucket_index_is_in_range_and_names_uniquely() {
        let mut name = Vec::new();
        for bucket in 0..BUCKET_COUNT {
            name.push(term_of_bucket(bucket).expect("in range"));
        }
        assert_eq!(BUCKET_COUNT, 77);
        let mut sorted = name.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), BUCKET_COUNT, "two buckets share a term");
        assert!(term_of_bucket(BUCKET_COUNT).is_none());
    }

    #[test]
    fn bucket_of_is_total_over_the_srgb_cube() {
        for r in (0..=255u16).step_by(15) {
            for g in (0..=255u16).step_by(15) {
                for b in (0..=255u16).step_by(15) {
                    let (l, a, bb) = srgb_to_oklab(r as u8, g as u8, b as u8);
                    let bucket = bucket_of(&Swatch { l, a, b: bb, weight: 1.0 });
                    assert!(bucket < BUCKET_COUNT, "{r},{g},{b} → {bucket}");
                }
            }
        }
    }

    #[test]
    fn hue_sector_hold_the_srgb_primary_they_are_named_for() {
        let expect = [
            ((255u8, 0u8, 0u8), "red"),
            ((255, 255, 0), "yellow"),
            ((0, 255, 0), "green"),
            ((0, 0, 255), "blue"),
            ((255, 0, 255), "magenta"),
        ];
        for (c, want) in expect {
            let (l, a, b) = srgb_to_oklab(c.0, c.1, c.2);
            let bucket = bucket_of(&Swatch { l, a, b, weight: 1.0 });
            let sector = (bucket - NEUTRAL_BAND) % HUE_SECTOR;
            assert_eq!(HUE_NAME[sector], want, "{c:?} landed in {}", HUE_NAME[sector]);
        }
    }

    #[test]
    fn distance_is_zero_to_self_and_symmetric() {
        let red = Palette::extract(&solid(16, 16, (255, 0, 0)), 16, 16, K_DEFAULT).unwrap();
        let blue = Palette::extract(&solid(16, 16, (0, 0, 255)), 16, 16, K_DEFAULT).unwrap();
        let mixed =
            Palette::extract(&split(16, 16, (255, 0, 0), (0, 0, 255)), 16, 16, K_DEFAULT).unwrap();

        assert!(red.distance(&red).abs() < 1e-6);
        assert!((red.distance(&blue) - blue.distance(&red)).abs() < 1e-6, "asymmetric");
        assert!(red.distance(&blue) > red.distance(&mixed), "mixed is nearer red than blue is");

        let empty = Palette::default();
        assert_eq!(empty.distance(&Palette::default()), 0.0);
        assert!(empty.distance(&red).is_infinite());
        assert!(red.distance(&empty).is_infinite());
    }

    #[test]
    fn term_merges_swatch_that_share_a_bucket() {
        // Two centroids a hair apart necessarily share a bucket; the term must appear once with
        // the summed weight, not twice.
        let (l, a, b) = srgb_to_oklab(255, 0, 0);
        let p = Palette(vec![
            Swatch { l, a, b, weight: 0.6 },
            Swatch { l: l + 0.001, a, b, weight: 0.4 },
        ]);
        let term = p.term();
        assert_eq!(term.len(), 1, "{term:?}");
        assert!((term[0].1 - 1.0).abs() < 1e-5);
    }
}
