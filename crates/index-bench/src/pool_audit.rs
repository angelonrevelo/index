//! `pool-audit` — **does the `p22` defect happen on real data, or only on the corpus built for it?**
//!
//! `bench/roadmap/p22-prune-consistency.md` reproduced a real correctness defect: the candidate pool
//! evicts by score while the final ranking orders by bucket-then-score, so a perfect-bucket document
//! can be thrown away before the ranking runs.
//!
//! Two repairs were built and **both were reverted**, on one specific ground:
//!
//! > The bug has not been observed on any real corpus, while the latency cost is unconditional.
//!
//! That reasoning is only as good as the looking behind it, and "all the consumer benches pass" is
//! weak evidence — **none of them was designed to detect this**. They measure precision@10 and
//! hit@1 against labels, which a pool eviction can survive: swap one correct document for another
//! correct document and every existing metric is unmoved.
//!
//! This bin looks directly. For every real fixture on disk it runs a large set of **real queries**
//! and compares `search` against `search_exhaustive_unpooled` — the same brute-force ground truth
//! `p22` uses. Any disagreement is the defect occurring on production data.
//!
//! **A single hit here flips the revert decision**, because the cost/benefit that justified it
//! assumed zero. That is what makes this worth running rather than assuming.

mod timer;

use index_text::{Doc, Field, IndexBuilder, Index, Schema};
use std::path::PathBuf;

fn corpus_dir() -> PathBuf {
    std::env::var("INDEX_CORPUS_DIR").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("..")
    })
}

