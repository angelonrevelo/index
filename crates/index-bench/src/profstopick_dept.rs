//! `profstopick-dept` — **a third corpus, chosen to test the condition rather than repeat the claim.**
//!
//! `bench/roadmap/p18-blead-industry.md` measured `learn_expansion` on a second corpus and came back
//! with a claim *and a condition*: it generalizes **where a facet's members reuse vocabulary**.
//! presyo's grocery categories retained **68 %** of their gain on held-out documents; blead's
//! business names retained **33 %**, because two logistics firms share almost nothing but legal
//! suffixes while half of `Baking Needs` and the other half share brands and product types.
//!
//! A third corpus that merely repeated the claim would add little. This one was chosen because the
//! condition makes a **falsifiable prediction about it**.
//!
//! # The corpus, and the prediction made before running it
//!
//! profstopick's research pack carries **2,253 distinct Ateneo course titles** across **92
//! departments**, where the department is the prefix of the course code and is *not* in the title:
//!
//! ```text
//! HSCI 60I   ->  FUNDAMENTALS OF GLOBAL HEALTH
//! MATH 21    ->  MATHEMATICAL ANALYSIS I
//! ```
//!
//! **Course titles within a department reuse vocabulary heavily** — chemistry courses say
//! *chemistry*, *organic*, *laboratory*; maths courses say *analysis*, *algebra*, *calculus*. By
//! the condition, held-out retention should therefore look like presyo's **68 %**, not blead's
//! **33 %**.
//!
//! Writing the prediction down before the measurement is the point. A condition that only ever
//! explains results after the fact explains nothing.
//!
//! # One caveat, stated up front
//!
//! **Label leakage is 19.6 % here** — a course title contains its own department code that often —
//! against 13.8 % for presyo and 11.8 % for blead. So the *plain* baseline should be expected to
//! start higher, and the comparison to watch is **retention**, not the absolute numbers.

mod timer;

use index_text::{Doc, Field, IndexBuilder, Schema};
use std::collections::HashMap;
use std::path::PathBuf;

/// A department needs this many courses to be worth querying.
const MIN_MEMBER: usize = 20;

struct Course {
    title: String,
    dept: String,
}

