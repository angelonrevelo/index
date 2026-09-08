//! `trin-catalog` — bench/roadmap/p86-trin-catalog.md
//!
//! The third p84 candidate run for real. **trin's opportunity catalog** filters candidates through
//! one predicate (`adapters/http/routes.ts`, the `/browse` endpoint):
//!
//! ```ts
//! if (q) items = items.filter((c) =>
//!   c.name.toLowerCase().includes(q) ||
//!   (c.description ?? "").toLowerCase().includes(q));
//! ```
//!
//! — case-folded substring on name OR description, after exact-match filters on domain/tier/
//! country. The on-disk seed catalog (`docs/product/eu-pilot-seed-catalog.json`) holds 52 entries
//! with rich descriptions (eligibility, visa route, funding) — the text column is long, which is
//! where substring matching usually hurts most. The engine indexes name + description as fields
//! and domain as a facet.
//!
//! The honest frame is the hobbycat one: **52 entries have no latency story.** What the bench
//! measures is recall against the consumer's own predicate, on clean terms, corrupted terms, and
//! — the filter half — domain-constrained queries where the engine answers facet AND text in one
//! pass. Run:
//!     cargo run -p index-bench --release --bin trin-catalog
//! with `INDEX_TRIN_DIR` pointing at the trin checkout (default: the sibling repo).

mod timer;

use index_text::{Doc, Field, IndexBuilder};
use serde_json::Value;
use std::path::PathBuf;

