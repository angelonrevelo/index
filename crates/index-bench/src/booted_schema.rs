//! p40-booted-schema :: search across every database in the house.
//!
//! The goal says the engine should work with *all* the apps and databases, and `booted` is the
//! catalogue that knows what those are: a schema crawl of **61 Postgres databases across five
//! machines**, banked to `~/.booted/schema.json`. This bin indexes that crawl and asks the
//! questions a developer actually asks at 2 a.m.:
//!
//! - *"Which table is `life_record` again, and which database is it in?"*
//! - *"Where do we store scraped articles?"* — answered from the table's own COMMENT, not its name.
//! - *"How many tables in each database mention `outlet`?"*
//! - *"Show me the tables over 100,000 rows."*
//!
//! **Nothing is committed.** The bank is read from the user's own machine and the bin skips cleanly
//! when it is absent, exactly as the presyo and DepEd corpora are handled. No schema, comment or
//! row count enters the repository.
//!
//! This is a different corpus shape from every other bench here: short, highly repetitive names
//! (`id`, `created_at`, `user_id` appear everywhere) with a long prose comment attached. It is the
//! shape that breaks BM25 tuned for product names.

use index_text::{Doc, Field, IndexBuilder, Schema};
use std::collections::HashMap;
use std::path::PathBuf;

mod timer;

/// Interactive budget for a developer typing into a palette.
const P99_NS_MAX: f64 = 5_000_000.0;

struct Table {
    origin: String,
    database: String,
    name: String,
    comment: String,
    column: String,
    rows: f64,
}

fn bank_path() -> PathBuf {
    if let Ok(p) = std::env::var("BOOTED_SCHEMA") {
        return PathBuf::from(p);
    }
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".booted").join("schema.json")
}

