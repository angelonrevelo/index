//! p42-alec-surface :: does any of this hold on LONG documents?
//!
//! Every corpus this engine has been measured on is short-field: product names (241 k x ~6 words),
//! place names, business names, course titles, table names. `alec`'s scrape is the opposite shape
//! and the largest single data file in the house — **119,179 rows, 139 MB, a fixed 1,000 characters
//! of raw HTML context per row**.
//!
//! That matters for three things nothing else here tests:
//!
//! 1. **BM25 length normalisation** at a document length two orders of magnitude above the rest.
//! 2. **The pruning gates** (`p24`–`p26`), whose bounds were tuned and audited on short fields.
//!    Long, boilerplate-heavy documents mean huge posting lists and near-tied scores — the shape
//!    that made block-max skipping worth having in the first place.
//! 3. **Build cost and index size** when the text is 500x the identifier.
//!
//! Exactness is the headline: `search` must agree with brute force, on this shape too.

use index_text::{Doc, Field, IndexBuilder, Schema};
use std::collections::HashMap;
use std::path::PathBuf;

mod timer;

/// Interactive budget.
const P99_NS_MAX: f64 = 25_000_000.0;

struct Row {
    url: String,
    kind: String,
    value: String,
    context: String,
}

fn corpus_dir() -> PathBuf {
    std::env::var("INDEX_CORPUS_DIR").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("..")
    })
}