fn sibling() -> PathBuf {
    if let Ok(d) = std::env::var("INDEX_TRIN_DIR") {
        return PathBuf::from(d);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("trin")
}

#[inline]
fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Corrupt one alphabetic character: delete, substitute, or transpose — never position 0.
fn corrupt(q: &str, st: &mut u64) -> String {
    let ch: Vec<char> = q.chars().collect();
    let cand: Vec<usize> = (1..ch.len()).filter(|&i| ch[i].is_alphabetic()).collect();
    if cand.is_empty() {
        return q.to_string();
    }
    let i = cand[(splitmix(st) % cand.len() as u64) as usize];
    let mut out = ch.clone();
    match splitmix(st) % 3 {
        0 => {
            out.remove(i);
        }
        1 => out[i] = char::from(b'a' + (splitmix(st) % 26) as u8),
        _ => {
            if i + 1 < out.len() {
                out.swap(i, i + 1);
            } else {
                out.remove(i);
            }
        }
    }
    out.into_iter().collect()
}

fn main() {
    let clock = timer::Clock::new();
    println!("p86-trin-catalog :: the opportunity browse filter, on trin's own data");
    println!("clock backend: {}\n", clock.backend());

    let path = sibling().join("docs/product/eu-pilot-seed-catalog.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        println!("OVERALL: NO CORPUS FOUND — expected {path:?} (set INDEX_TRIN_DIR)");
        return;
    };
    let v: Value = serde_json::from_str(&text).expect("seed catalog json");
    let catalog = v["catalog"].as_array().cloned().unwrap_or_default();

    let mut b = IndexBuilder::new(index_text::Schema::new(vec![
        Field::new("name", 3.0, 0.4),
        Field::new("description", 1.0, 0.6),
        Field::new("domain", 0.0, 0.6),
    ]))
    .with_facet(2);
    let mut entries: Vec<(String, String, String)> = Vec::new(); // (name, description, domain)
    for e in &catalog {
        let name = e["name"].as_str().unwrap_or_default().to_string();
        let desc = e["description"].as_str().unwrap_or_default().to_string();
        let domain = e["domain"].as_str().unwrap_or_default().to_string();
        b.add(&Doc::new(vec![name.clone(), desc.clone(), domain.clone()]));
        entries.push((name, desc, domain));
    }
    let ix = b.build().unwrap();
    println!(
        "  {} catalog entries · index {} B · {} terms",
        entries.len(),
        ix.dict_byte_len(),
        ix.term_count()
    );

    // Their predicate: case-folded substring on name OR description, composed with the domain
    // equality filter.
    let hits_of = |q: &str, domain: Option<&str>| -> Vec<usize> {
        let lq = q.to_lowercase();
        (0..entries.len())
            .filter(|&i| {
                (domain.is_none() || entries[i].2 == domain.unwrap_or_default())
                    && (entries[i].0.to_lowercase().contains(&lq)
                        || entries[i].1.to_lowercase().contains(&lq))
            })
            .collect()
    };
    let in_top = |hits: &[index_text::Hit], truth: usize, k: usize| -> bool {
        hits.iter().take(k).any(|h| h.doc as usize == truth)
    };

    // Clean terms: distinctive words from each entry's name.
    let stop = ["the", "of", "and", "for", "in", "at", "on", "a"];
    let mut terms: Vec<(usize, String)> = Vec::new();
    for (i, (name, _, _)) in entries.iter().enumerate() {
        for w in name.split_whitespace() {
            let w: String = w.chars().filter(|c| c.is_alphanumeric()).collect();
            if w.chars().count() >= 4 && !stop.contains(&w.to_lowercase().as_str()) {
                terms.push((i, w));
            }
        }
    }
    terms.dedup();

    #[allow(unused_mut)]
    let fail = 0usize;

    // ---- clean terms ------------------------------------------------------------------------------
    let (mut e10, mut b10) = (0f64, 0f64);
    for (i, q) in &terms {
        if in_top(&ix.search(q, 10), *i, 10) {
            e10 += 1.0;
        }
        if hits_of(q, None).contains(i) {
            b10 += 1.0;
        }
    }
    let n = terms.len() as f64;
    println!(
        "\n  --- clean name terms ({} queries) ---\n  engine top-10 hit {:.1}%   browse-filter in-results {:.1}% (uncapped)",
        terms.len(),
        e10 / n * 100.0,
        b10 / n * 100.0
    );

    // ---- corrupted terms ---------------------------------------------------------------------------
    let mut st = 0x5EED_0721u64;
    let typo_q: Vec<(usize, String)> = terms
        .iter()
        .filter_map(|(i, q)| {
            let c = corrupt(q, &mut st);
            (c != *q).then_some((*i, c))
        })
        .collect();
    let (mut et10, mut bt10, mut ezero, mut bzero) = (0f64, 0f64, 0usize, 0usize);
    for (i, q) in &typo_q {
        let bh = hits_of(q, None);
        if bh.is_empty() {
            bzero += 1;
        }
        let hits = ix.search(q, 10);
        if hits.is_empty() {
            ezero += 1;
        }
        if in_top(&hits, *i, 10) {
            et10 += 1.0;
        }
        if bh.contains(i) {
            bt10 += 1.0;
        }
    }
    let n = typo_q.len() as f64;
    println!(
        "\n  --- one corrupted letter ({} queries) ---\n  engine top-10 hit {:.1}%   browse-filter in-results {:.1}%\n  zero results: engine {}   browse {}",
        typo_q.len(),
        et10 / n * 100.0,
        bt10 / n * 100.0,
        ezero,
        bzero
    );

    // ---- the filter half: domain AND text in one pass ----------------------------------------------
    // Their UI composes the domain dropdown with the text box; the engine answers both in one
    // query. Same terms, domain taken from the truth entry.
    let domains: Vec<String> = {
        let mut d: Vec<String> = entries.iter().map(|e| e.2.clone()).filter(|d| !d.is_empty()).collect();
        d.sort_unstable();
        d.dedup();
        d
    };
    if domains.len() > 1 {
        let (mut ef, mut bf) = (0f64, 0f64);
        for (i, q) in &terms {
            let dom = entries[*i].2.as_str();
            if dom.is_empty() {
                continue;
            }
            let hits = ix.search_facet(q, 10, dom);
            if in_top(&hits, *i, 10) {
                ef += 1.0;
            }
            if hits_of(q, Some(dom)).contains(i) {
                bf += 1.0;
            }
        }
        let n = terms.len() as f64;
        println!(
            "\n  --- domain-filtered text queries ({} terms x {} domains) ---\n  engine facet+text top-10 hit {:.1}%   browse-filter in-results {:.1}%",
            terms.len(),
            domains.len(),
            ef / n * 100.0,
            bf / n * 100.0
        );
    }

    let lat: Vec<f64> = clock.time_each(terms.len() * 20, |i| {
        let (_, q) = &terms[i % terms.len()];
        ix.search(q, 20).len() as u64
    });
    println!(
        "\n  --- latency, {}-entry corpus ---\n  engine p50 {:.1}us p99 {:.1}us",
        entries.len(),
        timer::percentile(&lat, 0.50) / 1000.0,
        timer::percentile(&lat, 0.99) / 1000.0,
    );

    println!();
    if fail == 0 {
        println!("OVERALL: PASS");
    } else {
        println!("OVERALL: FAIL ({fail} gate(s) red)");
    }
}