fn split_csv(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut q = false;
    let mut it = line.chars().peekable();
    while let Some(c) = it.next() {
        match (c, q) {
            ('"', true) if it.peek() == Some(&'"') => {
                cur.push('"');
                it.next();
            }
            ('"', _) => q = !q,
            (',', false) => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// Compare pooled retrieval against brute force over a query set.
///
/// Reports *rank-1 disagreements* and *top-10 set differences* separately: the first is what a user
/// sees immediately, the second catches a correct document displaced further down.
fn audit(name: &str, ix: &Index, query: &[String], k: usize) {
    let mut rank1 = 0usize;
    let mut setdiff = 0usize;
    let mut degraded = 0usize;
    let mut bucket_loss = 0usize;
    let mut gapsum = 0.0f64;
    let mut gapn = 0usize;
    let mut gapmax = 0.0f64;
    let mut dumped = 0usize;
    let mut worst: Option<(String, u32, u32)> = None;
    for q in query {
        let got: Vec<u32> = ix.search(q, k).iter().map(|h| h.doc).collect();
        let want: Vec<u32> = ix.search_exhaustive_unpooled(q, k).iter().map(|h| h.doc).collect();
        if got.first() != want.first() {
            rank1 += 1;
            if worst.is_none() {
                worst = Some((
                    q.clone(),
                    got.first().copied().unwrap_or(u32::MAX),
                    want.first().copied().unwrap_or(u32::MAX),
                ));
            }
        }
        let a: std::collections::HashSet<u32> = got.into_iter().collect();
        let b: std::collections::HashSet<u32> = want.into_iter().collect();
        if a != b {
            setdiff += 1;
            // A different SET is not automatically a worse one. presyo carries many near-duplicate
            // listings with identical scores, so two pools can disagree on which of several
            // equal-ranking documents to keep and lose nothing at all. Compare the (bucket, score)
            // PROFILES: only if the pooled answer is strictly worse at some position has a real
            // document been lost.
            let ga: Vec<(u32, f32)> =
                ix.search(q, k).iter().map(|h| (h.typo_bucket, h.score)).collect();
            let wa: Vec<(u32, f32)> = ix
                .search_exhaustive_unpooled(q, k)
                .iter()
                .map(|h| (h.typo_bucket, h.score))
                .collect();
            // Severity, not just presence. A BUCKET difference means a document that matched more
            // of the query was replaced by one that matched less — the serious case. A score-only
            // difference is a reordering among equally-well-matched documents, and its magnitude
            // says whether it is meaningful or rounding.
            let bucket_worse = ga
                .iter()
                .zip(wa.iter())
                .any(|(g, w)| g.0 > w.0)
                || ga.len() < wa.len();
            let mut gap = 0.0f64;
            for (g, w) in ga.iter().zip(wa.iter()) {
                if g.0 == w.0 && w.1 > g.1 {
                    let rel = (w.1 - g.1) as f64 / (w.1.abs().max(1.0)) as f64;
                    if rel > gap {
                        gap = rel;
                    }
                }
            }
            let worse = bucket_worse || gap > 1e-6;
            if worse {
                degraded += 1;
            }
            if bucket_worse {
                bucket_loss += 1;
                // Dump the first few so the residual can be DIAGNOSED rather than guessed at.
                // `p24` closed the score-vs-rank pruning defect and left 2.23 % unexplained; the
                // rule there was that it must not be assumed to be the same bug in miniature.
                if std::env::var("INDEX_DUMP_LOSS").is_ok() && dumped < 3 {
                    dumped += 1;
                    println!("
      ---- bucket-loss case {dumped}: {q:?}");
                    println!("      {:<6} {:>8} {:>12}   |   {:<6} {:>8} {:>12}",
                        "got", "bucket", "score", "truth", "bucket", "score");
                    for i in 0..ga.len().max(wa.len()) {
                        let g = ga.get(i).map(|x| format!("{:>8} {:>12.4}", x.0, x.1))
                            .unwrap_or_else(|| format!("{:>8} {:>12}", "-", "-"));
                        let w = wa.get(i).map(|x| format!("{:>8} {:>12.4}", x.0, x.1))
                            .unwrap_or_else(|| format!("{:>8} {:>12}", "-", "-"));
                        println!("      {:<6} {}   |   {:<6} {}", i + 1, g, i + 1, w);
                    }
                }
            }
            if gap > 1e-6 {
                gapsum += gap;
                gapn += 1;
                if gap > gapmax {
                    gapmax = gap;
                }
            }
        }
    }
    let n = query.len().max(1);
    println!(
        "  {:<26} {:>8} {:>13} {:>15} {:>15}",
        name,
        query.len(),
        format!("{rank1} ({:.2}%)", 100.0 * rank1 as f64 / n as f64),
        format!("{setdiff} ({:.2}%)", 100.0 * setdiff as f64 / n as f64),
        format!("{degraded} ({:.2}%)", 100.0 * degraded as f64 / n as f64)
    );
    if bucket_loss > 0 || gapn > 0 {
        println!(
            "      of those: {bucket_loss} lost a BETTER-MATCHING document; {gapn} were score-only
                   (mean relative score gap {:.4}%, worst {:.2}%)",
            100.0 * gapsum / gapn.max(1) as f64,
            100.0 * gapmax
        );
    }
    if let Some((q, g, w)) = worst {
        println!("      first rank-1 disagreement: {:?} -> got {g}, truth {w}", q);
    }
}

fn main() {
    let clock = timer::Clock::new();
    println!("pool-audit :: does the p22 defect occur on REAL data?");
    println!("clock backend: {}", clock.backend());
    println!(
        "  Pooled `search` vs brute-force `search_exhaustive_unpooled` on real corpora and real\n  \
         queries. Any disagreement is the defect happening in production shape.\n"
    );
    println!(
        "  {:<26} {:>8} {:>13} {:>15} {:>15}",
        "corpus / query set", "queries", "rank-1 diff", "top-10 diff", "ACTUALLY WORSE"
    );

    // ---- presyo catalogue: product names and category names.
    let path = corpus_dir()
        .join("presyo")
        .join("data")
        .join("endless-prep")
        .join("catalog-active.csv");
    if let Ok(text) = std::fs::read_to_string(&path) {
        let mut line = text.lines();
        let head = split_csv(line.next().unwrap_or_default());
        let at = |k: &str| head.iter().position(|h| h == k);
        if let (Some(i_n), Some(i_b), Some(i_c)) =
            (at("product_name"), at("brand_name"), at("category_name"))
        {
            let mut name = Vec::new();
            let mut brand = Vec::new();
            let mut cat = Vec::new();
            for l in line {
                let f = split_csv(l);
                if f.len() <= i_c || f[i_n].trim().is_empty() {
                    continue;
                }
                name.push(f[i_n].trim().to_string());
                brand.push(f[i_b].trim().to_string());
                cat.push(f[i_c].trim().to_string());
            }
            let schema =
                Schema::new(vec![Field::new("name", 3.0, 0.5), Field::new("brand", 0.5, 0.75)]);
            let mut b = IndexBuilder::new(schema);
            for i in 0..name.len() {
                b.add(&Doc::new([name[i].as_str(), brand[i].as_str()]));
            }
            let ix = b.build().expect("build");

            let step = (name.len() / 4000).max(1);
            let q_name: Vec<String> = name.iter().step_by(step).cloned().collect();
            audit("presyo / product names", &ix, &q_name, 10);

            let mut q_cat: Vec<String> = cat.clone();
            q_cat.sort();
            q_cat.dedup();
            q_cat.retain(|c| !c.is_empty() && c != "Uncategorized");
            audit("presyo / category names", &ix, &q_cat, 10);

            // The p22 shape needs a COMMON word in the query. Two- and three-word suffixes of real
            // product names are the closest real analogue: "... milk drink", "... powder 500g".
            let q_tail: Vec<String> = name
                .iter()
                .step_by(step)
                .filter_map(|n| {
                    let w: Vec<&str> = n.split_whitespace().collect();
                    (w.len() >= 3).then(|| w[w.len() - 3..].join(" "))
                })
                .collect();
            audit("presyo / 3-word tails", &ix, &q_tail, 10);

            // Latency on the REAL consumer workload, not the synthetic scale bench. p23 priced the
            // ranking pool at 2-6% using DepEd queries over a 41,069-term dictionary; presyo's is
            // 117,472 terms with a different query shape, so the number has to be re-earned here
            // rather than assumed to transfer.
            let mut ns: Vec<f64> = Vec::with_capacity(q_name.len());
            for q in &q_name {
                let t = std::time::Instant::now();
                std::hint::black_box(ix.search(q, 10));
                ns.push(t.elapsed().as_nanos() as f64);
            }
            ns.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let pick = |f: f64| ns[((ns.len() as f64 * f) as usize).min(ns.len() - 1)];
            println!(
                "
  presyo real-query latency over {} product names: p50 {:.0}us  p99 {:.0}us",
                q_name.len(),
                pick(0.50) / 1000.0,
                pick(0.99) / 1000.0
            );
        }
    } else {
        println!("  (presyo catalogue not found — skipped)");
    }

    // ---- blead business names.
    if let Ok(text) = std::fs::read_to_string("bench/fixture/blead-lead.tsv") {
        let mut nm = Vec::new();
        let mut ind = Vec::new();
        for l in text.lines().skip(1) {
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() < 2 || f[0].is_empty() {
                continue;
            }
            nm.push(f[0].to_string());
            ind.push(f[1].to_string());
        }
        let schema = Schema::new(vec![Field::new("name", 3.0, 0.5), Field::new("ind", 0.5, 0.75)]);
        let mut b = IndexBuilder::new(schema);
        for i in 0..nm.len() {
            b.add(&Doc::new([nm[i].as_str(), ind[i].as_str()]));
        }
        let ix = b.build().expect("build");
        let step = (nm.len() / 4000).max(1);
        let q: Vec<String> = nm.iter().step_by(step).cloned().collect();
        audit("blead / business names", &ix, &q, 10);
    }

    // ---- maphy places.
    if let Ok(text) = std::fs::read_to_string("bench/fixture/maphy-place.txt") {
        let mut nm = Vec::new();
        let mut parent = Vec::new();
        for l in text.lines().skip(1) {
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() < 4 || f[1].is_empty() {
                continue;
            }
            nm.push(f[1].to_string());
            parent.push(f[3].to_string());
        }
        let schema = Schema::new(vec![Field::new("name", 4.0, 0.5), Field::new("p", 0.5, 0.75)]);
        let mut b = IndexBuilder::new(schema);
        for i in 0..nm.len() {
            b.add(&Doc::new([nm[i].as_str(), parent[i].as_str()]));
        }
        let ix = b.build().expect("build");
        audit("maphy / place names", &ix, &nm, 10);
    }

    // ---- profstopick course titles.
    if let Ok(text) = std::fs::read_to_string("bench/fixture/profstopick-course.tsv") {
        let mut t = Vec::new();
        let mut d = Vec::new();
        for l in text.lines().skip(1) {
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() < 2 || f[0].is_empty() {
                continue;
            }
            t.push(f[0].to_string());
            d.push(f[1].to_string());
        }
        let schema = Schema::new(vec![Field::new("title", 3.0, 0.5), Field::new("d", 0.5, 0.75)]);
        let mut b = IndexBuilder::new(schema);
        for i in 0..t.len() {
            b.add(&Doc::new([t[i].as_str(), d[i].as_str()]));
        }
        let ix = b.build().expect("build");
        audit("profstopick / titles", &ix, &t, 10);
    }

    println!(
        "\n  Read: `rank-1 differs` is what a user sees first; `top-10 differs` also counts a\n  \
         correct document displaced further down the page. Both are compared against brute force\n  \
         over the whole corpus, so a non-zero figure is the pool losing a document that the\n  \
         engine's own comparator says should have been there.\n\n  \
         A single non-zero cell reverses the decision recorded in p22, which reverted two working\n  \
         fixes on the grounds that the defect had never been seen outside a corpus built for it."
    );
}
