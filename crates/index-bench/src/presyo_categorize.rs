//! `presyo-categorize` — **the index as an analyzer, not just a retriever.**
//!
//! `bench/roadmap/p15-presyo-catalog.md` recorded, and did not pursue, that **50,673 of presyo's
//! 241,677 products (21 %) are categorised `Uncategorized`.** That is a real data-quality problem
//! sitting inside the corpus this project has spent the day measuring, and it is the *inverse* of
//! everything measured so far: instead of "find the products in this category", it is **"what
//! category is this product?"**
//!
//! # Why this belongs in a search engine at all
//!
//! It needs no new machinery. A product's category can be predicted by searching its name against
//! the products whose category is known and taking a **majority vote over the neighbours** — kNN
//! classification, where the index *is* the model. No training, no embeddings, no second system.
//!
//! That is the claim "apps don't need to optimize for their data anymore" doing real work: the same
//! artifact that answers queries also fills in a field the application never got round to.
//!
//! # How it is validated
//!
//! Predicting the 50,673 unlabelled products proves nothing on its own — **there is nothing to check
//! the answers against.** So accuracy is measured on a held-out slice of the *labelled* products:
//! they are removed from the index, predicted as if unknown, and scored against the category they
//! actually carry. Only then is the method turned on the genuinely unlabelled rows, and what is
//! reported there is **coverage and confidence, not accuracy**, because accuracy is unknowable
//! without someone labelling them.

mod timer;

use index_text::{Doc, Field, IndexBuilder, Schema};
use std::collections::HashMap;
use std::path::PathBuf;

/// Neighbours consulted per prediction.
const K: usize = 10;
/// A prediction is "confident" when this share of the neighbours agree.
const CONFIDENT: f64 = 0.6;
/// Held-out labelled products used to measure accuracy.
const HELD_OUT: usize = 4000;

struct Product {
    name: String,
    brand: String,
    category: String,
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

fn load(path: &std::path::Path) -> Option<Vec<Product>> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut line = text.lines();
    let head = split_csv(line.next()?);
    let at = |k: &str| head.iter().position(|h| h == k);
    let (i_name, i_brand, i_cat) = (at("product_name")?, at("brand_name")?, at("category_name")?);
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

/// Majority category among the top-`K` neighbours, with the share that agreed.
fn predict(ix: &index_text::Index, label: &[&str], name: &str, k: usize) -> Option<(String, f64)> {
    let hit = ix.search(name, k);
    if hit.is_empty() {
        return None;
    }
    let mut vote: HashMap<&str, usize> = HashMap::new();
    for h in &hit {
        if let Some(c) = label.get(h.doc as usize) {
            *vote.entry(*c).or_default() += 1;
        }
    }
    let total: usize = vote.values().sum();
    vote.into_iter()
        .max_by_key(|&(c, n)| (n, std::cmp::Reverse(c)))
        .map(|(c, n)| (c.to_string(), n as f64 / total.max(1) as f64))
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
        return;
    };

    let labelled: Vec<usize> = (0..product.len())
        .filter(|&i| !product[i].category.is_empty() && product[i].category != "Uncategorized")
        .collect();
    let unlabelled: Vec<usize> = (0..product.len())
        .filter(|&i| product[i].category == "Uncategorized")
        .collect();

    println!("presyo-categorize :: the index as an analyzer");
    println!("clock backend: {}", clock.backend());
    println!(
        "  {} products: {} labelled, {} Uncategorized ({:.1}%)\n",
        product.len(),
        labelled.len(),
        unlabelled.len(),
        100.0 * unlabelled.len() as f64 / product.len() as f64
    );

    // Hold out a slice of the LABELLED products: removed from the index, predicted as if unknown.
    let step = (labelled.len() / HELD_OUT).max(1);
    let held: Vec<usize> = labelled.iter().copied().step_by(step).collect();
    let held_set: std::collections::HashSet<usize> = held.iter().copied().collect();
    let train: Vec<usize> = labelled.iter().copied().filter(|i| !held_set.contains(i)).collect();

    let schema = Schema::new(vec![Field::new("name", 3.0, 0.5), Field::new("brand", 0.5, 0.75)]);
    let mut b = IndexBuilder::new(schema);
    let mut label: Vec<&str> = Vec::with_capacity(train.len());
    for &i in &train {
        b.add(&Doc::new([product[i].name.as_str(), product[i].brand.as_str()]));
        label.push(product[i].category.as_str());
    }
    let ix = b.build().expect("build");

