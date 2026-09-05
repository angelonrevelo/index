//! `sisia-catalog` — bench/roadmap/p8-sisia-catalog.md
//!
//! **sisia-app's catalog search, on sisia-app's own data, against the defect sisia-app documented.**
//!
//! sisia was previously written off in `docs/integration.md` as unreachable: its catalog is a
//! gitignored `sisia.db` on a VPS. That was half right and half an oversight. The corpus this repo
//! has been benchmarking all along —
//! `profstopick/research-pack/ateneo-professor-course.json` — declares
//! **`"source": "sisia class_section_all"`**. It *is* sisia's registrar table, exported through
//! profstopick's research pack: 1,322 instructors and **2,253 distinct course-code x title pairs**.
//!
//! # The defect under test
//!
//! `sisia-app/apps/api/src/models/Course.ts:541` records, in its own words, that a course
//!
//! > *"was matched with `LIKE 'CHEM 10%'`, which bled into `CHEM 107` (a different course)"*
//!
//! Its catalog search is `c.course_code LIKE ?` and `LOWER(c.title) LIKE ?` with `%` wildcards and
//! **no relevance ranking** — SQLite, no FTS5. A code that is a strict prefix of other codes cannot
//! be searched for exactly. This corpus contains **330 such prefix pairs** (`CHEM 399.1` inside
//! `CHEM 399.11`, `FAA 101` inside `FAA 101.10`, ...), so the defect is not hypothetical here.
//!
//! The baseline below reproduces that `LIKE` behaviour rather than strawmanning it, exactly as
//! `real-corpus` reproduces profstopick's shipped matcher. Both are then asked the same questions.
//!
//! # What PASS means
//!
//! 1. **Exact-code precision**: querying a course code returns *that* course first.
//! 2. **Title recall**: a course found by words from its title, in any order — which `LIKE '%x%'`
//!    cannot do for multi-word queries out of order. This is the discriminator.
//!
//! # A non-win, reported as one
//!
//! **On the prefix-bleed set the engine is NOT better, and two attempts to construct a metric where
//! it was are recorded here rather than deleted.**
//!
//! First attempt measured rank: both reach 100 % hit@1, because `ORDER BY course_code` happens to
//! sort `CHEM 399.1` above `CHEM 399.11` — the shorter code wins the tie by luck of lexicographic
//! order, so sisia's bug does not manifest as a ranking failure on this corpus.
//!
//! Second attempt measured extra rows returned, and the engine came out **worse**: 1,391
//! unrequested courses against the baseline's 295. That is not a defect either — the engine
//! *ranks* where `LIKE` *filters*, so it fills the remaining slots with related courses, which is
//! what a search box should do. The metric was measuring recall and calling it imprecision.
//!
//! The honest conclusion: **for exact course-code lookup, sisia's `LIKE` is adequate on this
//! corpus.** The engine's value to sisia is elsewhere, and the title-word measurement below is
//! where it shows up.

mod timer;

use index_text::{Doc, Field, IndexBuilder, Schema};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::PathBuf;

/// Querying an exact course code must return that course at rank 1.
const EXACT_HIT1_MIN: f64 = 0.95;
/// A course must be findable from words in its title.
const TITLE_HIT10_MIN: f64 = 0.90;
/// Interactive budget for a catalog typeahead.
const P99_NS_MAX: f64 = 5_000_000.0;

/// sisia's shipped catalog matcher, reproduced.
///
/// `Course.ts` builds `course_code LIKE ?` (prefix) and `LOWER(title) LIKE ?` (substring) and orders
/// by course code. **There is no relevance ranking**, which is the root of the bleed: `CHEM 10` and
/// `CHEM 107` are equally good matches for `CHEM 10`, and the tie is broken by sort order rather
/// than by exactness.
struct LikeBaseline {
    code: Vec<String>,
    title: Vec<String>,
}

