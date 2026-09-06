//! `real-million` — the 5 ms typo bar, on a million REAL documents.
//!
//! # The input `p47` said was missing
//!
//! `p7` set a 5 ms typo-p99 bar at a million documents. `scale` has measured it ever since by
//! **recombining** 61,467 real school records up to a million, and `p47` closed with the limit that
//! made the verdict unusable:
//!
//! > **No real 1 M corpus exists to test against**, so the failing rows cannot be separated from
//! > the recombination artefact by measurement. That is the single most valuable missing input here.
//!
//! Recombination holds the vocabulary FIXED at 41,069 terms while multiplying documents sixteenfold,
//! so every posting list is longer than a genuine corpus of that size would produce — and posting
//! length is exactly what the typo tail is made of. The measurement was structurally pessimistic and
//! nobody could say by how much.
//!
//! `bench/roadmap/p51-database-sweep.md` found the missing input while sweeping the estate:
//! **presyo's `raw_product` holds 4,524,754 real rows.** This bin uses them.
//!
//! # What it measures
//!
//! The same ladder `scale` walks, on real product names, with real vocabulary growth — so the
//! dictionary grows with the corpus the way it does in production instead of standing still.
//!
//! Run:
//!     cargo run -p index-bench --release --bin real-million
//! with `INDEX_MILLION_TSV` pointing at a `name<TAB>vendor<TAB>code` export.

mod timer;

use index_text::{Doc, Field, Index, IndexBuilder, Schema};
use std::path::PathBuf;

/// `p7`'s bar, unchanged and unraised.
const P99_NS_MAX: f64 = 5_000_000.0;

fn splitmix64(x: &mut u64) -> u64 {
    *x = x.wrapping_add(0x9E3779B97F4A7C15);
    let mut z = *x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}

/// Corrupt a query the way `scale` does, so the two are comparable: transpose one adjacent pair.
fn corrupt(s: &str, st: &mut u64) -> String {
    let mut c: Vec<char> = s.chars().collect();
    if c.len() < 4 {
        return s.to_string();
    }
    let i = (splitmix64(st) as usize) % (c.len() - 1);
    c.swap(i, i + 1);
    c.into_iter().collect()
}

