//! Deterministic, dependency-free data generation + SOSD loading.
//!
//! Real SOSD datasets (200M × u64) aren't downloadable in every environment, so we generate
//! distribution stand-ins (sequential / uniform-dense / lognormal) with a seeded `splitmix64`
//! RNG — fully reproducible, no `rand` crate. When a real SOSD binary file is available,
//! [`load_sosd_u64`] reads it (header: `u64` LE count, then `count` × `u64` LE keys).

use std::io::Read;
use std::path::Path;

/// splitmix64 — tiny, fast, deterministic. Same seed → same stream, on every platform.
#[inline]
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[inline]
fn next_f64_01(state: &mut u64) -> f64 {
    // 53-bit mantissa in [0, 1).
    (splitmix64(state) >> 11) as f64 / (1u64 << 53) as f64
}

/// Sort ascending and drop duplicates — used for externally-sourced (SOSD) keys.
fn sort_unique(mut v: Vec<u64>) -> Vec<u64> {
    v.sort_unstable();
    v.dedup();
    v
}

/// Sort, then bump any duplicate up to `prev+1` so the result is strictly increasing — exactly
/// `n` keys, no dedup-retry loop (which blows up for skewed distributions at large `n`).
fn enforce_strictly_increasing(v: &mut [u64]) {
    v.sort_unstable();
    let mut prev: Option<u64> = None;
    for x in v.iter_mut() {
        let lo = prev.map_or(0, |p| p.saturating_add(1));
        if *x < lo {
            *x = lo;
        }
        prev = Some(*x);
    }
}

/// `0, 1, 2, ... n-1` — the friendliest possible CDF (a perfect line).
pub fn gen_sequential(n: usize) -> Vec<u64> {
    (0..n as u64).collect()
}

/// Uniform-dense keys over a range ~`8n`, made strictly increasing. O(n log n), exactly `n` keys.
pub fn gen_uniform(n: usize, seed: u64) -> Vec<u64> {
    let mut s = seed ^ 0xA5A5_A5A5_A5A5_A5A5;
    let span = (n as u64).saturating_mul(8).max(16);
    let mut v: Vec<u64> = (0..n).map(|_| splitmix64(&mut s) % span).collect();
    enforce_strictly_increasing(&mut v);
    v
}

/// Lognormal-valued keys (Box–Muller normal → exp), the classic "irregular CDF" stress case.
/// O(n log n), exactly `n` keys — duplicates from the dense low end are bumped to consecutive
/// integers, leaving dense runs punctuated by tail jumps (a realistic PLA stressor).
pub fn gen_lognormal(n: usize, seed: u64) -> Vec<u64> {
    let mut s = seed ^ 0x5DEE_CE66_D0D0_0D1E;
    let mut v: Vec<u64> = (0..n)
        .map(|_| {
            // Box–Muller: two uniforms → one standard normal.
            let u1 = next_f64_01(&mut s).max(1e-12);
            let u2 = next_f64_01(&mut s);
            let z = (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos();
            ((z * 2.0).exp() * 1_000.0) as u64 // sigma = 2
        })
        .collect();
    enforce_strictly_increasing(&mut v);
    v
}

/// A PLA-hostile distribution: the local density (gap between consecutive keys) changes every
/// block, so a single line can't cover much — this forces many segments and stresses the
/// optimal-vs-greedy difference, standing in for irregular real data (SOSD `osmc`/`wiki`).
/// Strictly increasing by construction (each step ≥ 1), so it's already sorted & unique.
pub fn gen_hard(n: usize, seed: u64) -> Vec<u64> {
    let mut s = seed ^ 0x1357_9BDF_2468_ACE0;
    let mut cur: u64 = 0;
    let mut step: u64 = 1;
    let mut v = Vec::with_capacity(n);
    for i in 0..n {
        if i % 1024 == 0 {
            step = 1 + splitmix64(&mut s) % 4096; // new density regime each block
        }
        cur = cur.wrapping_add(1 + splitmix64(&mut s) % step);
        v.push(cur);
    }
    v
}

/// Random text of `n` bytes over `alphabet` distinct symbols (bytes in `[1, alphabet]`, so 0 is
/// free to use as an FM-index sentinel). Larger `alphabet` → worse fuzzy-search branching.
pub fn gen_text(n: usize, alphabet: usize, seed: u64) -> Vec<u8> {
    let a = alphabet.clamp(2, 255) as u64;
    let mut s = seed | 1;
    (0..n).map(|_| (1 + splitmix64(&mut s) % a) as u8).collect()
}

/// Pick `m` query keys drawn (with repetition) from existing keys — a realistic point-lookup set.
pub fn gen_queries(keys: &[u64], m: usize, seed: u64) -> Vec<u64> {
    let mut s = seed ^ 0xDEAD_BEEF_CAFE_F00D;
    let n = keys.len().max(1);
    (0..m).map(|_| keys[(splitmix64(&mut s) % n as u64) as usize]).collect()
}

/// Load a SOSD-format binary file: `u64` LE element count, then that many `u64` LE keys.
/// Returns them sorted & unique (SOSD ships sorted; we enforce the build invariant anyway).
pub fn load_sosd_u64<P: AsRef<Path>>(path: P) -> std::io::Result<Vec<u64>> {
    let mut f = std::fs::File::open(path)?;
    let mut hdr = [0u8; 8];
    f.read_exact(&mut hdr)?;
    let count = u64::from_le_bytes(hdr) as usize;
    let mut buf = vec![0u8; count * 8];
    f.read_exact(&mut buf)?;
    let mut keys = Vec::with_capacity(count);
    for chunk in buf.chunks_exact(8) {
        keys.push(u64::from_le_bytes(chunk.try_into().unwrap()));
    }
    Ok(sort_unique(keys))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generators_are_sorted_unique_and_sized() {
        for keys in [gen_sequential(5000), gen_uniform(5000, 1), gen_lognormal(5000, 2)] {
            assert_eq!(keys.len(), 5000);
            assert!(keys.windows(2).all(|w| w[0] < w[1]));
        }
    }

    #[test]
    fn generators_are_deterministic() {
        assert_eq!(gen_uniform(2000, 99), gen_uniform(2000, 99));
        assert_eq!(gen_lognormal(2000, 99), gen_lognormal(2000, 99));
    }
}
