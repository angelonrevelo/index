//! `wheresthefx-venues` — bench/roadmap/p87-wheresthefx-venues.md
//!
//! The fourth p84 candidate run for real, and the first at five-figure scale: **wheresthefx's OSM
//! venue ingest fixture holds 13,487 real Metro-Manila venues** (`package/ingest/fixture/osm-venue.json`),
//! each with a name, optional aliases (440 entries), a category slug and coordinates. The
//! user-facing search predicate (`app/api/src/db/event.ts`) is:
//!
//! ```ts
//! const term = `%${filter.search}%`;
//! or(ilike(event.title, term), ilike(event.description, term), ilike(event.venueName, term))
//! ```
//!
//! — case-insensitive substring over the columns, one term. The engine indexes venue name (boost
//! 3) plus aliases (boost 1, the 440 entries that have them) with the category slug as a facet.
//!
//! **The metric on this corpus is TOKEN-CLASS recall, and the doc explains why.** OSM names are
//! branch-heavy: "Jollibee" or "7-Eleven" name hundreds of distinct branches, so a brand token
//! maps to hundreds of DIFFERENT venue names and no top-10 can enumerate them — per-venue recall
//! would measure tie-breaking luck, not search quality. A query succeeds when the venues it
//! surfaces actually match what was typed: the answer set contains a venue whose name, with
//! punctuation stripped, contains the token. Punctuation is the honest trap here: a user types
//! "7eleven", the name is "7-Eleven", and a raw substring predicate misses its own venue.
//!
//! Run:
//!     cargo run -p index-bench --release --bin wheresthefx-venues
//! with `INDEX_WTFX_DIR` pointing at the wheresthefx checkout (default: the sibling repo).

mod timer;

use index_text::{Doc, Field, IndexBuilder};
use serde_json::Value;
use std::path::PathBuf;

/// Interactive budget.
const P99_NS_MAX: f64 = 5_000_000.0;
/// A clean brand token must surface its class in the top 10.
const CLEAN_CLASS_MIN: f64 = 0.95;

