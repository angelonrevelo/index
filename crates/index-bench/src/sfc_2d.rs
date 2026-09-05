//! `sfc-2d` — the sanity half of `bench/roadmap/p3-sfc-dimensionality.md`, run.
//!
//! That spec was written to kill an idea: map D-dimensional vectors to one Hilbert key and scan a
//! window around the query key. It predicted collapse by D=32 and was filed as the experiment that
//! would settle the "one ordered key space for everything" thesis. It also states the harness's own
//! correctness condition:
//!
//! > *"the harness is correct if at D=2 recall@10 ≈ 1.0 (sanity — SFC works in 2D)"*
//!
//! **That sanity condition is the actual product question for geometry.** A map is D=2. The
//! rejection in `docs/roadmap-rejected.md` — "vectors as a physical ordered-key range scan" — was
//! specifically about high dimensions, where space-filling-curve locality collapses for
//! information-theoretic reasons. It says nothing against 2D, and S2 is the production proof that
//! 2D works.
//!
//! So this bin measures the D=2 case on its own terms, against the geometry workload maphy actually
//! has: **41,966 barangay polygons, 95,200 POIs** over the Philippine bounding box. If a Hilbert key
//! plus a small window recovers the true neighbours, then a viewport query is a *range scan over an
//! ordered key* — which is exactly the shape this engine's postings, block skipping and portable
//! format are already built for.
//!
//! What is measured:
//!   1. **recall@10** of a ±W window around the query key, W as a fraction of the dataset.
//!   2. **Window efficiency** — candidates examined per true hit. A curve that needs to scan half
//!      the dataset to find ten neighbours is not an index.
//!   3. **Viewport recall** — the real query. Given a rectangle, how many of the points inside it
//!      does a small number of key ranges recover? This is the operation a map performs on every
//!      pan and zoom, and it is not the same question as kNN.

mod timer;

/// Interleave (Morton/Z-order) is the cheap curve; Hilbert preserves locality better and is what
/// S2 and FlatGeobuf's packed R-tree ordering use. Both are measured, because "Hilbert is better"
/// is folklore until this prints a number.
#[inline]
fn morton_2d(x: u32, y: u32) -> u64 {
    #[inline]
    fn spread(mut v: u64) -> u64 {
        v &= 0x0000_0000_ffff_ffff;
        v = (v | (v << 16)) & 0x0000_ffff_0000_ffff;
        v = (v | (v << 8)) & 0x00ff_00ff_00ff_00ff;
        v = (v | (v << 4)) & 0x0f0f_0f0f_0f0f_0f0f;
        v = (v | (v << 2)) & 0x3333_3333_3333_3333;
        v = (v | (v << 1)) & 0x5555_5555_5555_5555;
        v
    }
    spread(x as u64) | (spread(y as u64) << 1)
}

/// Hilbert index of a point on a 2^order x 2^order grid, via the standard rotation walk.
#[inline]
fn hilbert_2d(order: u32, mut x: u32, mut y: u32) -> u64 {
    let mut rx: u32;
    let mut ry: u32;
    let mut d: u64 = 0;
    let mut s: u32 = 1 << (order - 1);
    while s > 0 {
        rx = u32::from((x & s) > 0);
        ry = u32::from((y & s) > 0);
        d += (s as u64) * (s as u64) * ((3 * rx) ^ ry) as u64;
        // Rotate the quadrant.
        if ry == 0 {
            if rx == 1 {
                x = s.wrapping_sub(1).wrapping_sub(x);
                y = s.wrapping_sub(1).wrapping_sub(y);
            }
            std::mem::swap(&mut x, &mut y);
        }
        s /= 2;
    }
    d
}

#[inline]
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The Philippine bounding box, which is the coordinate space maphy actually covers.
///
/// Widened from the earlier 116.9..126.6 to include Kalayaan (the westernmost real POI in maphy's
/// own data sits at 114.287 E). The synthetic and real datasets are then quantized on an IDENTICAL
/// grid, which is the only way the two rows of the results table mean the same thing. Clamping the
/// outliers instead would have piled real points onto the grid edge and quietly flattered locality.
const LON_MIN: f64 = 114.2;
const LON_MAX: f64 = 126.7;
const LAT_MIN: f64 = 4.5;
const LAT_MAX: f64 = 20.9;
const ORDER: u32 = 16; // 65,536 x 65,536 grid ~ 21 m of longitude at this span

