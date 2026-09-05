//! `real-corpus` — bench/roadmap/p6-real-corpus.md
//!
//! **The benchmark that decides whether this engine is worth adopting.** Everything else in this
//! repo has been measured on synthetic data. This runs against two corpora exported from the
//! actual production databases of two of the four consumer applications, and it measures the two
//! things those applications measured about themselves and could not fix:
//!
//! 1. **profstopick** — 1,322 Ateneo professors and 5,143 professor×course pairings
//!    (`profstopick/research-pack/ateneo-professor-course.json`, "Ateneo registrar snapshot,
//!    sisia class_section_all"). Its production measurement on 2026-08-17: **109 of 267 real
//!    searches returned nothing, 60 of them name-shaped**, every one cross-checked to a professor
//!    already in the corpus. Its matcher is 8 tiers of prefix and substring logic with **no edit
//!    distance**, so a transposed letter finds nothing. This bench reproduces that baseline and
//!    measures against it.
//!
//! 2. **presyo** — 500 gold cross-store product clusters, 2,440 real retailer listings
//!    (`presyo/tests/fixtures/recall-gold-cases.json`, "exported from prod 2026-06-13"). Two
//!    listings match iff they share a `gold_product_id`. This is the hardest real task in the
//!    house: find the *same physical product* in another store's catalogue from one store's raw
//!    name, where naming disagrees on word order, abbreviation and spacing.
//!
//! Both corpora are read from the sibling repos on disk. Point `INDEX_CORPUS_DIR` elsewhere to
//! relocate them; the bench skips a corpus it cannot find and says so rather than inventing data.

mod timer;

use index_text::{AliasTable, Doc, Field, IndexBuilder, Schema};
use serde_json::Value;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------------------------
// Acceptance thresholds. Each is derived from a consumer measurement, not from taste.
// ---------------------------------------------------------------------------------------------

/// A clean query for a professor by their indexed name must land them first. If this fails the
/// engine is worse than the prefix matcher it replaces.
const PROF_EXACT_HIT1_MIN: f64 = 0.95;
/// The row this project exists for. profstopick measured 109/267 (**40.8 %**) of real searches
/// returning nothing. A typo'd name must be found in the top 10 far more often than that.
const PROF_TYPO_HIT10_MIN: f64 = 0.90;
/// Cross-store retrieval on presyo's gold clusters. Its own Fellegi–Sunter blocker reaches 95.7 %
/// pair-completeness using brand keys, fingerprints and trigram bands built for this exact job;
/// a general-purpose text engine is expected to land below that, and 0.85 is the bar at which it
/// is useful as a *candidate generator* feeding that blocker.
const PRESYO_RECALL10_MIN: f64 = 0.85;
// The size guard admits **zero** violations, so it is asserted with `is_empty()` rather than a
// threshold constant: whenever the corpus contains a listing of the size the query asked for, the
// engine must return that size first. Any exception is a price-comparison bug.
/// Interactive budget. profstopick's whole post-fix match pass measures 0.43 ms; one keystroke
/// frame is 16.7 ms.
const QUERY_P99_NS_MAX: f64 = 5_000_000.0;

fn corpus_dir() -> PathBuf {
    if let Ok(d) = std::env::var("INDEX_CORPUS_DIR") {
        return PathBuf::from(d);
    }
    // Default: the sibling checkouts this repo was surveyed against.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("..")
}

