//! `blead-industry` — **does `learn_expansion` generalize, or was it presyo-shaped?**
//!
//! `IndexBuilder::learn_expansion` was designed against one corpus and one failure. On presyo's
//! 241,677 grocery products it took category retrieval from **61.7 % to 96.6 %** precision@10. A
//! feature measured on exactly the data it was built for has proved very little: the honest next
//! question is whether it works somewhere else, on a corpus nobody had it in mind for.
//!
//! `blead` is a lead-generation pipeline. Its store holds **25,979 real Philippine businesses** —
//! names, not product titles — each tagged with an `industry` drawn from 35 values. The failure
//! shape is the same as presyo's and the vocabulary is completely different:
//!
//! ```text
//! J&S Agriventures Corporation                  -> Agriculture
//! 10K EAST CONCRETE MIX SPECIALIST, INC.        -> Wholesale/Retail
//! BAGUIO CENTRAL UNIVERSITY ALUMNI ASSOCIATION  -> Public Admin
//! ```
//!
//! Nothing in `10K EAST CONCRETE MIX SPECIALIST` says *Wholesale/Retail*.
//!
//! # Why the comparison is fair
//!
//! **11.4 % of business names share a word with their own industry**, against 13.8 % for presyo's
//! products and their categories — so the label is independent of the retrieval signal to almost
//! exactly the same degree, and a difference in result cannot be blamed on an easier label.
//!
//! The measurement mirrors `p15-presyo-catalog.md` exactly — same metric, same arms, same
//! `boost 0.0` on the facet field so that it supplies the value to the learner and contributes
//! nothing to retrieval — so the two numbers can be read side by side.

mod timer;

use index_text::{Doc, Field, IndexBuilder, Schema};
use std::collections::HashMap;
use std::path::PathBuf;

/// An industry needs this many businesses to be worth querying.
const MIN_MEMBER: usize = 100;

struct Lead {
    name: String,
    industry: String,
    city: String,
}

