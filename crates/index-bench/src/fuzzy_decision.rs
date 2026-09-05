//! bench/roadmap/p2-fuzzy-decision.md (Tier 1, gate-excluded).
//!
//! Decides whether fuzzy search over the compressed FM-index is viable, and up to what k. Measures
//! k-mismatch backtracking latency (p50/p99) as k and alphabet size σ grow. The known theory:
//! backtracking is exponential in k, and large σ makes the top-of-trie branching explode — so we
//! expect k≤2 to be fine and k=3 to blow up, especially at large σ.
//!
//! Env: INDEX_BENCH_N (text length), INDEX_BENCH_SIGMA (alphabet size).

mod timer;

use index_core::data::gen_text;
use index_core::FmIndex;
use timer::{percentile, Clock};

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key).ok().and_then(|s| s.parse().ok()).unwrap_or(default)
}

fn main() {
    println!("p2-fuzzy-decision :: k-mismatch fuzzy search over the FM-index");
    let clock = Clock::new();

    let n = env_usize("INDEX_BENCH_N", 200_000);
    let sigma = env_usize("INDEX_BENCH_SIGMA", 26);
    let queries = env_usize("INDEX_BENCH_Q", 300);
    let node_cap = env_usize("INDEX_BENCH_CAP", 30_000_000) as u64;

    let text = gen_text(n, sigma, 0xF022_1234);
    let fm = FmIndex::new(&text);
    println!(
        "text n={n}  sigma={sigma}  index bits/char={:.2}  (clock {})\n",
        fm.bits_per_char(),
        clock.backend()
    );
    println!("  len   k    p50         p99        capped/Q   avg_matches");

    let mut k1_p99 = f64::NAN;
    let mut k2_p99 = f64::NAN;
    let mut k3_p99 = f64::NAN;

    for &len in &[8usize, 16] {
        for k in 0..=3usize {
            // Query patterns = random substrings of the text.
            let mut s = 0x1234u64;
            let mut pats: Vec<Vec<u8>> = Vec::with_capacity(queries);
            for _ in 0..queries {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
                let start = (s >> 33) as usize % (text.len() - len);
                pats.push(text[start..start + len].to_vec());
            }

            let mut lat = Vec::with_capacity(queries);
            let mut capped = 0usize;
            let mut total_matches = 0u64;
            for p in &pats {
                let mut matches = 0u64;
                let mut was_capped = false;
                let t = clock.measure_ns(|| {
                    let (set, c) = fm.fuzzy_kmismatch(p, k, node_cap);
                    matches = set.len() as u64;
                    was_capped = c;
                    matches
                });
                if was_capped {
                    capped += 1;
                }
                total_matches += matches;
                lat.push(t);
            }
            lat.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap());
            let p50 = percentile(&lat, 0.50);
            let p99 = percentile(&lat, 0.99);
            if len == 16 && k == 1 {
                k1_p99 = p99;
            }
            if len == 16 && k == 2 {
                k2_p99 = p99;
            }
            if len == 16 && k == 3 {
                k3_p99 = p99;
            }
            println!(
                "  {len:>3} {k:>4}  {:>9}  {:>10}  {capped:>5}/{queries}  {:>8.1}",
                fmt_ns(p50),
                fmt_ns(p99),
                total_matches as f64 / queries as f64
            );
        }
        println!();
    }

    // Decision per the spec (using len=16).
    println!("DECISION (len=16):");
    println!("  k=1 p99 {} (bar: <1ms)", fmt_ns(k1_p99));
    println!("  k=2 p99 {} (bar: <20ms)", fmt_ns(k2_p99));
    println!("  k=3 p99 {}", fmt_ns(k3_p99));
    let viable_k2 = k2_p99 < 100e6; // 100 ms
    let viable_k3 = k3_p99 < 1e9; // 1 s
    if viable_k2 && viable_k3 {
        println!("  -> fuzzy-over-FM viable through k=3 at sigma={sigma}");
    } else if viable_k2 {
        println!("  -> VIABLE ONLY FOR k<=2 at sigma={sigma}: k=3 exceeds the 1s bar (route k>=3 to q-gram filter)");
    } else {
        println!("  -> VIABLE ONLY FOR k<=1 at sigma={sigma}: k=2 already exceeds 100ms");
    }
}

fn fmt_ns(ns: f64) -> String {
    if ns >= 1e6 {
        format!("{:.2}ms", ns / 1e6)
    } else if ns >= 1e3 {
        format!("{:.1}us", ns / 1e3)
    } else {
        format!("{ns:.0}ns")
    }
}
