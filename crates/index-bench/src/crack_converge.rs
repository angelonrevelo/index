//! bench/roadmap/p1-cracking-convergence.md (Tier 1, gate-excluded).
//!
//! Runs three workloads against naive and stochastic cracking and checks:
//!   (a) random:      the index converges — late queries >= 5x faster than early ones.
//!   (b) sequential:  cumulative time never exceeds a full-scan baseline by > 1.2x.
//!   (c) ends:        same "never worse than scan" bound under adversarial end-hammering.
//!
//! Naive cracking is expected to *fail* the "never worse than scan" bound on the sequential
//! workload (it does ~O(n) work per query, but with higher constants than a plain scan), while
//! stochastic cracking passes — the red→green that justifies shipping stochastic, not naive.

mod timer;

use index_core::data::gen_uniform;
use index_core::CrackerColumn;
use timer::Clock;

const QUERIES: usize = 3000;

fn splitmix(s: &mut u64) -> u64 {
    *s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *s;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn shuffled(n: usize, seed: u64) -> Vec<u64> {
    let mut v = gen_uniform(n, seed);
    let mut s = seed | 1;
    for i in (1..v.len()).rev() {
        let j = (splitmix(&mut s) as usize) % (i + 1);
        v.swap(i, j);
    }
    v
}

fn median(xs: &[f64]) -> f64 {
    let mut v = xs.to_vec();
    v.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

/// (lo, hi) query lists for each workload, over value span `span`.
fn workload(kind: &str, span: u64, q: usize) -> Vec<(u64, u64)> {
    let mut out = Vec::with_capacity(q);
    match kind {
        // Random ranges spread across the value space — cracking's friendly case.
        "random" => {
            let mut s = 0xBEEFu64;
            for _ in 0..q {
                let a = splitmix(&mut s) % span;
                let w = 1 + splitmix(&mut s) % 1000;
                out.push((a, (a + w).min(span)));
            }
        }
        // Consecutive small ranges marching forward — naive cracking's worst case (the big
        // remaining piece is re-partitioned on every query).
        "sequential" => {
            let w = 50u64;
            for i in 0..q as u64 {
                let a = (i * w) % span;
                out.push((a, a + w));
            }
        }
        // Tiny ranges hammering the two ends alternately — adversarial for adaptivity.
        "ends" => {
            let t = 100u64;
            for i in 0..q as u64 {
                if i % 2 == 0 {
                    out.push((0, t));
                } else {
                    out.push((span.saturating_sub(t), span));
                }
            }
        }
        _ => unreachable!(),
    }
    out
}

fn full_scan_ns(clock: &Clock, data: &[u64], span: u64) -> f64 {
    // Median scan cost over a few representative ranges.
    let mut samples = Vec::new();
    for k in 1..=5u64 {
        let lo = (span / 6) * k;
        let hi = lo + 500;
        samples.push(clock.measure_ns(|| {
            data.iter().filter(|&&x| x >= lo && x < hi).count() as u64
        }));
    }
    median(&samples)
}

fn run(clock: &Clock, kind: &str, stochastic: bool, base: &[u64], span: u64, scan_total: f64) -> bool {
    let queries = workload(kind, span, QUERIES);
    let mut col = CrackerColumn::new(base.to_vec(), stochastic, 99);
    let mut times = Vec::with_capacity(QUERIES);
    for &(lo, hi) in &queries {
        times.push(clock.measure_ns(|| col.query(lo, hi).len() as u64));
    }
    let cumulative: f64 = times.iter().sum();
    let first = median(&times[..10]);
    let last = median(&times[times.len() - 10..]);
    let speedup = if last > 0.0 { first / last } else { f64::INFINITY };
    let scan_ratio = cumulative / scan_total;

    let pass = match kind {
        "random" => speedup >= 5.0,
        _ => scan_ratio <= 1.2,
    };
    let mode = if stochastic { "stochastic" } else { "naive     " };
    println!(
        "  {kind:<11} {mode}  first={first:>8.0}ns last={last:>8.0}ns  speedup={speedup:>5.1}x  \
         cum/scan={scan_ratio:>4.2}x  pieces={:>6}  -> {}",
        col.piece_count(),
        if pass { "PASS" } else { "FAIL" }
    );
    // Where the time actually goes: a cracked query is dominated by element movement, so the
    // per-query move count is the number to watch, not the wall clock alone.
    let stat = col.stat();
    println!(
        "                          moved/query={:>8.0}  sorted/query={:>6.0}  \
         sorted-piece cracks={:>5}  index inserts={}",
        stat.partition_element as f64 / QUERIES as f64,
        stat.sort_element as f64 / QUERIES as f64,
        stat.sorted_hit,
        stat.index_insert
    );
    pass
}

fn main() {
    println!("p1-cracking-convergence :: naive vs stochastic database cracking");
    let clock = Clock::new();
    println!("clock backend: {} ({:.3} cycles/ns)", clock.backend(), clock.cycles_per_ns());

    let n: usize = std::env::var("INDEX_BENCH_N").ok().and_then(|s| s.parse().ok()).unwrap_or(1_000_000);
    let span = (n as u64) * 8;
    let base = shuffled(n, 0xC0FFEE);
    let scan_each = full_scan_ns(&clock, &base, span);
    let scan_total = scan_each * QUERIES as f64;
    println!(
        "n={n}  queries={QUERIES}  full-scan/query={scan_each:.0}ns  scan-baseline-total={:.1}ms\n",
        scan_total / 1e6
    );

    let mut all = true;
    for kind in ["random", "sequential", "ends"] {
        for stochastic in [false, true] {
            all &= run(&clock, kind, stochastic, &base, span, scan_total);
        }
        println!();
    }
    println!(
        "OVERALL: {}",
        if all { "PASS" } else { "MIXED — see per-row (naive failures are expected/the point)" }
    );
}