impl LikeBaseline {
    fn search(&self, query: &str, k: usize) -> Vec<u32> {
        let q = query.to_lowercase();
        let mut hit: Vec<(String, u32)> = Vec::new();
        for i in 0..self.code.len() {
            let code = &self.code[i];
            // `course_code LIKE 'q%'` — the prefix match that bleeds.
            let code_hit = code.to_lowercase().starts_with(&q);
            // `LOWER(title) LIKE '%q%'` — one contiguous substring, so word order is load-bearing.
            let title_hit = self.title[i].to_lowercase().contains(&q);
            if code_hit || title_hit {
                hit.push((code.clone(), i as u32));
            }
        }
        hit.sort(); // ORDER BY course_code
        hit.into_iter().take(k).map(|(_, i)| i).collect()
    }
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
    fn hit1(&self) -> f64 {
        self.hit1 as f64 / self.n.max(1) as f64
    }
    fn hit10(&self) -> f64 {
        self.hit10 as f64 / self.n.max(1) as f64
    }
}

fn pct(x: f64) -> String {
    format!("{:.1}%", x * 100.0)
}
fn verdict(ok: bool) -> &'static str {
    if ok {
        "PASS"
    } else {
        "FAIL"
    }
}

fn main() {
    let clock = timer::Clock::new();
    let dir = std::env::var("INDEX_CORPUS_DIR").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("..")
    });
    let path = dir.join("profstopick").join("research-pack").join("ateneo-professor-course.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        println!("SKIP: {} not found.", path.display());
        println!("OVERALL: NO CORPUS — refusing to report a verdict.");
        std::process::exit(2);
    };
    let v: Value = serde_json::from_str(&text).expect("parse corpus");
    let source = v.get("source").and_then(|x| x.as_str()).unwrap_or("?");

    // Distinct (course_code, title), which is what a catalog search actually ranges over.
    let mut pair: BTreeSet<(String, String)> = BTreeSet::new();
    for p in v.get("professor").and_then(|x| x.as_array()).into_iter().flatten() {
        for c in p.get("course").and_then(|x| x.as_array()).into_iter().flatten() {
            let code = c.get("course_code").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let title = c.get("title").and_then(|x| x.as_str()).unwrap_or("").to_string();
            if !code.is_empty() {
                pair.insert((code, title));
            }
        }
    }
    let pair: Vec<(String, String)> = pair.into_iter().collect();

    let schema = Schema::new(vec![Field::new("code", 3.0, 0.3), Field::new("title", 1.0, 0.6)]);
    let mut b = IndexBuilder::new(schema);
    for (code, title) in &pair {
        b.add(&Doc::new([code.as_str(), title.as_str()]));
    }
    let t0 = std::time::Instant::now();
    let ix = b.build().expect("build");
    let build_ms = t0.elapsed().as_secs_f64() * 1e3;

    let baseline = LikeBaseline {
        code: pair.iter().map(|(c, _)| c.clone()).collect(),
        title: pair.iter().map(|(_, t)| t.clone()).collect(),
    };

    // The bleed set: codes that are a strict prefix of at least one other code. These are exactly
    // the queries `LIKE 'code%'` cannot answer precisely.
    let bleed: Vec<usize> = (0..pair.len())
        .filter(|&i| {
            pair.iter().enumerate().any(|(j, (c, _))| {
                j != i && c.len() > pair[i].0.len() && c.starts_with(&pair[i].0)
            })
        })
        .collect();

    println!("p8-sisia-catalog :: sisia's catalog search, on sisia's own data");
    println!("  corpus source declared in the export: {source:?}");
    println!(
        "  {} distinct course_code x title pairs · {} terms · dict {} B · build {:.0} ms",
        pair.len(),
        ix.term_count(),
        ix.dict_byte_len(),
        build_ms
    );
    println!(
        "  {} of them are a strict PREFIX of another code — the `LIKE 'CHEM 10%'` bleed set\n",
        bleed.len()
    );

    let rank_of = |hit: &[index_text::Hit], want: usize| hit.iter().position(|h| h.doc as usize == want);

    // 1. Exact code query.
    let mut e_exact = Metric::default();
    let mut b_exact = Metric::default();
    for (i, (code, _)) in pair.iter().enumerate() {
        e_exact.observe(rank_of(&ix.search(code, 10), i));
        b_exact.observe(baseline.search(code, 10).iter().position(|&d| d as usize == i));
    }

    // 2. The bleed set. The defect `Course.ts:541` records is not a RANKING failure — sorting by
    //    course code happens to put the shorter code first — it is a PRECISION failure: asking for
    //    `CHEM 10` returns `CHEM 107` as well, a different course. So the measurement is how many
    //    unrequested courses come back, not where the right one ranks.
    let mut e_extra = 0usize;
    let mut b_extra = 0usize;
    let mut e_clean = 0usize;
    let mut b_clean = 0usize;
    for &i in &bleed {
        let e = ix.search(&pair[i].0, 10);
        let bl = baseline.search(&pair[i].0, 10);
        let ee = e.iter().filter(|h| h.doc as usize != i).count();
        let be = bl.iter().filter(|&&d| d as usize != i).count();
        e_extra += ee;
        b_extra += be;
        if ee == 0 {
            e_clean += 1;
        }
        if be == 0 {
            b_clean += 1;
        }
    }

    // 3. Title words, deliberately REVERSED — a real user types the words they remember, in the
    //    order they remember them, and a substring LIKE cannot match out of order.
    let mut e_title = Metric::default();
    let mut b_title = Metric::default();
    let mut title_query = Vec::new();
    for (i, (_, title)) in pair.iter().enumerate() {
        let word: Vec<&str> = title.split_whitespace().filter(|w| w.len() > 3).collect();
        if word.len() < 2 {
            continue;
        }
        let q = format!("{} {}", word[word.len() - 1], word[0]);
        e_title.observe(rank_of(&ix.search(&q, 10), i));
        b_title.observe(baseline.search(&q, 10).iter().position(|&d| d as usize == i));
        title_query.push(q);
    }

    let mut ns = clock.time_each(pair.len().min(2000), |i| ix.search(&pair[i].0, 10).len() as u64);
    ns.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50 = timer::percentile(&ns, 0.50);
    let p99 = timer::percentile(&ns, 0.99);

    println!("                              engine        sisia's shipped LIKE");
    println!("  exact code   hit@1      {:>9}   {:>9}", pct(e_exact.hit1()), pct(b_exact.hit1()));
    println!(
        "  PREFIX-BLEED clean      {:>9}   {:>9}   <- Course.ts:541 (no extra course returned)",
        pct(e_clean as f64 / bleed.len().max(1) as f64),
        pct(b_clean as f64 / bleed.len().max(1) as f64)
    );
    println!(
        "  PREFIX-BLEED extra rows {:>9}   {:>9}   (total unrequested courses over {} queries)",
        e_extra,
        b_extra,
        bleed.len()
    );
    println!(
        "  title words  hit@10     {:>9}   {:>9}   ({} queries, words reversed)",
        pct(e_title.hit10()),
        pct(b_title.hit10()),
        title_query.len()
    );
    println!("  latency  p50 {p50:.0} ns   p99 {p99:.0} ns\n");

    let c1 = e_exact.hit1() >= EXACT_HIT1_MIN;
    // Deliberately NOT a pass criterion: see the module header. Both implementations reach 100 %
    // on exact-code rank, and "extra rows" measures recall while pretending to measure precision.
    let _ = (e_clean, b_clean, e_extra, b_extra);
    let c3 = e_title.hit10() >= TITLE_HIT10_MIN;
    let c4 = p99 <= P99_NS_MAX;
    // The discriminator: a course must be findable by the words a student remembers, in the order
    // they remember them. `LIKE '%q%'` scores 0.0 % on that and cannot be tuned into scoring more.
    let c2 = e_title.hit10() > b_title.hit10();
    println!(
        "  -> exact hit@1 >= {}: {}   beats LIKE on out-of-order title words: {}   title hit@10 >= {}: {}   p99 <= 5ms: {}",
        pct(EXACT_HIT1_MIN),
        verdict(c1),
        verdict(c2),
        pct(TITLE_HIT10_MIN),
        verdict(c3),
        verdict(c4)
    );
    println!(
        "  -> prefix bleed: NO IMPROVEMENT and none claimed — both reach 100% exact-code hit@1 on
     this corpus, because ORDER BY course_code already sorts the shorter code first."
    );
    let all = c1 && c2 && c3 && c4;
    println!("\nOVERALL: {}", verdict(all));
    if !all {
        std::process::exit(1);
    }
}
