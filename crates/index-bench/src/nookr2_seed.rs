//! `nookr2-seed` — bench/roadmap/p89-nookr2-seed.md
//!
//! The last p84 candidate with data on disk. **nookr2's two comboboxes** filter with the same
//! shape every estate repo ships (`src/components/MemberSearchCombobox.tsx`,
//! `src/components/CategoryCombobox.tsx`, and the same inline copy in `MembersPage.tsx`):
//!
//! ```ts
//! c.name.toLowerCase().includes(search.toLowerCase()) ||
//! c.code.toLowerCase().includes(search.toLowerCase())
//! ```
//!
//! — case-folded substring on name OR code. What the seed holds: 7 profiles for the member
//! combobox (full_name + email), 26 income/expense categories for the category combobox
//! (name + code). That is hobbycat scale, and the size verdict is stated up front rather than
//! discovered: **at 26 rows there is no latency or scale story.** What IS measurable is the
//! defect class trin (`p86`) and wheresthefx (`p87`) already exposed on their own data: the
//! stored names carry punctuation the typed form does not — `'Salaries & Wages'`,
//! `'Electricity (MERALCO)'`, `'Contribution/Donation'`, code `CAR_STICKER` — and a substring
//! predicate cannot meet its own row when the user types the natural spacing.
//!
//! The corpus is read through `index_cli::sql::SqlDumpReader` — the FIRST consumer of the
//! `--sql` dump reader outside the CLI itself, which is why `index-cli` is a library. Run:
//!     cargo run -p index-bench --release --bin nookr2-seed
//! with `INDEX_NOOKR2_DIR` pointing at the nookr2 checkout (default: the sibling repo).

mod timer;

use index_cli::sql::SqlDumpReader;
use index_text::{Doc, Field, IndexBuilder};
use std::io::Cursor;
use std::path::PathBuf;