#[inline]
fn quantize(lon: f64, lat: f64) -> (u32, u32) {
    let span = (1u32 << ORDER) - 1;
    let fx = ((lon - LON_MIN) / (LON_MAX - LON_MIN)).clamp(0.0, 1.0);
    let fy = ((lat - LAT_MIN) / (LAT_MAX - LAT_MIN)).clamp(0.0, 1.0);
    ((fx * span as f64) as u32, (fy * span as f64) as u32)
}

/// Points clustered the way settlement actually is — dense around a few centres, sparse between —
/// because a uniform scatter would flatter any curve. Cluster centres are the real coordinates of
/// the largest Philippine population centres.
fn generate(n: usize, seed: u64) -> Vec<(f64, f64)> {
    const CENTRE: &[(f64, f64, f64)] = &[
        (120.98, 14.60, 0.35), // Metro Manila
        (123.89, 10.32, 0.30), // Cebu
        (125.61, 7.07, 0.30),  // Davao
        (120.59, 16.41, 0.25), // Baguio / Ilocos
        (122.56, 10.70, 0.25), // Iloilo
        (121.05, 13.41, 0.40), // Southern Luzon spread
        (124.65, 8.48, 0.30),  // Cagayan de Oro
    ];
    let mut st = seed;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        // 80% clustered, 20% scattered nationwide — roughly the shape of a barangay centroid set.
        if splitmix64(&mut st) % 100 < 80 {
            let c = CENTRE[(splitmix64(&mut st) % CENTRE.len() as u64) as usize];
            let u1 = (splitmix64(&mut st) >> 11) as f64 / (1u64 << 53) as f64;
            let u2 = (splitmix64(&mut st) >> 11) as f64 / (1u64 << 53) as f64;
            // Box-Muller, so clusters are gaussian rather than square.
            let r = (-2.0 * (u1.max(1e-12)).ln()).sqrt();
            let theta = 2.0 * std::f64::consts::PI * u2;
            out.push((
                (c.0 + c.2 * r * theta.cos()).clamp(LON_MIN, LON_MAX),
                (c.1 + c.2 * r * theta.sin()).clamp(LAT_MIN, LAT_MAX),
            ));
        } else {
            let u1 = (splitmix64(&mut st) >> 11) as f64 / (1u64 << 53) as f64;
            let u2 = (splitmix64(&mut st) >> 11) as f64 / (1u64 << 53) as f64;
            out.push((
                LON_MIN + u1 * (LON_MAX - LON_MIN),
                LAT_MIN + u2 * (LAT_MAX - LAT_MIN),
            ));
        }
    }
    out
}

fn dist2(a: (f64, f64), b: (f64, f64)) -> f64 {
    let dx = a.0 - b.0;
    let dy = a.1 - b.1;
    dx * dx + dy * dy
}

struct Curve {
    name: &'static str,
    /// Key of a point on the full ORDER-bit grid.
    key: fn(u32, u32) -> u64,
    /// Key of a CELL on a `lvl`-bit grid. Needed because a cell's key range depends on where the
    /// cell sits in the curve at its own level, not on where its corner sits at full resolution.
    key_at: fn(u32, u32, u32) -> u64,
}

/// Maximum cells a viewport cover may use before it starts approximating upward. S2's default
/// `max_cells` is 8 for a region cover; this is deliberately more generous because the cost here is
/// a key-range scan rather than a network round trip, and the over-fetch column shows what the
/// budget buys.
const MAX_COVER_CELL: usize = 64;

/// Cover budgets swept, so the range-count / over-fetch trade is a measured curve rather than one
/// point chosen by taste.
const BUDGET_SWEEP: &[usize] = &[8, 16, 32, 64, 128, 512];

/// The contiguous key range occupied by a quadtree cell.
///
/// `lvl` is the number of significant bits of the cell coordinate; the cell covers
/// `2^(ORDER-lvl)` grid units per side, so `4^(ORDER-lvl)` keys.
#[inline]
fn cell_range(c: &Curve, lvl: u32, cx: u32, cy: u32) -> (u64, u64) {
    let below = ORDER - lvl;
    let span = 1u64 << (2 * below);
    let base = if lvl == 0 { 0 } else { (c.key_at)(lvl, cx, cy) } * span;
    (base, base + span - 1)
}

