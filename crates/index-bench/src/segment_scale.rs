//! p38-segment-scale :: what does incremental updating actually cost?
//!
//! `p37` made live updates reachable from every host and closed with the limit that matters:
//!
//! > **Not measured at scale.** Every test is a handful of documents proving semantics. What a
//! > 50-segment collection costs at a million rows is unmeasured.
//!
//! Two costs, and the second is the one nobody measures:
//!
//! 1. **Latency.** A query runs against every segment, so cost should grow with segment count.
//! 2. **Ranking drift.** IDF and length norms are PER SEGMENT. A term rare overall but common
//!    inside a small delta scores differently there. `searcher.rs` has documented this as "the
//!    standard cost of segmented search" since before this session — but *how much* it actually
//!    moves the answer on real data has never been measured. An honest number here decides whether
//!    an application should append or rebuild.
//!
//! The oracle is a single monolithic `Index` over exactly the same rows.

use index_text::{Doc, Field, Index, IndexBuilder, Schema, Searcher};
use std::path::PathBuf;

mod timer;

/// Segment counts to sweep. 1 is the monolithic control.
const LADDER: [usize; 6] = [1, 2, 5, 10, 25, 50];

struct Product {
    name: String,
    brand: String,
}

fn corpus_dir() -> PathBuf {
    std::env::var("INDEX_CORPUS_DIR").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("..")
    })
}