fn main() {
    let clock = timer::Clock::new();
    let path = std::env::var("INDEX_BENCH_COURSE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("bench/fixture/profstopick-course.tsv"));
    let Ok(text) = std::fs::read_to_string(&path) else {
        println!("SKIP: no course fixture at {path:?} — see bench/fixture/README.md");
        return;
    };

    let mut course: Vec<Course> = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 2 || f[0].is_empty() || f[1].is_empty() {
            continue;
        }
        course.push(Course { title: f[0].to_string(), dept: f[1].to_string() });
    }
    if course.is_empty() {
        println!("SKIP: course fixture is empty");
        return;
    }

    let mut by: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, c) in course.iter().enumerate() {
        by.entry(c.dept.as_str()).or_default().push(i);
    }
    let mut dept: Vec<(&str, Vec<usize>)> =
        by.into_iter().filter(|(_, v)| v.len() >= MIN_MEMBER).collect();
    dept.sort_by(|a, b| a.0.cmp(b.0));

    let leak = course
        .iter()
        .filter(|c| c.title.to_lowercase().contains(&c.dept.to_lowercase()))
        .count();

    println!("profstopick-dept :: a third corpus, chosen to TEST the condition");
    println!("clock backend: {}", clock.backend());
    println!(
        "  {} course titles, {} departments with {MIN_MEMBER}+ courses, leakage {:.1}%",
        course.len(),
        dept.len(),
        100.0 * leak as f64 / course.len() as f64
    );
    println!(
        "  PREDICTION (written before running): course titles reuse vocabulary heavily, so held-out\n  \
         retention should resemble presyo's 68%, not blead's 33%.\n"
    );

    let schema = || {
        Schema::new(vec![Field::new("title", 3.0, 0.5), Field::new("dept", 0.0, 0.75)])
    };

    // --- in sample
    let mut b = IndexBuilder::new(schema());
    for c in &course {
        b.add(&Doc::new([c.title.as_str(), c.dept.as_str()]));
    }
    let plain = b.build().expect("plain");
    let mut b = IndexBuilder::new(schema()).learn_expansion(1, 20);
    for c in &course {
        b.add(&Doc::new([c.title.as_str(), c.dept.as_str()]));
    }
    let learned = b.build().expect("learned");

    let score = |ix: &index_text::Index| -> f64 {
        let mut prec = 0.0;
        for (name, want) in &dept {
            let hit: Vec<usize> = ix.search(name, 10).iter().map(|h| h.doc as usize).collect();
            prec += hit.iter().filter(|d| want.contains(d)).count() as f64 / 10.0;
        }
        prec / dept.len().max(1) as f64
    };
    let (p_in, l_in) = (score(&plain), score(&learned));

    println!("  --- in sample ---");
    println!("  {:<34} {:>13}", "", "precision@10");
    println!("  {:<34} {:>12.1}%", "plain", p_in * 100.0);
    println!(
        "  {:<34} {:>12.1}%   {:+.1} pt",
        "learn_expansion(dept, 20)",
        l_in * 100.0,
        (l_in - p_in) * 100.0
    );

    // --- held out: table from the train half, index is the test half.
    let train: Vec<usize> = (0..course.len()).filter(|i| i % 2 == 0).collect();
    let test: Vec<usize> = (0..course.len()).filter(|i| i % 2 == 1).collect();
    let tok = |t: &str| -> Vec<String> {
        t.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 2)
            .map(str::to_string)
            .collect()
    };

    let mut global: HashMap<String, usize> = HashMap::new();
    let mut per: HashMap<&str, HashMap<String, usize>> = HashMap::new();
    let mut n_train: HashMap<&str, usize> = HashMap::new();
    for &i in &train {
        let d = course[i].dept.as_str();
        *n_train.entry(d).or_default() += 1;
        for w in tok(&course[i].title) {
            *global.entry(w.clone()).or_default() += 1;
            *per.entry(d).or_default().entry(w).or_default() += 1;
        }
    }
    let total = train.len() as f64;
    let derived: HashMap<&str, Vec<String>> = per
        .iter()
        .map(|(&d, m)| {
            let n_d = n_train.get(d).copied().unwrap_or(1) as f64;
            let mut sc: Vec<(f64, &String)> = m
                .iter()
                .filter(|(_, &c)| c >= 2)
                .map(|(w, &c)| {
                    let inside = c as f64 / n_d;
                    let overall = global.get(w).copied().unwrap_or(1) as f64 / total;
                    (inside / (overall + 1e-9), w)
                })
                .collect();
            sc.sort_by(|a, b| b.0.total_cmp(&a.0));
            (d, sc.into_iter().take(20).map(|(_, w)| w.clone()).collect::<Vec<String>>())
        })
        .collect();

    let mut bb = IndexBuilder::new(schema());
    let mut local: HashMap<usize, usize> = HashMap::new();
    for (k, &i) in test.iter().enumerate() {
        local.insert(i, k);
        bb.add(&Doc::new([course[i].title.as_str(), course[i].dept.as_str()]));
    }
    let ixh = bb.build().expect("held");
    let mut want: HashMap<&str, Vec<usize>> = HashMap::new();
    for &i in &test {
        want.entry(course[i].dept.as_str()).or_default().push(local[&i]);
    }

    let score_h = |expand: bool| -> f64 {
        let mut prec = 0.0;
        let mut n = 0.0f64;
        for (name, _) in &dept {
            let Some(m) = want.get(*name) else { continue };
            if m.len() < 8 {
                continue;
            }
            n += 1.0;
            let mut q = name.to_string();
            if expand {
                if let Some(e) = derived.get(*name) {
                    for w in e {
                        q.push(' ');
                        q.push_str(w);
                    }
                }
            }
            // Ask for a large pool and truncate: the engine's expansion path does not fire here
                // (the query is not a bare facet value), so without this the held-out arm is
                // measuring pool eviction rather than the expansion's quality.
                let hit: Vec<usize> =
                    ixh.search(&q, 400).iter().take(10).map(|h| h.doc as usize).collect();
            prec += hit.iter().filter(|d| m.contains(d)).count() as f64 / 10.0;
        }
        prec / n.max(1.0)
    };
    let (p_h, l_h) = (score_h(false), score_h(true));

    println!("\n  --- held out ---");
    println!("  {:<34} {:>12.1}%", "plain", p_h * 100.0);
    println!(
        "  {:<34} {:>12.1}%   {:+.1} pt",
        "derived expansion",
        l_h * 100.0,
        (l_h - p_h) * 100.0
    );

    let gain_in = l_in - p_in;
    let gain_h = l_h - p_h;
    let retain = if gain_in > 0.0 { gain_h / gain_in } else { 0.0 };

    println!("\n  --- the condition, tested ---");
    println!("  {:<26} {:>12} {:>12} {:>12}", "corpus", "in-sample", "held-out", "retains");
    println!("  {:<26} {:>11.1}% {:>11.1}% {:>11.0}%", "presyo (grocery)", 34.9, 23.9, 68.0);
    println!("  {:<26} {:>11.1}% {:>11.1}% {:>11.0}%", "blead (business names)", 20.4, 6.7, 33.0);
    println!(
        "  {:<26} {:>11.1}% {:>11.1}% {:>11.0}%",
        "profstopick (courses)",
        gain_in * 100.0,
        gain_h * 100.0,
        retain * 100.0
    );
    println!(
        "\n  Prediction was: retention resembles presyo (68%), not blead (33%). Measured {:.0}%.\n  \
         {}",
        retain * 100.0,
        if retain >= 0.55 {
            "CONFIRMED — vocabulary reuse within a facet predicts what survives on unseen documents."
        } else if retain <= 0.40 {
            "REFUTED — the condition does not predict this corpus, and the explanation in p18 is wrong."
        } else {
            "AMBIGUOUS — between the two reference points; the condition is not sharp enough to be useful."
        }
    );
}