/// Cover the grid-aligned rectangle `[x0,x1] x [y0,y1]` with contiguous key ranges.
///
/// Recursive quadrant descent: a cell fully inside becomes a range, a cell fully outside is
/// dropped, a straddling cell is split. When the cell budget is exhausted the remaining straddling
/// cells are emitted whole, which trades over-fetch for a bounded number of ranges — the same
/// trade S2's `max_cells` makes.
fn cover(c: &Curve, x0: u32, y0: u32, x1: u32, y1: u32, budget: usize) -> Vec<(u64, u64)> {
    let mut out: Vec<(u64, u64)> = Vec::new();
    // (level, cell x, cell y)
    let mut stack: Vec<(u32, u32, u32)> = vec![(0, 0, 0)];
    while let Some((lvl, cx, cy)) = stack.pop() {
        let below = ORDER - lvl;
        let size = 1u32 << below;
        let (ax0, ay0) = (cx << below, cy << below);
        let (ax1, ay1) = (ax0.saturating_add(size - 1), ay0.saturating_add(size - 1));
        if ax1 < x0 || ax0 > x1 || ay1 < y0 || ay0 > y1 {
            continue; // disjoint
        }
        let inside = ax0 >= x0 && ax1 <= x1 && ay0 >= y0 && ay1 <= y1;
        if inside || below == 0 || out.len() + stack.len() >= budget {
            out.push(cell_range(c, lvl, cx, cy));
            continue;
        }
        for (dx, dy) in [(0u32, 0u32), (1, 0), (0, 1), (1, 1)] {
            stack.push((lvl + 1, cx * 2 + dx, cy * 2 + dy));
        }
    }
    out.sort_unstable();
    // Merge adjacent or overlapping ranges — the point of a cover is few scans, not many.
    let mut merged: Vec<(u64, u64)> = Vec::with_capacity(out.len());
    for r in out {
        match merged.last_mut() {
            Some(last) if r.0 <= last.1.saturating_add(1) => last.1 = last.1.max(r.1),
            _ => merged.push(r),
        }
    }
    merged
}

/// Load real coordinates from a `lon,lat,layer` CSV.
///
/// The fixture in `bench/fixture/maphy-poi.csv` is extracted verbatim from maphy's own
/// `apps/web/public/data/poi/*.geojson`: 50,412 schools, 2,070 hospitals, 1,209 ports, 24
/// volcanoes. `p9-sfc-2d.md` recorded "real coordinates" as an unmeasured gap; this closes it.
fn load_real(path: &str) -> std::io::Result<Vec<(f64, f64)>> {
    let text = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        let mut f = line.split(',');
        if let (Some(lon), Some(lat)) = (f.next(), f.next()) {
            if let (Ok(lon), Ok(lat)) = (lon.parse::<f64>(), lat.parse::<f64>()) {
                out.push((lon, lat));
            }
        }
    }
    Ok(out)
}