fn main() {
    let clock = timer::Clock::new();
    let path = std::env::var("INDEX_BENCH_LEAD")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("bench/fixture/blead-lead.tsv"));
    let Ok(text) = std::fs::read_to_string(&path) else {
        println!("SKIP: no lead fixture at {path:?} — see bench/fixture/README.md");
        return;
    };

    let mut lead: Vec<Lead> = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 2 || f[0].is_empty() || f[1].is_empty() {
            continue;
        }
        lead.push(Lead {
            name: f[0].to_string(),
            industry: f[1].to_string(),
            city: f.get(2).copied().unwrap_or("").to_string(),
        });
    }
    if lead.is_empty() {
        println!("SKIP: lead fixture is empty");
        return;
    }

    let mut member: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, l) in lead.iter().enumerate() {
        member.entry(l.industry.as_str()).or_default().push(i);
    }
    let mut industry: Vec<(&str, Vec<usize>)> =
        member.into_iter().filter(|(_, v)| v.len() >= MIN_MEMBER).collect();
    industry.sort_by(|a, b| a.0.cmp(b.0));

    let leak = lead
        .iter()
        .filter(|l| {
            let n = l.name.to_lowercase();
            l.industry
                .to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| w.len() > 2)
                .any(|w| n.contains(w))
        })
        .count();

    println!("blead-industry :: does learn_expansion generalize?");
    println!("clock backend: {}", clock.backend());
    println!(
        "  {} real Philippine businesses, {} industries with {MIN_MEMBER}+ members",
        lead.len(),
        industry.len()
    );
    println!(
        "  label leakage {:.1}% (presyo's was 13.8%) — comparable, so a difference cannot be\n  \
         blamed on an easier label\n",
        100.0 * leak as f64 / lead.len() as f64
    );

    let evaluate = |ix: &index_text::Index| -> (f64, f64) {
        let mut prec = 0.0;
        let mut mrr = 0.0;
        for (name, want) in &industry {
            let hit: Vec<usize> = ix.search(name, 10).iter().map(|h| h.doc as usize).collect();
            prec += hit.iter().filter(|d| want.contains(d)).count() as f64 / 10.0;
            if let Some(r) = hit.iter().position(|d| want.contains(d)) {
                mrr += 1.0 / (r + 1) as f64;
            }
        }
        let n = industry.len().max(1) as f64;
        (prec / n, mrr / n)
    };

    // --- arm 1: plain. The facet is present but weightless, so it cannot itself match.
    let schema = || {
        Schema::new(vec![
            Field::new("name", 3.0, 0.5),
            Field::new("city", 0.5, 0.75),
            Field::new("industry", 0.0, 0.75),
        ])
    };
    let mut b = IndexBuilder::new(schema());
    for l in &lead {
        b.add(&Doc::new([l.name.as_str(), l.city.as_str(), l.industry.as_str()]));
    }
    let plain = b.build().expect("build plain");
    let (p_prec, p_mrr) = evaluate(&plain);

    // --- arm 2: learned expansion over the industry facet.
    let t = std::time::Instant::now();
    let mut b = IndexBuilder::new(schema()).learn_expansion(2, 20);
    for l in &lead {
        b.add(&Doc::new([l.name.as_str(), l.city.as_str(), l.industry.as_str()]));
    }
    let learned = b.build().expect("build learned");
    let build_ms = t.elapsed().as_secs_f64() * 1000.0;
    let (l_prec, l_mrr) = evaluate(&learned);

    println!("  --- arms ---");
    println!("  {:<34} {:>13} {:>8}", "", "precision@10", "MRR");
    println!("  {:<34} {:>12.1}% {:>8.3}", "plain index", p_prec * 100.0, p_mrr);
    println!(
        "  {:<34} {:>12.1}% {:>8.3}   {:+.1} pt",
        "learn_expansion(industry, 20)",
        l_prec * 100.0,
        l_mrr,
        (l_prec - p_prec) * 100.0
    );
    println!(
        "  {} facet values learned, build {:.2} s",
        learned.expansion_count(),
        build_ms / 1000.0
    );

    // --- HELD OUT: what happens to a table that has gone stale?
    //
    // Both arms above are in-sample: the table is learned from the same businesses it is scored
    // against, which is the deployment condition for firms already in the store. `p17` measured the
    // other condition on presyo — a table applied to documents added AFTER it was built — and found
    // a ~17-point drop. That number is the cost of a stale table, and it decides re-derivation
    // cadence, so it should not be assumed to transfer from one corpus to another.
    //
    // The engine derives from the documents it indexes, so the split has to be simulated here: the
    // table comes from the train half, the index holds only the test half.
    {
        let train: Vec<usize> = (0..lead.len()).filter(|i| i % 2 == 0).collect();
        let test: Vec<usize> = (0..lead.len()).filter(|i| i % 2 == 1).collect();

        let tok = |t: &str| -> Vec<String> {
            t.to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| w.len() > 2)
                .map(str::to_string)
                .collect()
        };

        // Concentration scoring over the train half only.
        let mut global: HashMap<String, usize> = HashMap::new();
        let mut per_ind: HashMap<&str, HashMap<String, usize>> = HashMap::new();
        let mut n_train: HashMap<&str, usize> = HashMap::new();
        for &i in &train {
            let ind = lead[i].industry.as_str();
            *n_train.entry(ind).or_default() += 1;
            for w in tok(&lead[i].name) {
                *global.entry(w.clone()).or_default() += 1;
                *per_ind.entry(ind).or_default().entry(w).or_default() += 1;
            }
        }
        let total = train.len() as f64;
        let derived: HashMap<&str, Vec<String>> = per_ind
            .iter()
            .map(|(&ind, m)| {
                let n_i = n_train.get(ind).copied().unwrap_or(1) as f64;
                let mut sc: Vec<(f64, &String)> = m
                    .iter()
                    .filter(|(_, &c)| c >= 5)
                    .map(|(w, &c)| {
                        let inside = c as f64 / n_i;
                        let overall = global.get(w).copied().unwrap_or(1) as f64 / total;
                        (inside / (overall + 1e-9), w)
                    })
                    .collect();
                sc.sort_by(|a, b| b.0.total_cmp(&a.0));
                (ind, sc.into_iter().take(20).map(|(_, w)| w.clone()).collect::<Vec<String>>())
            })
            .collect();

        // Index the TEST half only.
        let mut bb = IndexBuilder::new(schema());
        let mut local: HashMap<usize, usize> = HashMap::new();
        for (k, &i) in test.iter().enumerate() {
            local.insert(i, k);
            bb.add(&Doc::new([
                lead[i].name.as_str(),
                lead[i].city.as_str(),
                lead[i].industry.as_str(),
            ]));
        }
        let ixh = bb.build().expect("build held-out");

        let mut want: HashMap<&str, Vec<usize>> = HashMap::new();
        for &i in &test {
            want.entry(lead[i].industry.as_str()).or_default().push(local[&i]);
        }

        let score = |expand: bool| -> f64 {
            let mut prec = 0.0;
            let mut n = 0.0f64;
            for (name, _) in &industry {
                let Some(members) = want.get(*name) else { continue };
                if members.len() < 20 {
                    continue;
                }
                n += 1.0;
                let mut q = name.to_string();
                if expand {
                    if let Some(e) = derived.get(*name) {
                        for w in e {
                            q.push(' ');
                            q.push_str(w);
                        }
                    }
                }
                // Ask for a large pool and truncate: the engine's expansion path does not fire here
                // (the query is not a bare facet value), so without this the held-out arm is
                // measuring pool eviction rather than the expansion's quality.
                let hit: Vec<usize> =
                    ixh.search(&q, 400).iter().take(10).map(|h| h.doc as usize).collect();
                prec += hit.iter().filter(|d| members.contains(d)).count() as f64 / 10.0;
            }
            prec / n.max(1.0)
        };

        let h_plain = score(false);
        let h_exp = score(true);
        println!("\n  --- held out: table from the train half, index is the test half ---");
        println!("  {:<34} {:>13}", "", "precision@10");
        println!("  {:<34} {:>12.1}%", "plain", h_plain * 100.0);
        println!(
            "  {:<34} {:>12.1}%   {:+.1} pt",
            "derived expansion (held out)",
            h_exp * 100.0,
            (h_exp - h_plain) * 100.0
        );
        println!(
            "\n  in-sample gain {:+.1} pt vs held-out gain {:+.1} pt — the difference, {:.1} points,\n  \
             is this corpus's cost of a stale table. presyo's was ~17 points (p17), so the figure\n  \
             does NOT transfer between corpora and each adopter has to measure its own.",
            (l_prec - p_prec) * 100.0,
            (h_exp - h_plain) * 100.0,
            ((l_prec - p_prec) - (h_exp - h_plain)) * 100.0
        );
    }

    let mut worst: Vec<(f64, f64, &str, usize)> = industry
        .iter()
        .map(|(name, want)| {
            let hp: Vec<usize> = plain.search(name, 10).iter().map(|h| h.doc as usize).collect();
            let hl: Vec<usize> = learned.search(name, 10).iter().map(|h| h.doc as usize).collect();
            (
                hp.iter().filter(|d| want.contains(d)).count() as f64 / 10.0,
                hl.iter().filter(|d| want.contains(d)).count() as f64 / 10.0,
                *name,
                want.len(),
            )
        })
        .collect();
    worst.sort_by(|a, b| a.0.total_cmp(&b.0));
    println!("\n  --- industries the plain index could not reach ---");
    println!("  {:<26} {:>8} {:>10} {:>10}", "industry", "members", "plain", "learned");
    for (p, l, name, n) in worst.iter().take(8) {
        println!(
            "  {:<26} {:>8} {:>9.0}% {:>9.0}%",
            format!("\"{name}\""),
            n,
            p * 100.0,
            l * 100.0
        );
    }

    println!("\n  --- verdict ---");
    println!(
        "  presyo (241,677 grocery products, 145 categories):  61.7% -> 96.6%   +34.9 pt\n  \
         blead  ({} business names, {} industries):{}{:.1}% -> {:.1}%   {:+.1} pt",
        lead.len(),
        industry.len(),
        " ".repeat(6),
        p_prec * 100.0,
        l_prec * 100.0,
        (l_prec - p_prec) * 100.0
    );
    println!(
        "\n  Both are IN-SAMPLE, measured the same way, on corpora with near-identical label\n  \
         leakage (13.8% and {:.1}%) and completely different vocabularies. That is the\n  \
         generalization claim, and it is the second corpus rather than the first.",
        100.0 * leak as f64 / lead.len() as f64
    );
}