fn split_csv(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut it = line.chars().peekable();
    while let Some(c) = it.next() {
        match (c, quoted) {
            ('"', true) if it.peek() == Some(&'"') => {
                cur.push('"');
                it.next();
            }
            ('"', _) => quoted = !quoted,
            (',', false) => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

fn load() -> Option<Vec<Row>> {
    // Every `*_dump` under alec has the same shape; expressway is the largest.
    let path = corpus_dir()
        .join("alec")
        .join("expressway_dump")
        .join("csv")
        .join("source_surface.csv");
    let text = std::fs::read_to_string(path).ok()?;
    let mut line = text.lines();
    let head = split_csv(line.next()?);
    let at = |k: &str| head.iter().position(|h| h == k);
    let (i_url, i_kind, i_val, i_ctx) =
        (at("source_url")?, at("surface_type")?, at("value")?, at("context")?);
    let mut out = Vec::new();
    for l in line {
        let f = split_csv(l);
        if f.len() <= i_ctx {
            continue;
        }
        let value = f[i_val].trim().to_string();
        if value.is_empty() {
            continue;
        }
        out.push(Row {
            url: f[i_url].trim().to_string(),
            kind: f[i_kind].trim().to_string(),
            value,
            context: f[i_ctx].clone(),
        });
    }
    (!out.is_empty()).then_some(out)
}

fn typo(s: &str) -> String {
    let mut c: Vec<char> = s.chars().collect();
    if c.len() > 6 {
        let at = c.len() / 2;
        c[at] = if c[at] == 'x' { 'q' } else { 'x' };
    }
    c.into_iter().collect()
}

fn main() {
    let clock = timer::Clock::new();
    println!("p42-alec-surface :: long documents, on the largest data file in the house");
    println!("clock backend: {}\n", clock.backend());

    let Some(row) = load() else {
        println!("SKIP: alec/expressway_dump/csv/source_surface.csv not found.");
        println!("      Set INDEX_CORPUS_DIR to the directory holding the alec checkout.");
        println!("OVERALL: NO CORPUS — refusing to report a verdict.");
        std::process::exit(2);
    };

    let text_bytes: usize = row.iter().map(|r| r.context.len() + r.value.len()).sum();
    println!(
        "  {} rows, {:.1} MB of text, mean context {} chars",
        row.len(),
        text_bytes as f64 / 1e6,
        row.iter().map(|r| r.context.len()).sum::<usize>() / row.len().max(1)
    );

    let mut b = IndexBuilder::new(Schema::new(vec![
        Field::new("value", 3.0, 0.4),
        Field::new("kind", 1.0, 0.6),
        Field::new("url", 1.0, 0.6),
        // The long one. b = 0.75 is the BM25 default and the right choice here: with 1,000-char
        // bodies, length normalisation is doing real work rather than the near-no-op it is on a
        // six-word product name.
        Field::new("context", 0.5, 0.75),
    ]))
    .with_facet(1);
    let t0 = std::time::Instant::now();
    for r in &row {
        b.add(&Doc::new(vec![
            r.value.as_str(),
            r.kind.as_str(),
            r.url.as_str(),
            r.context.as_str(),
        ]));
    }
    let ix = b.build().unwrap();
    let build_s = t0.elapsed().as_secs_f64();
    let bytes = ix.to_bytes().len();
    println!(
        "  built in {:.1} s, {} terms, {:.1} MB index ({:.0} B/doc, {:.1}% of the text)\n",
        build_s,
        ix.term_count(),
        bytes as f64 / 1e6,
        bytes as f64 / row.len() as f64,
        100.0 * bytes as f64 / text_bytes as f64
    );

    let mut fail = 0usize;

    // ---- 1. EXACTNESS on this shape ----------------------------------------------------------
    // The pruning gates were designed and audited on short fields. Long, boilerplate-heavy bodies
    // give huge posting lists and near-tied scores, which is exactly the shape block-max skipping
    // exists for -- and the shape most likely to expose a bound that is not really a bound.
    let probe: Vec<&Row> = row.iter().step_by((row.len() / 300).max(1)).collect();
    let mut differ = 0usize;
    let mut worse = 0usize;
    for r in probe.iter().take(300) {
        let q: String = r.context.split_whitespace().take(6).collect::<Vec<_>>().join(" ");
        if q.trim().is_empty() {
            continue;
        }
        let fast: Vec<u32> = ix.search(&q, 10).iter().map(|h| h.doc).collect();
        let slow = ix.search_exhaustive_unpooled(&q, 10);
        let slow_doc: Vec<u32> = slow.iter().map(|h| h.doc).collect();
        if fast != slow_doc {
            differ += 1;
            // Did pruning actually LOSE something better, or merely reorder near-ties?
            let fast_worst = ix.search(&q, 10).last().map(|h| h.typo_bucket).unwrap_or(0);
            if slow.iter().any(|h| h.typo_bucket < fast_worst && !fast.contains(&h.doc)) {
                worse += 1;
            }
        }
    }
    println!("  --- exactness on 1,000-char documents (300 queries from context text) ---");
    println!("  top-10 differs from brute force: {differ}");
    println!("  lost a better-bucket document:   {worse}");
    if worse > 0 {
        println!("    pruning loses documents on long bodies -- the gates do not hold on this shape");
        fail += 1;
    }

    // ---- 2. Find a row by its value ----------------------------------------------------------
    // Values repeat heavily (the same URL appears on many pages), so rank-1 is scored against the
    // SET of rows carrying that value, as `p12` and `p40` both had to do.
    let mut by_value: HashMap<&str, Vec<u32>> = HashMap::new();
    for (i, r) in row.iter().enumerate() {
        by_value.entry(r.value.as_str()).or_default().push(i as u32);
    }
    let mut exact_ok = 0usize;
    let mut typo_ok = 0usize;
    let vp: Vec<&Row> = probe.iter().take(300).copied().collect();
    for r in &vp {
        let want = &by_value[r.value.as_str()];
        if ix.search(&r.value, 1).first().is_some_and(|h| want.contains(&h.doc)) {
            exact_ok += 1;
        }
        if ix.search(&typo(&r.value), 1).first().is_some_and(|h| want.contains(&h.doc)) {
            typo_ok += 1;
        }
    }
    let pct = |n: usize| 100.0 * n as f64 / vp.len().max(1) as f64;
    println!("\n  --- find a row by its value ({} probes) ---", vp.len());
    println!("  exact value, rank 1: {:.1}%", pct(exact_ok));
    println!("  one typo,    rank 1: {:.1}%", pct(typo_ok));

    // ---- 3. Facet, and latency ---------------------------------------------------------------
    let tally = ix.facet_tally("https");
    println!("\n  --- surface types matching \"https\" ---");
    for (k, n) in tally.iter().take(5) {
        println!("  {n:>7}  {k}");
    }

    let q: Vec<String> = vp.iter().map(|r| r.value.clone()).collect();
    let long: Vec<String> = vp
        .iter()
        .map(|r| r.context.split_whitespace().take(8).collect::<Vec<_>>().join(" "))
        .collect();
    let mut e = clock.time_each(q.len(), |i| ix.search(&q[i], 10).len() as u64);
    e.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut l = clock.time_each(long.len(), |i| ix.search(&long[i], 10).len() as u64);
    l.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let us = |v: &[f64], p: f64| timer::percentile(v, p) / 1000.0;
    println!("\n  --- latency ---");
    println!("  short query (a value)   p50 {:>7.0}us  p99 {:>8.0}us", us(&e, 0.5), us(&e, 0.99));
    println!("  8-word query from body  p50 {:>7.0}us  p99 {:>8.0}us", us(&l, 0.5), us(&l, 0.99));

    let worst = timer::percentile(&l, 0.99);
    println!(
        "\n  -> body-query p99 {:.2} ms (bar {:.0} ms): {}",
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
