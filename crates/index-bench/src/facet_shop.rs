//! p30-facet-shop :: does the engine support the interaction shopping search actually is?
//!
//! Every prior bench asks "is the top-10 right". Shopping search is not only ranking — it is
//! **filter and count**: a shopper types "milk", sees `Dairy (412)  Snacks (37)  Beverage (18)`,
//! clicks one, and expects a full page of results *from inside that category*, not whatever
//! survived a global top-10.
//!
//! The engine had no facet capability at all before `p30`. This bin measures the one that was
//! added, on presyo's real catalogue, against brute force:
//!
//! 1. **Are tallies exact?** Counted over every matching document, not the top `k`.
//! 2. **Is filtered search filter-then-rank?** A rare category must still fill a page.
//! 3. **What do facets cost** in bytes and in latency?

use index_text::{Doc, Field, IndexBuilder, Schema};
use std::collections::HashMap;
use std::path::PathBuf;

mod timer;

/// A category needs this many products to be worth querying.
const MIN_CATEGORY: usize = 200;
/// Interactive budget for a tally, which runs on every keystroke that changes results.
const TALLY_P99_NS_MAX: f64 = 25_000_000.0;

struct Product {
    name: String,
    brand: String,
    category: String,
    /// `size_value` verbatim. Kept as text so the engine does the parsing it would do in
    /// production, including deciding what counts as absent.
    size: String,
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

fn load(path: &PathBuf) -> Option<Vec<Product>> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut line = text.lines();
    let head = split_csv(line.next()?);
    let at = |k: &str| head.iter().position(|h| h == k);
    let (i_name, i_brand, i_cat) =
        (at("product_name")?, at("brand_name")?, at("category_name")?);
    let i_size = at("size_value");
    let mut out = Vec::new();
    for l in line {
        let f = split_csv(l);
        if f.len() <= i_cat {
            continue;
        }
        let name = f[i_name].trim().to_string();
        if name.is_empty() {
            continue;
        }
        out.push(Product {
            name,
            brand: f[i_brand].trim().to_string(),
            category: f[i_cat].trim().to_string(),
            size: i_size.and_then(|i| f.get(i)).map(|v| v.trim().to_string()).unwrap_or_default(),
        });
    }
    Some(out)
}

