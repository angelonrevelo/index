//! `biasd-entity` — **entity linking from name variants, and what an alias table is worth.**
//!
//! `bench/roadmap/p15-presyo-catalog.md` found the project's first unsaturated workload: querying
//! `"Baking Needs"` returns nothing relevant because the products in it are named *Camote Powder*
//! and *Sago Tapioca*, which share no token with the query. It proposed three fixes and ranked a
//! **query-time alias/expansion table** first, on the grounds that `AliasTable` already exists and
//! needs no model.
//!
//! That proposal had no evidence behind it, and presyo has no alias ground truth to supply any.
//! **biasd does.**
//!
//! `biasd` is a PH news aggregator that resolves political entities mentioned in article text. Its
//! gazetteers carry, for every person, the **surface forms** that actually appear in print:
//!
//! ```text
//! label    Abdulghani Salapuddin
//! surface  Abdulghani A Salapuddin | Salapuddin, Abdulghani | Salapuddin, Gerry
//!          Gerry Salapuddin | Gerry
//! ```
//!
//! That is a labelled alias→canonical mapping over **4,560 real entities and 7,217 alias queries**,
//! which is exactly the ground truth p15 needed and could not get.
//!
//! # The question
//!
//! An app indexes the canonical name because that is what it has. A reader types what the newspaper
//! printed. **How much of that gap does lexical retrieval close on its own, and how much needs the
//! aliases to be indexed?**
//!
//!   - **arm A** — index the canonical label only. The naive setup.
//!   - **arm B** — index the label *plus* its surface forms as a second field. The fix.
//!
//! Arm B is not a clever technique; it is what an application does once it knows the aliases. Its
//! value here is as a **ceiling**: it is the most an alias table can possibly buy, and therefore the
//! upper bound on what p15's option 1 could achieve if the aliases had to be *derived* rather than
//! handed over.
//!
//! **4.1 % of alias queries share no token at all with their canonical label** — those are the ones
//! no amount of tokenized matching can reach, and they are reported separately, because an average
//! over 7,217 queries would bury the 293 that constitute the actual problem.

mod timer;

use index_text::{Doc, Field, IndexBuilder, Schema};
use serde_json::Value;
use std::path::PathBuf;

/// Entity linking has to be precise: the wrong politician is worse than none.
const HIT1_MIN: f64 = 0.80;

struct Entity {
    label: String,
    surface: Vec<String>,
}

fn corpus_dir() -> PathBuf {
    std::env::var("INDEX_CORPUS_DIR").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("..")
    })
}

fn load(dir: &std::path::Path) -> Vec<Entity> {
    let mut out = Vec::new();
    for file in ["entity-politician.json", "entity-congress.json"] {
        let path = dir.join("biasd").join("data").join(file);
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
        let Some(person) = v.get("person").and_then(|p| p.as_array()) else { continue };
        for p in person {
            let Some(label) = p.get("label").and_then(|l| l.as_str()) else { continue };
            let surface: Vec<String> = p
                .get("surface")
                .and_then(|s| s.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str()).map(str::to_string).collect())
                .unwrap_or_default();
            out.push(Entity { label: label.to_string(), surface });
        }
    }
    out
}

fn tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

#[derive(Default)]
struct Metric {
    n: usize,
    hit1: usize,
    hit10: usize,
}

impl Metric {
    fn observe(&mut self, rank: Option<usize>) {
        self.n += 1;
        if let Some(r) = rank {
            if r == 0 {
                self.hit1 += 1;
            }
            if r < 10 {
                self.hit10 += 1;
            }
        }
    }
    fn h1(&self) -> f64 {
        self.hit1 as f64 / self.n.max(1) as f64
    }
    fn h10(&self) -> f64 {
        self.hit10 as f64 / self.n.max(1) as f64
    }
}

fn pct(x: f64) -> String {
    format!("{:.1}%", x * 100.0)
}

