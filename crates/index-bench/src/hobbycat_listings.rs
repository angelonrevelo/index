//! `hobbycat-listings` — bench/roadmap/p84-hobbycat-listings.md
//!
//! The honest small-repo lane of the p84 sweep. **hobbycat's marketplace** searches active
//! listings through one SQL predicate (`src/db.rs`, `list_cards`):
//!
//! ```sql
//! AND (l.title LIKE ? OR l.summary LIKE ? OR l.description LIKE ?)   -- each bound to '%<q>%'
//! ```
//!
//! — a case-insensitive substring per term, OR'd across the three text columns. The engine indexes
//! the same three columns plus the facet (hobby) and the price, and answers the same terms plus the
//! ones a keyboard corrupts.
//!
//! **The size is the finding, and it is stated before any number: the corpus is 8 active
//! listings.** The survey put hobbycat in the p84 candidate list on matcher shape, not on volume;
//! this bench measures what the engine does for it today and, just as deliberately, records that
//! the measured need is the polkadoc/booted class — a scan that costs nothing. The bench re-runs
//! whenever the catalogue grows, which is what makes the verdict checkable rather than final.
//!
//! The corpus is exported from `db/hobbycat.db` with the recipe in the bench doc and is NOT
//! vendored (`INDEX_HOBBYCAT_CSV` relocates it). Run:
//!     cargo run -p index-bench --release --bin hobbycat-listings

mod timer;

use index_text::{Doc, Field, IndexBuilder};
use std::path::PathBuf;

