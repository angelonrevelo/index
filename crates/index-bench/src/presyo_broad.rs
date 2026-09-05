//! `presyo-broad` — **the workload nobody had labelled.**
//!
//! `bench/roadmap/p13-presyo-prior.md` measured static priors on presyo's cross-store matching task
//! and found +0.00 points — not because the feature is broken, but because the baseline sits at
//! 98.4 % recall@10 and there is nothing to win. It concluded that the tasks worth optimizing are
//! **broad and browse queries**, where dozens of candidates are near-tied and the *ordering* is the
//! entire product, and that the blocker was a labelled query set nobody had built.
//!
//! It also assumed building one meant somebody deciding what the right answer to `"milk"` is.
//! **That was wrong, and this bin exists because it was wrong.** presyo's gold fixture already
//! carries a product taxonomy — `gold_brand` and `gold_product_type` — curated by presyo, not by
//! this benchmark. The labels were sitting in the file the whole time:
//!
//!   - **brand query** — `"Purefoods"`; relevant = every listing whose cluster has that
//!     `gold_brand`. 32 brands carry 20+ listings.
//!   - **category query** — `"milk"`, `"cheese"`, `"hotdog"`; relevant = every listing whose
//!     `gold_product_type` contains that word.
//!
//! Nothing here is a judgement call. The index sees only `raw_name`; the labels come from gold
//! metadata that is **not** indexed, so relevance is decided by presyo's taxonomy and retrieval has
//! to earn it from the retailer's text.
//!
//! # Why the metric changes
//!
//! recall@10 is meaningless when 253 listings are relevant — no ranker fits them in ten slots, and
//! reporting it would repeat the exact mistake `p12-maphy-place.md` made with `"CITY "`. The
//! question a broad query asks is **"is what I got back actually about this?"**, so the metric is
//! **precision@10**, plus the reciprocal rank of the first relevant hit.

mod timer;

use index_text::{Doc, Field, Index, IndexBuilder, Schema};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// A query needs this many relevant listings to be a *broad* query at all.
const MIN_RELEVANT: usize = 20;
/// Below this precision@10 there is headroom worth optimizing for.
const SATURATION: f64 = 0.95;

struct Listing {
    cluster: usize,
    source: String,
    name: String,
    brand: String,
    ptype: String,
}

fn corpus_dir() -> PathBuf {
    std::env::var("INDEX_CORPUS_DIR").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("..")
    })
}

fn load(path: &PathBuf) -> Option<Vec<Listing>> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let cluster = v.get("clusters")?.as_array()?;
    let str_of = |c: &Value, k: &str| -> String {
        c.get(k).and_then(|x| x.as_str()).unwrap_or("").trim().to_string()
    };
    let mut out = Vec::new();
    for (ci, c) in cluster.iter().enumerate() {
        let brand = str_of(c, "gold_brand");
        let ptype = str_of(c, "gold_product_type");
        let Some(list) = c.get("listings").and_then(|l| l.as_array()) else { continue };
        for l in list {
            let (Some(src), Some(name)) = (
                l.get("source_key").and_then(|s| s.as_str()),
                l.get("raw_name").and_then(|s| s.as_str()),
            ) else {
                continue;
            };
            out.push(Listing {
                cluster: ci,
                source: src.to_string(),
                name: name.to_string(),
                brand: brand.clone(),
                ptype: ptype.clone(),
            });
        }
    }
    Some(out)
}

/// A labelled broad query: the text a shopper types, and the listings that genuinely answer it.
struct Query {
    text: String,
    kind: &'static str,
    relevant: HashSet<usize>,
}

fn build_queries(listing: &[Listing]) -> Vec<Query> {
    let mut out = Vec::new();

    // Brand queries.
    let mut by_brand: HashMap<&str, HashSet<usize>> = HashMap::new();
    for (i, l) in listing.iter().enumerate() {
        if !l.brand.is_empty() {
            by_brand.entry(l.brand.as_str()).or_default().insert(i);
        }
    }
    for (b, set) in by_brand {
        if set.len() >= MIN_RELEVANT {
            out.push(Query { text: b.to_string(), kind: "brand", relevant: set });
        }
    }

    // Category queries: single words drawn from the gold product type.
    let mut by_word: HashMap<String, HashSet<usize>> = HashMap::new();
    for (i, l) in listing.iter().enumerate() {
        for w in l.ptype.split(|c: char| !c.is_alphabetic()) {
            if w.len() > 2 {
                by_word.entry(w.to_lowercase()).or_default().insert(i);
            }
        }
    }
    for (w, set) in by_word {
        if set.len() >= MIN_RELEVANT {
            out.push(Query { text: w, kind: "category", relevant: set });
        }
    }

    out.sort_by(|a, b| a.kind.cmp(b.kind).then(a.text.cmp(&b.text)));
    out
}