fn read_json(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Deterministic RNG so the corrupted-query set is identical on every run.
#[inline]
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Corrupt one alphabetic character of a query, the way a real user does.
///
/// **Never touches position 0** (first-character protection is a policy the engine relies on) and
/// **never touches a digit** — corrupting a size would make the benchmark measure the opposite of
/// what the engine promises.
fn corrupt(q: &str, st: &mut u64) -> String {
    let ch: Vec<char> = q.chars().collect();
    let candidate: Vec<usize> =
        (1..ch.len()).filter(|&i| ch[i].is_alphabetic()).collect();
    if candidate.len() < 2 {
        return q.to_string();
    }
    let i = candidate[(splitmix64(st) % candidate.len() as u64) as usize];
    let mut out = ch.clone();
    match splitmix64(st) % 3 {
        0 => {
            out.remove(i);
        }
        1 => out[i] = char::from(b'a' + (splitmix64(st) % 26) as u8),
        _ => {
            if i + 1 < out.len() && out[i + 1].is_alphabetic() {
                out.swap(i, i + 1)
            } else {
                out.remove(i);
            }
        }
    }
    out.into_iter().collect()
}

// ---------------------------------------------------------------------------------------------
// The baseline: profstopick's shipped matcher, reproduced.
// ---------------------------------------------------------------------------------------------

/// A faithful reproduction of the tier structure in `profstopick/src/lib/search-match.ts`:
/// label-prefix, label-token-prefix, label-substring, then the sublabel equivalents.
/// **There is no edit distance anywhere in it** — which is the whole point.
struct PrefixBaseline {
    label: Vec<String>,
    sublabel: Vec<String>,
}

impl PrefixBaseline {
    fn tier(hay: &str, q: &str) -> Option<u8> {
        if hay.starts_with(q) {
            return Some(0);
        }
        if hay.split_whitespace().any(|w| w.starts_with(q)) {
            return Some(1);
        }
        if hay.contains(q) {
            return Some(2);
        }
        None
    }

    fn search(&self, query: &str, k: usize) -> Vec<u32> {
        let q = index_text::fold(query);
        let token: Vec<&str> = q.split_whitespace().collect();
        let mut hit: Vec<(u8, u32)> = Vec::new();
        for i in 0..self.label.len() {
            // Every query token must be placeable somewhere, mirroring the baseline's
            // all-token requirement.
            let mut worst = 0u8;
            let mut ok = true;
            for t in &token {
                let a = Self::tier(&self.label[i], t);
                let b = Self::tier(&self.sublabel[i], t).map(|x| x + 3);
                match a.into_iter().chain(b).min() {
                    Some(tier) => worst = worst.max(tier),
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok && !token.is_empty() {
                hit.push((worst, i as u32));
            }
        }
        hit.sort();
        hit.into_iter().take(k).map(|(_, d)| d).collect()
    }
}

// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct Metric {
    n: usize,
    hit1: usize,
    hit10: usize,
    recip_rank_sum: f64,
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
                self.recip_rank_sum += 1.0 / (r + 1) as f64;
            }
        }
    }
    fn hit1(&self) -> f64 {
        self.hit1 as f64 / self.n.max(1) as f64
    }
    fn hit10(&self) -> f64 {
        self.hit10 as f64 / self.n.max(1) as f64
    }
    fn mrr(&self) -> f64 {
        self.recip_rank_sum / self.n.max(1) as f64
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

// ---------------------------------------------------------------------------------------------
// Corpus 1 — profstopick
// ---------------------------------------------------------------------------------------------

fn run_profstopick(clock: &timer::Clock, dir: &Path) -> Option<bool> {
    let path = dir.join("profstopick").join("research-pack").join("ateneo-professor-course.json");
    let v = read_json(&path)?;
    let prof = v.get("professor")?.as_array()?;

    let schema = Schema::new(vec![
        Field::new("name", 3.0, 0.4),
        Field::new("course_code", 1.5, 0.4),
        Field::new("course_title", 1.0, 0.6),
    ]);
    let mut b = IndexBuilder::new(schema);
    let mut name: Vec<String> = Vec::new();
    let mut sub: Vec<String> = Vec::new();

    for p in prof {
        let n = p.get("instructor_name").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let mut code = String::new();
        let mut title = String::new();
        if let Some(c) = p.get("course").and_then(|x| x.as_array()) {
            for course in c {
                if let Some(s) = course.get("course_code").and_then(|x| x.as_str()) {
                    code.push_str(s);
                    code.push(' ');
                }
                if let Some(s) = course.get("title").and_then(|x| x.as_str()) {
                    title.push_str(s);
                    title.push(' ');
                }
            }
        }
        b.add(&Doc::new([n.clone(), code.clone(), title.clone()]));
        name.push(index_text::fold(&n));
        sub.push(index_text::fold(&format!("{code} {title}")));
    }

    let t0 = std::time::Instant::now();
    let ix = b.build().ok()?;
    let build_ms = t0.elapsed().as_secs_f64() * 1e3;
    let baseline = PrefixBaseline { label: name, sublabel: sub };

    println!("=== corpus 1: profstopick — Ateneo registrar snapshot (REAL) ===");
    println!(
        "  {} professors · {} terms · dict {} B ({:.2} B/term) · build {:.0} ms",
        ix.doc_count(),
        ix.term_count(),
        ix.dict_byte_len(),
        ix.dict_byte_len() as f64 / ix.term_count().max(1) as f64,
        build_ms
    );

    // Query sets: the exact indexed name, and a one-edit corruption of it.
    let mut st = 0xC0FFEEu64;
    let exact_q: Vec<(String, u32)> =
        prof.iter().enumerate().map(|(i, p)| {
            (p.get("instructor_name").and_then(|x| x.as_str()).unwrap_or("").to_string(), i as u32)
        }).collect();
    let typo_q: Vec<(String, u32)> =
        exact_q.iter().map(|(q, d)| (corrupt(q, &mut st), *d)).collect();

    let rank_of = |hit: &[index_text::Hit], want: u32| hit.iter().position(|h| h.doc == want);

    let mut m_exact = Metric::default();
    let mut m_typo = Metric::default();
    let mut m_base_exact = Metric::default();
    let mut m_base_typo = Metric::default();

    for (q, want) in &exact_q {
        m_exact.observe(rank_of(&ix.search(q, 10), *want));
        m_base_exact.observe(baseline.search(q, 10).iter().position(|&d| d == *want));
    }
    for (q, want) in &typo_q {
        m_typo.observe(rank_of(&ix.search(q, 10), *want));
        m_base_typo.observe(baseline.search(q, 10).iter().position(|&d| d == *want));
    }

    // Latency on the typo set — the expensive path, so this is the honest number to report.
    let mut ns = clock.time_each(typo_q.len(), |i| ix.search(&typo_q[i].0, 10).len() as u64);
    ns.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50 = timer::percentile(&ns, 0.50);
    let p99 = timer::percentile(&ns, 0.99);

    println!("                      engine              baseline (profstopick's shipped matcher)");
    println!(
        "  exact  hit@1     {:>8}            {:>8}",
        pct(m_exact.hit1()),
        pct(m_base_exact.hit1())
    );
    println!(
        "  exact  hit@10    {:>8}            {:>8}",
        pct(m_exact.hit10()),
        pct(m_base_exact.hit10())
    );
    println!(
        "  TYPO   hit@1     {:>8}            {:>8}",
        pct(m_typo.hit1()),
        pct(m_base_typo.hit1())
    );
    println!(
        "  TYPO   hit@10    {:>8}            {:>8}   <- the 109/267 problem",
        pct(m_typo.hit10()),
        pct(m_base_typo.hit10())
    );
    println!(
        "  TYPO   MRR@10    {:>8}            {:>8}",
        format!("{:.3}", m_typo.mrr()),
        format!("{:.3}", m_base_typo.mrr())
    );
    println!("  zero-result rate  engine {:>7}   baseline {:>7}",
        pct(1.0 - m_typo.hit10()),
        pct(1.0 - m_base_typo.hit10()));
    println!("  latency (typo set)  p50 {p50:.0} ns   p99 {p99:.0} ns");

    let c1 = m_exact.hit1() >= PROF_EXACT_HIT1_MIN;
    let c2 = m_typo.hit10() >= PROF_TYPO_HIT10_MIN;
    let c3 = p99 <= QUERY_P99_NS_MAX;
    let c4 = m_typo.hit10() > m_base_typo.hit10();
    println!(
        "  -> exact hit@1 >= {}: {}   typo hit@10 >= {}: {}   p99 <= 5ms: {}   beats baseline: {}",
        pct(PROF_EXACT_HIT1_MIN),
        verdict(c1),
        pct(PROF_TYPO_HIT10_MIN),
        verdict(c2),
        verdict(c3),
        verdict(c4)
    );
    println!();
    Some(c1 && c2 && c3 && c4)
}

// ---------------------------------------------------------------------------------------------
// Corpus 2 — presyo
// ---------------------------------------------------------------------------------------------

fn run_presyo(clock: &timer::Clock, dir: &Path) -> Option<bool> {
    let path = dir.join("presyo").join("tests").join("fixtures").join("recall-gold-cases.json");
    let v = read_json(&path)?;
    let cluster = v.get("clusters")?.as_array()?;

    let schema = Schema::new(vec![
        Field::new("brand", 3.0, 0.4),
        Field::new("name", 2.0, 0.4),
    ]);
    let mut b = IndexBuilder::new(schema).with_alias(AliasTable::philippine_grocery());

    // doc -> gold cluster id, so "same product" is ground truth rather than a guess.
    let mut doc_gold: Vec<u64> = Vec::new();
    let mut doc_name: Vec<String> = Vec::new();
    // The query set: one listing per cluster, with at least one sibling to find.
    let mut probe: Vec<(String, u64)> = Vec::new();

    for c in cluster {
        let gold = c.get("gold_product_id").and_then(|x| x.as_u64()).unwrap_or(0);
        let brand = c.get("gold_brand").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let listing = match c.get("listings").and_then(|x| x.as_array()) {
            Some(l) if l.len() >= 2 => l,
            _ => continue,
        };
        for (li, l) in listing.iter().enumerate() {
            let raw = l.get("raw_name").and_then(|x| x.as_str()).unwrap_or("").to_string();
            if raw.is_empty() {
                continue;
            }
            if li == 0 {
                // The first listing is the query; it is deliberately NOT indexed, so a hit can
                // only come from a *different store's* listing of the same product.
                probe.push((raw, gold));
                continue;
            }
            b.add(&Doc::new([brand.clone(), raw.clone()]));
            doc_gold.push(gold);
            doc_name.push(raw);
        }
    }

    let t0 = std::time::Instant::now();
    let ix = b.build().ok()?;
    let build_ms = t0.elapsed().as_secs_f64() * 1e3;

    println!("=== corpus 2: presyo — gold cross-store product clusters (REAL) ===");
    println!(
        "  {} indexed listings from {} clusters · {} terms · dict {} B ({:.2} B/term) · build {:.0} ms",
        ix.doc_count(),
        probe.len(),
        ix.term_count(),
        ix.dict_byte_len(),
        ix.dict_byte_len() as f64 / ix.term_count().max(1) as f64,
        build_ms
    );
    println!("  task: given ONE store's raw name, find the SAME product in a DIFFERENT store.");

    let rank_of_gold = |hit: &[index_text::Hit], gold: u64| -> Option<usize> {
        hit.iter().position(|h| doc_gold[h.doc as usize] == gold)
    };

    let mut m_clean = Metric::default();
    let mut m_typo = Metric::default();
    let mut st = 0xBADF00Du64;
    let typo_probe: Vec<(String, u64)> =
        probe.iter().map(|(q, g)| (corrupt(q, &mut st), *g)).collect();

    for (q, gold) in &probe {
        m_clean.observe(rank_of_gold(&ix.search(q, 10), *gold));
    }
    // `INDEX_DIAG=1` also scores the typo set exhaustively. Any divergence here means MaxScore's
    // pruning is unsound, which is the only thing that would make its speed worthless. Off by
    // default because it is O(postings) per query.
    let diag = std::env::var("INDEX_DIAG").is_ok();
    let mut m_typo_ex = Metric::default();
    for (q, gold) in &typo_probe {
        m_typo.observe(rank_of_gold(&ix.search(q, 10), *gold));
        if diag {
            m_typo_ex.observe(rank_of_gold(&ix.search_exhaustive(q, 10), *gold));
        }
    }
    if diag {
        println!(
            "  DIAG pruned vs exhaustive recall@10: {} vs {}  (must be identical)",
            pct(m_typo.hit10()),
            pct(m_typo_ex.hit10())
        );
    }

    // The correctness guard, measured on real retailer text.
    //
    // The claim under test is precise: **when the corpus contains a listing with the size the
    // query asked for, the engine must return that size first.** It is deliberately not "the top
    // hit always has the queried size" — the corpus genuinely does not always contain the asked-for
    // size, and refusing to answer at all would be worse than answering with a near neighbour. The
    // cases where no same-size listing exists are counted and reported separately rather than
    // folded into a pass.
    let mut size_stated = 0usize;
    let mut size_available = 0usize;
    let mut size_violation: Vec<(String, String)> = Vec::new();
    let mut size_absent = 0usize;
    for (q, _) in &probe {
        let Some(qs) = mass_or_volume(q) else { continue };
        size_stated += 1;
        // Does any indexed listing actually carry that size?
        let exists = doc_name.iter().any(|n| mass_or_volume(n) == Some(qs));
        if !exists {
            size_absent += 1;
            continue;
        }
        size_available += 1;
        let hit = ix.search(q, 1);
        if let Some(h) = hit.first() {
            let hs = mass_or_volume(&doc_name[h.doc as usize]);
            if hs != Some(qs) {
                size_violation.push((q.clone(), doc_name[h.doc as usize].clone()));
            }
        }
    }

    let mut ns = clock.time_each(probe.len(), |i| ix.search(&probe[i].0, 10).len() as u64);
    ns.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50 = timer::percentile(&ns, 0.50);
    let p99 = timer::percentile(&ns, 0.99);

    println!("  clean  recall@10 {:>8}   hit@1 {:>8}   MRR@10 {:.3}",
        pct(m_clean.hit10()), pct(m_clean.hit1()), m_clean.mrr());
    println!("  TYPO   recall@10 {:>8}   hit@1 {:>8}   MRR@10 {:.3}",
        pct(m_typo.hit10()), pct(m_typo.hit1()), m_typo.mrr());
    println!(
        "  size guard: {size_stated} of {} queries stated a mass/volume; {size_available} had that",
        probe.len()
    );
    println!(
        "              size present in the corpus -> {} violations ({size_absent} queries asked for a size no listing carries)",
        size_violation.len()
    );
    for (q, got) in size_violation.iter().take(5) {
        println!("      VIOLATION  query {q:?}  ->  {got:?}");
    }
    println!("  latency  p50 {p50:.0} ns   p99 {p99:.0} ns");

    let c1 = m_clean.hit10() >= PRESYO_RECALL10_MIN;
    let c2 = size_violation.is_empty();
    let c3 = p99 <= QUERY_P99_NS_MAX;
    println!(
        "  -> recall@10 >= {}: {}   zero size violations: {}   p99 <= 5ms: {}",
        pct(PRESYO_RECALL10_MIN),
        verdict(c1),
        verdict(c2),
        verdict(c3)
    );
    println!();
    Some(c1 && c2 && c3)
}

/// The mass-or-volume quantity a raw product name states, if any.
///
/// **Pack counts are deliberately excluded.** `6S` in `MILKMAN YOGURT DRINK STRAWBERRY 6S 100ML`
/// parses as 6 pieces and is not a size; comparing it against the query's `100 ml` produced three
/// spurious violations in the first version of this guard. A benchmark that reports the wrong
/// defect is worse than no benchmark.
fn mass_or_volume(s: &str) -> Option<index_text::Quantity> {
    index_text::tokenize(s)
        .into_iter()
        .filter(|t| t.is_numeric)
        .filter_map(|t| index_text::parse_quantity(&t.text))
        .find(|q| !matches!(q.unit, index_text::BaseUnit::Piece))
}

fn main() {
    let clock = timer::Clock::new();
    println!("p6-real-corpus :: index-text against two production corpora");
    println!("clock backend: {} ({:.3} cycles/ns)\n", clock.backend(), clock.cycles_per_ns());

    let dir = corpus_dir();
    let mut ran = 0usize;
    let mut all_pass = true;

    match run_profstopick(&clock, &dir) {
        Some(ok) => {
            ran += 1;
            all_pass &= ok;
        }
        None => println!(
            "SKIP corpus 1: profstopick/research-pack/ateneo-professor-course.json not found under {}\n",
            dir.display()
        ),
    }
    match run_presyo(&clock, &dir) {
        Some(ok) => {
            ran += 1;
            all_pass &= ok;
        }
        None => println!(
            "SKIP corpus 2: presyo/tests/fixtures/recall-gold-cases.json not found under {}\n",
            dir.display()
        ),
    }

    if ran == 0 {
        println!("OVERALL: NO CORPUS FOUND — set INDEX_CORPUS_DIR to the directory holding the");
        println!("         profstopick/ and presyo/ checkouts. Refusing to report a verdict.");
        std::process::exit(2);
    }
    println!("OVERALL: {} ({ran}/2 corpora available)", verdict(all_pass));
    if !all_pass {
        std::process::exit(1);
    }
}
