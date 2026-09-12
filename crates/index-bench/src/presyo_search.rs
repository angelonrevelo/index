//! presyo-search :: per-query cost on presyo's REAL storefront corpus, in process.
//!
//! WHY THIS EXISTS. Every latency figure taken against presyo so far was measured through the
//! CLI, one process per query, and was therefore useless: on both machines tried, 20 `search`
//! invocations and 20 no-op `stat` invocations cost the SAME wall clock (on the VPS `search`
//! came out *faster* than the no-op, 5.41 s vs 6.33 s). That is a measurement of `fork`+`exec`
//! +mmap, not of the engine. Comparing it to a warm in-process Postgres query would have been
//! meaningless in whichever direction the arithmetic happened to fall.
//!
//! This loads the corpus once and times the queries themselves, which is the shape the actual
//! integration has: presyo's API mmaps the artifact and never spawns anything.
//!
//! The corpus is the same CSV `scripts/index-build.sh` feeds the artifact — 35,637 rows of
//! `mv_pilot_ready_product` joined to product/brand, the exact document set that storefront is
//! permitted to return.
//!
//!   INDEX_PRESYO_CSV=/var/lib/presyo/idx/live/corpus.csv cargo run --release -p index-bench --bin presyo_search

use index_text::{Doc, Field, IndexBuilder, Schema};
use std::time::Instant;

mod timer;

/// The queries presyo's own latency work used, so the two sides are comparable: the gate-hit
/// class, the gate-miss class that the SQL path degrades on, and the two Filipino terms where
/// the engines actually disagree on quality.
const QUERY: &[&str] = &[
    "toblerone", "milo", "kitkat", "colgate", "nescafe", "safeguard", "coke",
    "shampoo", "sabon", "tsokolate", "tuna", "milk", "bear brand", "lucky me",
    "chocolate",
];

/// Minimal RFC4180-ish splitter. The corpus has quoted fields containing commas AND embedded
/// newlines (search_text is a concatenation), so a line-oriented split would corrupt rows.
fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut row = Vec::new();
    let mut cur_row = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        if quoted {
            if c == '"' {
                if it.peek() == Some(&'"') {
                    it.next();
                    cur.push('"');
                } else {
                    quoted = false;
                }
            } else {
                cur.push(c);
            }
        } else if c == '"' {
            quoted = true;
        } else if c == ',' {
            cur_row.push(std::mem::take(&mut cur));
        } else if c == '\n' {
            cur_row.push(std::mem::take(&mut cur));
            row.push(std::mem::take(&mut cur_row));
        } else if c != '\r' {
            cur.push(c);
        }
    }
    if !cur.is_empty() || !cur_row.is_empty() {
        cur_row.push(cur);
        row.push(cur_row);
    }
    row
}

fn main() {
    let path = std::env::var("INDEX_PRESYO_CSV")
        .unwrap_or_else(|_| "/tmp/presyo4h.csv".to_string());
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("presyo-search: cannot read {path}: {e}");
            eprintln!("set INDEX_PRESYO_CSV to the corpus.csv index-build.sh writes");
            std::process::exit(2);
        }
    };

    let row = parse_csv(&text);
    if row.is_empty() {
        eprintln!("presyo-search: empty corpus");
        std::process::exit(2);
    }
    // Header row maps names -> columns, exactly as `index build` does.
    let head = &row[0];
    let col = |name: &str| head.iter().position(|h| h == name);
    let (c_id, c_name, c_brand, c_text) = match (
        col("product_id"), col("product_name"), col("brand_name"), col("search_text"),
    ) {
        (Some(a), Some(b), Some(c), Some(d)) => (a, b, c, d),
        _ => {
            eprintln!("presyo-search: header must carry product_id,product_name,brand_name,search_text");
            eprintln!("  got: {head:?}");
            std::process::exit(2);
        }
    };

    // Same schema and weights scripts/index-build.sh ships, so this measures the artifact the
    // integration actually publishes rather than a bench-only tuning.
    let build_start = Instant::now();
    let mut builder = IndexBuilder::new(Schema::new(vec![
        Field::new("product_id", 0.0, 0.6),
        Field::new("product_name", 3.0, 0.4),
        Field::new("brand_name", 1.0, 0.6),
        Field::new("search_text", 1.0, 0.4),
    ]));
    let mut n = 0usize;
    for r in row.iter().skip(1) {
        if r.len() <= c_text {
            continue;
        }
        builder.add(&Doc::new(vec![
            r[c_id].clone(),
            r[c_name].clone(),
            r[c_brand].clone(),
            r[c_text].clone(),
        ]));
        n += 1;
    }
    let ix = builder.build().expect("index build failed");
    let build_ms = build_start.elapsed().as_secs_f64() * 1e3;

    println!("corpus      {n} docs from {path}");
    println!("build       {build_ms:.0} ms (in process)");

    // Warm once: the first query on a cold structure pays page-in costs that no subsequent
    // storefront request would.
    for q in QUERY {
        let _ = ix.search(q, 20);
    }

    // time_each returns per-op nanoseconds already sorted ascending, overhead subtracted.
    // REP passes over the query set so the percentiles have something to rank.
    const REP: usize = 200;
    let clock = timer::Clock::new();
    let n_op = QUERY.len() * REP;
    let ns = clock.time_each(n_op, |i| ix.search(QUERY[i % QUERY.len()], 20).len() as u64);

    println!();
    println!("per-query, k=20, {} queries x {REP} reps ({n_op} ops, {} timer)",
             QUERY.len(), clock.backend());
    println!("  p50       {:>10.1} us", timer::percentile(&ns, 0.50) / 1e3);
    println!("  p99       {:>10.1} us", timer::percentile(&ns, 0.99) / 1e3);
    println!("  max       {:>10.1} us", ns.last().copied().unwrap_or(0.0) / 1e3);

    println!();
    println!("per query (p50 of {REP}):");
    for q in QUERY {
        let one = clock.time_each(REP, |_| ix.search(q, 20).len() as u64);
        println!("  {:<12} {:>10.1} us", q, timer::percentile(&one, 0.50) / 1e3);
    }
}
