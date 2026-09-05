//! bench/roadmap/p0-beat-btreemap.md (Tier 1, gate-excluded).
//!
//! bytes/key + p50/**p99** lookup latency for the learned indexes vs `BTreeMap`, using the
//! `rdtsc` clock (see `timer.rs`). p99 on a loaded desktop is noisy (±~100 ns), so each number is
//! the **median p99 over `REPS` passes** — a single pass can't decide the close cases.
//!
//! Usage:
//!   beat-btreemap                      # synthetic distributions (incl. a PLA-hostile one)
//!   beat-btreemap path/to/sosd_u64 ... # real SOSD binary files (u64 LE count + keys)
//!   INDEX_BENCH_N=10000000 beat-btreemap   # override synthetic size

mod timer;

use std::collections::BTreeMap;
use std::path::Path;

use index_core::data::{
    gen_hard, gen_lognormal, gen_queries, gen_sequential, gen_uniform, load_sosd_u64,
};
use index_core::{PgmIndex, PlaIndex};

use timer::{percentile, Clock};

const QUERIES: usize = 200_000;
const REPS: usize = 5;

/// Median (p50, p99) in ns over REPS timing passes — robust to desktop scheduling noise.
fn p50_p99(clock: &Clock, queries: &[u64], search: impl Fn(u64) -> u64) -> (f64, f64) {
    let mut p50s = Vec::with_capacity(REPS);
    let mut p99s = Vec::with_capacity(REPS);
    for _ in 0..REPS {
        let lat = clock.time_each(queries.len(), |i| search(queries[i % queries.len()]));
        p50s.push(percentile(&lat, 0.50));
        p99s.push(percentile(&lat, 0.99));
    }
    p50s.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap());
    p99s.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap());
    (p50s[REPS / 2], p99s[REPS / 2])
}

fn run_dataset(clock: &Clock, name: &str, keys: &[u64], epsilon: usize) -> bool {
    let pla = PlaIndex::build(keys, epsilon); // single-level: binary search over segments
    let pgm = PgmIndex::build(keys, epsilon); // recursive top layer
    let btree: BTreeMap<u64, u64> =
        keys.iter().enumerate().map(|(i, &k)| (k, i as u64)).collect();
    let queries = gen_queries(keys, QUERIES, 0x1234_5678);

    // Warmup.
    for &q in queries.iter().take(20_000) {
        std::hint::black_box(pla.search(q));
        std::hint::black_box(pgm.search(q));
        std::hint::black_box(btree.get(&q));
    }

    let (pla_p50, pla_p99) =
        p50_p99(clock, &queries, |q| pla.search(q).pos.map(|p| p as u64).unwrap_or(u64::MAX));
    let (pgm_p50, pgm_p99) =
        p50_p99(clock, &queries, |q| pgm.search(q).pos.map(|p| p as u64).unwrap_or(u64::MAX));
    let (bt_p50, bt_p99) = p50_p99(clock, &queries, |q| *btree.get(&q).unwrap_or(&u64::MAX));

    let pgm_bpk = pgm.index_bytes_per_key();

    println!("\n[{name}]  n={}  eps={epsilon}  (median p50/p99 over {REPS} passes)", keys.len());
    println!(
        "  pgm: leaf_segments={}  height={}  bytes/key={pgm_bpk:.4}   (BTreeMap≈ 32-48)",
        pgm.leaf_segment_count(),
        pgm.height()
    );
    println!("  p50 ns : PLA={pla_p50:>7.1}  PGM={pgm_p50:>7.1}  BTree={bt_p50:>7.1}");
    println!("  p99 ns : PLA={pla_p99:>7.1}  PGM={pgm_p99:>7.1}  BTree={bt_p99:>7.1}");

    // The best learned variant at this scale is what we report against the baseline.
    let best_p50 = pla_p50.min(pgm_p50);
    let best_p99 = pla_p99.min(pgm_p99);
    let win_space = pgm_bpk < 32.0;
    let win_p50 = best_p50 <= bt_p50;
    let win_p99 = best_p99 <= bt_p99;
    println!(
        "  -> space: {}   p50: {}   p99: {}",
        if win_space { "PASS" } else { "FAIL" },
        if win_p50 { "PASS" } else { "FAIL" },
        if win_p99 { "PASS" } else { "FAIL" }
    );
    win_space && win_p50 && win_p99
}

fn main() {
    println!("p0-beat-btreemap :: learned index (PLA / PGM) vs BTreeMap");
    let clock = Clock::new();
    println!(
        "clock backend: {} ({:.3} cycles/ns, overhead {} cycles)",
        clock.backend(),
        clock.cycles_per_ns(),
        clock.overhead_cycles()
    );

    // Default ε=16: the sweep showed ε=64 loses p99 on irregular data at 10M (last-mile cache
    // misses), while ε≈8-16 keeps the last-mile window cache-resident and wins everywhere.
    let epsilon: usize = std::env::var("INDEX_BENCH_EPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(16);

    let args: Vec<String> = std::env::args().skip(1).collect();
    let datasets: Vec<(String, Vec<u64>)> = if args.is_empty() {
        let n: usize = std::env::var("INDEX_BENCH_N")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1_000_000);
        vec![
            ("sequential".into(), gen_sequential(n)),
            ("uniform".into(), gen_uniform(n, 0xC0FFEE)),
            ("lognormal".into(), gen_lognormal(n, 0xBADF00D)),
            ("hard".into(), gen_hard(n, 0xF00DBABE)),
        ]
    } else {
        args.iter()
            .filter_map(|p| match load_sosd_u64(p) {
                Ok(keys) => Some((
                    Path::new(p)
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| p.clone()),
                    keys,
                )),
                Err(e) => {
                    eprintln!("skip {p}: {e}");
                    None
                }
            })
            .collect()
    };

    let mut all = true;
    for (name, keys) in &datasets {
        all &= run_dataset(&clock, name, keys, epsilon);
    }
    println!("\nOVERALL: {}", if all { "PASS on all metrics & distributions" } else { "MIXED — see per-dataset" });
}