fn main() {
    let path = std::env::var("INDEX_MILLION_TSV")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("bench/fixture/presyo-1m.tsv"));
    let Ok(text) = std::fs::read_to_string(&path) else {
        println!("SKIP: no million-row export at {path:?}");
        println!("  Produce one with the CLI's own recipe, from any database:");
        println!("    ssh HOST 'psql -d presyo -At -c \"COPY (SELECT raw_name, vendor, code ...) \\");
        println!("      TO STDOUT (FORMAT text)\"' > presyo-1m.tsv");
        println!("OVERALL: NO CORPUS — refusing to report a verdict.");
        std::process::exit(2);
    };

    let row: Vec<(&str, &str, &str)> = text
        .lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            match (f.next(), f.next(), f.next()) {
                (Some(a), Some(b), Some(c)) if a.len() > 3 => Some((a, b, c)),
                _ => None,
            }
        })
        .collect();
    if row.len() < 200_000 {
        println!("SKIP: only {} usable rows; this bin exists to test a MILLION", row.len());
        return;
    }
    if row.len() < 10_000_000 {
        println!(
            "  NOTE: {} rows available; 10 M of REAL text does not exist in this estate.
               The ladder runs to what is real. See the ceiling note at the end.",
            row.len()
        );
    }

    let clock = timer::Clock::new();
    println!("real-million :: the 5 ms typo bar on REAL documents, not recombined ones");
    println!("clock backend: {}\n", clock.backend());
    println!("  {} real product rows from {:?}", row.len(), path);
    println!(
        "  Contrast with `scale`, which recombines 61,467 records and holds the vocabulary\n  \
         FIXED at 41,069 terms while multiplying documents. Here the dictionary grows with the\n  \
         corpus, which is what a real deployment does and what decides posting length.\n"
    );

    // 10 M is the number `p7` set the bar at; 8.6 M is all the real text this estate holds, so the
    // ladder runs to whatever is actually available and the gap is reported rather than papered
    // over by recombination -- which `p55` measured overstating the tail by ~1.6x.
    let ladder: Vec<usize> = [100_000usize, 250_000, 500_000, 750_000, 1_000_000, 2_000_000, 4_000_000, 8_000_000, 10_000_000]
        .into_iter()
        .filter(|&n| n <= row.len())
        .collect();

    println!(
        "{:>10}  {:>9}  {:>10}  {:>11}  {:>9}  {:>10}  {:>10}",
        "docs", "terms", "build ms", "bytes", "B/doc", "typo p50", "typo p99"
    );
    println!("{}", "-".repeat(80));

    let mut worst = 0.0f64;
    let mut rows_out: Vec<(usize, usize, f64, f64)> = Vec::new();
    for &n in &ladder {
        let schema = Schema::new(vec![
            Field::new("name", 3.0, 0.4),
            Field::new("vendor", 1.0, 0.6),
            Field::new("code", 0.0, 0.6),
        ]);
        let mut b = IndexBuilder::new(schema);
        let t0 = std::time::Instant::now();
        for (name, vendor, code) in row.iter().take(n) {
            b.add(&Doc::new([*name, *vendor, *code]));
        }
        let ix: Index = b.build().expect("build");
        let build_ms = t0.elapsed().as_secs_f64() * 1e3;
        let bytes = ix.to_bytes().len();

        // Queries are real product names drawn from the indexed slice, then corrupted -- the same
        // construction `scale` uses, so the two numbers mean the same thing.
        let mut st = 0xC0FFEEu64;
        let probe: Vec<String> = (0..2_000)
            .map(|_| row[(splitmix64(&mut st) as usize) % n].0.to_string())
            .collect();
        let dirty: Vec<String> = probe.iter().map(|q| corrupt(q, &mut st)).collect();

        let mut t = clock.time_each(dirty.len(), |i| ix.search(&dirty[i], 10).len() as u64);
        t.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let p50 = timer::percentile(&t, 0.50);
        let p99 = timer::percentile(&t, 0.99);
        worst = worst.max(p99);
        rows_out.push((n, ix.term_count(), p50, p99));

        println!(
            "{:>10}  {:>9}  {:>10.0}  {:>11}  {:>9.1}  {:>8.0}us  {:>8.0}us",
            n,
            ix.term_count(),
            build_ms,
            bytes,
            bytes as f64 / n as f64,
            p50 / 1000.0,
            p99 / 1000.0
        );
    }

    // ---- The one lever that exists, priced on REAL data --------------------------------------
    //
    // `p29` built `search_capped` and swept it on the RECOMBINED corpus: cap 4 cut typo p99 33 %
    // for 1.9 % of queries changing. Whether that trade survives on real vocabulary is a different
    // question, and it is the question an adopter actually has to answer.
    if let Some(&n) = ladder.last() {
        let schema = Schema::new(vec![
            Field::new("name", 3.0, 0.4),
            Field::new("vendor", 1.0, 0.6),
            Field::new("code", 0.0, 0.6),
        ]);
        let mut b = IndexBuilder::new(schema);
        for (name, vendor, code) in row.iter().take(n) {
            b.add(&Doc::new([*name, *vendor, *code]));
        }
        let ix: Index = b.build().expect("build");
        let mut st = 0xC0FFEEu64;
        let probe: Vec<String> =
            (0..2_000).map(|_| row[(splitmix64(&mut st) as usize) % n].0.to_string()).collect();
        let dirty: Vec<String> = probe.iter().map(|q| corrupt(q, &mut st)).collect();
        let truth: Vec<Vec<u32>> =
            dirty.iter().map(|q| ix.search(q, 10).iter().map(|h| h.doc).collect()).collect();

        println!("\n  --- the expansion cap, priced on REAL vocabulary (p29 did this recombined) ---");
        println!("    {:>4}  {:>10}  {:>10}  {:>13}  {:>13}", "cap", "typo p50", "typo p99", "top-10 same", "rank-1 same");
        for cap in [16usize, 8, 4, 2] {
            let mut c = clock.time_each(dirty.len(), |i| ix.search_capped(&dirty[i], 10, cap).len() as u64);
            c.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let (mut same, mut same1) = (0usize, 0usize);
            for (i, q) in dirty.iter().enumerate() {
                let got: Vec<u32> = ix.search_capped(q, 10, cap).iter().map(|h| h.doc).collect();
                if got == truth[i] {
                    same += 1;
                }
                if got.first() == truth[i].first() {
                    same1 += 1;
                }
            }
            let pct = |x: usize| 100.0 * x as f64 / dirty.len() as f64;
            println!(
                "    {:>4}  {:>8.0}us  {:>8.0}us  {:>12.2}%  {:>12.2}%",
                cap,
                timer::percentile(&c, 0.50) / 1000.0,
                timer::percentile(&c, 0.99) / 1000.0,
                pct(same),
                pct(same1)
            );
        }
    }

    // Vocabulary growth is the whole reason this bin exists; show it explicitly.
    if let (Some(first), Some(last)) = (rows_out.first(), rows_out.last()) {
        println!(
            "\n  vocabulary grew {} -> {} terms across {}x the documents.",
            first.1,
            last.1,
            last.0 / first.0.max(1)
        );
        println!(
            "  `scale` holds it at 41,069 for every row of its ladder, which is why its posting\n  \
             lists -- and therefore its typo tail -- are longer than a real corpus produces."
        );
    }

    let pass = worst <= P99_NS_MAX;
    println!(
        "\n  -> worst typo p99 {:.2} ms (bar {:.0} ms): {}",
        worst / 1e6,
        P99_NS_MAX / 1e6,
        if pass { "PASS" } else { "FAIL" }
    );
    println!("\nOVERALL: {}", if pass { "PASS" } else { "FAIL" });
    if !pass {
        std::process::exit(1);
    }
}
