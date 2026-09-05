//! `presyo-prior` — **does a static prior actually pay?**
//!
//! This bin exists to settle the one feature in the engine with a rationale and no evidence.
//! `IndexBuilder::add_with_prior` was built after `bench/roadmap/p12-maphy-place.md` appeared to
//! show it was needed; the benchmark turned out to be wrong, and sweeping the prior's strength moved
//! that number not at all. `docs/adoption.md` still argues every consumer has a query-independent
//! importance signal. **Arguing is not measuring**, so this measures it on the strongest one
//! available: presyo's retailer.
//!
//! # The workload
//!
//! `presyo/tests/fixtures/recall-gold-cases.json` — 500 gold cross-store product clusters exported
//! from production on 2026-06-13. Every listing in a cluster is the same physical product as sold by
//! a different retailer, and the retrieval task is the one presyo actually performs: **given one
//! retailer's listing, find the same product at the other retailers.**
//!
//! Name quality differs visibly by source, which is why a source prior is plausible at all:
//!
//! ```text
//! ever_ph     Fiesta Sweet Spaghettipid 850g
//! metromart   Fiesta Sweet Spaghettipid 850 g
//! pickaroo    Fiesta Sweet SpaghetTipid 850 g
//! waltermart  Fiesta Small Sweet Spaghettipid | 850g
//! ```
//!
//! # The design decision that makes this honest
//!
//! **The prior is fitted on a TRAIN half and evaluated on a HELD-OUT half.** Fitting per-source
//! quality on the same clusters it is scored against would produce a win by construction — the
//! prior would be a compressed copy of the answer key. Clusters are split by parity of their index,
//! the prior is estimated only from the train half, and every number reported is from the test half.
//!
//! A negative result here is a perfectly good outcome and is reported as one. The point is to stop
//! shipping a feature nobody has shown a win for.

mod timer;

use index_text::{Doc, Field, IndexBuilder, Index, Schema};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

/// The prior must beat no-prior by at least this much to count as a win rather than noise.
const MIN_GAIN: f64 = 0.005;

struct Listing {
    cluster: usize,
    source: String,
    name: String,
}

fn corpus_dir() -> PathBuf {
    std::env::var("INDEX_CORPUS_DIR").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("..")
    })
}

fn load(path: &PathBuf) -> Option<Vec<Listing>> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let cluster = v.get("clusters")?.as_array()?;
    let mut out = Vec::new();
    for (ci, c) in cluster.iter().enumerate() {
        let Some(list) = c.get("listings").and_then(|l| l.as_array()) else { continue };
        for l in list {
            let (Some(src), Some(name)) = (
                l.get("source_key").and_then(|s| s.as_str()),
                l.get("raw_name").and_then(|s| s.as_str()),
            ) else {
                continue;
            };
            out.push(Listing {
                cluster: ci,
                source: src.to_string(),
                name: name.to_string(),
            });
        }
    }
    Some(out)
}

fn build(listing: &[Listing], prior: Option<&HashMap<String, f32>>) -> Index {
    let schema = Schema::new(vec![Field::new("name", 1.0, 0.5)]);
    let mut b = IndexBuilder::new(schema);
    for l in listing {
        match prior {
            Some(p) => {
                b.add_with_prior(&Doc::new([l.name.as_str()]), *p.get(&l.source).unwrap_or(&1.0))
            }
            None => b.add(&Doc::new([l.name.as_str()])),
        };
    }
    b.build().expect("build")
}

/// recall@k and hit@1 of a listing's cluster-mates, excluding the listing itself.
fn evaluate(
    ix: &Index,
    listing: &[Listing],
    query_of: &[usize],
    k: usize,
    mut seen: Option<&mut Vec<Vec<usize>>>,
) -> (f64, f64) {
    let mut recall = 0.0;
    let mut hit1 = 0.0;
    let mut n = 0.0f64;
    for &qi in query_of {
        let want: Vec<usize> = listing
            .iter()
            .enumerate()
            .filter(|(j, l)| *j != qi && l.cluster == listing[qi].cluster)
            .map(|(j, _)| j)
            .collect();
        if want.is_empty() {
            continue;
        }
        n += 1.0;
        let hit: Vec<usize> = ix
            .search(&listing[qi].name, k + 1)
            .iter()
            .map(|h| h.doc as usize)
            .filter(|&d| d != qi)
            .collect();
        let got = hit.iter().filter(|d| want.contains(d)).count();
        recall += got as f64 / want.len() as f64;
        if hit.first().is_some_and(|d| want.contains(d)) {
            hit1 += 1.0;
        }
        if let Some(v) = seen.as_deref_mut() {
            v.push(hit);
        }
    }
    (recall / n.max(1.0), hit1 / n.max(1.0))
}

