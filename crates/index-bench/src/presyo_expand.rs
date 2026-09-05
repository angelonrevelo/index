//! `presyo-expand` — **can the aliases be derived, not curated?**
//!
//! `bench/roadmap/p15-presyo-catalog.md` found the project's one unsaturated workload: category
//! retrieval at 61.7 % precision@10, because `"Baking Needs"` holds *Camote Powder* and *Sago
//! Tapioca*, which share no token with the query.
//!
//! `bench/roadmap/p16-biasd-entity.md` then measured what an alias table is worth on `biasd`'s
//! curated political gazetteer: **+14.9 points hit@1, and 0.7 % → 95.5 % hit@10 on the queries with
//! no shared token.** But it said plainly that this was a **ceiling, not a forecast**, because
//! biasd's aliases were handed over and presyo's would have to be *derived*.
//!
//! This derives them.
//!
//! # The mechanism
//!
//! For a category, find the terms that are characteristic of its products — terms that occur far
//! more often inside it than outside — and append them to the query. `"Baking Needs"` becomes
//! `"Baking Needs" + {tapioca, tartar, hotcake, ...}`. That is query-time expansion, the cheaper of
//! the two mechanisms `p16` distinguished, and it needs no model and no new storage.
//!
//! # The leak this design exists to avoid
//!
//! Deriving `"Baking Needs" → {tapioca, ...}` from the same category assignments the run is scored
//! against would be **using the answer key**, and would report a large win by construction. It is
//! the same trap `p13-presyo-prior.md` avoided with a held-out split, and the same one that made
//! `p12-maphy-place.md` publish a wrong conclusion when it was not avoided.
//!
//! So products are split in half by index parity:
//!
//!   - expansions are derived from the **train** half only;
//!   - the index contains the **test** half only;
//!   - relevance is test-half membership.
//!
//! No product contributes both to a term's expansion weight and to the score it earns.

mod timer;

use index_text::{Doc, Field, IndexBuilder, Schema};
use std::collections::HashMap;
use std::path::PathBuf;

/// A category needs this many products in each half to be worth querying.
const MIN_CATEGORY: usize = 100;

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