fn sibling() -> PathBuf {
    if let Ok(d) = std::env::var("INDEX_NOOKR2_DIR") {
        return PathBuf::from(d);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("nookr2")
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

/// The typed form of a stored label: every separator a keyboard does not naturally produce —
/// `&`, `/`, parens, commas — folded to the space the user types instead, and code underscores
/// with them. `"Electricity (MERALCO)"` becomes `"electricity meralco"`; `CAR_STICKER` becomes
/// `"car sticker"`.
fn natural(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn rows_of(dump: &str, table: &str) -> Vec<std::collections::HashMap<String, String>> {
    let mut r = SqlDumpReader::new(Cursor::new(dump.to_string()), &[table.to_string()]);
    let mut out = Vec::new();
    while let Some(rec) = r.next_row().unwrap() {
        out.push(rec);
    }
    out
}

struct Surface {
    /// (name, code) — for members, the email stands in as the code arm.
    rows: Vec<(String, String)>,
}

fn main() {
    let clock = timer::Clock::new();
    println!("p89-nookr2-seed :: the two comboboxes, on nookr2's own seed");
    println!("clock backend: {}\n", clock.backend());

    let path = sibling().join("supabase").join("seed.sql");
    let Ok(dump) = std::fs::read_to_string(&path) else {
        println!("OVERALL: NO CORPUS FOUND — expected {path:?} (set INDEX_NOOKR2_DIR)");
        return;
    };

    // The dump reader's first consumer outside the CLI: the same parser `index build --sql`
    // runs, reading the seed a database already wrote.
    let profiles = rows_of(&dump, "profile");
    let income = rows_of(&dump, "income_category");
    let expense = rows_of(&dump, "expense_category");
    println!(
        "  seed.sql read through SqlDumpReader: {} profiles, {} income + {} expense categories",
        profiles.len(),
        income.len(),
        expense.len()
    );

    let member = Surface {
        rows: profiles
            .iter()
            .map(|r| {
                (
                    r.get("full_name").cloned().unwrap_or_default(),
                    r.get("email").cloned().unwrap_or_default(),
                )
            })
            .collect(),
    };
    let mut category_rows: Vec<(String, String)> = income
        .iter()
        .chain(expense.iter())
        .map(|r| {
            (
                r.get("name").cloned().unwrap_or_default(),
                r.get("code").cloned().unwrap_or_default(),
            )
        })
        .collect();
    category_rows.dedup();
    let category = Surface { rows: category_rows };

    let mut st: u64 = 0x6E6F_6B72; // "nokr"
    let mut fail = 0;

    for (label, surface, k) in [
        ("member combobox", &member, 3usize),
        ("category combobox", &category, 3usize),
    ] {
        println!("\n=== {label}: {} rows ===", surface.rows.len());
        let mut b = IndexBuilder::new(index_text::Schema::new(vec![
            Field::new("name", 3.0, 0.4),
            Field::new("code", 1.0, 0.6),
        ]));
        for (name, code) in &surface.rows {
            b.add(&Doc::new([name.clone(), code.clone()]));
        }
        let ix = b.build().unwrap();
        println!("  index {} B · {} terms", ix.dict_byte_len(), ix.term_count());

        // Their predicate, reproduced exactly: case-folded substring on name OR code.
        let theirs = |q: &str| -> Vec<usize> {
            let lq = q.to_lowercase();
            (0..surface.rows.len())
                .filter(|&i| {
                    surface.rows[i].0.to_lowercase().contains(&lq)
                        || surface.rows[i].1.to_lowercase().contains(&lq)
                })
                .collect()
        };
        let in_top = |q: &str, truth: usize| -> bool {
            ix.search(q, k).iter().any(|h| h.doc as usize == truth)
        };

        // Members type a NAME or paste an email verbatim; folding an email's separators into
        // spaces would manufacture a miss nobody can produce. Categories are where the stored
        // form carries separators the typed form does not — '&' and parens in names, underscores
        // in codes — so both arms are generated there and the name arm only for members.
        let paste_email = label == "member combobox";
        for (family, make) in [
            (
                "verbatim lowercase (sanity: both must match)",
                Box::new(|name: &str, _code: &str, st: &mut u64| {
                    let _ = st;
                    name.to_lowercase()
                }) as Box<dyn Fn(&str, &str, &mut u64) -> String>,
            ),
            (
                "natural typing (separators folded: 'salaries wages', 'car sticker')",
                Box::new(move |name: &str, code: &str, st: &mut u64| {
                    let n = natural(name);
                    let c = if paste_email {
                        code.to_lowercase() // an email is pasted, never re-spaced
                    } else {
                        natural(code)
                    };
                    if !c.is_empty() && splitmix(st) % 2 == 0 {
                        c
                    } else {
                        n
                    }
                }),
            ),
            (
                "one corrupted letter on the verbatim form",
                Box::new(|name: &str, _code: &str, st: &mut u64| {
                    corrupt(&name.to_lowercase(), st)
                }),
            ),
        ] {
            let mut q_rows: Vec<(usize, String)> = Vec::new();
            for (i, (name, code)) in surface.rows.iter().enumerate() {
                if name.is_empty() {
                    continue;
                }
                let q = make(name, code, &mut st);
                if !q.trim().is_empty() {
                    q_rows.push((i, q));
                }
            }
            let mut their_hit = 0usize;
            let mut their_zero = 0usize;
            let mut engine_hit = 0usize;
            for (truth, q) in &q_rows {
                let th = theirs(q);
                if th.is_empty() {
                    their_zero += 1;
                }
                if th.contains(truth) {
                    their_hit += 1;
                }
                if in_top(q, *truth) {
                    engine_hit += 1;
                }
            }
            let n = q_rows.len();
            let lat: Vec<f64> = clock.time_each(n * 50, |i| {
                let (_, q) = &q_rows[i % n];
                ix.search(q, k).len() as u64
            });
            println!(
                "  {:<56} theirs {their_hit:>3}/{n} (zero-result {their_zero:>3})   engine(top-{k}) {engine_hit:>3}/{n}   p50 {:.1}us p99 {:.1}us",
                family,
                timer::percentile(&lat, 0.50) / 1000.0,
                timer::percentile(&lat, 0.99) / 1000.0,
            );
            // Verdicts stated before the run: verbatim is a sanity family both must sweep;
            // the two defect families are where the engine must hold near-perfect and the
            // substring predicate is expected to miss its own rows.
            match family {
                f if f.starts_with("verbatim") => {
                    if their_hit != n || engine_hit != n {
                        println!("    -> FAIL both matchers must sweep their own verbatim rows");
                        fail += 1;
                    }
                }
                f if f.starts_with("natural") => {
                    if engine_hit * 20 < n * 19 {
                        println!("    -> FAIL engine below 95% on natural input");
                        fail += 1;
                    }
                    if their_hit == n {
                        println!("    -> NOTE their predicate survived natural input here");
                    }
                }
                _ => {
                    if engine_hit * 10 < n * 9 {
                        println!("    -> FAIL engine below 90% on corrupted input");
                        fail += 1;
                    }
                }
            }
        }
    }

    println!("\n  size verdict, stated up front: {} members + {} categories is the hobbycat\n  class — there is no scale or latency story at this size, and none is claimed. What is\n  measurable is the punctuation/typo defect class their predicate shares with trin (p86)\n  and wheresthefx (p87); this bench re-runs against the seed's growth.", member.rows.len(), category.rows.len());
    println!("\n{}", if fail == 0 { "OVERALL: PASS" } else { "OVERALL: FAIL" });
    std::process::exit(if fail == 0 { 0 } else { 1 });
}