fn main() {
    let clock = timer::Clock::new();
    let n: usize = std::env::var("INDEX_BENCH_N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(95_200); // maphy's POI count

    // Real maphy coordinates when the fixture is available, synthetic clusters otherwise. The
    // point of the fixture is that a curve's locality is a property of the DATA, not of the curve,
    // so a synthetic distribution can only ever be a stand-in.
    let real_path = std::env::var("INDEX_BENCH_POI")
        .unwrap_or_else(|_| "bench/fixture/maphy-poi.csv".to_string());
    let (point, source) = match load_real(&real_path) {
        Ok(p) if !p.is_empty() => {
            let k = p.len();
            (p, format!("REAL maphy POI ({k} points, {real_path})"))
        }
        _ => (generate(n, 0xC0FFEE), format!("synthetic clusters ({n} points)")),
    };
    let n = point.len();
    println!("sfc-2d :: is a viewport a range scan over an ordered key?");
    println!("clock backend: {}", clock.backend());
    println!("  source: {source}");
    println!(
        "  {n} points over the Philippine bbox, {}-bit grid (~21 m cells)\n",
        ORDER * 2
    );

    let curve = [
        Curve {
            name: "morton/z-order",
            key: |x, y| morton_2d(x, y),
            key_at: |_lvl, x, y| morton_2d(x, y),
        },
        Curve {
            name: "hilbert",
            key: |x, y| hilbert_2d(ORDER, x, y),
            key_at: |lvl, x, y| hilbert_2d(lvl, x, y),
        },
    ];

    let mut st = 0x5EED_u64;
    let probe: Vec<(f64, f64)> =
        (0..300).map(|_| point[(splitmix64(&mut st) as usize) % point.len()]).collect();

    println!("  --- kNN: recall@10 of a +/-W window around the query key ---");
    println!("  {:<16} {:>8} {:>10} {:>14}", "curve", "W", "recall@10", "cand/true-hit");
    for c in &curve {
        let mut keyed: Vec<(u64, usize)> =
            point.iter().enumerate().map(|(i, &(lon, lat))| {
                let (x, y) = quantize(lon, lat);
                ((c.key)(x, y), i)
            }).collect();
        keyed.sort_unstable();
        let order_of: Vec<usize> = keyed.iter().map(|&(_, i)| i).collect();
        let mut rank_of = vec![0usize; point.len()];
        for (r, &i) in order_of.iter().enumerate() {
            rank_of[i] = r;
        }

        for &w_frac in &[0.001f64, 0.01] {
            let w = ((n as f64 * w_frac) as usize).max(1);
            let mut recall_sum = 0.0;
            let mut cand_sum = 0usize;
            for &q in &probe {
                // Ground truth: exact 10 nearest by brute force.
                let mut d: Vec<(f64, usize)> =
                    point.iter().enumerate().map(|(i, &p)| (dist2(q, p), i)).collect();
                d.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
                let truth: std::collections::HashSet<usize> =
                    d.iter().take(10).map(|&(_, i)| i).collect();

                let (qx, qy) = quantize(q.0, q.1);
                let qk = (c.key)(qx, qy);
                let at = keyed.partition_point(|&(k, _)| k < qk);
                let lo = at.saturating_sub(w);
                let hi = (at + w).min(order_of.len());
                let found = order_of[lo..hi].iter().filter(|i| truth.contains(i)).count();
                recall_sum += found as f64 / 10.0;
                cand_sum += hi - lo;
            }
            let recall = recall_sum / probe.len() as f64;
            println!(
                "  {:<16} {:>7.1}% {:>9.1}% {:>14.1}",
                c.name,
                w_frac * 100.0,
                recall * 100.0,
                cand_sum as f64 / (recall_sum * 10.0).max(1.0)
            );
        }
    }

    // The query a map actually issues.
    //
    // A rectangle is covered by recursively descending the quadtree: a cell entirely inside the
    // rectangle becomes one key range, a cell that straddles the boundary is split, and a cell
    // outside is dropped. This is what S2 does, and it is the only correct way to turn a rectangle
    // into key ranges.
    //
    // An earlier version of this bench SAMPLED the cell grid with a stride instead, and measured
    // 0.7% recall with 4,356 "ranges". A sampled cover is not a cover — it silently omits most of
    // the rectangle. The failure looked like a property of the curve and was a bug in the harness,
    // which is exactly the confusion a benchmark is supposed to prevent.
    //
    // The key property both curves have: a quadtree cell occupies a CONTIGUOUS run of keys. For a
    // cell at level `lvl` with cell-coordinates (cx, cy), the run is
    // `[cell_key * 4^(ORDER-lvl), cell_key * 4^(ORDER-lvl) + 4^(ORDER-lvl) - 1]`, because both
    // curves are defined recursively by quadrant.
    println!("\n  --- viewport: a rectangle, answered by contiguous key ranges ---");
    println!(
        "  {:<16} {:>9} {:>9} {:>8} {:>12} {:>10}",
        "curve", "viewport", "recall", "ranges", "over-fetch", "us/query"
    );
    for c in &curve {
        let mut keyed: Vec<(u64, usize)> =
            point.iter().enumerate().map(|(i, &(lon, lat))| {
                let (x, y) = quantize(lon, lat);
                ((c.key)(x, y), i)
            }).collect();
        keyed.sort_unstable();

        for &frac in &[0.01f64, 0.05] {
            let _ = MAX_COVER_CELL;
            let wx = (LON_MAX - LON_MIN) * frac;
            let wy = (LAT_MAX - LAT_MIN) * frac;
            let mut recall_sum = 0.0;
            let mut range_sum = 0usize;
            let mut fetched = 0usize;
            let mut truth_sum = 0usize;
            let mut probes = 0usize;
            let mut nanos = 0.0f64;

            for &q in probe.iter().take(60) {
                let (x0, x1) = (q.0 - wx / 2.0, q.0 + wx / 2.0);
                let (y0, y1) = (q.1 - wy / 2.0, q.1 + wy / 2.0);
                let truth: Vec<usize> = point
                    .iter()
                    .enumerate()
                    .filter(|(_, &(lon, lat))| lon >= x0 && lon <= x1 && lat >= y0 && lat <= y1)
                    .map(|(i, _)| i)
                    .collect();
                if truth.is_empty() {
                    continue;
                }
                probes += 1;
                truth_sum += truth.len();

                let (gx0, gy0) = quantize(x0, y0);
                let (gx1, gy1) = quantize(x1, y1);

                let t0 = std::time::Instant::now();
                let range = cover(c, gx0, gy0, gx1, gy1, MAX_COVER_CELL);
                let mut got = 0usize;
                for (a, b) in &range {
                    let lo = keyed.partition_point(|&(k, _)| k < *a);
                    let hi = keyed.partition_point(|&(k, _)| k <= *b);
                    fetched += hi - lo;
                    for &(_, i) in &keyed[lo..hi] {
                        let p = point[i];
                        if p.0 >= x0 && p.0 <= x1 && p.1 >= y0 && p.1 <= y1 {
                            got += 1;
                        }
                    }
                }
                nanos += t0.elapsed().as_nanos() as f64;
                range_sum += range.len();
                recall_sum += got as f64 / truth.len() as f64;
            }
            println!(
                "  {:<16} {:>8.0}% {:>8.1}% {:>8.0} {:>11.2}x {:>10.0}",
                c.name,
                frac * 100.0,
                100.0 * recall_sum / probes.max(1) as f64,
                range_sum as f64 / probes.max(1) as f64,
                fetched as f64 / truth_sum.max(1) as f64,
                nanos / probes.max(1) as f64 / 1000.0
            );
        }
    }

    // The cover budget is a real knob: fewer cells means fewer scans but coarser cells and more
    // over-fetch. Swept so the trade is a measured curve rather than one point chosen by taste.
    println!("\n  --- cover budget sweep (hilbert, 1% viewport) ---");
    println!("  {:>8} {:>9} {:>8} {:>12}", "budget", "recall", "ranges", "over-fetch");
    {
        let c = &curve[1];
        let mut keyed: Vec<(u64, usize)> = point
            .iter()
            .enumerate()
            .map(|(i, &(lon, lat))| {
                let (x, y) = quantize(lon, lat);
                ((c.key)(x, y), i)
            })
            .collect();
        keyed.sort_unstable();
        let frac = 0.01f64;
        let wx = (LON_MAX - LON_MIN) * frac;
        let wy = (LAT_MAX - LAT_MIN) * frac;
        for &budget in BUDGET_SWEEP {
            let mut recall_sum = 0.0;
            let mut range_sum = 0usize;
            let mut fetched = 0usize;
            let mut truth_sum = 0usize;
            let mut probes = 0usize;
            for &q in probe.iter().take(60) {
                let (x0, x1) = (q.0 - wx / 2.0, q.0 + wx / 2.0);
                let (y0, y1) = (q.1 - wy / 2.0, q.1 + wy / 2.0);
                let truth = point
                    .iter()
                    .filter(|&&(lon, lat)| lon >= x0 && lon <= x1 && lat >= y0 && lat <= y1)
                    .count();
                if truth == 0 {
                    continue;
                }
                probes += 1;
                truth_sum += truth;
                let (gx0, gy0) = quantize(x0, y0);
                let (gx1, gy1) = quantize(x1, y1);
                let range = cover(c, gx0, gy0, gx1, gy1, budget);
                range_sum += range.len();
                let mut got = 0usize;
                for (a, b) in &range {
                    let lo = keyed.partition_point(|&(k, _)| k < *a);
                    let hi = keyed.partition_point(|&(k, _)| k <= *b);
                    fetched += hi - lo;
                    for &(_, i) in &keyed[lo..hi] {
                        let pt = point[i];
                        if pt.0 >= x0 && pt.0 <= x1 && pt.1 >= y0 && pt.1 <= y1 {
                            got += 1;
                        }
                    }
                }
                recall_sum += got as f64 / truth as f64;
            }
            println!(
                "  {:>8} {:>8.1}% {:>8.0} {:>11.2}x",
                budget,
                100.0 * recall_sum / probes.max(1) as f64,
                range_sum as f64 / probes.max(1) as f64,
                fetched as f64 / truth_sum.max(1) as f64
            );
        }
    }

    println!("\n  Read: recall is what fraction of the points truly inside the rectangle a key-range");
    println!("  scan returns; over-fetch is candidates examined per true hit. An index is useful");
    println!("  when recall is ~100% and over-fetch is small — 1.0x would be a perfect index.");
}