fn corpus() -> PathBuf {
    if let Ok(d) = std::env::var("INDEX_HOBBYCAT_CSV") {
        return PathBuf::from(d);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("bench")
        .join("fixture")
        .join("hobbycat-listings.csv")
}

fn main() {
    let clock = timer::Clock::new();
    println!("p84-hobbycat-listings :: the marketplace search, on hobbycat's own data");
    println!("clock backend: {}\n", clock.backend());

    let path = corpus();
    let Ok(text) = std::fs::read_to_string(&path) else {
        println!("OVERALL: NO CORPUS FOUND — expected {path:?}; export recipe in the bench doc");
        return;
    };

    // Deliberately hand-rolled CSV split: 8 rows with no embedded quotes, and the bench must not
    // grow a CSV dependency for what the engine itself parses with the same rules.
    let mut rows: Vec<(String, String, String, String, String, f64)> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if i == 0 || line.trim().is_empty() {
            continue;
        }
        let c: Vec<&str> = line.split(',').collect();
        if c.len() < 6 {
            continue;
        }
        rows.push((
            c[0].to_string(),
            c[1].to_string(),
            c[2].to_string(),
            c[3].to_string(),
            c[4].to_string(),
            c[5].parse().unwrap_or(0.0),
        ));
    }
    if rows.is_empty() {
        println!("OVERALL: NO CORPUS FOUND — {path:?} held no rows");
        return;
    }

    // Three text fields — the three columns their LIKE predicate touches. Hobby/price are in the
    // export but out of scope: the baseline has no facet or range story, so the comparison is text.
    let mut b = IndexBuilder::new(index_text::Schema::new(vec![
        Field::new("title", 3.0, 0.4),
        Field::new("summary", 1.0, 0.6),
        Field::new("description", 1.0, 0.6),
    ]));
    for (_code, title, summary, desc, _hobby, _price) in &rows {
        b.add(&Doc::new(vec![title.clone(), summary.clone(), desc.clone()]));
    }
    let ix = b.build().unwrap();
    println!(
        "  {} active listings ({} bytes of text) · index {} B · {} terms",
        rows.len(),
        rows.iter().map(|r| r.1.len() + r.2.len() + r.3.len()).sum::<usize>(),
        ix.dict_byte_len(),
        ix.term_count()
    );

    // Their matcher: `LIKE '%q%'` on title OR summary OR description. SQLite LIKE is
    // case-insensitive for ASCII; this is the same predicate in Rust.
    let like_hits = |q: &str| -> Vec<usize> {
        let lq = q.to_lowercase();
        (0..rows.len())
            .filter(|&i| {
                rows[i].1.to_lowercase().contains(&lq)
                    || rows[i].2.to_lowercase().contains(&lq)
                    || rows[i].3.to_lowercase().contains(&lq)
            })
            .collect()
    };

    // Query terms drawn from the corpus itself: distinctive words of each title. Truth for a term
    // is the listing it came from.
    let stop = ["the", "a", "and", "for", "of", "with", "in", "on"];
    let mut terms: Vec<(usize, String)> = Vec::new();
    for (i, (_, title, _, _, _, _)) in rows.iter().enumerate() {
        for w in title.split_whitespace() {
            let w: String = w.chars().filter(|c| c.is_alphanumeric()).collect();
            if w.len() >= 3 && !stop.contains(&w.to_lowercase().as_str()) {
                terms.push((i, w));
            }
        }
    }
    terms.dedup();

    let mut st = 0x5EED_C475u64;
    let corrupt = |q: &str, st: &mut u64| -> String {
        let ch: Vec<char> = q.chars().collect();
        let cand: Vec<usize> = (1..ch.len()).filter(|&i| ch[i].is_alphabetic()).collect();
        if cand.is_empty() {
            return q.to_string();
        }
        let i = cand[(*st % cand.len() as u64) as usize];
        let mut out = ch.clone();
        match splitmix(st) % 3 {
            0 => {
                out.remove(i);
            }
            1 => out[i] = char::from(b'a' + (splitmix(st) % 26) as u8),
            _ => {
                let j = (i + 1) % out.len();
                out.swap(i, j);
            }
        }
        out.into_iter().collect()
    };

    let fail = 0usize;

    // ---- clean terms ------------------------------------------------------------------------------
    let (mut e10, mut b10) = (0f64, 0f64);
    let _ns: Vec<f64> = Vec::new();
    for (i, q) in &terms {
        if ix.search(q, 50).iter().any(|h| h.doc as usize == *i) {
            e10 += 1.0;
        }
        if like_hits(q).contains(i) {
            b10 += 1.0;
        }
    }
    let n = terms.len() as f64;
    println!(
        "\n  --- clean title terms ({} queries) ---\n  engine top-50 hit {:.1}%   LIKE matcher hit {:.1}%",
        terms.len(),
        e10 / n * 100.0,
        b10 / n * 100.0
    );

    // ---- corrupted terms ---------------------------------------------------------------------------
    let typo_q: Vec<(usize, String)> = terms
        .iter()
        .filter_map(|(i, q)| {
            let c = corrupt(q, &mut st);
            (c != *q).then_some((*i, c))
        })
        .collect();
    let (mut et10, mut bt10, mut ezero, mut bzero) = (0f64, 0f64, 0usize, 0usize);
    for (i, q) in &typo_q {
        let bh = like_hits(q);
        if bh.is_empty() {
            bzero += 1;
        }
        let hits = ix.search(q, 50);
        if hits.is_empty() {
            ezero += 1;
        }
        if hits.iter().any(|h| h.doc as usize == *i) {
            et10 += 1.0;
        }
        if bh.contains(i) {
            bt10 += 1.0;
        }
    }
    let n = typo_q.len() as f64;
    println!(
        "\n  --- one corrupted letter ({} queries) ---\n  engine top-50 hit {:.1}%   LIKE matcher hit {:.1}%\n  zero results: engine {}   LIKE {}",
        typo_q.len(),
        et10 / n * 100.0,
        bt10 / n * 100.0,
        ezero,
        bzero
    );

    // ---- latency -----------------------------------------------------------------------------------
    let lat: Vec<f64> = clock.time_each(terms.len() * 10, |i| {
        let (_, q) = &terms[i % terms.len()];
        ix.search(q, 20).len() as u64
    });
    println!(
        "\n  --- latency, {}-listing corpus ---\n  engine p50 {:.1}us p99 {:.1}us   (a LIKE scan of 8 rows is ~1us: neither engine nor baseline has a latency story here)",
        rows.len(),
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

#[inline]
fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}