/// Pull every table out of the bank, local and remote.
fn load(path: &PathBuf) -> Option<Vec<Table>> {
    let text = std::fs::read_to_string(path).ok()?;
    let root: serde_json::Value = serde_json::from_str(&text).ok()?;
    let mut out = Vec::new();

    let mut take = |origin: &str, dbs: Option<&serde_json::Value>| {
        let Some(list) = dbs.and_then(|d| d.as_array()) else { return };
        for db in list {
            let dbname =
                db.get("database_name").and_then(|v| v.as_str()).unwrap_or("?").to_string();
            let Some(tables) = db.pointer("/schema/table").and_then(|t| t.as_array()) else {
                continue;
            };
            for t in tables {
                let name = t.get("tableName").and_then(|v| v.as_str()).unwrap_or("").to_string();
                if name.is_empty() {
                    continue;
                }
                let comment =
                    t.get("comment").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let column = t
                    .get("columns")
                    .and_then(|c| c.as_array())
                    .map(|c| {
                        c.iter()
                            .filter_map(|col| col.get("name").and_then(|v| v.as_str()))
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();
                let rows =
                    t.get("estimatedRowCount").and_then(|v| v.as_f64()).unwrap_or(0.0);
                out.push(Table {
                    origin: origin.to_string(),
                    database: dbname.clone(),
                    name,
                    comment,
                    column,
                    rows,
                });
            }
        }
    };

    take("local", root.get("database"));
    // `remote` is a LIST of per-host records, each carrying its own `database` array -- not a map
    // keyed by host, which is what the first draft assumed. It reported "1 machines" and silently
    // indexed only the local box.
    if let Some(remote) = root.get("remote").and_then(|r| r.as_array()) {
        for entry in remote {
            let host = entry.get("host_code").and_then(|v| v.as_str()).unwrap_or("remote");
            take(host, entry.get("database"));
        }
    }
    (!out.is_empty()).then_some(out)
}

/// A deterministic single-character typo, biased off the first character so the term stays findable.
fn typo(s: &str) -> String {
    let mut c: Vec<char> = s.chars().collect();
    if c.len() > 4 {
        let at = c.len() / 2;
        c[at] = if c[at] == 'x' { 'q' } else { 'x' };
    }
    c.into_iter().collect()
}

fn main() {
    let clock = timer::Clock::new();
    println!("p40-booted-schema :: searching every database in the house");
    println!("clock backend: {}\n", clock.backend());

    let path = bank_path();
    let Some(table) = load(&path) else {
        println!("SKIP: {} not found or empty.", path.display());
        println!("      Run `node web/server/schema-probe.mjs --remote` in booted, or set");
        println!("      BOOTED_SCHEMA to a schema bank.");
        println!("OVERALL: NO CORPUS — refusing to report a verdict.");
        std::process::exit(2);
    };

    let db_count = table.iter().map(|t| t.database.as_str()).collect::<std::collections::HashSet<_>>().len();
    let host_count = table.iter().map(|t| t.origin.as_str()).collect::<std::collections::HashSet<_>>().len();
    let with_comment = table.iter().filter(|t| !t.comment.is_empty()).count();
    println!(
        "  {} tables, {} databases, {} machines, {} with a comment, {:.0} estimated rows total",
        table.len(),
        db_count,
        host_count,
        with_comment,
        table.iter().map(|t| t.rows).sum::<f64>()
    );

    // name is what you type; database narrows it; the comment is what you search when you have
    // forgotten the name; columns catch "which table has `outlet_id`".
    let mut b = IndexBuilder::new(Schema::new(vec![
        Field::new("name", 3.0, 0.4),
        Field::new("database", 1.0, 0.6),
        // MAX_FIELD is 4, so comment and column names share a field. They answer the same kind
        // of question -- "what does this table do" and "what does it hold" -- and both are searched
        // when the name has been forgotten, so the merge costs nothing a separate boost would buy.
        Field::new("about", 0.5, 0.75),
        Field::new("rows", 0.0, 0.75),
    ]))
    .with_facet(1)
    .with_numeric(3);
    let t0 = std::time::Instant::now();
    for t in &table {
        b.add(&Doc::new(vec![
            t.name.as_str(),
            t.database.as_str(),
            &format!("{} {}", t.comment, t.column),
            &format!("{:.0}", t.rows),
        ]));
    }
    let ix = b.build().unwrap();
    let build_s = t0.elapsed().as_secs_f64();
    println!(
        "  built in {:.2} s, {} terms, {:.1} MB, {} databases as facets\n",
        build_s,
        ix.term_count(),
        ix.to_bytes().len() as f64 / 1e6,
        ix.facet_label_at(0).len()
    );

    let mut fail = 0usize;

    // ---- 1. Find a table by name, exactly and with a typo ------------------------------------
    // Table names repeat across databases -- `life_record` is in both `life` and
    // `life_restore_check` -- so rank-1 is scored against the SET of same-named tables. Demanding
    // one specific ordinal would be scoring the corpus, not the engine. (`p12` learned this on
    // maphy, where 12.8 % of place names were shared.)
    let mut by_name: HashMap<&str, Vec<u32>> = HashMap::new();
    for (i, t) in table.iter().enumerate() {
        by_name.entry(t.name.as_str()).or_default().push(i as u32);
    }
    let probe: Vec<&Table> = table.iter().step_by((table.len() / 400).max(1)).collect();

    let mut exact_ok = 0usize;
    let mut typo_ok = 0usize;
    let mut miss: Vec<(String, String)> = Vec::new();
    for t in &probe {
        let want = &by_name[t.name.as_str()];
        let got = ix.search(&t.name, 1);
        if got.first().is_some_and(|h| want.contains(&h.doc)) {
            exact_ok += 1;
        } else if miss.len() < 8 {
            // Print what actually won. A percentage says the name field underperforms; only the
            // losing pairs say whether that is the engine or the corpus.
            miss.push((
                t.name.clone(),
                got.first().map_or("(nothing)".into(), |h| {
                    let w = &table[h.doc as usize];
                    format!("{}/{}.{}", w.origin, w.database, w.name)
                }),
            ));
        }
        if ix.search(&typo(&t.name), 1).first().is_some_and(|h| want.contains(&h.doc)) {
            typo_ok += 1;
        }
    }
    let pct = |n: usize| 100.0 * n as f64 / probe.len() as f64;
    println!("  --- find a table by name ({} probes) ---", probe.len());
    println!("  exact name, rank 1: {:.1}%", pct(exact_ok));
    println!("  one typo,   rank 1: {:.1}%", pct(typo_ok));
    if !miss.is_empty() {
        println!("  exact-name misses -- queried name, then what outranked it:");
        for (q, w) in &miss {
            println!("    {q:<34} -> {w}");
        }
    }
    if pct(exact_ok) < 95.0 {
        println!("    exact-name recall below 95% -- the name field is not doing its job");
        fail += 1;
    }

    // ---- 2. Find a table from its COMMENT, not its name --------------------------------------
    // "Where do we store scraped articles?" -- the developer remembers what it does, not what it
    // is called. Queried with words from the comment that do NOT appear in the table name, so a
    // hit cannot be the name matching by accident.
    let mut concept = 0usize;
    let mut tried = 0usize;
    for t in table.iter().filter(|t| t.comment.split_whitespace().count() >= 8) {
        let name_words: std::collections::HashSet<String> =
            t.name.split(['_', ' ']).map(|w| w.to_lowercase()).collect();
        let q: Vec<&str> = t
            .comment
            .split_whitespace()
            .filter(|w| w.len() > 4 && !name_words.contains(&w.to_lowercase()))
            .take(5)
            .collect();
        if q.len() < 3 {
            continue;
        }
        tried += 1;
        if ix.search(&q.join(" "), 10).iter().any(|h| table[h.doc as usize].name == t.name) {
            concept += 1;
        }
        if tried >= 200 {
            break;
        }
    }
    println!("\n  --- find a table from its comment, name words removed ({tried} probes) ---");
    if tried > 0 {
        println!("  right table in top 10: {:.1}%", 100.0 * concept as f64 / tried as f64);
    } else {
        println!("  no table has a comment long enough to test");
    }

    // ---- 3. Facet by database, and range by row count ----------------------------------------
    let tally = ix.facet_tally("id");
    println!("\n  --- which databases have a table matching \"id\" ---");
    for (db, n) in tally.iter().take(5) {
        println!("  {n:>5}  {db}");
    }
    let big = ix.search_range("id", 20, 0, 100_000.0, f64::MAX);
    println!("\n  --- matching tables over 100,000 rows ---");
    for h in big.iter().take(5) {
        let t = &table[h.doc as usize];
        println!("  {:>10.0}  {}/{}.{}", t.rows, t.origin, t.database, t.name);
    }

    // ---- 4. Latency ---------------------------------------------------------------------------
    let q: Vec<String> = probe.iter().map(|t| t.name.clone()).collect();
    let dirty: Vec<String> = q.iter().map(|s| typo(s)).collect();
    let mut e = clock.time_each(q.len(), |i| ix.search(&q[i], 10).len() as u64);
    e.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut d = clock.time_each(dirty.len(), |i| ix.search(&dirty[i], 10).len() as u64);
    d.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let us = |v: &[f64], p: f64| timer::percentile(v, p) / 1000.0;
    println!("\n  --- latency ---");
    println!("  exact  p50 {:>6.0}us   p99 {:>6.0}us", us(&e, 0.5), us(&e, 0.99));
    println!("  typo   p50 {:>6.0}us   p99 {:>6.0}us", us(&d, 0.5), us(&d, 0.99));

    let worst = timer::percentile(&d, 0.99);
    println!(
        "\n  -> typo p99 {:.2} ms (bar {:.0} ms): {}",
        worst / 1e6,
        P99_NS_MAX / 1e6,
        if worst <= P99_NS_MAX { "PASS" } else { "FAIL" }
    );
    if worst > P99_NS_MAX {
        fail += 1;
    }

    println!("\nOVERALL: {}", if fail == 0 { "PASS" } else { "FAIL" });
    std::process::exit(i32::from(fail != 0));
}
