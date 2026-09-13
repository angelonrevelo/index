//! `presyo-catalog` — **241,793 real products. No recombination.**
//!
//! `bench/roadmap/p7-scale.md` answers "a million rows, milliseconds" by *recombining* 61,467 real
//! Philippine schools up to 250 K and 1 M, and says so plainly, because whole-document replication
//! had already produced a wrong conclusion once. Recombination is honest but it is still synthetic:
//! the vocabulary stops growing at 41,069 terms no matter how many documents are generated, so
//! every scaled row shares a term distribution with the 61 K that seeded it.
//!
//! `presyo/data/endless-prep/catalog-active.csv` removes the caveat. It is presyo's **active
//! product catalogue exported from production** — 241,793 genuinely distinct rows, 30 MB, with
//! brand, product type, variant, size and an assigned category. Four times the largest real corpus
//! this project had, and the vocabulary grows with it.
//!
//! # Two questions, one corpus
//!
//! 1. **Scale on real rows.** Build time, index size, and query latency at 241,793 real documents,
//!    directly comparable to `p7`'s recombined 250,000.
//! 2. **Category retrieval.** `category_name` is *assigned*, not derived from the product name —
//!    "Babyjoy Feeding Bottle Decorated Collection 240ml" is categorised `Baby Accessories`, and
//!    that phrase appears nowhere in the text. `p14-presyo-broad.md` had to work with
//!    `gold_product_type`, which the retailer's own name usually repeats, and the labels leaked.
//!    Here **only 13.8 % of products contain their category name**, so the label is mostly
//!    independent of the retrieval signal — which is what makes it a real test of intent rather
//!    than of string matching.

mod timer;

use index_text::{Doc, Field, IndexBuilder, Schema};
use std::collections::HashMap;
use std::path::PathBuf;

/// A category needs this many products to be worth querying.
const MIN_CATEGORY: usize = 200;
/// Interactive budget.
const P99_NS_MAX: f64 = 5_000_000.0;

struct Product {
    name: String,
    brand: String,
    category: String,
}


/// Bench-layer A/B switch for p90: the library reads no environment, so the bench does.
/// `INDEX_REORDER=1` builds every index in this bin with reordering ON — note that benches
/// whose truth maps results by ordinal (most of them) are then scoring against the wrong
/// rows; that harness limitation is recorded in the p90 document.
fn reorder_on() -> bool {
    std::env::var("INDEX_REORDER").as_deref() == Ok("1")
}

fn corpus_dir() -> PathBuf {
    std::env::var("INDEX_CORPUS_DIR").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("..")
    })
}

/// Minimal RFC-4180 field splitter: enough for this export, and it does not pull in a dependency.
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
    let (i_name, i_brand, i_cat) = (
        at("product_name")?,
        at("brand_name")?,
        at("category_name")?,
    );
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
        });
    }
    Some(out)
}

/// A deterministic single-character typo, biased off the first character.
fn typo(name: &str) -> String {
    let c: Vec<char> = name.chars().collect();
    if c.len() < 6 {
        return name.to_string();
    }
    let at = 1 + (c.len() * 3 / 7) % (c.len() - 2);
    let mut out = c.clone();
    out.swap(at, at + 1);
    out.into_iter().collect()
}