fn main() {
    let clock = timer::Clock::new();
    println!("p30-facet-shop :: filter and count on presyo's real catalogue");
    println!("clock backend: {}\n", clock.backend());

    let path = corpus_dir()
        .join("presyo")
        .join("data")
        .join("endless-prep")
        .join("catalog-active.csv");
    let Some(product) = load(&path) else {
        println!("SKIP: {} not found.", path.display());
        println!("      Set INDEX_CORPUS_DIR to the directory holding the presyo checkout.");
        println!("OVERALL: NO CORPUS — refusing to report a verdict.");
        std::process::exit(2);
    };
    println!("  {} real products", product.len());

    let schema = || {
        Schema::new(vec![
            Field::new("name", 3.0, 0.5),
            Field::new("brand", 0.5, 0.75),
            // boost 0.0 keeps the category out of scoring. It is NOT match-free -- a match still
            // earns bucket 0, which cost 9.4 points of falsely-attributed gain in p15 -- but here
            // the field exists to be faceted on, and that is orthogonal to scoring.
            Field::new("category", 0.0, 0.75),
            // Numeric, so boost 0.0: it exists to be ranged on, not scored on.
            Field::new("size", 0.0, 0.75),
        ])
    };

    // Two indexes over identical rows: one faceted, one not. The pair prices the feature.
    let build = |facet: bool| {
        let mut b = IndexBuilder::new(schema());
        if facet {
            // Two facet slots -- brand and category, the pair a storefront filters on -- plus a
            // numeric column for the size slider.
            b = b.with_facet(1).with_facet(2).with_numeric(3);
        }
        let t0 = std::time::Instant::now();
        for p in &product {
            b.add(&Doc::new(vec![
                p.name.as_str(),
                p.brand.as_str(),
                p.category.as_str(),
                p.size.as_str(),
            ]));
        }
        let ix = b.build().unwrap();
        (ix, t0.elapsed().as_secs_f64())
    };
    let (plain, plain_s) = build(false);
    let (ix, facet_s) = build(true);

    let plain_byte = plain.to_bytes().len();
    let facet_byte = ix.to_bytes().len();
    println!(
        "  build {plain_s:.1} s -> {facet_s:.1} s   index {:.1} MB -> {:.1} MB  (+{} B, {:.2} B/doc)",
        plain_byte as f64 / 1e6,
        facet_byte as f64 / 1e6,
        facet_byte - plain_byte,
        (facet_byte - plain_byte) as f64 / product.len() as f64
    );
    println!(
        "  {} facet slots: {} brands (slot 0), {} categories (slot 1)\n",
        ix.facet_slot_count(),
        ix.facet_label_at(0).len(),
        ix.facet_label_at(1).len()
    );

    // Query set: category names with enough products to be worth faceting.
    let mut by_cat: HashMap<&str, usize> = HashMap::new();
    for p in &product {
        if !p.category.is_empty() && p.category != "Uncategorized" {
            *by_cat.entry(p.category.as_str()).or_default() += 1;
        }
    }
    let mut big: Vec<&str> =
        by_cat.iter().filter(|(_, &n)| n >= MIN_CATEGORY).map(|(c, _)| *c).collect();
    big.sort_unstable();

    // Queries a shopper would type: the first word of a real product name.
    let mut probe: Vec<String> = Vec::new();
    let step = (product.len() / 400).max(1);
    for p in product.iter().step_by(step) {
        if let Some(w) = p.name.split_whitespace().next() {
            if w.len() >= 4 {
                probe.push(w.to_string());
            }
        }
        if probe.len() >= 400 {
            break;
        }
    }
    println!("  {} facetable categories, {} probe queries\n", big.len(), probe.len());

    let mut fail = 0usize;

    // ---- 1. Tallies must be exact, counted over every match ----------------------------------
    // Brute force: a document counts if any of its analyzed text matches the query the way the
    // engine's own planner would. Rather than reimplement the planner, the oracle is the engine's
    // exhaustive search with k = doc_count, which is a different code path from `facet_tally`.
    let mut checked = 0usize;
    let mut wrong = 0usize;
    for q in probe.iter().take(40) {
        let tally: HashMap<&str, usize> = ix.facet_tally_at(q, 1).into_iter().collect();
        let mut truth: HashMap<&str, usize> = HashMap::new();
        for h in ix.search_exhaustive(q, product.len()) {
            if let Some(c) = ix.facet_of_at(h.doc, 1) {
                *truth.entry(c).or_default() += 1;
            }
        }
        checked += 1;
        if tally != truth {
            wrong += 1;
            if wrong <= 2 {
                let mut a: Vec<_> = tally.iter().collect();
                let mut b: Vec<_> = truth.iter().collect();
                a.sort();
                b.sort();
                println!("    MISMATCH {q:?}: got {} categories, truth {}", a.len(), b.len());
            }
        }
    }
    println!("  --- tally correctness ---");
    println!("  {checked} queries checked against exhaustive scoring, {wrong} wrong");
    if wrong > 0 {
        fail += 1;
    }

    // ---- 2. Filter-then-rank: a rare category must still fill a page --------------------------
    // The trap this exists to catch is rank-then-filter, where asking for 10 in a small category
    // returns 1 because only 1 of the global top-10 was in it.
    let mut short = 0usize;
    let mut leaked = 0usize;
    let mut tried = 0usize;
    for q in probe.iter().take(120) {
        let tally = ix.facet_tally_at(q, 1);
        // The SMALLEST category with at least 10 matches: the hardest case for rank-then-filter.
        let Some(&(cat, n)) = tally.iter().rfind(|(_, n)| *n >= 10) else {
            continue;
        };
        tried += 1;
        let hit = ix.search_facet_at(q, 10, 1, cat);
        if hit.len() < 10 {
            short += 1;
            if short <= 2 {
                println!("    SHORT {q:?} in {cat:?}: {} of 10, {n} exist", hit.len());
            }
        }
        if hit.iter().any(|h| ix.facet_of_at(h.doc, 1) != Some(cat)) {
            leaked += 1;
        }
    }
    println!("\n  --- filter-then-rank ---");
    println!("  {tried} (query, smallest category with >=10 matches) pairs");
    println!("  {short} returned a short page, {leaked} leaked a document from another category");
    if short > 0 || leaked > 0 {
        fail += 1;
    }

    // ---- 3. Cost -----------------------------------------------------------------------------
    let mut s = clock.time_each(probe.len(), |i| ix.search(&probe[i], 10).len() as u64);
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let cat: Vec<&str> = probe.iter().map(|_| big[0]).collect();
    let mut f = clock.time_each(probe.len(), |i| ix.search_facet_at(&probe[i], 10, 1, cat[i]).len() as u64);
    f.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut t = clock.time_each(probe.len(), |i| ix.facet_tally_at(&probe[i], 1).len() as u64);
    t.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let us = |v: &[f64], p: f64| timer::percentile(v, p) / 1000.0;
    // ---- 4. Conjunction: brand AND category ---------------------------------------------------
    // The pair a storefront filters on. Checked against the intersection computed independently
    // from a single-slot result, so a bug in the one-pass path cannot hide behind itself.
    let mut pair = 0usize;
    let mut mismatch = 0usize;
    for q in probe.iter().take(80) {
        let bt = ix.facet_tally_at(q, 0);
        let ct = ix.facet_tally_at(q, 1);
        let (Some(&(brand, _)), Some(&(cat, _))) = (bt.first(), ct.first()) else { continue };
        pair += 1;
        let got: Vec<u32> =
            ix.search_facet_all(q, 50, &[(0, brand), (1, cat)]).iter().map(|h| h.doc).collect();
        // Filter a single-slot result by the other slot. That result is the top 50 by brand, so it
        // can only be a SUBSET of the true conjunction -- a document ranked below 50 on brand alone
        // may still belong. Containment in this direction is the direction that can be wrong.
        let want: Vec<u32> = ix
            .search_facet_at(q, 50, 0, brand)
            .iter()
            .filter(|h| ix.facet_of_at(h.doc, 1) == Some(cat))
            .map(|h| h.doc)
            .collect();
        if !want.iter().all(|d| got.contains(d)) {
            mismatch += 1;
            if mismatch <= 2 {
                println!(
                    "    MISMATCH {q:?} {brand:?}+{cat:?}: got {}, missing from superset of {}",
                    got.len(),
                    want.len()
                );
            }
        }
        if got
            .iter()
            .any(|&d| ix.facet_of_at(d, 0) != Some(brand) || ix.facet_of_at(d, 1) != Some(cat))
        {
            mismatch += 1;
        }
    }
    println!("\n  --- conjunction (brand AND category) ---");
    println!("  {pair} pairs vs independently intersected single-slot results, {mismatch} wrong");
    if mismatch > 0 {
        fail += 1;
    }

    // ---- 5. Numeric range: histogram partitions, filter agrees with it ------------------------
    // presyo's export carries no price column, so this ranges over `size_value`, which is the
    // numeric attribute it does carry. The shape is identical to a price slider.
    let edge = [0.0f64, 50.0, 100.0, 250.0, 500.0, 1000.0, 1.0e9];
    let mut hist_wrong = 0usize;
    let mut checked_hist = 0usize;
    for q in probe.iter().take(60) {
        let hist = ix.range_tally(q, 0, &edge);
        checked_hist += 1;
        // 1. Each bucket count must equal what the range FILTER returns for that bucket, computed
        //    by a different code path. k is the corpus size so the filter is not truncated.
        for (i, &c) in hist.iter().enumerate() {
            let got = ix.search_range(q, product.len(), 0, edge[i], edge[i + 1]).len();
            if got != c {
                hist_wrong += 1;
                if hist_wrong <= 2 {
                    println!("    HIST {q:?} bucket {i}: tally {c}, filter {got}");
                }
                break;
            }
        }
        // 2. Buckets must partition: the total cannot exceed the number of matching documents, and
        //    a boundary value must not be counted twice.
        let total: usize = hist.iter().sum();
        let matched = ix.search_exhaustive(q, product.len()).len();
        if total > matched {
            hist_wrong += 1;
            if hist_wrong <= 3 {
                println!("    OVERCOUNT {q:?}: buckets sum to {total} of {matched} matches");
            }
        }
    }
    println!("\n  --- numeric range (size_value) ---");
    println!("  {checked_hist} histograms vs the range filter and vs the match count, {hist_wrong} wrong");
    if hist_wrong > 0 {
        fail += 1;
    }

    // ---- 6. Sort by value ---------------------------------------------------------------------
    // Two properties, neither of which a self-consistent implementation would give for free:
    //   a) the result is actually ordered by the column, ascending and descending;
    //   b) it agrees with the RANGE FILTER about membership -- same documents, different order --
    //      so a bug that drops or invents rows cannot hide behind the ordering being right.
    let mut sort_wrong = 0usize;
    let mut checked_sort = 0usize;
    for q in probe.iter().take(60) {
        let asc = ix.search_sorted(q, 25, 0, true);
        let desc = ix.search_sorted(q, 25, 0, false);
        checked_sort += 1;

        let val = |h: &index_text::Hit| ix.numeric_of(h.doc, 0).unwrap_or(f64::NAN);
        if asc.windows(2).any(|w| val(&w[0]) > val(&w[1])) {
            sort_wrong += 1;
            if sort_wrong <= 2 {
                println!("    UNSORTED asc {q:?}");
            }
            continue;
        }
        if desc.windows(2).any(|w| val(&w[0]) < val(&w[1])) {
            sort_wrong += 1;
            continue;
        }
        // Every returned row must have a value, and must be inside the range that spans them.
        if asc.iter().any(|h| ix.numeric_of(h.doc, 0).is_none()) {
            sort_wrong += 1;
            continue;
        }
        // Membership: the k cheapest must all appear in a range filter that covers them.
        if let (Some(first), Some(last)) = (asc.first(), asc.last()) {
            let lo = val(first);
            let hi = val(last);
            let within: std::collections::HashSet<u32> = ix
                .search_range(q, product.len(), 0, lo, hi + 1.0)
                .iter()
                .map(|h| h.doc)
                .collect();
            if !asc.iter().all(|h| within.contains(&h.doc)) {
                sort_wrong += 1;
                if sort_wrong <= 3 {
                    println!("    MEMBERSHIP {q:?}: sorted rows missing from the equivalent range");
                }
            }
        }
    }
    println!("\n  --- sort by value ---");
    println!("  {checked_sort} queries checked for order and for agreement with the range filter, {sort_wrong} wrong");
    if sort_wrong > 0 {
        fail += 1;
    }

    // ---- 6b. p46: the two sort arms -----------------------------------------------------------
    //
    // `p32` measured the sort tail at 8x a ranked search and named the reason: a numeric order
    // gives the posting scan nothing to stop on. `p46` added a second arm that walks the value
    // order and stops after k matches, and a cost estimate that picks between them.
    //
    // Two implementations of one answer is a bug factory. The unit test holds them to agreeing on
    // a 61-row fixture; this holds them to agreeing on every real query in the probe set, which is
    // the only version of that claim worth making.
    let mut arm_wrong = 0usize;
    for q in probe.iter() {
        for asc in [true, false] {
            let walk = ix.search_sorted_arm(q, 10, 0, asc, true);
            let scan = ix.search_sorted_arm(q, 10, 0, asc, false);
            if walk != scan {
                arm_wrong += 1;
                if arm_wrong <= 3 {
                    println!("    ARMS DIFFER {q:?} ascending={asc}");
                }
            }
        }
    }
    println!("\n  --- p46: sort arms agree ---");
    println!("  {} queries x 2 directions, {arm_wrong} disagreements", probe.len());
    if arm_wrong > 0 {
        fail += 1;
    }

    let mut walk_t = clock.time_each(probe.len(), |i| {
        ix.search_sorted_arm(&probe[i], 10, 0, true, true).len() as u64
    });
    walk_t.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut scan_t = clock.time_each(probe.len(), |i| {
        ix.search_sorted_arm(&probe[i], 10, 0, true, false).len() as u64
    });
    scan_t.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let mut so = clock.time_each(probe.len(), |i| {
        ix.search_sorted(&probe[i], 10, 0, true).len() as u64
    });
    so.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let mut r = clock.time_each(probe.len(), |i| {
        ix.range_tally(&probe[i], 0, &edge).len() as u64
    });
    r.sort_by(|a, b| a.partial_cmp(b).unwrap());

    println!("\n  --- cost, {} queries ---", probe.len());
    println!("  {:<22} {:>10} {:>10}", "", "p50", "p99");
    println!("  {:<22} {:>8.0}us {:>8.0}us", "search (unfiltered)", us(&s, 0.5), us(&s, 0.99));
    println!("  {:<22} {:>8.0}us {:>8.0}us", "search_facet", us(&f, 0.5), us(&f, 0.99));
    println!("  {:<22} {:>8.0}us {:>8.0}us", "facet_tally", us(&t, 0.5), us(&t, 0.99));
    println!("  {:<22} {:>8.0}us {:>8.0}us", "range_tally", us(&r, 0.5), us(&r, 0.99));
    println!("  {:<22} {:>8.0}us {:>8.0}us", "search_sorted", us(&so, 0.5), us(&so, 0.99));
    println!("  {:<22} {:>8.0}us {:>8.0}us", "  arm: scan (p32)", us(&scan_t, 0.5), us(&scan_t, 0.99));
    println!("  {:<22} {:>8.0}us {:>8.0}us", "  arm: walk (p46)", us(&walk_t, 0.5), us(&walk_t, 0.99));

    let tally_p99 = timer::percentile(&t, 0.99);
    println!(
        "\n  -> tally p99 {:.1} ms (bar {:.0} ms): {}",
        tally_p99 / 1e6,
        TALLY_P99_NS_MAX / 1e6,
        if tally_p99 <= TALLY_P99_NS_MAX { "PASS" } else { "FAIL" }
    );
    if tally_p99 > TALLY_P99_NS_MAX {
        fail += 1;
    }

    println!("\nOVERALL: {}", if fail == 0 { "PASS" } else { "FAIL" });
    std::process::exit(if fail == 0 { 0 } else { 1 });
}