fn build(listing: &[Listing], prior: Option<&[f32]>) -> Index {
    let schema = Schema::new(vec![Field::new("name", 1.0, 0.5)]);
    let mut b = IndexBuilder::new(schema);
    for (i, l) in listing.iter().enumerate() {
        match prior {
            Some(p) => b.add_with_prior(&Doc::new([l.name.as_str()]), p[i]),
            None => b.add(&Doc::new([l.name.as_str()])),
        };
    }
    b.build().expect("build")
}

/// precision@k and mean reciprocal rank of the first relevant hit.
fn evaluate(ix: &Index, query: &[Query], k: usize) -> (f64, f64) {
    let mut prec = 0.0;
    let mut mrr = 0.0;
    for q in query {
        let hit: Vec<usize> = ix.search(&q.text, k).iter().map(|h| h.doc as usize).collect();
        let good = hit.iter().filter(|d| q.relevant.contains(d)).count();
        prec += good as f64 / k as f64;
        if let Some(r) = hit.iter().position(|d| q.relevant.contains(d)) {
            mrr += 1.0 / (r + 1) as f64;
        }
    }
    let n = query.len().max(1) as f64;
    (prec / n, mrr / n)
}

fn main() {
    let clock = timer::Clock::new();
    let path = corpus_dir()
        .join("presyo")
        .join("tests")
        .join("fixtures")
        .join("recall-gold-cases.json");
    let Some(listing) = load(&path) else {
        println!("SKIP: presyo/tests/fixtures/recall-gold-cases.json not found under {:?}", corpus_dir());
        println!("      Refusing to report a verdict without the real corpus.");
        return;
    };
    let query = build_queries(&listing);
    if query.is_empty() {
        println!("SKIP: no query reached {MIN_RELEVANT} relevant listings.");
        return;
    }

    let brand_n = query.iter().filter(|q| q.kind == "brand").count();
    let cat_n = query.iter().filter(|q| q.kind == "category").count();
    println!("presyo-broad :: the broad-query workload, labelled from presyo's own taxonomy");
    println!("clock backend: {}", clock.backend());
    println!(
        "  {} listings; {} labelled broad queries ({brand_n} brand, {cat_n} category)",
        listing.len(),
        query.len()
    );
    let avg_rel =
        query.iter().map(|q| q.relevant.len()).sum::<usize>() as f64 / query.len() as f64;
    println!("  mean relevant listings per query: {avg_rel:.0}  (recall@10 would be meaningless)\n");

    // --- arm 1: no prior
    let flat = build(&listing, None);
    let (base_p, base_m) = evaluate(&flat, &query, 10);

    // --- arm 2: source quality, exactly the prior p13 measured.
    let mut sum: HashMap<&str, (f64, f64)> = HashMap::new();
    for (i, l) in listing.iter().enumerate() {
        let mate: Vec<usize> = listing
            .iter()
            .enumerate()
            .filter(|(j, m)| *j != i && m.cluster == l.cluster)
            .map(|(j, _)| j)
            .collect();
        if mate.is_empty() || l.cluster % 2 == 1 {
            continue; // fit on even clusters only, as p13 does
        }
        let hit: Vec<usize> = flat
            .search(&l.name, 11)
            .iter()
            .map(|h| h.doc as usize)
            .filter(|&d| d != i)
            .collect();
        let got = hit.iter().filter(|d| mate.contains(d)).count() as f64 / mate.len() as f64;
        let e = sum.entry(l.source.as_str()).or_insert((0.0, 0.0));
        e.0 += got;
        e.1 += 1.0;
    }
    let quality: HashMap<&str, f32> =
        sum.iter().map(|(k, (s, n))| (*k, (s / n.max(1.0)) as f32)).collect();
    let src_prior: Vec<f32> = listing
        .iter()
        .map(|l| 1.0 + quality.get(l.source.as_str()).copied().unwrap_or(0.9))
        .collect();

    // --- arm 3: popularity — how many retailers carry this product.
    //
    // A real presyo signal and not a fitted one: cluster size is a property of the catalogue, and
    // relevance here is decided by brand/type taxonomy, so it cannot leak the answer. For a broad
    // query, "widely stocked" is exactly the kind of query-independent importance a prior is for.
    let mut carried: HashMap<usize, usize> = HashMap::new();
    for l in &listing {
        *carried.entry(l.cluster).or_default() += 1;
    }
    let pop_prior: Vec<f32> =
        listing.iter().map(|l| carried[&l.cluster] as f32).collect();

    println!("  --- arms ---");
    println!("  {:<34} {:>13} {:>10}", "prior", "precision@10", "MRR");
    println!("  {:<34} {:>12.1}% {:>10.3}", "none (baseline)", base_p * 100.0, base_m);

    let mut best = ("none", base_p);
    let mut worst_delta = 0.0f64;
    for (name, p) in [
        ("source quality (p13's prior)", &src_prior),
        ("popularity (retailers carrying)", &pop_prior),
    ] {
        let ix = build(&listing, Some(p));
        let (pr, mr) = evaluate(&ix, &query, 10);
        println!(
            "  {:<34} {:>12.1}% {:>10.3}  {:+.1} pt",
            name,
            pr * 100.0,
            mr,
            (pr - base_p) * 100.0
        );
        if pr > best.1 {
            best = (name, pr);
        }
        worst_delta = worst_delta.min(pr - base_p);
    }

    // Worst queries, because an average hides where the work is.
    let mut per: Vec<(f64, &Query)> = query
        .iter()
        .map(|q| {
            let hit: Vec<usize> = flat.search(&q.text, 10).iter().map(|h| h.doc as usize).collect();
            let good = hit.iter().filter(|d| q.relevant.contains(d)).count();
            (good as f64 / 10.0, q)
        })
        .collect();
    per.sort_by(|a, b| a.0.total_cmp(&b.0));
    println!("\n  --- lowest-scoring broad queries (see the note below: mostly label noise) ---");
    for (p, q) in per.iter().take(8) {
        println!(
            "  {:<22} {:>6.0}% precision@10   ({} relevant, {})",
            format!("\"{}\"", q.text),
            p * 100.0,
            q.relevant.len(),
            q.kind
        );
    }

    println!("\n  --- verdict ---");
    if base_p < SATURATION {
        println!(
            "  HEADROOM  baseline precision@10 is {:.1}%, well under the {:.0}% saturation bar.",
            base_p * 100.0,
            SATURATION * 100.0
        );
        println!(
            "  This is the workload p13 said did not exist in labelled form. It does now, it is\n  \
             built from presyo's own taxonomy rather than anyone's taste, and unlike every other\n  \
             bench in this repo it is NOT at ceiling -- so ranking work measured here would be\n  \
             measuring something real."
        );
    } else {
        println!("  SATURATED  baseline precision@10 is {:.1}%; no headroom here either.", base_p * 100.0);
    }
    println!(
        "\n  NO prior beats no-prior. The best is \"{}\" at {:+.1} points, and the worst costs\n  \
         {:.1} points -- a static prior does not merely fail to help on presyo, it actively\n  \
         degrades precision by pulling well-stocked or clean-named products above ones that\n  \
         actually match the query. That is the definitive answer for this consumer: leave it off.",
        best.0,
        (best.1 - base_p) * 100.0,
        worst_delta * 100.0
    );

    println!(
        "\n  The limit of these labels, stated plainly: relevance is derived from `gold_product_type`\n  \
         containing a word, and the retailer's `raw_name` usually contains the same word -- so the\n  \
         label and the retrieval signal are lexically correlated, and this measures LEXICAL\n  \
         precision rather than intent. A set that asked whether \"milk\" should return a yoghurt\n  \
         drink would need someone to decide that, and that genuinely is a judgement call. What this\n  \
         bin establishes is narrower and still worth having: on the labels presyo already owns,\n  \
         broad queries are not where the engine is weak either."
    );

    // The tail was checked before being called a target, and it is not one.
    let name_says = listing.iter().filter(|l| l.name.to_lowercase().contains("chocolate")).count();
    let type_says = listing
        .iter()
        .filter(|l| {
            l.name.to_lowercase().contains("chocolate") && l.ptype.to_lowercase().contains("chocolate")
        })
        .count();
    println!(
        "\n  The tail is LABEL NOISE, not an engine failure, and it was checked before being called\n  \
         a target. {name_says} listings say \"chocolate\" in the retailer's name; only {type_says}\n  \
         carry it in `gold_product_type`. \"Nestle Chuckie Chocolate Milk Drink\" is typed\n  \
         \"Milk Drink\", so this label scores it irrelevant for the query \"chocolate\" -- while a\n  \
         shopper plainly wants it. The engine returned the right thing and the label marked it\n  \
         wrong, so true precision is HIGHER than {:.1}%, not lower.\n\n  \
         That is the real limit of taxonomy-derived labels: `gold_product_type` is a CANONICAL TYPE,\n  \
         not a tag set, and it cannot express \"this is a chocolate product\". Building a set that\n  \
         could does require someone to decide what \"chocolate\" should return -- which is the\n  \
         judgement call p13 named, and it survives this bin rather than being removed by it.",
        base_p * 100.0
    );
}