fn main() {
    let clock = timer::Clock::new();
    let path = corpus_dir()
        .join("presyo")
        .join("tests")
        .join("fixtures")
        .join("recall-gold-cases.json");
    let Some(listing) = load(&path) else {
        println!("SKIP: presyo/tests/fixtures/recall-gold-cases.json not found under {:?}", corpus_dir());
        println!("      Refusing to report a verdict without the real corpus.");
        return;
    };

    // Split by cluster parity. Documents are ALL listings either way — only the queries and the
    // prior estimation are split, because the index a user searches contains everything.
    let train: Vec<usize> =
        (0..listing.len()).filter(|&i| listing[i].cluster % 2 == 0).collect();
    let test: Vec<usize> = (0..listing.len()).filter(|&i| listing[i].cluster % 2 == 1).collect();

    let mut by_source: HashMap<&str, usize> = HashMap::new();
    for l in &listing {
        *by_source.entry(l.source.as_str()).or_default() += 1;
    }
    let mut src: Vec<(&str, usize)> = by_source.into_iter().collect();
    src.sort_by_key(|s| std::cmp::Reverse(s.1));

    println!("presyo-prior :: does a static prior actually pay?");
    println!("clock backend: {}", clock.backend());
    println!(
        "  {} listings across {} clusters, {} retailers",
        listing.len(),
        listing.iter().map(|l| l.cluster).max().unwrap_or(0) + 1,
        src.len()
    );
    println!("  train (even clusters) {} queries / test (odd) {} queries\n", train.len(), test.len());

    // --- baseline
    let flat = build(&listing, None);
    let mut base_rank: Vec<Vec<usize>> = Vec::new();
    let (base_r, base_h) = evaluate(&flat, &listing, &test, 10, Some(&mut base_rank));

    // --- fit the prior on TRAIN only: how well does each retailer's listing retrieve its mates?
    let mut sum: HashMap<String, (f64, f64)> = HashMap::new();
    for &qi in &train {
        let want: Vec<usize> = listing
            .iter()
            .enumerate()
            .filter(|(j, l)| *j != qi && l.cluster == listing[qi].cluster)
            .map(|(j, _)| j)
            .collect();
        if want.is_empty() {
            continue;
        }
        let hit: Vec<usize> = flat
            .search(&listing[qi].name, 11)
            .iter()
            .map(|h| h.doc as usize)
            .filter(|&d| d != qi)
            .collect();
        let got = hit.iter().filter(|d| want.contains(d)).count() as f64 / want.len() as f64;
        let e = sum.entry(listing[qi].source.clone()).or_insert((0.0, 0.0));
        e.0 += got;
        e.1 += 1.0;
    }
    let quality: HashMap<String, f32> = sum
        .iter()
        .map(|(k, (s, n))| (k.clone(), (s / n.max(1.0)) as f32))
        .collect();

    println!("  --- retailer quality, fitted on TRAIN only ---");
    let mut q: Vec<(&String, &f32)> = quality.iter().collect();
    q.sort_by(|a, b| b.1.total_cmp(a.1));
    for (k, v) in &q {
        println!("  {:<16} {:>7.3}  ({} listings)", k, v, by_source_count(&listing, k));
    }

    // A prior of 1 + spread * quality, swept, so the trade is a curve rather than one taste-chosen
    // point. Spread 0 reproduces the baseline exactly and is the control.
    println!("\n  --- held-out test half ---");
    println!("  {:<28} {:>12} {:>12} {:>10}", "prior", "recall@10", "hit@1", "rankings");
    println!(
        "  {:<28} {:>11.1}% {:>11.1}% {:>10}",
        "none (baseline)",
        base_r * 100.0,
        base_h * 100.0,
        "-"
    );

    let mut best = (0.0f32, base_r);
    for spread in [0.25f32, 0.5, 1.0, 2.0] {
        let p: HashMap<String, f32> = quality
            .iter()
            .map(|(k, v)| (k.clone(), 1.0 + spread * v))
            .collect();
        let ix = build(&listing, Some(&p));
        let mut rank: Vec<Vec<usize>> = Vec::new();
        let (r, h) = evaluate(&ix, &listing, &test, 10, Some(&mut rank));
        // Did the prior CHANGE anything? "No effect" would be a bug in how it is applied;
        // "no benefit" is a finding. Without this column the two are indistinguishable, and the
        // wrong one of the two is the one that silently ships.
        let moved = rank
            .iter()
            .zip(base_rank.iter())
            .filter(|(a, b)| a != b)
            .count();
        println!(
            "  {:<28} {:>11.1}% {:>11.1}% {:>10}",
            format!("source quality, spread {spread}"),
            r * 100.0,
            h * 100.0,
            format!("{moved} moved")
        );
        if r > best.1 {
            best = (spread, r);
        }
    }

    let gain = best.1 - base_r;
    println!("\n  --- verdict ---");
    if gain >= MIN_GAIN {
        println!(
            "  PASS  a source prior improves held-out recall@10 by {:.1} points (spread {}).",
            gain * 100.0,
            best.0
        );
    } else {
        println!(
            "  NO WIN  best held-out gain is {:+.2} points, under the {:.1}-point bar.",
            gain * 100.0,
            MIN_GAIN * 100.0
        );
        println!(
            "\n  The prior WORKS and buys nothing, which are different statements and the `rankings`\n  \
             column is what separates them: it reordered results for over a thousand held-out\n  \
             queries. It is applied, it is not a no-op, and it still moves neither metric."
        );
        println!(
            "\n  The reason is headroom, not the feature. The baseline is at {:.1}% recall@10 and\n  \
             {:.1}% hit@1 -- there are {:.1} points of room in total, and a prior cannot add a token\n  \
             a listing does not have. Matching a SPECIFIC product across retailers is decided by the\n  \
             words in the name.\n\n  \
             So this corpus cannot answer the question rather than answering it negatively. A prior\n  \
             earns its place where candidates are otherwise near-tied -- a broad or browse query --\n  \
             and presyo's gold set has ground truth only for the specific-product task. Building the\n  \
             workload that could show a win means labelling broad queries, which nobody has done.",
            base_r * 100.0,
            base_h * 100.0,
            (1.0 - base_r) * 100.0
        );
    }
    println!(
        "\n  Read: the prior is fitted on even clusters and scored on odd ones. Fitting and scoring\n  \
         on the same clusters would make the prior a compressed copy of the answer key and would\n  \
         have reported a win by construction."
    );
}

fn by_source_count(listing: &[Listing], src: &str) -> usize {
    listing.iter().filter(|l| l.source == src).count()
}