fn split_csv(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut it = line.chars().peekable();
    while let Some(c) = it.next() {
        match (c, quoted) {
            ('"', true) if it.peek() == Some(&'"') => {
                cur.push('"');
                it.next();
            }
            ('"', _) => quoted = !quoted,
            (',', false) => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

fn load() -> Option<Vec<Product>> {
    let path = corpus_dir()
        .join("presyo")
        .join("data")
        .join("endless-prep")
        .join("catalog-active.csv");
    let text = std::fs::read_to_string(path).ok()?;
    let mut line = text.lines();
    let head = split_csv(line.next()?);
    let at = |k: &str| head.iter().position(|h| h == k);
    let (i_name, i_brand) = (at("product_name")?, at("brand_name")?);
    let mut out = Vec::new();
    for l in line {
        let f = split_csv(l);
        if f.len() <= i_brand {
            continue;
        }
        let name = f[i_name].trim().to_string();
        if name.is_empty() {
            continue;
        }
        out.push(Product { name, brand: f[i_brand].trim().to_string() });
    }
    Some(out)
}

fn schema() -> Schema {
    Schema::new(vec![Field::new("name", 3.0, 0.5), Field::new("brand", 0.5, 0.75)])
}

/// Build one index over `row`.
fn build(row: &[Product]) -> Index {
    let mut b = IndexBuilder::new(schema());
    for p in row {
        b.add(&Doc::new(vec![p.name.as_str(), p.brand.as_str()]));
    }
    b.build().unwrap()
}

/// Split into `n` segments **in corpus order**, so document ordinals are identical to the
/// monolithic build and the two can be compared document for document.
///
/// Sizes are deliberately uneven: a large base plus small deltas, which is what an application
/// that indexes once and appends daily actually produces. Equal segments would be a friendlier
/// shape than reality.
fn segmented(row: &[Product], n: usize) -> Searcher {
    if n <= 1 {
        return Searcher::new(build(row));
    }
    // 60 % base, the remaining 40 % split evenly across n-1 deltas.
    let base = row.len() * 6 / 10;
    let rest = row.len() - base;
    let each = rest.div_ceil(n - 1);
    let mut s = Searcher::new(build(&row[..base]));
    let mut at = base;
    while at < row.len() {
        let end = (at + each).min(row.len());
        s.push(build(&row[at..end]));
        at = end;
    }
    s
}

fn main() {
    let clock = timer::Clock::new();
    println!("p38-segment-scale :: what incremental updating costs, on presyo's real catalogue");
    println!("clock backend: {}\n", clock.backend());

    let Some(product) = load() else {
        println!("SKIP: presyo catalogue not found. Set INDEX_CORPUS_DIR.");
        println!("OVERALL: NO CORPUS — refusing to report a verdict.");
        std::process::exit(2);
    };
    println!("  {} real products", product.len());

    // Queries a shopper types: the first word of a real product name.
    let mut probe: Vec<String> = Vec::new();
    let step = (product.len() / 500).max(1);
    for p in product.iter().step_by(step) {
        if let Some(w) = p.name.split_whitespace().next() {
            if w.len() >= 4 {
                probe.push(w.to_string());
            }
        }
        if probe.len() >= 500 {
            break;
        }
    }
    // A typo arm, because typo queries touch more postings and should feel segmentation harder.
    let dirty: Vec<String> = probe
        .iter()
        .map(|q| {
            let mut c: Vec<char> = q.chars().collect();
            if c.len() > 4 {
                c[3] = 'x';
            }
            c.into_iter().collect()
        })
        .collect();
    // A SELECTIVE arm: whole product names. The broad arm above is a single first word, which
    // matches thousands of near-identical products whose scores are nearly tied -- and among ties
    // the top-10 is arbitrary, so any scoring change reshuffles it. Measuring only that would blame
    // segmentation for instability the query set already had.
    let mut exact: Vec<String> = Vec::new();
    for p in product.iter().step_by(step) {
        exact.push(p.name.clone());
        if exact.len() >= 500 {
            break;
        }
    }
    println!(
        "  {} broad queries (one word), {} selective (whole product name), {} with a typo\n",
        probe.len(),
        exact.len(),
        dirty.len()
    );

    // ---- The oracle: one index over the same rows, same ordinals ----------------------------
    let mono = build(&product);
    let truth: Vec<Vec<u32>> =
        probe.iter().map(|q| mono.search(q, 10).iter().map(|h| h.doc).collect()).collect();
    let truth_exact: Vec<Vec<u32>> =
        exact.iter().map(|q| mono.search(q, 10).iter().map(|h| h.doc).collect()).collect();

    // How tied is the broad arm, really? If the 10th and 1st scores are within a hair, the ordering
    // was never stable and segmentation is not what made it unstable.
    let mut tied = 0usize;
    for q in probe.iter() {
        let h = mono.search(q, 10);
        if h.len() >= 2 {
            let (top, last) = (h[0].score, h[h.len() - 1].score);
            if (top - last).abs() <= 0.05 * top.abs().max(1.0) {
                tied += 1;
            }
        }
    }
    println!(
        "  broad arm: {:.1}% of queries have a top-10 whose scores span under 5% -- near-ties\n",
        100.0 * tied as f64 / probe.len() as f64
    );

    println!(
        "  {:>6}  {:>9}  {:>9}  {:>9}  {:>12}  {:>12}  {:>12}",
        "segs", "build s", "p50", "p99", "broad overlap", "sel rank-1", "sel overlap"
    );
    println!("  {}", "-".repeat(70));

    let mut fail = 0usize;
    // p52: what collection-wide statistics cost, measured INTERLEAVED in one process.
    //
    // Comparing against a number from a previous run is not a measurement on this machine: the
    // one-segment p50 alone swung 15 -> 30 us between runs while the quality columns stayed
    // bit-identical, because another build was running. So both arms are timed back to back on the
    // same collection, which is the method `p27` and `p46` already use here.
    let mut ab: Vec<(usize, f64, f64, f64, f64)> = Vec::new();
    for &n in &LADDER {
        let t0 = std::time::Instant::now();
        let mut s = segmented(&product, n);
        let build_s = t0.elapsed().as_secs_f64();
        assert_eq!(s.doc_count(), product.len(), "segmentation must not lose documents");

        if n > 1 {
            s.set_collection_stat(false);
            let mut off = clock.time_each(probe.len(), |i| s.search(&probe[i], 10).len() as u64);
            off.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let mut off_t = clock.time_each(dirty.len(), |i| s.search(&dirty[i], 10).len() as u64);
            off_t.sort_by(|a, b| a.partial_cmp(b).unwrap());
            s.set_collection_stat(true);
            let mut on = clock.time_each(probe.len(), |i| s.search(&probe[i], 10).len() as u64);
            on.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let mut on_t = clock.time_each(dirty.len(), |i| s.search(&dirty[i], 10).len() as u64);
            on_t.sort_by(|a, b| a.partial_cmp(b).unwrap());
            ab.push((
                n,
                timer::percentile(&off, 0.50) / 1000.0,
                timer::percentile(&on, 0.50) / 1000.0,
                timer::percentile(&off_t, 0.99) / 1000.0,
                timer::percentile(&on_t, 0.99) / 1000.0,
            ));
        }

        let mut e = clock.time_each(probe.len(), |i| s.search(&probe[i], 10).len() as u64);
        e.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let mut t = clock.time_each(dirty.len(), |i| s.search(&dirty[i], 10).len() as u64);
        t.sort_by(|a, b| a.partial_cmp(b).unwrap());

        // Agreement against the monolithic index, document for document.
        let mut same = 0usize;
        let mut overlap = 0.0f64;
        for (i, q) in probe.iter().enumerate() {
            let got: Vec<u32> = s.search(q, 10).iter().map(|h| h.doc).collect();
            if got == truth[i] {
                same += 1;
            }
            // SET overlap, not order. This is what separates "the same documents, reordered" from
            // "different documents" -- two failures with completely different consequences, and a
            // number that only counts exact sequence equality cannot tell them apart.
            if !truth[i].is_empty() {
                let hit = truth[i].iter().filter(|d| got.contains(d)).count();
                overlap += hit as f64 / truth[i].len() as f64;
            }
        }
        // The selective arm, scored the same way.
        // Three numbers, because exact sequence equality alone is the WRONG bar and saying so
        // needs the other two beside it. The diagnostic showed the disagreements are adjacent swaps
        // between documents whose scores differ in the third decimal -- the same documents, in a
        // slightly different order. A shopper notices a missing product; they do not notice ranks 2
        // and 3 trading places.
        let mut same_exact = 0usize;
        let mut first_exact = 0usize;
        let mut overlap_exact = 0.0f64;
        for (i, q) in exact.iter().enumerate() {
            let got: Vec<u32> = s.search(q, 10).iter().map(|h| h.doc).collect();
            if got == truth_exact[i] {
                same_exact += 1;
            }
            if got.first() == truth_exact[i].first() {
                first_exact += 1;
            }
            if !truth_exact[i].is_empty() {
                let hit = truth_exact[i].iter().filter(|d| got.contains(d)).count();
                overlap_exact += hit as f64 / truth_exact[i].len() as f64;
            }
        }
        // Why do they differ? Print the two rankings side by side for the first few selective
        // queries that disagree. A percentage says something is wrong; only the triples say what.
        if std::env::var("INDEX_SEG_DIAG").is_ok() && n == 2 {
            let mut shown = 0;
            for (i, q) in exact.iter().enumerate() {
                let got = s.search(q, 10);
                let want = mono.search(q, 10);
                let gd: Vec<u32> = got.iter().map(|h| h.doc).collect();
                if gd == truth_exact[i] || shown >= 3 {
                    continue;
                }
                shown += 1;
                println!("\n    ---- disagreement {shown}: {q:?}");
                println!(
                    "    {:<5} {:>8} {:>7} {:>10}   |   {:<5} {:>8} {:>7} {:>10}",
                    "seg", "doc", "bucket", "score", "mono", "doc", "bucket", "score"
                );
                for r in 0..got.len().max(want.len()).min(4) {
                    let g = got.get(r).map_or("      -        -          -".to_string(), |h| {
                        format!("{:>8} {:>7} {:>10.4}", h.doc, h.typo_bucket, h.score)
                    });
                    let w = want.get(r).map_or("      -        -          -".to_string(), |h| {
                        format!("{:>8} {:>7} {:>10.4}", h.doc, h.typo_bucket, h.score)
                    });
                    println!("    {:<5} {}   |   {:<5} {}", r + 1, g, r + 1, w);
                }
            }
        }

        let pct = |x: usize| 100.0 * x as f64 / probe.len() as f64;
        let us = |v: &[f64], p: f64| timer::percentile(v, p) / 1000.0;
        println!(
            "  {:>6}  {:>9.2}  {:>7.0}us  {:>7.0}us  {:>11.2}%  {:>11.2}%  {:>11.2}%",
            n,
            build_s,
            us(&e, 0.5),
            us(&t, 0.99),
            100.0 * overlap / probe.len() as f64,
            100.0 * first_exact as f64 / exact.len() as f64,
            100.0 * overlap_exact / exact.len() as f64
        );
        if n == 1 && same_exact != exact.len() {
            println!("    CONTROL FAILED: one segment must equal the monolithic index exactly");
            fail += 1;
        }
        if n == 1 && (pct(same) - 100.0).abs() > f64::EPSILON {
            println!("    CONTROL FAILED: one segment must equal the monolithic index exactly");
            fail += 1;
        }
    }

    // p52: the cost of collection-wide statistics, both arms timed on the same collection.
    if !ab.is_empty() {
        println!(
            "\n  --- p52: what collection-wide statistics cost (interleaved, same process) ---"
        );
        println!(
            "  {:>6}  {:>11}  {:>11}  {:>7}  {:>13}  {:>13}  {:>7}",
            "segs", "p50 off", "p50 on", "x", "typo p99 off", "typo p99 on", "x"
        );
        for (n, o50, n50, o99, n99) in &ab {
            println!(
                "  {:>6}  {:>9.0}us  {:>9.0}us  {:>6.2}x  {:>11.0}us  {:>11.0}us  {:>6.2}x",
                n, o50, n50, n50 / o50.max(0.001), o99, n99, n99 / o99.max(0.001)
            );
        }
        println!(
            "  A second dictionary expansion per segment is the price of knowing a term's"
        );
        println!(
            "  corpus-wide document frequency at all. `Searcher::set_collection_stat(false)`"
        );
        println!("  trades the parity back for the latency.");
    }

    println!("\n  Read: all columns compare against a single index over identical rows.");
    println!("  BROAD is one word matching thousands of near-tied products; SELECTIVE is a whole");
    println!("  product name. Overlap is SET agreement -- did the same documents come back -- and");
    println!("  rank-1 is what a shopper sees first. Exact sequence equality is deliberately NOT");
    println!("  reported as the headline: the disagreements are adjacent swaps between documents");
    println!("  whose scores differ in the third decimal (run with INDEX_SEG_DIAG=1 to see them).");
    println!("\nOVERALL: {}", if fail == 0 { "PASS" } else { "FAIL" });
    std::process::exit(i32::from(fail != 0));
}