fn main() {
    let clock = timer::Clock::new();
    let dir = corpus_dir();
    let entity = load(&dir);
    if entity.is_empty() {
        println!("SKIP: biasd/data/entity-*.json not found under {dir:?}");
        println!("      Refusing to report a verdict without the real gazetteer.");
        return;
    }

    // Query set: every surface form that is not just the canonical label restated.
    // `disjoint` marks the ones sharing no token with their label — the hard core.
    let mut query: Vec<(String, usize, bool)> = Vec::new();
    for (i, e) in entity.iter().enumerate() {
        let lt = tokens(&e.label);
        for s in &e.surface {
            if s.eq_ignore_ascii_case(&e.label) {
                continue;
            }
            let st = tokens(s);
            let shared = st.iter().any(|t| lt.contains(t));
            query.push((s.clone(), i, !shared));
        }
    }
    let hard = query.iter().filter(|q| q.2).count();

    println!("biasd-entity :: entity linking from name variants");
    println!("clock backend: {}", clock.backend());
    println!(
        "  {} real political entities, {} alias queries",
        entity.len(),
        query.len()
    );
    println!(
        "  {hard} of them ({}) share NO token with their canonical label\n",
        pct(hard as f64 / query.len() as f64)
    );

    // --- arm A: canonical label only, which is what an app has before it knows about aliases.
    let schema_a = Schema::new(vec![Field::new("label", 1.0, 0.5)]);
    let mut ba = IndexBuilder::new(schema_a);
    for e in &entity {
        ba.add(&Doc::new([e.label.as_str()]));
    }
    let ix_a = ba.build().expect("build A");

    // --- arm B: label plus its surface forms in a second field. The ceiling.
    let schema_b = Schema::new(vec![Field::new("label", 2.0, 0.5), Field::new("alias", 1.0, 0.6)]);
    let mut bb = IndexBuilder::new(schema_b);
    for e in &entity {
        let alias = e.surface.join(" ");
        bb.add(&Doc::new([e.label.as_str(), alias.as_str()]));
    }
    let ix_b = bb.build().expect("build B");

    let mut a_all = Metric::default();
    let mut b_all = Metric::default();
    let mut a_hard = Metric::default();
    let mut b_hard = Metric::default();
    let mut fixed: Vec<(&str, &str)> = Vec::new();

    for (q, want, is_hard) in &query {
        let ra = ix_a.search(q, 10).iter().position(|h| h.doc as usize == *want);
        let rb = ix_b.search(q, 10).iter().position(|h| h.doc as usize == *want);
        a_all.observe(ra);
        b_all.observe(rb);
        if *is_hard {
            a_hard.observe(ra);
            b_hard.observe(rb);
            if ra.is_none() && rb == Some(0) && fixed.len() < 6 {
                fixed.push((q.as_str(), entity[*want].label.as_str()));
            }
        }
    }

    println!("  --- arms ---");
    println!(
        "  {:<34} {:>10} {:>10}",
        "", "hit@1", "hit@10"
    );
    println!(
        "  {:<34} {:>10} {:>10}",
        "A: canonical label only",
        pct(a_all.h1()),
        pct(a_all.h10())
    );
    println!(
        "  {:<34} {:>10} {:>10}",
        "B: label + aliases indexed",
        pct(b_all.h1()),
        pct(b_all.h10())
    );

    println!("\n  --- the {hard} lexically disjoint queries, where the average hides everything ---");
    println!(
        "  {:<34} {:>10} {:>10}",
        "A: canonical label only",
        pct(a_hard.h1()),
        pct(a_hard.h10())
    );
    println!(
        "  {:<34} {:>10} {:>10}",
        "B: label + aliases indexed",
        pct(b_hard.h1()),
        pct(b_hard.h10())
    );

    if !fixed.is_empty() {
        println!("\n  unreachable without aliases, rank 1 with them:");
        for (q, l) in &fixed {
            println!("    \"{q}\"  ->  {l}");
        }
    }

    println!("\n  --- gate ---");
    let ok_a = a_all.h1() >= HIT1_MIN;
    let ok_b = b_all.h1() >= HIT1_MIN;
    println!(
        "  {} arm A hit@1 >= {}  (got {})",
        if ok_a { "PASS" } else { "FAIL" },
        pct(HIT1_MIN),
        pct(a_all.h1())
    );
    println!(
        "  {} arm B hit@1 >= {}  (got {})",
        if ok_b { "PASS" } else { "FAIL" },
        pct(HIT1_MIN),
        pct(b_all.h1())
    );

    println!(
        "\n  Read for p15: arm B is the CEILING an alias table can reach, measured on aliases that\n  \
         were handed over rather than derived. Whatever a co-occurrence-derived table achieves on\n  \
         presyo's categories, it cannot beat the gain shown here, and it starts from a harder\n  \
         position -- biasd's aliases are curated, presyo's would have to be inferred."
    );
}