fn tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 2)
        .map(str::to_string)
        .collect()
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

    // Half the products train the expansion, the other half are the corpus it is scored against.
    let train: Vec<usize> = (0..product.len()).filter(|i| i % 2 == 0).collect();
    let test: Vec<usize> = (0..product.len()).filter(|i| i % 2 == 1).collect();

    // Categories with enough products on BOTH sides of the split.
    let mut n_train: HashMap<&str, usize> = HashMap::new();
    let mut n_test: HashMap<&str, usize> = HashMap::new();
    for &i in &train {
        *n_train.entry(product[i].category.as_str()).or_default() += 1;
    }
    for &i in &test {
        *n_test.entry(product[i].category.as_str()).or_default() += 1;
    }
    let mut category: Vec<&str> = n_train
        .keys()
        .copied()
        .filter(|c| {
            !c.is_empty()
                && *c != "Uncategorized"
                && n_train[c] >= MIN_CATEGORY
                && n_test.get(c).copied().unwrap_or(0) >= MIN_CATEGORY
        })
        .collect();
    category.sort_unstable();

    println!("presyo-expand :: can the aliases be DERIVED rather than curated?");
    println!("clock backend: {}", clock.backend());
    println!(
        "  {} products, split {} train / {} test; {} categories with {MIN_CATEGORY}+ on both sides\n",
        product.len(),
        train.len(),
        test.len(),
        category.len()
    );

    // --- derive expansions from the TRAIN half only.
    //
    // Score a term by how concentrated it is inside the category: in-category rate against overall
    // rate, with a small prior so a term appearing twice in one category does not outrank a term
    // appearing four hundred times in it.
    let mut global: HashMap<&str, usize> = HashMap::new();
    let mut per_cat: HashMap<&str, HashMap<String, usize>> = HashMap::new();
    for &i in &train {
        let c = product[i].category.as_str();
        for t in tokens(&product[i].name) {
            *global.entry(Box::leak(t.clone().into_boxed_str())).or_default() += 1;
            *per_cat.entry(c).or_default().entry(t).or_default() += 1;
        }
    }
    let train_total: usize = train.len();

    let expansion: HashMap<&str, Vec<String>> = category
        .iter()
        .map(|&c| {
            let cat_n = n_train[c] as f64;
            let mut score: Vec<(f64, &String)> = per_cat
                .get(c)
                .map(|m| {
                    m.iter()
                        .filter(|(_, &n)| n >= 5)
                        .map(|(t, &n)| {
                            let inside = n as f64 / cat_n;
                            let overall =
                                global.get(t.as_str()).copied().unwrap_or(1) as f64 / train_total as f64;
                            (inside / (overall + 1e-9), t)
                        })
                        .collect()
                })
                .unwrap_or_default();
            score.sort_by(|a, b| b.0.total_cmp(&a.0));
            (c, score.into_iter().map(|(_, t)| t.clone()).collect::<Vec<String>>())
        })
        .collect();

    // --- index the TEST half only.
    let schema = Schema::new(vec![Field::new("name", 3.0, 0.5), Field::new("brand", 0.5, 0.75)]);
    let mut b = IndexBuilder::new(schema);
    let mut local_of: HashMap<usize, usize> = HashMap::new();
    for (local, &i) in test.iter().enumerate() {
        local_of.insert(i, local);
        b.add(&Doc::new([product[i].name.as_str(), product[i].brand.as_str()]));
    }
    let ix = b.build().expect("build");

    let member: HashMap<&str, Vec<usize>> = {
        let mut m: HashMap<&str, Vec<usize>> = HashMap::new();
        for &i in &test {
            m.entry(product[i].category.as_str()).or_default().push(local_of[&i]);
        }
        m
    };

    let eval = |q: &dyn Fn(&str) -> String| -> f64 {
        let mut prec = 0.0;
        for &c in &category {
            let want = &member[c];
            // Ask for a large pool and truncate. This bench builds the expanded query as a
            // STRING, so the engine's expansion path never fires and the pool is not widened for
            // it -- without this the arm measures pool eviction rather than expansion quality.
            // The same artifact understated blead's held-out gain by 8 points (p20).
            let hit: Vec<usize> =
                ix.search(&q(c), 400).iter().take(10).map(|h| h.doc as usize).collect();
            prec += hit.iter().filter(|d| want.contains(d)).count() as f64 / 10.0;
        }
        prec / category.len().max(1) as f64
    };

    let base = eval(&|c: &str| c.to_string());
    println!("  --- arms ---");
    println!("  {:<40} {:>13}", "query", "precision@10");
    println!("  {:<40} {:>12.1}%", "category name alone (p15 baseline)", base * 100.0);

    let mut best = (0usize, base);
    for k in [3usize, 5, 10, 20] {
        let p = eval(&|c: &str| {
            let mut q = c.to_string();
            if let Some(e) = expansion.get(c) {
                for t in e.iter().take(k) {
                    q.push(' ');
                    q.push_str(t);
                }
            }
            q
        });
        println!(
            "  {:<40} {:>12.1}%  {:+.1} pt",
            format!("+ top {k} derived terms"),
            p * 100.0,
            (p - base) * 100.0
        );
        if p > best.1 {
            best = (k, p);
        }
    }

    // --- the control that decides whether the DERIVATION is doing the work.
    //
    // Adding any 20 terms to a query changes what it matches. If a random expansion of the same
    // size and drawn from the same vocabulary helps as much, then the gain is "longer queries
    // retrieve more", not "these are the right terms" -- and the derivation would be decoration.
    // This is the same role the `rankings` column played in p13-presyo-prior.md.
    let mut pool: Vec<&str> = global
        .iter()
        .filter(|(_, &n)| n >= 5)
        .map(|(t, _)| *t)
        .collect();
    pool.sort_unstable();
    let mut seed = 0x9E37_79B9_u64;
    let mut rand_next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let random_of: HashMap<&str, Vec<&str>> = category
        .iter()
        .map(|&c| {
            let pick: Vec<&str> =
                (0..20).map(|_| pool[(rand_next() as usize) % pool.len()]).collect();
            (c, pick)
        })
        .collect();

    for k in [5usize, 20] {
        let p = eval(&|c: &str| {
            let mut q = c.to_string();
            if let Some(e) = random_of.get(c) {
                for t in e.iter().take(k) {
                    q.push(' ');
                    q.push_str(t);
                }
            }
            q
        });
        println!(
            "  {:<40} {:>12.1}%  {:+.1} pt",
            format!("+ top {k} RANDOM terms (control)"),
            p * 100.0,
            (p - base) * 100.0
        );
    }

    // --- is the mechanism learning CONCEPTS, or just brands?
    //
    // p17 recorded that most derived terms are brand names and flagged it as the main caveat: an
    // expansion built from history cannot cover a brand it has never seen. The catalogue carries a
    // `brand_name` column, so the caveat is testable rather than merely stated -- strike every
    // brand token out of the expansion and see what survives.
    let brand_token: std::collections::HashSet<String> = product
        .iter()
        .flat_map(|p| tokens(&p.brand))
        .collect();
    let no_brand: HashMap<&str, Vec<String>> = expansion
        .iter()
        .map(|(&c, v)| {
            (c, v.iter().filter(|t| !brand_token.contains(*t)).cloned().collect::<Vec<String>>())
        })
        .collect();
    for k in [5usize, 20] {
        let p = eval(&|c: &str| {
            let mut q = c.to_string();
            if let Some(e) = no_brand.get(c) {
                for t in e.iter().take(k) {
                    q.push(' ');
                    q.push_str(t);
                }
            }
            q
        });
        println!(
            "  {:<40} {:>12.1}%  {:+.1} pt",
            format!("+ top {k} derived, BRANDS REMOVED"),
            p * 100.0,
            (p - base) * 100.0
        );
    }

    println!("\n  --- what was derived, for the categories p15 scored 0% ---");
    for c in ["Baking Needs", "Pet Accessories", "Native Deli", "Soup Mixes"] {
        if let Some(e) = expansion.get(c) {
            let show: Vec<&str> = e.iter().take(8).map(String::as_str).collect();
            println!("  {:<18} -> {}", format!("\"{c}\""), show.join(", "));
        }
    }

    // ------------------------------------------------------------------
    // THE GATE: does expansion damage ORDINARY product queries?
    //
    // Everything above expands a *category* query, which is the case the mechanism was built for.
    // But a shipped expander does not know what kind of query it was handed. The dangerous design
    // fires whenever a query happens to contain a category's words -- and "milk" is both a category
    // and something a shopper types when they want one specific carton.
    //
    // So: take real product names as queries, ask for the product itself back, and fire the
    // expansion whenever the query contains all tokens of some category name. If precision on
    // ordinary lookups collapses, the feature makes browse better by making search worse, and that
    // is a trade that has to be seen before it ships rather than after.
    // --- p15's option 2, as an UPPER BOUND rather than a proposal.
    //
    // Indexing the category name on every product trivially makes a category query match, which is
    // close to cheating -- the label becomes part of the document. Its value is as a ceiling: it
    // says how much of the residual gap is "the label is simply absent from the text" versus
    // something an expansion cannot reach.
    {
        let sch = Schema::new(vec![
            Field::new("name", 3.0, 0.5),
            Field::new("brand", 0.5, 0.75),
            Field::new("category", 1.0, 0.75),
        ]);
        let mut bb = IndexBuilder::new(sch);
        for &i in &test {
            bb.add(&Doc::new([
                product[i].name.as_str(),
                product[i].brand.as_str(),
                product[i].category.as_str(),
            ]));
        }
        let ixc = bb.build().expect("build cat");
        let mut prec = 0.0;
        for &c in &category {
            let want = &member[c];
            let hit: Vec<usize> = ixc.search(c, 10).iter().map(|h| h.doc as usize).collect();
            prec += hit.iter().filter(|d| want.contains(d)).count() as f64 / 10.0;
        }
        let p = prec / category.len().max(1) as f64;
        println!(
            "  {:<40} {:>12.1}%  {:+.1} pt   <- p15 option 2, upper bound",
            "category INDEXED as a field",
            p * 100.0,
            (p - base) * 100.0
        );
    }

    println!("\n  --- the gate: damage to ORDINARY product queries ---");
    {
        let cat_tokens: Vec<(&str, Vec<String>)> =
            category.iter().map(|&c| (c, tokens(c))).collect();

        // A spread of real product names from the test half.
        let step = (test.len() / 3000).max(1);
        let probe: Vec<(usize, usize)> = test
            .iter()
            .enumerate()
            .filter(|(n, _)| n % step == 0)
            .map(|(_, &i)| (i, local_of[&i]))
            .collect();

        let mut plain_h1 = 0usize;
        let mut plain_h10 = 0usize;
        let mut exp_h1 = 0usize;
        let mut exp_h10 = 0usize;
        let mut strict_h1 = 0usize;
        let mut strict_h10 = 0usize;
        let mut fired = 0usize;
        let mut strict_fired = 0usize;
        let mut hurt: Vec<(&str, &str)> = Vec::new();

        for &(gi, local) in &probe {
            let name = product[gi].name.as_str();
            let qt = tokens(name);

            let ra = ix.search(name, 10).iter().position(|h| h.doc as usize == local);
            if ra == Some(0) {
                plain_h1 += 1;
            }
            if ra.is_some() {
                plain_h10 += 1;
            }

            // Fire if some category's words are all present in the query.
            let mut q = name.to_string();
            let mut matched: Option<&str> = None;
            for (c, ct) in &cat_tokens {
                if !ct.is_empty() && ct.iter().all(|t| qt.contains(t)) {
                    matched = Some(c);
                    break;
                }
            }
            if let Some(c) = matched {
                fired += 1;
                if let Some(e) = expansion.get(c) {
                    for t in e.iter().take(best.0.max(5)) {
                        q.push(' ');
                        q.push_str(t);
                    }
                }
            }
            let rb = ix.search(&q, 10).iter().position(|h| h.doc as usize == local);
            if rb == Some(0) {
                exp_h1 += 1;
            }
            if rb.is_some() {
                exp_h10 += 1;
            }
            if ra == Some(0) && rb != Some(0) && hurt.len() < 5 {
                hurt.push((name, matched.unwrap_or("-")));
            }

            // STRICT trigger: fire only when the query IS a category, not when it merely contains
            // one. "Ice Cream Butter Pecan" is a product; "Cream" is a category. Requiring the token
            // sets to match makes that distinction, and it is one comparison.
            let mut qs = name.to_string();
            let mut strict_hit = false;
            for (c, ct) in &cat_tokens {
                if !ct.is_empty() && ct.len() == qt.len() && ct.iter().all(|t| qt.contains(t)) {
                    strict_hit = true;
                    if let Some(e) = expansion.get(c) {
                        for t in e.iter().take(best.0.max(5)) {
                            qs.push(' ');
                            qs.push_str(t);
                        }
                    }
                    break;
                }
            }
            if strict_hit {
                strict_fired += 1;
            }
            let rc = ix.search(&qs, 10).iter().position(|h| h.doc as usize == local);
            if rc == Some(0) {
                strict_h1 += 1;
            }
            if rc.is_some() {
                strict_h10 += 1;
            }
        }

        let n = probe.len().max(1) as f64;
        println!(
            "  {} product-name queries, expansion fired on {fired} ({:.1}%)",
            probe.len(),
            100.0 * fired as f64 / n
        );
        println!("  {:<34} {:>10} {:>10}", "", "hit@1", "hit@10");
        println!(
            "  {:<34} {:>9.1}% {:>9.1}%",
            "plain query",
            100.0 * plain_h1 as f64 / n,
            100.0 * plain_h10 as f64 / n
        );
        println!(
            "  {:<34} {:>9.1}% {:>9.1}%",
            "expansion fires when it can",
            100.0 * exp_h1 as f64 / n,
            100.0 * exp_h10 as f64 / n
        );
        println!(
            "  {:<34} {:>9.1}% {:>9.1}%   (fired on {strict_fired})",
            "STRICT: query IS a category",
            100.0 * strict_h1 as f64 / n,
            100.0 * strict_h10 as f64 / n
        );
        println!(
            "\n  damage to exact product lookup:  loose {:+.1} pt   strict {:+.1} pt  (hit@1)",
            (exp_h1 as f64 - plain_h1 as f64) / n * 100.0,
            (strict_h1 as f64 - plain_h1 as f64) / n * 100.0
        );
        if !hurt.is_empty() {
            println!("  demoted from rank 1 by expansion:");
            for (nm, c) in &hurt {
                println!("    {:<52} (matched category \"{c}\")", &nm[..nm.len().min(52)]);
            }
        }
    }

    println!("\n  --- verdict ---");
    if best.1 > base {
        println!(
            "  Derived expansion is worth {:+.1} points at k={} ({:.1}% -> {:.1}%).",
            (best.1 - base) * 100.0,
            best.0,
            base * 100.0,
            best.1 * 100.0
        );
    } else {
        println!("  Derived expansion does not beat the plain category name.");
    }
    println!(
        "\n  Read: expansions come from the TRAIN half and are scored on a disjoint TEST half, so no\n  \
         product contributes both to a term's weight and to the score it earns. Deriving from the\n  \
         same assignments being scored would be using the answer key and would report a large win\n  \
         by construction -- the trap p13 avoided and p12 fell into."
    );
}