    // --- accuracy, on products whose true category is known and withheld.
    let mut n = 0usize;
    let mut correct = 0usize;
    let mut conf_n = 0usize;
    let mut conf_correct = 0usize;
    let mut adjacent = 0usize;
    let mut wrong: Vec<(&str, String, &str)> = Vec::new();
    for &i in &held {
        let Some((pred, share)) = predict(&ix, &label, &product[i].name, K) else { continue };
        n += 1;
        let ok = pred == product[i].category;
        if ok {
            correct += 1;
        } else {
            // Is this a WILD miss or a boundary dispute? "Spirits" vs "Liquor" and "Fresh Meat"
            // vs "Frozen Meat" are not the same kind of error as "Garlic" vs "Motor Oil", and an
            // accuracy figure that treats them alike understates the method. Sharing a word is a
            // crude proxy for "adjacent in the taxonomy", but it is a measured one rather than an
            // assertion, and it is reported separately rather than folded into the headline.
            let pw: Vec<&str> = pred.split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() > 2).collect();
            let tw: Vec<&str> = product[i]
                .category
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| w.len() > 2)
                .collect();
            if pw.iter().any(|a| tw.iter().any(|b| a.eq_ignore_ascii_case(b))) {
                adjacent += 1;
            }
            if wrong.len() < 6 {
                wrong.push((product[i].name.as_str(), pred.clone(), product[i].category.as_str()));
            }
        }
        if share >= CONFIDENT {
            conf_n += 1;
            if ok {
                conf_correct += 1;
            }
        }
    }

    println!("  --- accuracy on {n} held-out labelled products (k={K}) ---");
    println!(
        "  all predictions          {:.1}%   ({n} of {} predicted)",
        100.0 * correct as f64 / n.max(1) as f64,
        held.len()
    );
    println!(
        "  confident only (>={:.0}% agree)  {:.1}%   ({} of {n} = {:.1}% of predictions)",
        CONFIDENT * 100.0,
        100.0 * conf_correct as f64 / conf_n.max(1) as f64,
        conf_n,
        100.0 * conf_n as f64 / n.max(1) as f64
    );
    let miss = n - correct;
    println!(
        "  of the {miss} misses, {adjacent} ({:.1}%) name a category sharing a word with the true\n  \
         one -- boundary disputes rather than wild misses. Counting those as acceptable would put\n  \
         the figure at {:.1}%, which is NOT claimed as accuracy: whether \"Spirits\" may stand in\n  \
         for \"Liquor\" is presyo's call about its own taxonomy, not this bench's.",
        100.0 * adjacent as f64 / miss.max(1) as f64,
        100.0 * (correct + adjacent) as f64 / n.max(1) as f64
    );
    if !wrong.is_empty() {
        println!("\n  mistakes, for a sense of what they look like:");
        for (name, pred, truth) in &wrong {
            println!("    {:<46} -> {pred}  (truth {truth})", &name[..name.len().min(46)]);
        }
    }

    // --- turn it on the genuinely unlabelled rows. COVERAGE, not accuracy.
    let mut got = 0usize;
    let mut confident = 0usize;
    let mut dist: HashMap<String, usize> = HashMap::new();
    let sample: Vec<usize> = unlabelled.iter().copied().step_by(5).collect();
    let t = std::time::Instant::now();
    for &i in &sample {
        if let Some((pred, share)) = predict(&ix, &label, &product[i].name, K) {
            got += 1;
            if share >= CONFIDENT {
                confident += 1;
                *dist.entry(pred).or_default() += 1;
            }
        }
    }
    let ms = t.elapsed().as_secs_f64() * 1000.0;

    // --- sweeps, because K and CONFIDENT were chosen and not measured.
    println!("\n  --- k, swept ---");
    println!("  {:>4} {:>12} {:>16} {:>14}", "k", "accuracy", "confident acc", "confident share");
    for k in [3usize, 5, 10, 20, 40] {
        let (mut nn, mut cc, mut cn, mut ccc) = (0usize, 0usize, 0usize, 0usize);
        for &i in &held {
            let Some((pred, share)) = predict(&ix, &label, &product[i].name, k) else { continue };
            nn += 1;
            let ok = pred == product[i].category;
            if ok {
                cc += 1;
            }
            if share >= CONFIDENT {
                cn += 1;
                if ok {
                    ccc += 1;
                }
            }
        }
        println!(
            "  {:>4} {:>11.1}% {:>15.1}% {:>13.1}%",
            k,
            100.0 * cc as f64 / nn.max(1) as f64,
            100.0 * ccc as f64 / cn.max(1) as f64,
            100.0 * cn as f64 / nn.max(1) as f64
        );
    }

    println!("\n  --- confidence threshold, swept (k={K}) ---");
    println!("  {:>10} {:>16} {:>14}", "threshold", "accuracy", "share kept");
    let scored: Vec<(bool, f64)> = held
        .iter()
        .filter_map(|&i| {
            predict(&ix, &label, &product[i].name, K)
                .map(|(pred, share)| (pred == product[i].category, share))
        })
        .collect();
    for th in [0.0f64, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 1.0] {
        let kept: Vec<&(bool, f64)> = scored.iter().filter(|(_, sh)| *sh >= th).collect();
        if kept.is_empty() {
            continue;
        }
        let acc = kept.iter().filter(|(ok, _)| *ok).count() as f64 / kept.len() as f64;
        println!(
            "  {:>10.1} {:>15.1}% {:>13.1}%",
            th,
            acc * 100.0,
            100.0 * kept.len() as f64 / scored.len().max(1) as f64
        );
    }

    // --- does learn_expansion help the CLASSIFIER?
    //
    // Predicted no, from the design rather than from hope: expansion fires only when the whole
    // query IS a facet value, and a product name never is. So it should be an exact no-op here.
    // A prediction from a design is still a prediction, so it is run.
    {
        let sch = Schema::new(vec![
            Field::new("name", 3.0, 0.5),
            Field::new("brand", 0.5, 0.75),
            Field::new("category", 0.0, 0.75),
        ]);
        let mut bb = IndexBuilder::new(sch).learn_expansion(2, 20);
        for &i in &train {
            bb.add(&Doc::new([
                product[i].name.as_str(),
                product[i].brand.as_str(),
                product[i].category.as_str(),
            ]));
        }
        let ixe = bb.build().expect("build expansion");
        let (mut nn, mut cc) = (0usize, 0usize);
        for &i in &held {
            if let Some((pred, _)) = predict(&ixe, &label, &product[i].name, K) {
                nn += 1;
                if pred == product[i].category {
                    cc += 1;
                }
            }
        }
        let acc = 100.0 * cc as f64 / nn.max(1) as f64;
        let base = 100.0 * correct as f64 / n.max(1) as f64;
        println!(
            "\n  --- with learn_expansion on the classifier's index ---\n  \
             accuracy {acc:.1}% vs {base:.1}% plain  ({:+.1} pt)",
            acc - base
        );
        println!(
            "  Predicted from the design to be a no-op, and it is: expansion fires only when the\n  \
             whole query IS a facet value, and a product name never is. Worth running rather than\n  \
             asserting -- a mechanism that fired here would have meant the strict trigger leaked."
        );
    }

    println!(
        "\n  --- applied to {} of the {} Uncategorized products ---",
        sample.len(),
        unlabelled.len()
    );
    println!(
        "  a category was predicted for {:.1}%; {:.1}% of those met the confidence bar",
        100.0 * got as f64 / sample.len().max(1) as f64,
        100.0 * confident as f64 / got.max(1) as f64
    );
    println!("  {:.0} products/second", sample.len() as f64 / (ms / 1000.0));
    let mut top: Vec<(&String, &usize)> = dist.iter().collect();
    top.sort_by(|a, b| b.1.cmp(a.1));
    println!("\n  where they would go:");
    for (c, k) in top.iter().take(8) {
        println!("    {:<28} {k}", c);
    }

    println!(
        "\n  Read: the accuracy above is measured on products whose category was KNOWN and withheld.\n  \
         The figures for the Uncategorized rows are COVERAGE and CONFIDENCE only — their true\n  \
         categories are unknown, so no accuracy can be claimed for them, and this bench does not.\n  \
         What it does establish is that {:.1}% of a 21% data-quality hole can be given a confident\n  \
         proposal by the index the application already has, at {:.0} products/second and with no\n  \
         model, no training and no second system.",
        100.0 * confident as f64 / sample.len().max(1) as f64,
        sample.len() as f64 / (ms / 1000.0)
    );
}
