//! `phrase-cost` — **what a positional index costs, and whether it is right.**
//!
//! `bench/roadmap/p45-phrase.md` shipped phrase queries on an opt-in positional index. Two claims
//! were made about it and neither is safe to assert from a unit test:
//!
//! 1. **What positions cost.** `p45` shipped them fixed-width and measured +74.7 % of artifact
//!    size, with the OFFSET array costing 2.2x the positions it addressed. `p54` delta-varint
//!    encoded both; this is what tracks that. A four-document unit test can only say "bigger".
//! 2. **The phrase verifier is correct on real text**, not just on a corpus built to exercise it.
//!
//! The second is the one that needs an independent arm. This project's sharpest methodological
//! lesson, from `p11`, is that **two agreeing implementations that share an input parser agree
//! about the parser, not about the answer**. So the reference here does not use the index at all:
//! it normalizes the raw fixture line and looks for the phrase as a substring on token boundaries.
//! It shares no code with the tokenizer, the dictionary, the postings or the verifier.
//!
//! That reference is deliberately *cruder* than the engine — it knows nothing about aliases,
//! folding or quantity tokens — so it is used as a **one-way** check, in the direction where
//! crudeness cannot produce a false alarm: every document the engine returns for a phrase must
//! contain that phrase literally. A document the reference finds and the engine does not is
//! reported separately as analyzer drift rather than as a failure, because that is what it is.
//!
//! Run:
//!     cargo run -p index-bench --release --bin phrase-cost

mod timer;

use index_text::{Doc, Field, Index, IndexBuilder, Schema};
use std::path::PathBuf;

/// Build the fixture twice from identical rows, differing only in positions.
fn build(row: &[(String, String)], position: bool) -> Index {
    let schema = Schema::new(vec![Field::new("name", 3.0, 0.4), Field::new("industry", 1.0, 0.6)]);
    let mut b = IndexBuilder::new(schema);
    if position {
        assert!(b.set_position(), "positions must be accepted before the first row");
    }
    for (name, industry) in row {
        b.add(&Doc::new([name.as_str(), industry.as_str()]));
    }
    b.build().expect("build")
}

/// Does `line` contain `phrase` as whole, adjacent, space-separated words?
///
/// Deliberately independent of the engine: no tokenizer, no dictionary, no postings. Its only
/// input is the raw fixture text, which is the one thing the engine and this cannot share.
fn contains_phrase(line: &str, phrase: &str) -> bool {
    let norm = |s: &str| {
        s.chars()
            .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { ' ' })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let (hay, needle) = (norm(line), norm(phrase));
    if needle.is_empty() {
        return false;
    }
    // Word-boundary substring: pad both so a match cannot start mid-token.
    format!(" {hay} ").contains(&format!(" {needle} "))
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[i]
}

fn main() {
    let path = std::env::var("INDEX_BENCH_LEAD")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("bench/fixture/blead-lead.tsv"));
    let Ok(text) = std::fs::read_to_string(&path) else {
        println!("SKIP: no lead fixture at {path:?} — see bench/fixture/README.md");
        return;
    };

    let mut row: Vec<(String, String)> = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 2 || f[0].is_empty() || f[1].is_empty() {
            continue;
        }
        row.push((f[0].to_string(), f[1].to_string()));
    }
    if row.is_empty() {
        println!("SKIP: lead fixture is empty");
        return;
    }

    println!("phrase-cost :: {} rows from {:?}\n", row.len(), path);

    let plain = build(&row, false);
    let with = build(&row, true);

    // ---- 1. The size claim, measured -----------------------------------------------------
    let a = plain.to_bytes().len();
    let b = with.to_bytes().len();
    let token: usize = row
        .iter()
        .map(|(n, i)| n.split_whitespace().count() + i.split_whitespace().count())
        .sum();
    println!("  artifact");
    println!("    without positions   {a:>12} bytes");
    println!(
        "    with positions      {b:>12} bytes   ({:+.1} %)",
        (b as f64 / a as f64 - 1.0) * 100.0
    );
    println!("    delta               {:>12} bytes", b - a);
    println!("    rough token count   {token:>12}");
    println!(
        "    delta / token       {:>12.2} bytes  (was 12.64 with fixed-width offsets, p45)",
        (b - a) as f64 / token as f64
    );

    // ---- 2. Correctness against an independent arm ---------------------------------------
    //
    // Phrases are taken from the corpus itself — adjacent word pairs from real names — so they are
    // phrases that actually occur, not synthetic ones that would trivially return nothing.
    let mut phrase: Vec<String> = Vec::new();
    for (name, _) in row.iter().step_by(row.len() / 200 + 1) {
        let w: Vec<&str> = name.split_whitespace().collect();
        if w.len() >= 2 {
            phrase.push(format!("{} {}", w[0], w[1]));
        }
    }
    phrase.sort();
    phrase.dedup();

    let mut checked = 0usize;
    let mut wrong = 0usize;
    let mut drift = 0usize;
    let mut empty = 0usize;
    for q in &phrase {
        let hit = with.search_phrase(q, 50);
        if hit.is_empty() {
            empty += 1;
        }
        for h in &hit {
            checked += 1;
            // The one-way claim: everything the engine returns must literally contain the phrase.
            let (name, industry) = &row[h.doc as usize];
            if !contains_phrase(name, q) && !contains_phrase(industry, q) {
                wrong += 1;
                if wrong <= 5 {
                    println!("    WRONG  {q:?} returned {name:?}");
                }
            }
        }
        // The other direction is reported, never asserted: the reference has no analyzer, so a
        // document it finds and the engine does not is alias/folding drift, not a phrase bug.
        let want = row
            .iter()
            .filter(|(n, i)| contains_phrase(n, q) || contains_phrase(i, q))
            .count();
        if want > hit.len() && hit.len() < 50 {
            drift += 1;
        }
    }
    println!("\n  correctness ({} phrases, {checked} hits verified against raw text)", phrase.len());
    println!("    hits not containing the phrase   {wrong}");
    println!("    phrases returning nothing        {empty}");
    println!("    phrases the crude reference found more of (analyzer drift, informational) {drift}");

    // ---- 3. What it costs at query time --------------------------------------------------
    let clock = timer::Clock::new();
    let n = phrase.len();
    let mut term: Vec<f64> = clock.time_each(n, |i| with.search(&phrase[i % n], 10).len() as u64);
    term.sort_by(f64::total_cmp);
    let mut phr: Vec<f64> =
        clock.time_each(n, |i| with.search_phrase(&phrase[i % n], 10).len() as u64);
    phr.sort_by(f64::total_cmp);

    println!("\n  latency ({} backend)", clock.backend());
    println!("    {:<18} {:>10} {:>10}", "", "p50", "p99");
    println!(
        "    {:<18} {:>9.1}us {:>9.1}us",
        "search",
        pct(&term, 0.50) / 1000.0,
        pct(&term, 0.99) / 1000.0
    );
    println!(
        "    {:<18} {:>9.1}us {:>9.1}us",
        "search_phrase",
        pct(&phr, 0.50) / 1000.0,
        pct(&phr, 0.99) / 1000.0
    );

    let ok = wrong == 0;
    println!("\nOVERALL: {}", if ok { "PASS" } else { "FAIL" });
    if !ok {
        std::process::exit(1);
    }
}