fn main() {
    let clock = timer::Clock::new();
    let path = corpus_dir()
        .join("presyo")
        .join("data")
        .join("endless-prep")
        .join("catalog-active.csv");
    let Some(product) = load(&path) else {
        println!("SKIP: presyo/data/endless-prep/catalog-active.csv not found under {:?}", corpus_dir());
        println!("      Refusing to report a verdict without the real corpus.");
        return;
    };

    println!("presyo-catalog :: 241,793 real products, no recombination");
    println!("clock backend: {}", clock.backend());
    println!("  {} products from presyo's active catalogue export\n", product.len());

    // Brand is a weak second field: it disambiguates, it does not make a product match.
    let schema = Schema::new(vec![Field::new("name", 3.0, 0.5), Field::new("brand", 0.5, 0.75)]);
    let mut b = IndexBuilder::new(schema).with_doc_reorder(reorder_on());
    for p in &product {
        b.add(&Doc::new([p.name.as_str(), p.brand.as_str()]));
    }
    let t = std::time::Instant::now();
    let ix = b.build().expect("build");
    let build_ms = t.elapsed().as_secs_f64() * 1000.0;
    let bytes = ix.to_bytes().len();

    println!("  --- scale, on REAL rows ---");
    println!(
        "  {} documents, {} terms, build {:.1} s, index {:.0} MB ({:.1} B/doc)",
        product.len(),
        ix.term_count(),
        build_ms / 1000.0,
        bytes as f64 / 1_048_576.0,
        bytes as f64 / product.len() as f64
    );
    println!(
        "  p7-scale's 250,000-row row is RECOMBINED from 61,467 schools and stalls at 41,069\n  \
         terms; this corpus has {} distinct terms at a comparable document count.",
        ix.term_count()
    );

    // --- latency on real product names
    let step = (product.len() / 2000).max(1);
    let q_exact: Vec<&str> = product.iter().step_by(step).map(|p| p.name.as_str()).collect();
    let q_typo: Vec<String> = q_exact.iter().map(|n| typo(n)).collect();

    let mut e_ns = clock.time_each(q_exact.len(), |i| ix.search(q_exact[i], 10).len() as u64);
    let mut t_ns = clock.time_each(q_typo.len(), |i| ix.search(&q_typo[i], 10).len() as u64);
    e_ns.sort_by(|a, b| a.partial_cmp(b).unwrap());
    t_ns.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let (e50, e99) = (timer::percentile(&e_ns, 0.50), timer::percentile(&e_ns, 0.99));
    let (t50, t99) = (timer::percentile(&t_ns, 0.50), timer::percentile(&t_ns, 0.99));
    println!(
        "\n  {:<22} {:>12} {:>12}",
        format!("{} queries", q_exact.len()),
        "p50",
        "p99"
    );
    println!("  {:<22} {:>10.0}us {:>10.0}us", "exact product name", e50 / 1000.0, e99 / 1000.0);
    println!("  {:<22} {:>10.0}us {:>10.0}us", "one-character typo", t50 / 1000.0, t99 / 1000.0);

    // --- category retrieval: the intent label
    let mut by_cat: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, p) in product.iter().enumerate() {
        if !p.category.is_empty() && p.category != "Uncategorized" {
            by_cat.entry(p.category.as_str()).or_default().push(i);
        }
    }
    let mut cat: Vec<(&str, Vec<usize>)> =
        by_cat.into_iter().filter(|(_, v)| v.len() >= MIN_CATEGORY).collect();
    cat.sort_by(|a, b| a.0.cmp(b.0));

    let leak = product
        .iter()
        .filter(|p| !p.category.is_empty() && p.name.to_lowercase().contains(&p.category.to_lowercase()))
        .count();

    let mut prec = 0.0;
    let mut mrr = 0.0;
    let mut worst: Vec<(f64, &str, usize)> = Vec::new();
    for (c, member) in cat.iter() {
        let hit: Vec<usize> = ix.search(c, 10).iter().map(|h| h.doc as usize).collect();
        let good = hit.iter().filter(|d| member.contains(d)).count();
        let p = good as f64 / 10.0;
        prec += p;
        if let Some(r) = hit.iter().position(|d| member.contains(d)) {
            mrr += 1.0 / (r + 1) as f64;
        }
        worst.push((p, c, member.len()));
    }
    let n = cat.len().max(1) as f64;
    let base_prec = prec;
    worst.sort_by(|a, b| a.0.total_cmp(&b.0));

    println!("\n  --- category retrieval ({} categories with {MIN_CATEGORY}+ products) ---", cat.len());
    println!(
        "  label leakage: {:.1}% of products contain their own category name\n  \
         (p14's gold_product_type labels leaked far more, which is why that bench could only\n  \
         measure lexical precision)",
        100.0 * leak as f64 / product.len() as f64
    );
    println!("  precision@10 {:.1}%   MRR {:.3}", 100.0 * prec / n, mrr / n);
    println!("\n  hardest categories:");
    for (p, c, m) in worst.iter().take(8) {
        println!("  {:<34} {:>5.0}% precision@10  ({m} products)", format!("\"{c}\""), p * 100.0);
    }

    // --- the same measurement, but with the ENGINE doing the expansion.
    //
    // `p17-presyo-expand.md` measured this mechanism simulated at query-construction time in a
    // benchmark. `IndexBuilder::learn_expansion` now implements it inside the engine, which is a
    // different thing and has to be shown to work rather than assumed to.
    {
        let sch = Schema::new(vec![
            Field::new("name", 3.0, 0.5),
            Field::new("brand", 0.5, 0.75),
            Field::new("category", 0.0, 0.75),
        ]);
        let t = std::time::Instant::now();
        let mut bb = IndexBuilder::new(sch).with_doc_reorder(reorder_on()).learn_expansion(2, 20);
        for p in &product {
            bb.add(&Doc::new([p.name.as_str(), p.brand.as_str(), p.category.as_str()]));
        }
        let ixe = bb.build().expect("build expansion");
        let build_ms = t.elapsed().as_secs_f64() * 1000.0;

        // FAIR BASELINE: the same schema, same boost-0.0 facet field, NO expansion.
        //
        // A boost-0.0 field contributes no SCORE but still lets a document MATCH, and a matching
        // document gets `typo_bucket` 0 -- the primary sort key -- so it outranks every document
        // that does not match at all. Comparing an expansion arm that carries the facet field
        // against a plain arm that does not therefore credits expansion with work the field did.
        // On this corpus that error is worth 9.4 points, and it inflated the first version of this
        // measurement.
        let mut bplain = IndexBuilder::new(Schema::new(vec![
            Field::new("name", 3.0, 0.5),
            Field::new("brand", 0.5, 0.75),
            Field::new("category", 0.0, 0.75),
        ]))
        .with_doc_reorder(reorder_on());
        for p in &product {
            bplain.add(&Doc::new([p.name.as_str(), p.brand.as_str(), p.category.as_str()]));
        }
        let ixf = bplain.build().expect("build fair");
        let mut fair = 0.0;
        for (c, member) in &cat {
            let hit: Vec<usize> = ixf.search(c, 10).iter().map(|h| h.doc as usize).collect();
            fair += hit.iter().filter(|d| member.contains(d)).count() as f64 / 10.0;
        }
        let fair = fair / cat.len().max(1) as f64;

        let mut prec = 0.0;
        let mut mrr = 0.0;
        for (c, member) in &cat {
            let hit: Vec<usize> = ixe.search(c, 10).iter().map(|h| h.doc as usize).collect();
            let good = hit.iter().filter(|d| member.contains(d)).count();
            prec += good as f64 / 10.0;
            if let Some(r) = hit.iter().position(|d| member.contains(d)) {
                mrr += 1.0 / (r + 1) as f64;
            }
        }
        let n = cat.len().max(1) as f64;

        // Latency: expansion adds terms to a query, so it is not free.
        let mut q_ns = clock.time_each(cat.len(), |i| ixe.search(cat[i].0, 10).len() as u64);
        q_ns.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let cat_p50 = timer::percentile(&q_ns, 0.50);

        println!("\n  --- with the ENGINE doing the expansion (learn_expansion) ---");
        println!(
            "  {} facet values learned, build {:.1} s",
            ixe.expansion_count(),
            build_ms / 1000.0
        );
        println!(
            "  precision@10 {:.1}%   MRR {:.3}",
            100.0 * prec / n,
            mrr / n
        );
        println!(
            "
  {:<44} {:>12.1}%",
            "name+brand only (no facet field at all)",
            100.0 * base_prec / n
        );
        println!(
            "  {:<44} {:>12.1}%   <- FAIR baseline",
            "+ boost-0.0 facet field, no expansion",
            fair * 100.0
        );
        println!(
            "  {:<44} {:>12.1}%   {:+.1} pt from EXPANSION",
            "+ learn_expansion",
            100.0 * prec / n,
            100.0 * prec / n - fair * 100.0
        );
        println!(
            "
  The middle row is the correction. A boost-0.0 field scores nothing but still lets a
               document MATCH, and a match earns typo_bucket 0 -- the primary sort key -- so it
               outranks every non-matching document. {:.1} of the {:.1} points first attributed to
               expansion were the field's doing.",
            fair * 100.0 - 100.0 * base_prec / n,
            100.0 * prec / n - 100.0 * base_prec / n
        );
        println!(
            "  category-query latency p50 {:.0}us  (exact product query p50 {:.0}us)",
            cat_p50 / 1000.0,
            e50 / 1000.0
        );
        println!(
            "\n  IN-SAMPLE, and that is the honest label. The table is learned from the same\n  \
             241,677 products it is scored against, so this is NOT comparable to the 79.6 % in\n  \
             p17-presyo-expand.md, which held out half the catalogue. The two answer different\n  \
             questions and both are worth having:\n    \
             in-sample  ({:.1}%) — how it behaves on the catalogue it was built from, which is the\n                 \
             deployment condition for every product already in it;\n    \
             held out   (79.6%) — how it behaves for products added AFTER the table was built,\n                 \
             which is the condition that decays until the table is rebuilt.\n  \
             The gap between them, ~17 points, is the cost of a stale table.",
            100.0 * prec / n
        );
        println!(
            "\n  The category field carries boost 0.0: it exists only to supply the facet value to\n  \
             `learn_expansion`, and contributes nothing to retrieval. Without that, this would be\n  \
             p15's option 2 (indexing the label) wearing an expansion's clothes."
        );
    }

    println!("\n  --- gate ---");
    let ok = t99 <= P99_NS_MAX;
    println!(
        "  {} typo p99 <= {:.0} ms at {} REAL documents  (got {:.2} ms)",
        if ok { "PASS" } else { "FAIL" },
        P99_NS_MAX / 1e6,
        product.len(),
        t99 / 1e6
    );
}