fn sibling() -> PathBuf {
    if let Ok(d) = std::env::var("INDEX_WTFX_DIR") {
        return PathBuf::from(d);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("wheresthefx")
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

/// What a user's keystrokes compare against: lowercase, punctuation gone. "7-Eleven" becomes
/// "7eleven", so the typed form and the stored form meet.
fn alnum(s: &str) -> String {
    s.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect()
}

fn main() {
    let clock = timer::Clock::new();
    println!("p87-wheresthefx-venues :: the venue surface, on the OSM ingest fixture");
    println!("clock backend: {}\n", clock.backend());

    let path = sibling().join("package/ingest/fixture/osm-venue.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        println!("OVERALL: NO CORPUS FOUND — expected {path:?} (set INDEX_WTFX_DIR)");
        return;
    };
    let venues: Vec<Value> =
        serde_json::from_str(&text).expect("osm-venue.json is an array of venues");

    let t0 = std::time::Instant::now();
    let mut b = IndexBuilder::new(index_text::Schema::new(vec![
        Field::new("name", 3.0, 0.4),
        Field::new("alias", 1.0, 0.6),
        Field::new("slug", 0.0, 0.6),
    ]))
    .with_facet(2);
    let mut names: Vec<String> = Vec::new();
    for v in &venues {
        let name = v["name"].as_str().unwrap_or_default().to_string();
        let alias = v["aliasList"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        let slug = v["slug"].as_str().unwrap_or_default().to_string();
        b.add(&Doc::new(vec![name.clone(), alias, slug]));
        names.push(name);
    }
    let ix = b.build().unwrap();
    println!(
        "  {} venues · index {} terms, {} B ({} B/doc) · built in {} ms",
        names.len(),
        ix.term_count(),
        ix.dict_byte_len(),
        ix.dict_byte_len() / ix.doc_count().max(1),
        t0.elapsed().as_millis(),
    );
    // The class check runs over the venue's FULL searchable text — name plus aliases — because a
    // venue found through its alias ("SMX Convention" as an alias of "SMX") is a correct answer
    // whose NAME alone would fail the check.
    let text_alnum: Vec<String> = venues
        .iter()
        .enumerate()
        .map(|(idx, v)| {
            let alias = v["aliasList"]
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" "))
                .unwrap_or_default();
            let mut t = alnum(&names[idx]);
            t.push_str(&alnum(&alias));
            t
        })
        .collect();

    // Their predicate: `ilike('%q%')` — RAW case-insensitive substring, punctuation intact.
    let hits_of = |q: &str| -> Vec<usize> {
        let lq = q.to_lowercase();
        (0..names.len()).filter(|&i| names[i].to_lowercase().contains(&lq)).collect()
    };

    let mut fail = 0usize;

    // Clean distinctive tokens: first alphanumeric word of length >= 5 per venue, one per venue.
    let mut terms: Vec<(usize, String)> = Vec::new();
    for (i, name) in names.iter().enumerate() {
        for w in name.split_whitespace() {
            let w: String = w.chars().filter(|c| c.is_alphanumeric()).collect();
            if w.chars().count() >= 5 {
                terms.push((i, w));
                break;
            }
        }
    }

    // ---- clean tokens: token-CLASS recall ----------------------------------------------------------
    let (mut e10, mut b10, mut bempty) = (0f64, 0f64, 0usize);
    let ns: Vec<f64> = clock.time_each(terms.len(), |i| {
        ix.search(&terms[i].1, 10).len() as u64
    });
    for (_i, q) in &terms {
        let tq = alnum(q);
        let hits = ix.search(q, 10);
        if hits
            .iter()
            .take(10)
            .any(|h| text_alnum[h.doc as usize].contains(&tq))
        {
            e10 += 1.0;
        }
        let bh = hits_of(q);
        if bh.is_empty() {
            bempty += 1;
        }
        if bh.iter().any(|&d| text_alnum[d].contains(&tq)) {
            b10 += 1.0;
        }
    }
    let n = terms.len() as f64;
    println!(
        "\n  --- clean distinctive tokens ({} queries) ---\n  engine token-class recall in top-10 {:.1}%   ilike-filter class recall {:.1}% — {} queries found NOTHING\n  engine p50 {:.1}us p99 {:.1}us",
        terms.len(),
        e10 / n * 100.0,
        b10 / n * 100.0,
        bempty,
        timer::percentile(&ns, 0.50) / 1000.0,
        timer::percentile(&ns, 0.99) / 1000.0,
    );
    let clean_pass = e10 / n >= CLEAN_CLASS_MIN;
    println!(
        "  -> clean class recall >= {CLEAN_CLASS_MIN}: {}",
        if clean_pass { "PASS" } else { "FAIL" }
    );
    if !clean_pass {
        fail += 1;
    }

    // ---- corrupted tokens: does the correction reach the intended class? ---------------------------
    let mut st = 0x5EED_3FAAu64;
    let typo_q: Vec<(usize, String, String)> = terms
        .iter()
        .filter_map(|(i, q)| {
            let c = corrupt(q, &mut st);
            (c != *q).then_some((*i, q.clone(), c))
        })
        .collect();
    let (mut et10, mut bt10, mut ezero, mut bzero) = (0f64, 0f64, 0usize, 0usize);
    for (_i, orig, corr) in &typo_q {
        let bh = hits_of(corr);
        if bh.is_empty() {
            bzero += 1;
        }
        let hits = ix.search(corr, 10);
        if hits.is_empty() {
            ezero += 1;
        }
        // The correction worked when the top-10 surfaces a venue of the ORIGINAL token's class.
        let tq = alnum(orig);
        if hits.iter().take(10).any(|h| text_alnum[h.doc as usize].contains(&tq)) {
            et10 += 1.0;
        }
        if bh.iter().any(|&d| text_alnum[d].contains(&tq)) {
            bt10 += 1.0;
        }
    }
    let n = typo_q.len() as f64;
    println!(
        "\n  --- one corrupted letter ({} queries) ---\n  engine reaches the intended class in top-10 {:.1}%   ilike-filter reaches it {:.1}%\n  zero results: engine {}   ilike {}",
        typo_q.len(),
        et10 / n * 100.0,
        bt10 / n * 100.0,
        ezero,
        bzero
    );

    // ---- typeahead on a five-figure corpus ---------------------------------------------------------
    let mut pref_q: Vec<(usize, String)> = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let words: Vec<&str> = name.split_whitespace().collect();
        if words.is_empty() {
            continue;
        }
        let w: String = words[0].chars().filter(|c| c.is_alphanumeric()).collect();
        if w.chars().count() >= 3 {
            pref_q.push((i, w.chars().take(3).collect()));
        }
    }
    let plast: Vec<f64> = clock.time_each(pref_q.len(), |i| {
        ix.search_prefix(&pref_q[i].1, 10).len() as u64
    });
    let mut e1p = 0f64;
    for (_i, q) in &pref_q {
        let hits = ix.search_prefix(q, 10);
        if hits
            .iter()
            .take(10)
            .any(|h| text_alnum[h.doc as usize].contains(&alnum(q)))
        {
            e1p += 1.0;
        }
    }
    let n = pref_q.len() as f64;
    println!(
        "\n  --- 3-letter typeahead ({} keystrokes) ---\n  engine class recall in top-10 {:.1}%\n  engine p50 {:.1}us p99 {:.1}us   (an ilike scan of {} rows answers a prefix in a full scan: the baseline has no typeahead latency story)",
        pref_q.len(),
        e1p / n * 100.0,
        timer::percentile(&plast, 0.50) / 1000.0,
        timer::percentile(&plast, 0.99) / 1000.0,
        names.len(),
    );
    let p99 = timer::percentile(&plast, 0.99);
    let ta_pass = p99 <= P99_NS_MAX;
    println!(
        "  -> typeahead p99 <= {P99_NS_MAX:.0}us: {}",
        if ta_pass { "PASS" } else { "FAIL" }
    );
    if !ta_pass {
        fail += 1;
    }

    println!();
    if fail == 0 {
        println!("OVERALL: PASS");
    } else {
        println!("OVERALL: FAIL ({fail} gate(s) red)");
    }
}
